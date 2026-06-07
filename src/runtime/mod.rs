use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use spindle_extension_sdk::{
    ActionInvocation, ActionOutput, ExtensionRegistration, HostRequest, HostResponse,
};

pub use self::path::resolve_manifest_path;
use self::stdio::StdioJsonlSession;
use crate::{
    ExtensionManifest, ExtensionRuntime, RegisteredExtension, SpindleError, extension::sha256_file,
};

mod path;
mod stdio;

#[cfg(test)]
mod tests;

const DEFAULT_HOST_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Owns running extension host sessions for dispatch.
#[derive(Debug)]
pub struct ExtensionRuntimeHost {
    inner: Arc<RuntimeInner>,
}

#[derive(Debug)]
struct RuntimeInner {
    stdio_sessions: Mutex<StdioSessionMap>,
    stdio_session_creations: Mutex<StdioSessionCreationMap>,
    session_epoch: AtomicU64,
    host_request_timeout: Duration,
}

impl RuntimeInner {
    const fn new(host_request_timeout: Duration) -> Self {
        Self {
            stdio_sessions: Mutex::new(BTreeMap::new()),
            stdio_session_creations: Mutex::new(BTreeMap::new()),
            session_epoch: AtomicU64::new(0),
            host_request_timeout,
        }
    }

    fn bump_session_epoch(&self) {
        self.session_epoch.fetch_add(1, Ordering::AcqRel);
    }
}

type StdioSessionMap = BTreeMap<StdioSessionKey, Arc<Mutex<StdioJsonlSession>>>;
type StdioSessionCreationMap = BTreeMap<StdioSessionKey, Arc<Mutex<()>>>;
type StdioSessionMapGuard<'a> = MutexGuard<'a, StdioSessionMap>;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct StdioSessionKey {
    id: String,
    version: String,
    manifest_path: PathBuf,
    execution_path: PathBuf,
}

impl StdioSessionKey {
    fn from_extension(extension: &RegisteredExtension) -> Result<Self, SpindleError> {
        Ok(Self {
            id: extension.id.clone(),
            version: extension.version.clone(),
            manifest_path: extension.manifest_path.clone(),
            execution_path: extension_execution_path(extension)?,
        })
    }
}

