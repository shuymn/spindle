//! Spindle daemon kernel: event log, manifest validation, and route dispatch.
//!
//! Extension hosts use the sibling `spindle-extension-sdk` crate for the stdio
//! JSONL contract.

#![warn(clippy::pedantic)]
#![warn(clippy::nursery)]
#![warn(clippy::cargo)]
// Cargo-level lint only: current transitive graph contains duplicate versions
// from upstream crates and uuid/getrandom target support outside this crate's control.
#![allow(clippy::multiple_crate_versions)]

pub mod cli;
mod continuation;
mod dispatch;
mod event;
mod extension;
mod handler;
pub(crate) mod lock;
mod policy;
mod protocol;
pub(crate) mod runtime;
mod server;
mod store;

use std::{io, path::PathBuf, time::SystemTimeError};

pub use continuation::{
    ContinuationConfig, ContinuationGrant, ContinuationGrantRequest, ContinuationStore,
};
pub use dispatch::{DispatchReport, Dispatcher};
pub use event::{ActionRequest, ContinuationAudit, Event, EventBuilder, EventFilter};
pub use extension::{
    ExtensionAction, ExtensionManifest, ExtensionRegistry, ExtensionRoute, ExtensionRuntime,
    RegisteredExtension, RegisteredRuntimeTrust,
};
pub use handler::{execute_request, execute_request_with_continuations};
pub use policy::{CapabilityPolicy, RouteGrantPolicy, validate_extension_routes};
pub use protocol::{HubRequest, HubResponse};
pub use runtime::ExtensionRuntimeHost;
pub use server::{send_request, send_request_with_timeout, serve};
pub use store::EventLog;
use thiserror::Error;

/// Error type shared by the spindle kernel and CLI.
#[derive(Debug, Error)]
pub enum SpindleError {
    /// A named field failed validation.
    #[error("invalid {field}: {reason}")]
    InvalidField {
        /// Field name.
        field: &'static str,
        /// Validation failure reason.
        reason: &'static str,
    },

    /// A dispatch request did not grant a capability required by an action.
    #[error("action {action} requires capability {capability}")]
    MissingActionCapability {
        /// Action name.
        action: String,
        /// Missing capability.
        capability: String,
    },

