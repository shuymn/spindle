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
    /// Event types this extension's actions can produce.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub produces: Vec<String>,
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

    /// Register an action-produced event type.
    #[must_use]
    pub fn produce(mut self, event: impl Into<String>) -> Self {
        self.produces.push(event.into());
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

    /// Register a capless event handler action and route it from an event.
    ///
    /// Use [`Self::on_from`] when the handler action requires capabilities.
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

    /// Register an event handler action for a specific event source.
    ///
    /// Action capabilities are copied to the generated source-bound route.
    #[must_use]
    pub fn on_from(
        self,
        source: impl Into<String>,
        event: impl Into<String>,
        action_name: impl Into<String>,
        action: RegistrationAction,
    ) -> Self {
        let action_name = action_name.into();
        self.on_capability_route(
            RegistrationRoute::new(event, action_name.clone()).source(source),
            action_name,
            action,
        )
    }

    /// Register a capless event handler action with static route arguments.
    ///
    /// Use [`Self::on_with_args_from`] when the handler action requires
    /// capabilities.
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

    /// Register an event handler action for a specific source with static route arguments.
    ///
    /// Action capabilities are copied to the generated source-bound route.
    #[must_use]
    pub fn on_with_args_from(
        self,
        source_event: (impl Into<String>, impl Into<String>),
        action_name: impl Into<String>,
        action: RegistrationAction,
        args: Value,
    ) -> Self {
        let (source, event) = source_event;
        let action_name = action_name.into();
        self.on_capability_route(
            RegistrationRoute::new(event, action_name.clone())
                .source(source)
                .with_args(args),
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
        self.actions.insert(action_name, action);
        self.routes.push(route);
        self
    }

    fn on_capability_route(
        self,
        route: RegistrationRoute,
        action_name: String,
        action: RegistrationAction,
    ) -> Self {
        self.on_route(
            route_with_action_capabilities(route, &action),
            action_name,
            action,
        )
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
        push_unique(&mut self.capabilities, capability.into());
        self
    }
}

/// Route registration returned by an extension host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistrationRoute {
    /// Event type that activates this route.
    pub event: String,
    /// Event source that activates this route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
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
            source: None,
            action: action.into(),
            capabilities: Vec::new(),
            args: empty_object(),
        }
    }

    /// Match this route only when the source also matches.
    #[must_use]
    pub fn source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// Grant a capability when this route invokes its action.
    #[must_use]
    pub fn capability(mut self, capability: impl Into<String>) -> Self {
        push_unique(&mut self.capabilities, capability.into());
        self
    }

    /// Attach static route arguments.
    #[must_use]
    pub fn with_args(mut self, args: Value) -> Self {
        self.args = args;
        self
    }
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
}

fn route_with_action_capabilities(
    mut route: RegistrationRoute,
    action: &RegistrationAction,
) -> RegistrationRoute {
    for capability in &action.capabilities {
        push_unique(&mut route.capabilities, capability.clone());
    }
    route
}
