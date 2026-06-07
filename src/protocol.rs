use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::SpindleError;

/// JSONL request accepted by the spindle socket server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "kebab-case")]
pub enum HubRequest {
    /// Append an event.
    Emit {
        /// Event type.
        #[serde(rename = "type")]
        kind: String,
        /// Event source.
        source: String,
        /// Optional event subject.
        #[serde(skip_serializing_if = "Option::is_none")]
        subject: Option<String>,
        /// Event payload.
        #[serde(default = "empty_object")]
        data: Value,
    },
    /// Query events from the append-only log.
    QueryEvents {
        /// Optional event type filter.
        #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
        /// Optional source filter.
        #[serde(skip_serializing_if = "Option::is_none")]
        source: Option<String>,
        /// Optional tail limit.
        #[serde(skip_serializing_if = "Option::is_none")]
        limit: Option<usize>,
    },
    /// Record and dispatch an installed action request.
    Invoke {
        /// Action name.
        action: String,
        /// Logical requester.
        source: String,
        /// Capabilities granted to this direct action request.
        #[serde(default)]
        capabilities: Vec<String>,
        /// Action arguments.
        #[serde(default = "empty_object")]
        args: Value,
    },
    /// Validate an extension manifest.
    ValidateExtension {
        /// Manifest path.
        manifest: PathBuf,
    },
    /// Register an extension manifest.
    RegisterExtension {
        /// Manifest path.
        manifest: PathBuf,
    },
    /// List registered extensions.
    ListExtensions,
}

/// JSONL response emitted by the spindle socket server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum HubResponse {
    /// Successful request.
    Ok {
        /// Response data.
        data: Value,
    },
    /// Failed request.
    Error {
        /// Error message.
        error: String,
    },
}

impl From<Result<Value, SpindleError>> for HubResponse {
    fn from(result: Result<Value, SpindleError>) -> Self {
        match result {
            Ok(data) => Self::Ok { data },
            Err(error) => Self::Error {
                error: error.to_string(),
            },
        }
    }
}

fn empty_object() -> Value {
    Value::Object(serde_json::Map::new())
}
