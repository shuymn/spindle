use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

use crate::{ActionInvocation, ExtensionSdkError};

/// Capability-scoped handle for deferred extension work through spindle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinuationContext {
    /// Opaque continuation identifier validated by the core.
    pub id: String,
    /// Unix socket path accepting continuation-backed requests.
    pub socket: String,
    /// Expiry time as milliseconds since Unix epoch.
    pub expires_unix_ms: u64,
}

impl ContinuationContext {
    /// Create a continuation context.
    #[must_use]
    pub fn new(id: impl Into<String>, socket: impl Into<String>, expires_unix_ms: u64) -> Self {
        Self {
            id: id.into(),
            socket: socket.into(),
            expires_unix_ms,
        }
    }
}

/// Typed context passed to an extension action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionContext {
    action: Option<String>,
    args: Value,
    event: Option<EventContext>,
    extension: Option<ExtensionContext>,
    continuation: Option<ContinuationContext>,
}

impl ActionContext {
    /// Build an action context from a host invocation.
    #[must_use]
    pub fn from_invocation(invocation: ActionInvocation) -> Self {
        invocation.into_context()
    }

    /// Return the action name supplied by spindle, if present.
    #[must_use]
    pub fn action(&self) -> Option<&str> {
        self.action.as_deref()
    }

    /// Return the raw action argument JSON value.
    #[must_use]
    pub const fn args_value(&self) -> &Value {
        &self.args
    }

    /// Decode the action argument object into an extension-owned type.
    ///
    /// # Errors
    ///
    /// Returns an error when the JSON value cannot be decoded as `T`.
    pub fn args<T>(&self) -> Result<T, ExtensionSdkError>
    where
        T: DeserializeOwned,
    {
        Ok(serde_json::from_value(self.args.clone())?)
    }

    /// Return the source event that triggered this action, if any.
    #[must_use]
    pub const fn event(&self) -> Option<&EventContext> {
        self.event.as_ref()
    }

    /// Return the extension-visible spindle surface, if supplied by the core.
    #[must_use]
    pub const fn extension(&self) -> Option<&ExtensionContext> {
        self.extension.as_ref()
    }

    /// Return a deferred-work continuation handle, if supplied by the core.
    #[must_use]
    pub const fn continuation(&self) -> Option<&ContinuationContext> {
        self.continuation.as_ref()
    }

    pub(crate) const fn from_parts(
        action: Option<String>,
        args: Value,
        event: Option<EventContext>,
        extension: Option<ExtensionContext>,
        continuation: Option<ContinuationContext>,
    ) -> Self {
        Self {
            action,
            args,
            event,
            extension,
            continuation,
        }
    }
}

/// Event metadata supplied with a routed action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventContext {
    pub(crate) kind: String,
    pub(crate) data: Value,
}

impl EventContext {
    /// Create source event metadata.
    #[must_use]
    pub fn new(kind: impl Into<String>, data: Value) -> Self {
        Self {
            kind: kind.into(),
            data,
        }
    }

    /// Return the source event type.
    #[must_use]
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// Return the source event payload.
    #[must_use]
    pub const fn data(&self) -> &Value {
        &self.data
    }
}

/// Spindle surface supplied by the core to an extension action.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionContext {
    /// Current extension identifier.
    pub id: String,
    /// Event types visible through installed providers.
    #[serde(default)]
    pub events: Vec<EventDescriptor>,
    /// Actions visible through installed providers.
    #[serde(default)]
    pub actions: Vec<ActionDescriptor>,
    /// Capabilities declared by installed providers.
    #[serde(default)]
    pub capabilities: Vec<String>,
}

impl ExtensionContext {
    /// Create an extension context.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            events: Vec::new(),
            actions: Vec::new(),
            capabilities: Vec::new(),
        }
    }

    /// Return whether an event type is visible to this extension.
    #[must_use]
    pub fn has_event(&self, kind: &str) -> bool {
        self.events.iter().any(|event| event.kind == kind)
    }

    /// Return whether an action is visible to this extension.
    #[must_use]
    pub fn has_action(&self, name: &str) -> bool {
        self.actions.iter().any(|action| action.name == name)
    }

    /// Return whether a capability is visible to this extension.
    #[must_use]
    pub fn has_capability(&self, name: &str) -> bool {
        self.capabilities
            .iter()
            .any(|capability| capability == name)
    }
}

/// Event type visible to extensions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventDescriptor {
    /// Event type.
    #[serde(rename = "type")]
    pub kind: String,
    /// Extension that declares this event.
    pub source_extension: String,
}

/// Action visible to extensions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionDescriptor {
    /// Action name.
    pub name: String,
    /// Extension that provides the action.
    pub extension: String,
    /// Capabilities required by this action.
    #[serde(default)]
    pub capabilities: Vec<String>,
}
