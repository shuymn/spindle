use std::collections::BTreeSet;

use crate::{ExtensionRegistry, ExtensionRuntime, RegisteredExtension, SpindleError};

/// Validate one extension's routes against installed extension surfaces.
///
/// # Errors
///
/// Returns an error when a route references an unknown event, source, or action.
pub fn validate_extension_routes(
    extension: &RegisteredExtension,
    extensions: &[RegisteredExtension],
) -> Result<(), SpindleError> {
    let actions = routable_actions(extensions);
    for route in &extension.routes {
        if !route.capabilities.is_empty() && route.source.is_none() {
            return Err(SpindleError::InvalidField {
                field: "route.source",
                reason: "is required when route grants capabilities",
            });
        }
        if !route_event_is_known(&route.event, extensions) {
            return Err(SpindleError::UnknownRouteEvent {
                extension: extension.id.clone(),
                event: route.event.clone(),
            });
        }
        if let Some(source) = &route.source
            && !route_source_is_valid(source, &route.event, extensions)
        {
            return Err(SpindleError::UnknownRouteSource {
                extension: extension.id.clone(),
                route_source: source.clone(),
            });
        }
        if !actions.contains(route.action.as_str()) {
            return Err(SpindleError::UnknownRouteAction {
                extension: extension.id.clone(),
                action: route.action.clone(),
            });
        }
    }
    Ok(())
}

/// Validate installed extensions before daemon startup.
///
/// # Errors
///
/// Returns an aggregate error when any installed route references unresolved
/// event, source, or action surface.
pub fn validate_installed_registry(registry: &ExtensionRegistry) -> Result<(), SpindleError> {
    let extensions = registry.list()?;
    validate_registry(&extensions)
}

/// Validate installed extensions.
///
/// # Errors
///
/// Returns an aggregate error when any installed route references unresolved
/// event, source, or action surface.
pub fn validate_registry(extensions: &[RegisteredExtension]) -> Result<(), SpindleError> {
    let mut messages = Vec::new();

    for extension in extensions {
        if let Err(error) = validate_extension_routes(extension, extensions) {
            messages.push(format!("{}: {error}", extension.id));
        }
    }

    if messages.is_empty() {
        return Ok(());
    }

    Err(SpindleError::RegistryValidationFailed { messages })
}

fn routable_actions(extensions: &[RegisteredExtension]) -> BTreeSet<&str> {
    extensions
        .iter()
        .filter(|extension| extension.runtime != ExtensionRuntime::Recipe)
        .flat_map(|extension| extension.actions.keys().map(String::as_str))
        .collect()
}

fn route_event_is_known(event: &str, extensions: &[RegisteredExtension]) -> bool {
    extensions
        .iter()
        .any(|extension| extension_owns_event(extension, event))
}

fn route_source_is_valid(source: &str, event: &str, extensions: &[RegisteredExtension]) -> bool {
    extensions
        .iter()
        .find(|extension| extension.id == source)
        .is_some_and(|extension| extension_owns_event(extension, event))
}

fn extension_owns_event(extension: &RegisteredExtension, event: &str) -> bool {
    extension.emits.iter().any(|kind| kind == event)
        || extension.produces.iter().any(|kind| kind == event)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::{ExtensionAction, ExtensionRoute};

    #[test]
    fn validate_extension_routes_rejects_source_less_capability_route() {
        let provider = registered_extension(
            "aerospace",
            &["aerospace.workspace.changed"],
            &[],
            &[("workspace-indicator.render", ExtensionAction::default())],
            &[],
        );
        let consumer = registered_extension(
            "workspace-indicator",
            &[],
            &[],
            &[],
            &[ExtensionRoute {
                event: String::from("aerospace.workspace.changed"),
                source: None,
                action: String::from("workspace-indicator.render"),
                capabilities: vec![String::from("aerospace.state.read")],
                args: serde_json::json!({}),
            }],
        );

        let result = validate_extension_routes(&consumer, &[provider, consumer.clone()]);

        assert!(matches!(result, Err(SpindleError::InvalidField { .. })));
    }

    #[test]
    fn validate_extension_routes_rejects_unknown_source() {
        let provider = registered_extension(
            "aerospace",
            &["aerospace.workspace.changed"],
            &[],
            &[("workspace-indicator.render", ExtensionAction::default())],
            &[],
        );
        let consumer = registered_extension(
            "workspace-indicator",
            &[],
            &[],
            &[],
            &[ExtensionRoute {
                event: String::from("aerospace.workspace.changed"),
                source: Some(String::from("nonexistent-extension")),
                action: String::from("workspace-indicator.render"),
                capabilities: Vec::new(),
                args: serde_json::json!({}),
            }],
        );

        let result = validate_extension_routes(&consumer, &[provider, consumer.clone()]);

        assert!(matches!(
            result,
            Err(SpindleError::UnknownRouteSource { .. })
        ));
    }

    #[test]
    fn validate_extension_routes_rejects_unknown_action() {
        let provider = registered_extension(
            "aerospace",
            &["aerospace.workspace.changed"],
            &[],
            &[("workspace-indicator.render", ExtensionAction::default())],
            &[],
        );
        let consumer = registered_extension(
            "workspace-indicator",
            &[],
            &[],
            &[],
            &[ExtensionRoute {
                event: String::from("aerospace.workspace.changed"),
                source: Some(String::from("aerospace")),
                action: String::from("nonexistent.action"),
                capabilities: Vec::new(),
                args: serde_json::json!({}),
            }],
        );

        let result = validate_extension_routes(&consumer, &[provider, consumer.clone()]);

        assert!(matches!(
            result,
            Err(SpindleError::UnknownRouteAction { .. })
        ));
    }

    #[test]
    fn validate_extension_routes_rejects_unknown_event() {
        let provider = registered_extension(
            "aerospace",
            &["aerospace.workspace.changed"],
            &[],
            &[("workspace-indicator.render", ExtensionAction::default())],
            &[],
        );
        let consumer = registered_extension(
            "workspace-indicator",
            &[],
            &[],
            &[("workspace-indicator.render", ExtensionAction::default())],
            &[ExtensionRoute {
                event: String::from("workspace-indicator.rendered"),
                source: Some(String::from("aerospace")),
                action: String::from("workspace-indicator.render"),
                capabilities: Vec::new(),
                args: serde_json::json!({}),
            }],
        );

        let result = validate_extension_routes(&consumer, &[provider, consumer.clone()]);

        assert!(matches!(
            result,
            Err(SpindleError::UnknownRouteEvent { .. })
        ));
    }

    fn registered_extension(
        id: &str,
        emits: &[&str],
        produces: &[&str],
        actions: &[(&str, ExtensionAction)],
        routes: &[ExtensionRoute],
    ) -> RegisteredExtension {
        RegisteredExtension {
            id: String::from(id),
            version: String::from("0.1.0"),
            package_root: PathBuf::from(format!("/tmp/{id}")),
            runtime: ExtensionRuntime::StdioJsonl,
            capabilities: Vec::new(),
            emits: emits.iter().map(|event| String::from(*event)).collect(),
            produces: produces.iter().map(|event| String::from(*event)).collect(),
            actions: actions
                .iter()
                .map(|(name, action)| (String::from(*name), action.clone()))
                .collect(),
            routes: routes.to_vec(),
            runtime_trust: None,
        }
    }
}
