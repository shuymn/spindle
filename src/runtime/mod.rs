use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};

use spindle_extension_sdk::{
    ActionInvocation, ActionOutput, ExtensionRegistration, HostRequest, HostResponse,
};

use self::{path::resolve_manifest_path, stdio::StdioJsonlSession};
use crate::{ExtensionManifest, ExtensionRuntime, RegisteredExtension, SpindleError};

mod path;
mod stdio;

#[cfg(test)]
mod tests;

const DEFAULT_HOST_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Owns running extension host sessions for dispatch.
#[derive(Debug)]
pub struct ExtensionRuntimeHost {
    stdio_sessions: BTreeMap<StdioSessionKey, StdioJsonlSession>,
    host_request_timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct StdioSessionKey {
    id: String,
    version: String,
    manifest_path: PathBuf,
    entrypoint: String,
}

impl StdioSessionKey {
    fn from_extension(extension: &RegisteredExtension) -> Result<Self, SpindleError> {
        Ok(Self {
            id: extension.id.clone(),
            version: extension.version.clone(),
            manifest_path: extension.manifest_path.clone(),
            entrypoint: registered_entrypoint(extension)?.to_owned(),
        })
    }
}

impl Default for ExtensionRuntimeHost {
    fn default() -> Self {
        Self {
            stdio_sessions: BTreeMap::new(),
            host_request_timeout: DEFAULT_HOST_REQUEST_TIMEOUT,
        }
    }
}

impl ExtensionRuntimeHost {
    /// Create an empty runtime host.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a runtime host with an explicit extension host request timeout.
    #[must_use]
    pub const fn with_timeout(host_request_timeout: Duration) -> Self {
        Self {
            stdio_sessions: BTreeMap::new(),
            host_request_timeout,
        }
    }

    /// Load extension-owned registration through the manifest runtime.
    ///
    /// # Errors
    ///
    /// Returns an error when the runtime host cannot start, cannot speak the
    /// extension protocol, or receives an invalid registration response.
    pub fn load_registration(
        &mut self,
        manifest: &ExtensionManifest,
        manifest_path: &Path,
    ) -> Result<Option<ExtensionRegistration>, SpindleError> {
        match manifest.runtime {
            ExtensionRuntime::Recipe => Ok(None),
            ExtensionRuntime::StdioJsonl => {
                let entrypoint = manifest_entrypoint(manifest)?;
                let mut session =
                    StdioJsonlSession::spawn(&manifest.id, manifest_path, entrypoint)?;
                let response =
                    session.request(&HostRequest::Register, self.host_request_timeout)?;
                let registration = registration_response(&manifest.id, response)?;
                session.shutdown(self.host_request_timeout)?;
                Ok(Some(registration))
            }
        }
    }

    /// Invoke an installed extension action through its runtime.
    ///
    /// # Errors
    ///
    /// Returns an error when the runtime host cannot execute the action or the
    /// action returns invalid output.
    pub fn invoke_action(
        &mut self,
        extension: &RegisteredExtension,
        action: &str,
        invocation: &ActionInvocation,
    ) -> Result<ActionOutput, SpindleError> {
        match extension.runtime {
            ExtensionRuntime::Recipe => Ok(ActionOutput::empty()),
            ExtensionRuntime::StdioJsonl => {
                let session_key = StdioSessionKey::from_extension(extension)?;
                let response = {
                    let timeout = self.host_request_timeout;
                    let session = self.stdio_session(extension, &session_key)?;
                    session.request(
                        &HostRequest::Invoke {
                            invocation: Box::new(invocation.clone()),
                        },
                        timeout,
                    )
                };
                if matches!(response, Err(SpindleError::ExtensionHostTimedOut { .. })) {
                    self.stdio_sessions.remove(&session_key);
                }
                let response = response?;
                action_response(&extension.id, action, response)
            }
        }
    }

    fn stdio_session(
        &mut self,
        extension: &RegisteredExtension,
        key: &StdioSessionKey,
    ) -> Result<&mut StdioJsonlSession, SpindleError> {
        if !self.stdio_sessions.contains_key(key) {
            let session =
                StdioJsonlSession::spawn(&extension.id, &extension.manifest_path, &key.entrypoint)?;
            self.stdio_sessions.insert(key.clone(), session);
        }

        self.stdio_sessions
            .get_mut(key)
            .ok_or_else(|| SpindleError::ExtensionHostClosed {
                extension: extension.id.clone(),
            })
    }

    /// Shut down all running stdio extension hosts.
    ///
    /// # Errors
    ///
    /// Returns an error when any running host fails to acknowledge shutdown.
    pub fn shutdown(&mut self) -> Result<(), SpindleError> {
        let sessions = std::mem::take(&mut self.stdio_sessions);
        for (_key, mut session) in sessions {
            session.shutdown(self.host_request_timeout)?;
        }
        Ok(())
    }

    /// Drop any cached stdio host session for an extension id.
    pub fn invalidate_extension(&mut self, extension: &str) {
        let matching = self
            .stdio_sessions
            .keys()
            .filter(|key| key.id == extension)
            .cloned()
            .collect::<Vec<_>>();
        for key in matching {
            let Some(mut session) = self.stdio_sessions.remove(&key) else {
                continue;
            };
            session.terminate();
        }
    }
}

impl Drop for ExtensionRuntimeHost {
    fn drop(&mut self) {
        for (_key, mut session) in std::mem::take(&mut self.stdio_sessions) {
            session.terminate();
        }
    }
}

fn timeout_millis(timeout: Duration) -> u64 {
    u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX)
}

fn registration_response(
    extension: &str,
    response: HostResponse,
) -> Result<ExtensionRegistration, SpindleError> {
    match response {
        HostResponse::Registration { registration } => Ok(registration),
        HostResponse::Error { error } => Err(SpindleError::ExtensionRegistrationFailed {
            extension: String::from(extension),
            status: None,
            stderr: error,
        }),
        response => Err(unexpected_host_response(extension, &response)),
    }
}

fn action_response(
    extension: &str,
    action: &str,
    response: HostResponse,
) -> Result<ActionOutput, SpindleError> {
    match response {
        HostResponse::ActionOutput { output } => Ok(output),
        HostResponse::Error { error } => Err(SpindleError::ExtensionActionFailed {
            extension: String::from(extension),
            action: String::from(action),
            status: None,
            stderr: error,
        }),
        response => Err(unexpected_host_response(extension, &response)),
    }
}

fn unexpected_host_response(extension: &str, response: &HostResponse) -> SpindleError {
    SpindleError::ExtensionHostError {
        extension: String::from(extension),
        message: format!("unexpected host response: {response:?}"),
    }
}

fn manifest_entrypoint(manifest: &ExtensionManifest) -> Result<&str, SpindleError> {
    manifest
        .entrypoint
        .as_deref()
        .ok_or(SpindleError::InvalidField {
            field: "entrypoint",
            reason: "is required unless runtime is recipe",
        })
}

fn registered_entrypoint(extension: &RegisteredExtension) -> Result<&str, SpindleError> {
    extension
        .entrypoint
        .as_deref()
        .ok_or_else(|| SpindleError::MissingExtensionEntrypoint {
            extension: extension.id.clone(),
        })
}