impl Default for ExtensionRuntimeHost {
    fn default() -> Self {
        Self {
            inner: Arc::new(RuntimeInner::new(DEFAULT_HOST_REQUEST_TIMEOUT)),
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
    pub fn with_timeout(host_request_timeout: Duration) -> Self {
        Self {
            inner: Arc::new(RuntimeInner::new(host_request_timeout)),
        }
    }

    /// Load extension-owned registration through the manifest runtime.
    ///
    /// # Errors
    ///
    /// Returns an error when the runtime host cannot start, cannot speak the
    /// extension protocol, or receives an invalid registration response.
    pub fn load_registration(
        &self,
        manifest: &ExtensionManifest,
        manifest_path: &Path,
    ) -> Result<Option<ExtensionRegistration>, SpindleError> {
        match manifest.runtime {
            ExtensionRuntime::Recipe => Ok(None),
            ExtensionRuntime::StdioJsonl => {
                let entrypoint = manifest_entrypoint(manifest)?;
                let executable = resolve_manifest_path(manifest_path, entrypoint);
                let mut session = StdioJsonlSession::spawn(&manifest.id, &executable)?;
                let response =
                    session.request(HostRequest::Register, self.inner.host_request_timeout)?;
                let registration = registration_response(&manifest.id, response)?;
                session.shutdown(self.inner.host_request_timeout)?;
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
        &self,
        extension: &RegisteredExtension,
        action: &str,
        invocation: &ActionInvocation,
    ) -> Result<ActionOutput, SpindleError> {
        match extension.runtime {
            ExtensionRuntime::Recipe => Ok(ActionOutput::empty()),
            ExtensionRuntime::StdioJsonl => {
                verify_runtime_trust(extension)?;
                let session_key = StdioSessionKey::from_extension(extension)?;
                let session = self.stdio_session(extension, &session_key)?;
                let response = {
                    let timeout = self.inner.host_request_timeout;
                    let mut session =
                        session
                            .lock()
                            .map_err(|_error| SpindleError::InvalidField {
                                field: "runtime",
                                reason: "session lock poisoned",
                            })?;
                    session.request(
                        HostRequest::Invoke {
                            invocation: invocation.clone(),
                        },
                        timeout,
                    )
                };
                if response_breaks_session(&response) {
                    self.remove_session_if_cached(&session_key, &session)?;
                }
                let response = response?;
                if invoke_response_breaks_session(&response) {
                    self.remove_session_if_cached(&session_key, &session)?;
                    return Err(unexpected_host_response(&extension.id, &response));
                }
                action_response(&extension.id, action, response)
            }
        }
    }

    fn stdio_session(
        &self,
        extension: &RegisteredExtension,
        key: &StdioSessionKey,
    ) -> Result<Arc<Mutex<StdioJsonlSession>>, SpindleError> {
        if let Some(session) = self.lookup_session(key)? {
            return Ok(session);
        }

        let creation_lock = self.creation_lock(key)?;
        let _creation = creation_lock
            .lock()
            .map_err(|_error| SpindleError::InvalidField {
                field: "runtime",
                reason: "session creation lock poisoned",
            })?;

        if let Some(session) = self.lookup_session(key)? {
            return Ok(session);
        }

        let epoch = self.inner.session_epoch.load(Ordering::Acquire);
        let spawned = Arc::new(Mutex::new(StdioJsonlSession::spawn(
            &extension.id,
            &key.execution_path,
        )?));

        if self.inner.session_epoch.load(Ordering::Acquire) != epoch {
            if let Ok(mut session) = spawned.lock() {
                session.terminate();
            }
            return Err(SpindleError::ExtensionHostClosed {
                extension: extension.id.clone(),
            });
        }

        self.lock_sessions()?.insert(key.clone(), spawned.clone());
        Ok(spawned)
    }

    /// Shut down all running stdio extension hosts.
    ///
    /// # Errors
    ///
    /// Returns an error when any running host fails to acknowledge shutdown.
    pub fn shutdown(&self) -> Result<(), SpindleError> {
        let sessions =
            {
                let mut sessions = self.inner.stdio_sessions.lock().map_err(|_error| {
                    SpindleError::InvalidField {
                        field: "runtime",
                        reason: "session map lock poisoned",
                    }
                })?;
                std::mem::take(&mut *sessions)
            };
        self.inner.bump_session_epoch();
        self.clear_creation_locks()?;
        for (_key, session) in sessions {
            let mut session = session
                .lock()
                .map_err(|_error| SpindleError::InvalidField {
                    field: "runtime",
                    reason: "session lock poisoned",
                })?;
            session.shutdown(self.inner.host_request_timeout)?;
        }
        Ok(())
    }

    /// Drop any cached stdio host session for an extension id.
    pub fn invalidate_extension(&self, extension: &str) {
        let sessions_to_terminate = {
            let Ok(mut sessions) = self.inner.stdio_sessions.lock() else {
                return;
            };
            let mut retained = BTreeMap::new();
            let mut removed = Vec::new();
            for (key, session) in std::mem::take(&mut *sessions) {
                if key.id == extension {
                    removed.push(session);
                } else {
                    retained.insert(key, session);
                }
            }
            *sessions = retained;
            removed
        };
        self.inner.bump_session_epoch();
        self.remove_creation_locks_for_extension(extension);

        for session in sessions_to_terminate {
            if let Ok(mut session) = session.lock() {
                session.terminate();
            }
        }
    }

    fn remove_session_if_cached(
        &self,
        key: &StdioSessionKey,
        session: &Arc<Mutex<StdioJsonlSession>>,
    ) -> Result<(), SpindleError> {
        let removed = {
            let mut sessions = self.lock_sessions()?;
            match sessions.get(key) {
                Some(cached) if Arc::ptr_eq(cached, session) => sessions.remove(key),
                _ => None,
            }
        };

        if let Some(removed) = removed {
            if let Ok(mut removed) = removed.lock() {
                removed.terminate();
            }
            self.remove_creation_lock(key)?;
        }

        Ok(())
    }

    fn lookup_session(
        &self,
        key: &StdioSessionKey,
    ) -> Result<Option<Arc<Mutex<StdioJsonlSession>>>, SpindleError> {
        Ok(self.lock_sessions()?.get(key).cloned())
    }

    fn creation_lock(&self, key: &StdioSessionKey) -> Result<Arc<Mutex<()>>, SpindleError> {
        Ok(self
            .lock_session_creations()?
            .entry(key.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone())
    }

    fn remove_creation_lock(&self, key: &StdioSessionKey) -> Result<(), SpindleError> {
        self.lock_session_creations()?.remove(key);
        Ok(())
    }

    fn remove_creation_locks_for_extension(&self, extension: &str) {
        let Ok(mut creations) = self.lock_session_creations() else {
            return;
        };
        creations.retain(|key, _lock| key.id != extension);
    }

    fn clear_creation_locks(&self) -> Result<(), SpindleError> {
        self.lock_session_creations()?.clear();
        Ok(())
    }

    fn lock_sessions(&self) -> Result<StdioSessionMapGuard<'_>, SpindleError> {
        self.inner
            .stdio_sessions
            .lock()
            .map_err(|_error| SpindleError::InvalidField {
                field: "runtime",
                reason: "session map lock poisoned",
            })
    }

    fn lock_session_creations(
        &self,
    ) -> Result<MutexGuard<'_, StdioSessionCreationMap>, SpindleError> {
        self.inner
            .stdio_session_creations
            .lock()
            .map_err(|_error| SpindleError::InvalidField {
                field: "runtime",
                reason: "session creation map lock poisoned",
            })
    }
}

const fn invoke_response_breaks_session(response: &HostResponse) -> bool {
    !matches!(
        response,
        HostResponse::ActionOutput { .. } | HostResponse::Error { .. }
    )
}

fn response_breaks_session(response: &Result<HostResponse, SpindleError>) -> bool {
    matches!(
        response,
        Err(SpindleError::ExtensionHostTimedOut { .. }
            | SpindleError::ExtensionHostClosed { .. }
            | SpindleError::ExtensionHostProtocolInvalid { .. }
            | SpindleError::MessageTooLarge { .. })
    ) || matches!(
        response,
        Err(SpindleError::Io(error)) if error.kind() == std::io::ErrorKind::InvalidData
    )
}

fn verify_runtime_trust(extension: &RegisteredExtension) -> Result<(), SpindleError> {
    let Some(trust) = &extension.runtime_trust else {
        return Ok(());
    };
    let current = sha256_file(&trust.entrypoint_path)?;
    if current == trust.entrypoint_sha256 {
        return Ok(());
    }
    Err(SpindleError::ExtensionTrustChanged {
        extension: extension.id.clone(),
        entrypoint: trust.entrypoint_path.clone(),
    })
}

impl Drop for RuntimeInner {
    fn drop(&mut self) {
        let Ok(sessions) = self.stdio_sessions.get_mut() else {
            return;
        };
        for (_key, session) in std::mem::take(sessions) {
            if let Ok(mut session) = session.lock() {
                session.terminate();
            }
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

fn extension_execution_path(extension: &RegisteredExtension) -> Result<PathBuf, SpindleError> {
    if let Some(trust) = &extension.runtime_trust {
        return Ok(trust.entrypoint_path.clone());
    }
    Ok(resolve_manifest_path(
        &extension.manifest_path,
        registered_entrypoint(extension)?,
    ))
}
