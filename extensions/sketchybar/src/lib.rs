//! `SketchyBar` IPC primitives and spindle actions.

#![warn(clippy::pedantic)]
#![warn(clippy::nursery)]
#![warn(clippy::cargo)]

use std::env;

use thiserror::Error;

/// Error returned by the `SketchyBar` extension.
#[derive(Debug, Error)]
pub enum SketchybarError {
    /// `SketchyBar` returned an error response.
    #[error("sketchybar command failed: {0}")]
    CommandFailed(String),

    /// Low-level Mach IPC failed.
    #[error("mach ipc failed: {operation} returned {code}")]
    Mach {
        /// Operation name.
        operation: &'static str,
        /// Native return code.
        code: i32,
    },

    /// This transport is only available on macOS.
    #[error("{transport} transport is only available on macOS")]
    UnsupportedPlatform {
        /// Transport name.
        transport: &'static str,
    },
}

const DEFAULT_SKETCHYBAR_NAME: &str = "sketchybar";

/// `SketchyBar` Mach message payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SketchybarMessage {
    bytes: Vec<u8>,
}

impl SketchybarMessage {
    /// Build the same NUL-separated payload that the `sketchybar` CLI sends.
    #[must_use]
    pub fn from_args<S>(args: &[S]) -> Self
    where
        S: AsRef<str>,
    {
        let total_len = args.iter().map(|arg| arg.as_ref().len() + 1).sum::<usize>() + 1;
        let mut bytes = Vec::with_capacity(total_len);
        for arg in args {
            bytes.extend_from_slice(arg.as_ref().as_bytes());
            bytes.push(0);
        }
        bytes.push(0);
        Self { bytes }
    }

    /// Return the raw wire-format bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Low-level `SketchyBar` client.
pub trait SketchybarClient {
    /// Send a preformatted `SketchyBar` message without spawning the CLI.
    ///
    /// # Errors
    ///
    /// Returns an error when the Mach port cannot be reached or `SketchyBar`
    /// reports a failure.
    fn send_message(&self, message: &SketchybarMessage) -> Result<String, SketchybarError>;

    /// Send command-line style arguments through the `SketchyBar` IPC protocol.
    ///
    /// # Errors
    ///
    /// Returns an error when the Mach port cannot be reached or `SketchyBar`
    /// reports a failure.
    fn send_args<S>(&self, args: &[S]) -> Result<String, SketchybarError>
    where
        S: AsRef<str>,
    {
        self.send_message(&SketchybarMessage::from_args(args))
    }
}

/// `SketchyBar` Mach IPC client.
#[derive(Debug, Clone)]
pub struct SketchybarMachClient {
    bar_name: String,
}

impl SketchybarMachClient {
    /// Create a client for a named `SketchyBar` instance.
    #[must_use]
    pub const fn new(bar_name: String) -> Self {
        Self { bar_name }
    }

    /// Create a client for the current `SketchyBar` instance.
    #[must_use]
    pub fn from_env() -> Self {
        let bar_name =
            env::var("BAR_NAME").unwrap_or_else(|_err| String::from(DEFAULT_SKETCHYBAR_NAME));
        Self::new(bar_name)
    }
}

impl Default for SketchybarMachClient {
    fn default() -> Self {
        Self::from_env()
    }
}

impl SketchybarClient for SketchybarMachClient {
    fn send_message(&self, message: &SketchybarMessage) -> Result<String, SketchybarError> {
        let response = sketchybar_ipc::send(&self.bar_name, message.as_bytes())?;
        if response.len() > 2 && response.as_bytes().get(1) == Some(&b'!') {
            return Err(SketchybarError::CommandFailed(response));
        }
        Ok(response)
    }
}

/// Send raw `SketchyBar` command arguments through Mach IPC.
///
/// # Errors
///
/// Returns an error when the Mach port cannot be reached or `SketchyBar` reports
/// a failure.
pub fn send_args(args: &[String]) -> Result<String, SketchybarError> {
    SketchybarMachClient::from_env().send_args(args)
}

#[cfg(target_os = "macos")]
// SketchyBar updates are latency-sensitive; use one isolated FFI bridge to
// avoid spawning the `sketchybar` CLI for every message.
#[allow(unsafe_code)]
mod sketchybar_ipc {
    use std::ffi::{CString, c_char};

    use crate::SketchybarError;

    unsafe extern "C" {
        fn spindle_sketchybar_send(
            bar_name: *const c_char,
            message: *const u8,
            message_len: u32,
        ) -> i32;
    }

    pub fn send(bar_name: &str, bytes: &[u8]) -> Result<String, SketchybarError> {
        let bar_name = CString::new(bar_name).map_err(|_err| {
            SketchybarError::CommandFailed(String::from("bar name contains NUL"))
        })?;
        let message_len = u32::try_from(bytes.len()).map_err(|_err| SketchybarError::Mach {
            operation: "message_len",
            code: -1,
        })?;

        let code =
            unsafe { spindle_sketchybar_send(bar_name.as_ptr(), bytes.as_ptr(), message_len) };
        if code == 0 {
            Ok(String::new())
        } else {
            Err(SketchybarError::Mach {
                operation: "spindle_sketchybar_send",
                code,
            })
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod sketchybar_ipc {
    use crate::SketchybarError;

    pub(super) fn send(_bar_name: &str, _bytes: &[u8]) -> Result<String, SketchybarError> {
        Err(SketchybarError::UnsupportedPlatform {
            transport: "sketchybar mach",
        })
    }
}
