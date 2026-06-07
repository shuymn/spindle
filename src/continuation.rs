use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use spindle_extension_sdk::ContinuationContext;

use crate::{SpindleError, now_unix_ms, validate_name};

const DEFAULT_CONTINUATION_TTL: Duration = Duration::from_secs(30);

/// In-memory registry for capability-scoped continuation handles.
#[derive(Debug, Clone, Default)]
pub struct ContinuationStore {
    inner: Arc<Mutex<BTreeMap<String, ContinuationGrant>>>,
}

/// Capability grant preserved for deferred extension work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinuationGrant {
    /// Opaque continuation identifier.
    pub id: String,
    /// Extension that received the continuation.
    pub extension: String,
    /// Capabilities granted by the original route or direct invocation.
    pub capabilities: Vec<String>,
    /// Original action that received the continuation.
    pub action: String,
    /// Expiry time as milliseconds since Unix epoch.
    pub expires_unix_ms: u128,
}

impl ContinuationStore {
    /// Create and store a continuation grant.
    ///
    /// # Errors
    ///
    /// Returns an error when the system clock cannot produce a Unix timestamp.
    pub fn create(
        &self,
        extension: &str,
        action: &str,
        capabilities: &[String],
        socket: &std::path::Path,
    ) -> Result<ContinuationContext, SpindleError> {
        self.create_with_lifetime(ContinuationGrantRequest {
            extension,
            action,
            capabilities,
            socket,
            ttl: DEFAULT_CONTINUATION_TTL,
        })
    }

    /// Create and store a continuation grant with an explicit lifetime.
    ///
    /// # Errors
    ///
    /// Returns an error when the system clock cannot produce a Unix timestamp.
    pub fn create_with_lifetime(
        &self,
        request: ContinuationGrantRequest<'_>,
    ) -> Result<ContinuationContext, SpindleError> {
        let expires_unix_ms = now_unix_ms()? + request.ttl.as_millis();
        let id = uuid::Uuid::now_v7().to_string();
        let grant = ContinuationGrant {
            id: id.clone(),
            extension: String::from(request.extension),
            capabilities: request.capabilities.to_vec(),
            action: String::from(request.action),
            expires_unix_ms,
        };
        self.inner
            .lock()
            .map_err(|_error| SpindleError::InvalidField {
                field: "continuation",
                reason: "store lock poisoned",
            })?
            .insert(id.clone(), grant);
        Ok(ContinuationContext::new(
            id,
            request.socket.display().to_string(),
            expires_unix_ms,
        ))
    }

    /// Validate and return an existing continuation grant.
    ///
    /// # Errors
    ///
    /// Returns an error when the handle is invalid, expired, or malformed.
    pub fn validate(&self, id: &str) -> Result<ContinuationGrant, SpindleError> {
        validate_name("continuation", id)?;
        let now = now_unix_ms()?;
        let mut grants = self
            .inner
            .lock()
            .map_err(|_error| SpindleError::InvalidField {
                field: "continuation",
                reason: "store lock poisoned",
            })?;
        let removed = grants.remove(id);
        grants.retain(|_, grant| grant.expires_unix_ms > now);
        drop(grants);
        let Some(grant) = removed else {
            return Err(SpindleError::ContinuationInvalid);
        };
        if grant.expires_unix_ms <= now {
            return Err(SpindleError::ContinuationExpired);
        }
        Ok(grant)
    }
}

/// Request data used to create a continuation grant.
#[derive(Debug, Clone, Copy)]
pub struct ContinuationGrantRequest<'a> {
    pub extension: &'a str,
    pub action: &'a str,
    pub capabilities: &'a [String],
    pub socket: &'a std::path::Path,
    pub ttl: Duration,
}

#[derive(Debug, Clone)]
pub struct ContinuationConfig {
    pub store: ContinuationStore,
    pub socket: PathBuf,
}