    /// An extension surface is already owned by another installed extension.
    #[error(
        "{surface} {name} is already owned by extension {existing_extension}, cannot register extension {new_extension}"
    )]
    SurfaceConflict {
        /// Surface kind.
        surface: &'static str,
        /// Surface name.
        name: String,
        /// Extension that already owns the surface.
        existing_extension: String,
        /// Extension being registered.
        new_extension: String,
    },

    /// A client or route attempted to grant an unauthorized capability.
    #[error("{grant_kind} {grantor} is not allowed to grant capability {capability}")]
    CapabilityGrantDenied {
        /// Grant kind.
        grant_kind: &'static str,
        /// Client source or extension id trying to grant a capability.
        grantor: String,
        /// Capability being granted.
        capability: String,
    },

    /// A direct client attempted to emit an unauthorized event.
    #[error("source {event_source} is not allowed to emit event {kind}")]
    EventEmitDenied {
        /// Event source.
        event_source: String,
        /// Event kind.
        kind: String,
    },

    /// A route references an event kind that no installed extension owns.
    #[error("extension {extension} route references unknown event {event}")]
    UnknownRouteEvent {
        /// Route-owning extension identifier.
        extension: String,
        /// Unknown event kind.
        event: String,
    },

    /// A route references an unknown event source extension.
    #[error("extension {extension} route references unknown source {route_source}")]
    UnknownRouteSource {
        /// Route-owning extension identifier.
        extension: String,
        /// Unknown source extension identifier.
        route_source: String,
    },

    /// A route references an action that no installed extension exposes.
    #[error("extension {extension} route references unknown action {action}")]
    UnknownRouteAction {
        /// Route-owning extension identifier.
        extension: String,
        /// Unknown action name.
        action: String,
    },

    /// A route grant policy references an unknown route-owning extension.
    #[error("capabilities.json routes grantor {grantor} is not an installed extension")]
    UnknownPolicyGrantor {
        /// Route grant policy grantor.
        grantor: String,
    },

    /// A route grant policy references an unknown event source extension.
    #[error(
        "capabilities.json routes[{grantor}] references unknown source extension {grant_source}"
    )]
    UnknownPolicyGrantSource {
        /// Route grant policy grantor.
        grantor: String,
        /// Unknown source extension identifier.
        grant_source: String,
    },

    /// A route grant policy references an unknown event kind.
    #[error("capabilities.json routes[{grantor}] references unknown event {event}")]
    UnknownPolicyGrantEvent {
        /// Route grant policy grantor.
        grantor: String,
        /// Unknown event kind.
        event: String,
    },

    /// A route grant policy does not match any route owned by the grantor extension.
    #[error(
        "capabilities.json routes[{grantor}] has no matching route for source {grant_source} and event {event}"
    )]
    OrphanPolicyGrant {
        /// Route grant policy grantor.
        grantor: String,
        /// Event source in the grant policy.
        grant_source: String,
        /// Event kind in the grant policy.
        event: String,
    },

    /// A legacy route grant policy shape was used instead of route grant objects.
    #[error(
        "capabilities.json routes[{grantor}] must be an array of objects with source, event, and capabilities fields; legacy capability string lists are not supported"
    )]
    LegacyRouteGrantPolicy {
        /// Route grant policy grantor.
        grantor: String,
    },

    /// An extension action produced an undeclared event kind.
    #[error("extension {extension} action {action} produced undeclared event {kind}")]
    UndeclaredProducedEvent {
        /// Extension identifier.
        extension: String,
        /// Action name.
        action: String,
        /// Event kind.
        kind: String,
    },

    /// A stdio extension requires trusted runtime execution to discover surface.
    #[error(
        "extension {extension} requires --trust-runtime to execute its entrypoint for registration"
    )]
    RuntimeTrustRequired {
        /// Extension identifier.
        extension: String,
    },

    /// A JSONL protocol message exceeded the configured byte limit.
    #[error("protocol message exceeded {limit} bytes")]
    MessageTooLarge {
        /// Maximum accepted bytes.
        limit: usize,
    },

    /// A trusted runtime entrypoint changed after registration.
    #[error("extension {extension} trusted entrypoint changed: {}", entrypoint.display())]
    ExtensionTrustChanged {
        /// Extension identifier.
        extension: String,
        /// Trusted entrypoint path.
        entrypoint: PathBuf,
    },

    /// The user's home directory could not be resolved.
    #[error("HOME is not set and SPINDLE_STATE_DIR was not provided")]
    MissingHome,

    /// A Unix socket already has a live server.
    #[error("socket is already in use: {}", path.display())]
    SocketInUse {
        /// Socket path.
        path: PathBuf,
    },

    /// No installed extension exposes the requested action.
    #[error("no installed extension exposes action {action}")]
    ActionNotInstalled {
        /// Requested action.
        action: String,
    },

    /// An installed extension exposes an action but has no entrypoint.
    #[error("extension {extension} has no entrypoint")]
    MissingExtensionEntrypoint {
        /// Extension identifier.
        extension: String,
    },

    /// An extension action process failed.
    #[error("extension {extension} action {action} failed: {stderr}")]
    ExtensionActionFailed {
        /// Extension identifier.
        extension: String,
        /// Action name.
        action: String,
        /// Process exit code.
        status: Option<i32>,
        /// Standard error output.
        stderr: String,
    },

    /// Extension registration process failed.
    #[error("extension {extension} registration failed: {stderr}")]
    ExtensionRegistrationFailed {
        /// Extension identifier.
        extension: String,
        /// Process exit code.
        status: Option<i32>,
        /// Standard error output.
        stderr: String,
    },

    /// Extension registration returned invalid structured output.
    #[error("extension {extension} returned invalid registration: {source}")]
    ExtensionRegistrationInvalid {
        /// Extension identifier.
        extension: String,
        /// Parser error.
        #[source]
        source: serde_json::Error,
    },

    /// Extension host returned an invalid JSONL protocol message.
    #[error("extension {extension} returned invalid host protocol: {source}")]
    ExtensionHostProtocolInvalid {
        /// Extension identifier.
        extension: String,
        /// Parser error.
        #[source]
        source: serde_json::Error,
    },

    /// Extension host closed unexpectedly.
    #[error("extension {extension} host closed unexpectedly")]
    ExtensionHostClosed {
        /// Extension identifier.
        extension: String,
    },

    /// Extension host returned an unexpected protocol response.
    #[error("extension {extension} host failed: {message}")]
    ExtensionHostError {
        /// Extension identifier.
        extension: String,
        /// Failure message.
        message: String,
    },

    /// Extension host did not answer before the request timeout.
    #[error("extension {extension} host timed out after {timeout_ms}ms")]
    ExtensionHostTimedOut {
        /// Extension identifier.
        extension: String,
        /// Timeout in milliseconds.
        timeout_ms: u64,
    },

    /// Process-extension contract serialization failed.
    #[error(transparent)]
    ExtensionContract(#[from] spindle_extension_sdk::ExtensionSdkError),

    /// An extension action returned invalid structured output.
    #[error("extension {extension} action {action} returned invalid output: {source}")]
    ExtensionOutputInvalid {
        /// Extension identifier.
        extension: String,
        /// Action name.
        action: String,
        /// Parser error.
        #[source]
        source: serde_json::Error,
    },

    /// Continuation handle is unknown or malformed.
    #[error("continuation handle is invalid")]
    ContinuationInvalid,

    /// Continuation handle is expired.
    #[error("continuation handle is expired")]
    ContinuationExpired,

    /// Event dispatch recursed too deeply.
    #[error("extension dispatch exceeded maximum depth")]
    DispatchDepthExceeded,

    /// System clock returned a time before the Unix epoch.
    #[error("system clock is before the Unix epoch")]
    Clock(#[from] SystemTimeError),

    /// Filesystem or stream I/O failed.
    #[error(transparent)]
    Io(#[from] io::Error),

    /// JSON parsing or serialization failed.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub(crate) fn now_unix_ms() -> Result<u64, SpindleError> {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis();
    u64::try_from(millis).map_err(|_error| SpindleError::InvalidField {
        field: "time_unix_ms",
        reason: "system clock exceeds u64 millisecond range",
    })
}

pub(crate) fn validate_name(field: &'static str, value: &str) -> Result<(), SpindleError> {
    validate_non_empty_text(field, value)?;

    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return Err(SpindleError::InvalidField {
            field,
            reason: "must not be empty",
        });
    };
    if !first.is_ascii_alphanumeric() {
        return Err(SpindleError::InvalidField {
            field,
            reason: "must start with an ASCII letter or number",
        });
    }
    if chars.any(|ch| !matches!(ch, 'A'..='Z' | 'a'..='z' | '0'..='9' | '.' | '_' | ':' | '-')) {
        return Err(SpindleError::InvalidField {
            field,
            reason: "must contain only ASCII letters, numbers, '.', '_', ':', or '-'",
        });
    }

    Ok(())
}

pub(crate) fn validate_subject(field: &'static str, value: &str) -> Result<(), SpindleError> {
    validate_non_empty_text(field, value)
}

pub(crate) fn validate_path_string(field: &'static str, value: &str) -> Result<(), SpindleError> {
    validate_non_empty_text(field, value)?;
    if value.contains('\0') {
        return Err(SpindleError::InvalidField {
            field,
            reason: "must not contain NUL bytes",
        });
    }

    Ok(())
}

fn validate_non_empty_text(field: &'static str, value: &str) -> Result<(), SpindleError> {
    if value.trim().is_empty() {
        return Err(SpindleError::InvalidField {
            field,
            reason: "must not be empty",
        });
    }

    if value.chars().any(char::is_control) {
        return Err(SpindleError::InvalidField {
            field,
            reason: "must not contain control characters",
        });
    }

    Ok(())
}

pub(crate) fn validate_json_object(
    field: &'static str,
    value: &serde_json::Value,
) -> Result<(), SpindleError> {
    if value.is_object() {
        return Ok(());
    }

    Err(SpindleError::InvalidField {
        field,
        reason: "must be a JSON object",
    })
}
