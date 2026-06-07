use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ExtensionSdkError, empty_object};

/// Extension registration returned by extension hosts during installation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionRegistration {
    /// Event types this extension can emit.
    #[serde(default)]
    pub emits: Vec<String>,
    /// Capabilities declared by this extension.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Actions exposed by this extension.
    #[serde(default)]
    pub actions: BTreeMap<String, RegistrationAction>,
    /// Routes contributed by this extension package.
    #[serde(default)]
    pub routes: Vec<RegistrationRoute>,
}

impl ExtensionRegistration {
    /// Create an empty registration.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an emitted event type.
    #[must_use]
    pub fn emit(mut self, event: impl Into<String>) -> Self {
        self.emits.push(event.into());
        self
    }

    /// Register a capability.
    #[must_use]
    pub fn capability(mut self, capability: impl Into<String>) -> Self {
        self.capabilities.push(capability.into());
        self
    }

    /// Register an action.
    #[must_use]
    pub fn action(mut self, name: impl Into<String>, action: RegistrationAction) -> Self {
        self.actions.insert(name.into(), action);
        self
    }

    /// Register a route.
    #[must_use]
    pub fn route(mut self, route: RegistrationRoute) -> Self {
        self.routes.push(route);
        self
    }

    /// Register an event handler action and route it from an event.
    #[must_use]
    pub fn on(
        self,
        event: impl Into<String>,
        action_name: impl Into<String>,
        action: RegistrationAction,
    ) -> Self {
        let action_name = action_name.into();
        self.on_route(
            RegistrationRoute::new(event, action_name.clone()),
            action_name,
            action,
        )
    }

    /// Register an event handler action with static route arguments.
    #[must_use]
    pub fn on_with_args(
        self,
        event: impl Into<String>,
        action_name: impl Into<String>,
        action: RegistrationAction,
        args: Value,
    ) -> Self {
        let action_name = action_name.into();
        self.on_route(
            RegistrationRoute::new(event, action_name.clone()).with_args(args),
            action_name,
            action,
        )
    }

    fn on_route(
        mut self,
        route: RegistrationRoute,
        action_name: String,
        action: RegistrationAction,
    ) -> Self {
        let route = route_with_action_capabilities(route, &action);
        self.actions.insert(action_name, action);
        self.routes.push(route);
        self
    }

    /// Serialize this registration as JSON.
    ///
    /// # Errors
    ///
    /// Returns an error when registration cannot be serialized.
    pub fn to_json_string(&self) -> Result<String, ExtensionSdkError> {
        serde_json::to_string(self).map_err(|source| ExtensionSdkError::Json {
            name: "ExtensionRegistration",
            source,
        })
    }
}

/// Action registration returned by an extension host.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistrationAction {
    /// Capabilities required to invoke this action.
    #[serde(default)]
    pub capabilities: Vec<String>,
}

impl RegistrationAction {
    /// Create an action registration.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            capabilities: Vec::new(),
        }
    }

    /// Require a capability for this action.
    #[must_use]
    pub fn capability(mut self, capability: impl Into<String>) -> Self {
        self.capabilities.push(capability.into());
        self
    }
}

/// Route registration returned by an extension host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistrationRoute {
    /// Event type that activates this route.
    pub event: String,
    /// Action requested when the event is observed.
    pub action: String,
    /// Capabilities granted by this installed route.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// Static action arguments merged with the event payload.
    #[serde(default = "empty_object")]
    pub args: Value,
}

impl RegistrationRoute {
    /// Create a route registration.
    #[must_use]
    pub fn new(event: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            event: event.into(),
            action: action.into(),
            capabilities: Vec::new(),
            args: empty_object(),
        }
    }

    /// Grant a capability when this route invokes its action.
    #[must_use]
    pub fn capability(mut self, capability: impl Into<String>) -> Self {
        self.capabilities.push(capability.into());
        self
    }

    /// Attach static route arguments.
    #[must_use]
    pub fn with_args(mut self, args: Value) -> Self {
        self.args = args;
        self
    }
}

fn route_with_action_capabilities(
    mut route: RegistrationRoute,
    action: &RegistrationAction,
) -> RegistrationRoute {
    route
        .capabilities
        .extend(action.capabilities.iter().cloned());
    route
}
