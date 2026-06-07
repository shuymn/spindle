use std::io;

use thiserror::Error;

/// Error returned by the spindle extension host contract.
#[derive(Debug, Error)]
pub enum ExtensionSdkError {
    /// Contract JSON failed to parse.
    #[error("failed to parse {name}: {source}")]
    Json {
        /// JSON value or type name.
        name: &'static str,
        /// JSON parser error.
        #[source]
        source: serde_json::Error,
    },
    /// Stdio host I/O failed.
    #[error("stdio host I/O failed: {source}")]
    Io {
        /// I/O error source.
        #[from]
        source: io::Error,
    },
}

impl From<serde_json::Error> for ExtensionSdkError {
    fn from(source: serde_json::Error) -> Self {
        Self::Json {
            name: "json",
            source,
        }
    }
}
