use std::{
    io::{self, BufRead, Read},
    path::PathBuf,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::SpindleError;

pub const DEFAULT_JSONL_MESSAGE_LIMIT: usize = 1024 * 1024;

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
        /// Action arguments.
        #[serde(default = "empty_object")]
        args: Value,
    },
    /// Invoke an action through a core-validated continuation handle.
    ContinuationInvoke {
        /// Opaque continuation identifier.
        continuation: String,
        /// Action name.
        action: String,
        /// Action arguments.
        #[serde(default = "empty_object")]
        args: Value,
    },
    /// Emit an extension-produced event through a core-validated continuation handle.
    ContinuationEmit {
        /// Opaque continuation identifier.
        continuation: String,
        /// Event type.
        #[serde(rename = "type")]
        kind: String,
        /// Optional event subject.
        #[serde(skip_serializing_if = "Option::is_none")]
        subject: Option<String>,
        /// Event payload.
        #[serde(default = "empty_object")]
        data: Value,
    },
    /// Validate an extension manifest.
    ValidateExtension {
        /// Manifest path.
        manifest: PathBuf,
    },
    /// Install an extension package.
    InstallExtension {
        /// Extension package directory.
        package: PathBuf,
        /// Execute the package entrypoint to collect dynamic surface.
        #[serde(default)]
        trust_runtime: bool,
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

pub fn read_limited_jsonl_line(
    reader: &mut impl BufRead,
    limit: usize,
) -> Result<Option<String>, SpindleError> {
    let mut bytes = Vec::new();
    let read = reader
        .by_ref()
        .take(u64::try_from(limit.saturating_add(1)).unwrap_or(u64::MAX))
        .read_until(b'\n', &mut bytes)?;
    if read == 0 {
        return Ok(None);
    }
    if bytes.len() > limit {
        return Err(SpindleError::MessageTooLarge { limit });
    }
    if !bytes.ends_with(b"\n") {
        return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error).into())
}

fn empty_object() -> Value {
    Value::Object(serde_json::Map::new())
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, ErrorKind};

    use super::*;

    #[test]
    fn limited_line_reads_normal_framed_message() -> Result<(), SpindleError> {
        let mut reader = Cursor::new(b"{}\nnext".as_slice());

        let line = read_limited_jsonl_line(&mut reader, 8)?.ok_or(SpindleError::InvalidField {
            field: "line",
            reason: "missing",
        })?;

        assert_eq!(line, "{}\n");
        Ok(())
    }

    #[test]
    fn limited_line_rejects_newline_less_message_at_limit() {
        let mut reader = Cursor::new(b"12345".as_slice());

        let result = read_limited_jsonl_line(&mut reader, 5);

        assert!(matches!(
            result,
            Err(SpindleError::Io(error)) if error.kind() == ErrorKind::UnexpectedEof
        ));
    }

    #[test]
    fn limited_line_rejects_oversized_message() {
        let mut reader = Cursor::new(b"123456".as_slice());

        let result = read_limited_jsonl_line(&mut reader, 5);

        assert!(matches!(
            result,
            Err(SpindleError::MessageTooLarge { limit: 5 })
        ));
    }

    #[test]
    fn limited_line_rejects_non_utf8_input() {
        let mut reader = Cursor::new([0xff, b'\n']);

        let result = read_limited_jsonl_line(&mut reader, 8);

        assert!(matches!(
            result,
            Err(SpindleError::Io(error)) if error.kind() == ErrorKind::InvalidData
        ));
    }

    #[test]
    fn limited_line_reports_empty_eof() -> Result<(), SpindleError> {
        let mut reader = Cursor::new(Vec::<u8>::new());

        assert!(read_limited_jsonl_line(&mut reader, 8)?.is_none());
        Ok(())
    }
}
