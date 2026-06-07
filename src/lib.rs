//! Minimal local automation harness primitives.
//!
//! `spindle` keeps the core small: it records events, validates extension
//! manifests, and dispatches installed actions through generic routes. Concrete
//! integrations such as `AeroSpace`, `SketchyBar`, `Glimpse`, `Raycast`, and
//! coding-agent hooks should live in extensions rather than in the kernel.

#![warn(clippy::pedantic)]
#![warn(clippy::nursery)]
#![warn(clippy::cargo)]

pub mod cli;
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

pub use dispatch::{DispatchReport, Dispatcher};
pub use event::{ActionRequest, Event, EventBuilder, EventFilter};
pub use extension::{
    ExtensionAction, ExtensionManifest, ExtensionRegistry, ExtensionRoute, ExtensionRuntime,
    RegisteredExtension,
};
pub use handler::execute_request;
pub use policy::CapabilityPolicy;
pub use protocol::{HubRequest, HubResponse};
pub use runtime::ExtensionRuntimeHost;
pub use server::{send_request, serve};
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

    /// A manifest action requested a capability not declared by the extension.
    #[error("action {action} requires undeclared capability {capability}")]
    UndeclaredCapability {
        /// Action name.
        action: String,
        /// Missing capability.
        capability: String,
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

pub(crate) fn validate_token(field: &'static str, value: &str) -> Result<(), SpindleError> {
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
