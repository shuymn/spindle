use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ActionContext, EventContext, ExtensionContext, ExtensionSdkError, empty_object};

/// Action invocation serialized by the spindle kernel for an extension host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionInvocation {
    pub(crate) action: String,
    pub(crate) args: Value,
    pub(crate) event: Option<EventContext>,
    pub(crate) extension: Option<ExtensionContext>,
}

impl ActionInvocation {
    /// Create an action invocation without source event metadata.
    #[must_use]
    pub fn new(action: impl Into<String>, args: Value) -> Self {
        Self {
            action: action.into(),
            args,
            event: None,
            extension: None,
        }
    }

    /// Attach source event metadata to this invocation.
    #[must_use]
    pub fn with_event(mut self, event: Option<EventContext>) -> Self {
        self.event = event;
        self
    }

    /// Attach the extension-visible spindle surface to this invocation.
    #[must_use]
    pub fn with_extension(mut self, extension: Option<ExtensionContext>) -> Self {
        self.extension = extension;
        self
    }

    /// Convert this invocation into an in-process action context.
    #[must_use]
    pub fn into_context(self) -> ActionContext {
        ActionContext::from_parts(Some(self.action), self.args, self.event, self.extension)
    }
}

/// Structured output returned by an extension action.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionOutput {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    events: Vec<ActionOutputEvent>,
}

impl ActionOutput {
    /// Create empty action output.
    #[must_use]
    pub const fn empty() -> Self {
        Self { events: Vec::new() }
    }

    /// Create output containing one emitted event.
    #[must_use]
    pub fn event(event: ActionOutputEvent) -> Self {
        Self {
            events: vec![event],
        }
    }

    /// Create output containing multiple emitted events.
    #[must_use]
    pub fn events(events: impl IntoIterator<Item = ActionOutputEvent>) -> Self {
        Self {
            events: events.into_iter().collect(),
        }
    }

    /// Append one emitted event.
    #[must_use]
    pub fn with_event(mut self, event: ActionOutputEvent) -> Self {
        self.events.push(event);
        self
    }

    /// Push one emitted event into this output.
    pub fn push_event(&mut self, event: ActionOutputEvent) {
        self.events.push(event);
    }

    /// Return events emitted by this action.
    #[must_use]
    pub fn emitted_events(&self) -> &[ActionOutputEvent] {
        &self.events
    }

    /// Serialize this output as JSON.
    ///
    /// # Errors
    ///
    /// Returns an error when output cannot be serialized.
    pub fn to_json_string(&self) -> Result<String, ExtensionSdkError> {
        serde_json::to_string(self).map_err(|source| ExtensionSdkError::Json {
            name: "ActionOutput",
            source,
        })
    }
}

/// Event emitted by an extension action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionOutputEvent {
    /// Event type.
    #[serde(rename = "type")]
    pub kind: String,
    /// Optional event subject.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// Event payload.
    #[serde(default = "empty_object")]
    pub data: Value,
}

impl ActionOutputEvent {
    /// Create an output event with empty object data.
    #[must_use]
    pub fn new(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            subject: None,
            data: empty_object(),
        }
    }

    /// Attach a subject.
    #[must_use]
    pub fn with_subject(mut self, subject: impl Into<String>) -> Self {
        self.subject = Some(subject.into());
        self
    }

    /// Attach event data.
    #[must_use]
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = data;
        self
    }
}
