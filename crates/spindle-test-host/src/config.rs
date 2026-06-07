use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use spindle_extension_sdk::ExtensionRegistration;

/// Full test-host behavior loaded from `{executable}.json`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TestHostConfig {
    /// Optional actions before the stdio loop begins.
    #[serde(default)]
    pub startup: StartupAction,
    /// Surface returned from `register` when `register_response` is `Registration`.
    #[serde(default)]
    pub registration: ExtensionRegistration,
    /// Response emitted for `register` requests.
    #[serde(default)]
    pub register_response: RegisterResponse,
    /// Ordered invoke matchers; the first match wins.
    #[serde(default)]
    pub invoke_rules: Vec<InvokeRule>,
    /// Fallback invoke response.
    #[serde(default)]
    pub invoke_default: InvokeEffect,
    /// Response emitted for `shutdown` requests.
    #[serde(default)]
    pub shutdown: ShutdownResponse,
}

/// Startup side effects executed once per process.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct StartupAction {
    /// Write the current pid to this file.
    pub write_pid: Option<PathBuf>,
    /// Append the current pid to this file.
    pub append_pid: Option<PathBuf>,
    /// Create this marker file.
    pub touch: Option<PathBuf>,
    /// Marker file used to count cross-spawn sessions.
    pub session_marker: Option<PathBuf>,
    /// Sleep before entering the stdio loop.
    #[serde(default)]
    pub sleep_ms: u64,
    /// Exit immediately with this code.
    pub exit_code: Option<i32>,
    /// Replace the executable with a stub after the first register response.
    #[serde(default)]
    pub mutate_on_register: bool,
}

/// Register-time response behavior.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum RegisterResponse {
    /// Return `registration` from the config.
    #[default]
    Registration,
    /// Write raw invalid JSON to stdout.
    InvalidJson { line: String },
    /// Write an oversized line to stdout.
    Oversized { bytes: usize },
    /// Sleep without responding.
    Sleep { ms: u64 },
}

/// One invoke matcher.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct InvokeRule {
    /// Match when the request line contains this substring.
    #[serde(default)]
    pub when_contains: Option<String>,
    /// Match on the Nth invoke in this process (1-based).
    #[serde(default)]
    pub when_invoke_index: Option<u64>,
    /// Match on the Nth session across marker-backed restarts (1-based).
    #[serde(default)]
    pub when_session_index: Option<u64>,
    /// Response to emit when this rule matches.
    #[serde(flatten)]
    pub effect: InvokeEffect,
}

/// Invoke-time response behavior.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct InvokeEffect {
    #[serde(flatten)]
    pub response: ResponseTemplate,
    /// Sleep before responding.
    #[serde(default)]
    pub sleep_ms: u64,
    /// Create this marker file before responding.
    pub touch: Option<PathBuf>,
    /// Create this marker file when `touch` already exists.
    pub touch_else: Option<PathBuf>,
    /// Delete this file before responding.
    pub remove_file: Option<PathBuf>,
    /// Increment an internal counter and include it in event data.
    #[serde(default)]
    pub count_event: Option<CountEventTemplate>,
    /// Include a fixed marker string in event data.
    pub marker: Option<String>,
}

impl Default for InvokeEffect {
    fn default() -> Self {
        Self {
            response: ResponseTemplate::EmptyOutput,
            sleep_ms: 0,
            touch: None,
            touch_else: None,
            remove_file: None,
            count_event: None,
            marker: None,
        }
    }
}

/// Template for counting invoke responses.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CountEventTemplate {
    pub event_type: String,
    pub field: String,
}

/// Shutdown response behavior.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ShutdownResponse {
    #[serde(default)]
    pub sleep_ms: u64,
}

/// Response body written to stdout.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ResponseTemplate {
    #[default]
    EmptyOutput,
    Output {
        output: Value,
    },
    Error {
        error: String,
    },
    /// Emit a registration-shaped response during invoke (protocol error tests).
    RegistrationSurface {
        registration: ExtensionRegistration,
    },
    #[serde(rename = "fail")]
    Failure(FailResponse),
}

/// Failure modes used by recovery tests.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "mode", rename_all = "kebab-case")]
pub enum FailResponse {
    InvalidJson { line: String },
    Oversized { bytes: usize },
    NonUtf8,
    Sleep { ms: u64 },
    Exit { code: i32 },
}

impl TestHostConfig {
    /// Marker-backed session counter file derived from the config path.
    #[must_use]
    pub fn session_marker_path(config_path: &std::path::Path) -> PathBuf {
        config_path.with_extension("session")
    }

    /// Build an empty-output config with the given registration surface.
    #[must_use]
    pub fn with_registration(registration: ExtensionRegistration) -> Self {
        Self {
            registration,
            ..Self::default()
        }
    }

    /// Build a config that emits incrementing count events on invoke.
    #[must_use]
    pub fn with_counting_invoke(registration: ExtensionRegistration, event_type: &str) -> Self {
        Self {
            invoke_default: InvokeEffect {
                count_event: Some(CountEventTemplate {
                    event_type: String::from(event_type),
                    field: String::from("count"),
                }),
                ..InvokeEffect::default()
            },
            ..Self::with_registration(registration)
        }
    }

    /// Build a config that emits a marker value in invoke event data.
    #[must_use]
    pub fn with_marker_invoke(registration: ExtensionRegistration, marker: &str) -> Self {
        Self {
            invoke_default: InvokeEffect {
                marker: Some(String::from(marker)),
                ..InvokeEffect::default()
            },
            ..Self::with_registration(registration)
        }
    }
}
