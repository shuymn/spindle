use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use serde::{Deserialize, Serialize};

use crate::{ExtensionRoute, ExtensionRuntime, RegisteredExtension, SpindleError, validate_name};

const POLICY_FILE: &str = "capabilities.json";
const WILDCARD: &str = "*";

/// Local allow policy for capability grants.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityPolicy {
    /// Event kinds direct clients may emit by event source.
    #[serde(default)]
    pub emits: BTreeMap<String, Vec<String>>,
    /// Capabilities direct clients may grant by request source.
    #[serde(default)]
    pub direct: BTreeMap<String, Vec<String>>,
    /// Capabilities route-owning extensions may grant by source and event.
    #[serde(default)]
    pub routes: BTreeMap<String, Vec<RouteGrantPolicy>>,
}

impl CapabilityPolicy {
    /// Load the capability policy for a spindle state directory.
    ///
    /// # Errors
    ///
    /// Returns an error when the policy file cannot be read, cannot be parsed,
    /// or contains invalid grant names.
    pub fn load(state_dir: &Path) -> Result<Self, SpindleError> {
        let path = state_dir.join(POLICY_FILE);
        if !path.exists() {
            let policy = Self::default();
            policy.validate()?;
            return Ok(policy);
        }

        let contents = fs::read_to_string(path)?;
        let value: serde_json::Value = serde_json::from_str(&contents)?;
        detect_legacy_route_policy(&value)?;
        let policy: Self = serde_json::from_value(value)?;
        policy.validate()?;
        Ok(policy)
    }

    /// Validate route grant policy against installed extension surfaces.
    ///
    /// # Errors
    ///
    /// Returns an error when a grantor, source, event, or grant entry does not
    /// match an installed extension route.
    pub fn validate_against_extensions(
        &self,
        extensions: &[RegisteredExtension],
    ) -> Result<(), SpindleError> {
        for (grantor, grants) in &self.routes {
            let grantor_extension = extensions
                .iter()
                .find(|extension| extension.id == *grantor)
                .ok_or_else(|| SpindleError::UnknownPolicyGrantor {
                    grantor: grantor.clone(),
                })?;
            for grant in grants {
                if grant.source != WILDCARD
                    && !route_source_is_valid(&grant.source, &grant.event, extensions, self)
                {
                    return Err(SpindleError::UnknownPolicyGrantSource {
                        grantor: grantor.clone(),
                        grant_source: grant.source.clone(),
                    });
                }
                if grant.event != WILDCARD
                    && !route_event_is_known(
                        &grant.event,
                        Some(grant.source.as_str()),
                        extensions,
                        self,
                    )
                {
                    return Err(SpindleError::UnknownPolicyGrantEvent {
                        grantor: grantor.clone(),
                        event: grant.event.clone(),
                    });
                }
                if grant.source != WILDCARD
                    && grant.event != WILDCARD
                    && !grantor_extension
                        .routes
                        .iter()
                        .any(|route| route_matches_policy_route(route, &grant.source, &grant.event))
                {
                    return Err(SpindleError::OrphanPolicyGrant {
                        grantor: grantor.clone(),
                        grant_source: grant.source.clone(),
                        event: grant.event.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Ensure a direct event source may emit one event kind.
    ///
    /// # Errors
    ///
    /// Returns an error if the source/event pair is not allowed.
    pub fn ensure_emit(&self, source: &str, kind: &str) -> Result<(), SpindleError> {
        if !is_allowed(&self.emits, source, kind) {
            return Err(SpindleError::EventEmitDenied {
                event_source: String::from(source),
                kind: String::from(kind),
            });
        }
        Ok(())
    }

    /// Ensure a direct client may grant the supplied capabilities.
    ///
    /// # Errors
    ///
    /// Returns an error if any capability is not allowed for this source.
    pub fn ensure_direct_grants(
        &self,
        source: &str,
        capabilities: &[String],
    ) -> Result<(), SpindleError> {
        ensure_grants("direct client", source, capabilities, &self.direct)
    }

    /// Ensure a route-owning extension may grant all route capabilities.
    ///
    /// # Errors
    ///
    /// Returns an error if any route grants a capability not allowed for this
    /// extension id.
    pub fn ensure_route_grants(&self, extension: &RegisteredExtension) -> Result<(), SpindleError> {
        for route in &extension.routes {
            self.ensure_route_capabilities(&extension.id, route)?;
        }
        Ok(())
    }

    /// Ensure one route-owning extension may grant the supplied capabilities.
    ///
    /// # Errors
    ///
    /// Returns an error if any capability is not allowed for this extension id.
    pub fn ensure_route_capabilities(
        &self,
        extension_id: &str,
        route: &ExtensionRoute,
    ) -> Result<(), SpindleError> {
        if route.capabilities.is_empty() {
            return Ok(());
        }
        let source = route.source.as_deref().ok_or(SpindleError::InvalidField {
            field: "route.source",
            reason: "is required when route grants capabilities",
        })?;
        for capability in &route.capabilities {
            if !self.route_capability_allowed(extension_id, source, &route.event, capability) {
                return Err(SpindleError::CapabilityGrantDenied {
                    grant_kind: "route extension",
                    grantor: String::from(extension_id),
                    capability: capability.clone(),
                });
            }
        }
        Ok(())
    }

    fn route_capability_allowed(
        &self,
        extension_id: &str,
        source: &str,
        event: &str,
        capability: &str,
    ) -> bool {
        self.routes.get(extension_id).is_some_and(|grants| {
            grants.iter().any(|grant| {
                value_matches(&grant.source, source)
                    && value_matches(&grant.event, event)
                    && grant
                        .capabilities
                        .iter()
                        .any(|allowed| allowed == capability || allowed == WILDCARD)
            })
        })
    }

    fn validate(&self) -> Result<(), SpindleError> {
        validate_policy_map("emits", &self.emits)?;
        validate_policy_map("direct", &self.direct)?;
        validate_route_policy_map("routes", &self.routes)
    }
}

/// Validate one extension's routes against installed extension surfaces.
///
/// # Errors
///
/// Returns an error when a route references an unknown event, source, or action.
pub fn validate_extension_routes(
    extension: &RegisteredExtension,
    extensions: &[RegisteredExtension],
    policy: &CapabilityPolicy,
) -> Result<(), SpindleError> {
    let actions = routable_actions(extensions);
    for route in &extension.routes {
        if !route_event_is_known(&route.event, route.source.as_deref(), extensions, policy) {
            return Err(SpindleError::UnknownRouteEvent {
                extension: extension.id.clone(),
                event: route.event.clone(),
            });
        }
        if let Some(source) = &route.source
            && !route_source_is_valid(source, &route.event, extensions, policy)
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

fn routable_actions(extensions: &[RegisteredExtension]) -> BTreeSet<&str> {
    extensions
        .iter()
        .filter(|extension| extension.runtime != ExtensionRuntime::Recipe)
        .flat_map(|extension| extension.actions.keys().map(String::as_str))
        .collect()
}

fn route_event_is_known(
    event: &str,
    source: Option<&str>,
    extensions: &[RegisteredExtension],
    policy: &CapabilityPolicy,
) -> bool {
    if extensions
        .iter()
        .any(|extension| extension_owns_event(extension, event))
    {
        return true;
    }
    source.map_or_else(
        || {
            policy.emits.values().any(|events| {
                events
                    .iter()
                    .any(|allowed| allowed == event || allowed == WILDCARD)
            })
        },
        |source| is_allowed(&policy.emits, source, event),
    )
}

fn route_source_is_valid(
    source: &str,
    event: &str,
    extensions: &[RegisteredExtension],
    policy: &CapabilityPolicy,
) -> bool {
    if let Some(owner) = extensions.iter().find(|extension| extension.id == source) {
        return extension_owns_event(owner, event);
    }
    is_allowed(&policy.emits, source, event)
}

fn extension_owns_event(extension: &RegisteredExtension, event: &str) -> bool {
    extension.emits.iter().any(|kind| kind == event)
        || extension.produces.iter().any(|kind| kind == event)
}

fn route_matches_policy_route(
    route: &ExtensionRoute,
    grant_source: &str,
    grant_event: &str,
) -> bool {
    if !value_matches(grant_event, &route.event) {
        return false;
    }
    route
        .source
        .as_deref()
        .is_none_or(|route_source| value_matches(grant_source, route_source))
}

fn detect_legacy_route_policy(value: &serde_json::Value) -> Result<(), SpindleError> {
    let Some(routes) = value.get("routes").and_then(serde_json::Value::as_object) else {
        return Ok(());
    };
    for (grantor, grants) in routes {
        let Some(array) = grants.as_array() else {
            continue;
        };
        if array.iter().any(serde_json::Value::is_string) {
            return Err(SpindleError::LegacyRouteGrantPolicy {
                grantor: grantor.clone(),
            });
        }
    }
    Ok(())
}

fn ensure_grants(
    grant_kind: &'static str,
    grantor: &str,
    capabilities: &[String],
    allowed: &BTreeMap<String, Vec<String>>,
) -> Result<(), SpindleError> {
    for capability in capabilities {
        if !is_allowed(allowed, grantor, capability) {
            return Err(SpindleError::CapabilityGrantDenied {
                grant_kind,
                grantor: String::from(grantor),
                capability: capability.clone(),
            });
        }
    }
    Ok(())
}

/// Capability grant policy for one route source/event boundary.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteGrantPolicy {
    /// Event source this route may match.
    pub source: String,
    /// Event kind this route may match.
    pub event: String,
    /// Capabilities this route may grant.
    #[serde(default)]
    pub capabilities: Vec<String>,
}

fn validate_policy_map(
    field: &'static str,
    policy: &BTreeMap<String, Vec<String>>,
) -> Result<(), SpindleError> {
    for (grantor, capabilities) in policy {
        validate_grantor(field, grantor)?;
        validate_capability_values(field, capabilities)?;
    }
    Ok(())
}

fn validate_route_policy_map(
    field: &'static str,
    policy: &BTreeMap<String, Vec<RouteGrantPolicy>>,
) -> Result<(), SpindleError> {
    for (grantor, grants) in policy {
        validate_grantor(field, grantor)?;
        for grant in grants {
            validate_policy_value(field, &grant.source)?;
            validate_policy_value(field, &grant.event)?;
            validate_capability_values(field, &grant.capabilities)?;
        }
    }
    Ok(())
}

fn validate_grantor(field: &'static str, grantor: &str) -> Result<(), SpindleError> {
    if grantor == WILDCARD {
        return Err(SpindleError::InvalidField {
            field,
            reason: "grantor wildcard is not supported",
        });
    }
    validate_name(field, grantor)
}

fn validate_capability_values(
    field: &'static str,
    capabilities: &[String],
) -> Result<(), SpindleError> {
    let mut seen = BTreeSet::new();
    for capability in capabilities {
        validate_policy_value(field, capability)?;
        if !seen.insert(capability) {
            return Err(SpindleError::InvalidField {
                field,
                reason: "must not contain duplicate capabilities",
            });
        }
    }
    Ok(())
}

fn validate_policy_value(field: &'static str, value: &str) -> Result<(), SpindleError> {
    if value == WILDCARD {
        return Ok(());
    }
    validate_name(field, value)
}

fn is_allowed(allowed: &BTreeMap<String, Vec<String>>, grantor: &str, capability: &str) -> bool {
    allowed.get(grantor).is_some_and(|capabilities| {
        capabilities
            .iter()
            .any(|allowed| allowed == capability || allowed == WILDCARD)
    })
}

fn value_matches(pattern: &str, value: &str) -> bool {
    pattern == WILDCARD || pattern == value
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::*;
    use crate::ExtensionAction;

    #[test]
    fn default_policy_denies_route_grants() {
        let policy = CapabilityPolicy::default();
        let route = route(
            "aerospace.workspace.changed",
            "aerospace",
            &["aerospace.state.read"],
        );

        let result = policy.ensure_route_capabilities("workspace-indicator", &route);

        assert!(matches!(
            result,
            Err(SpindleError::CapabilityGrantDenied { .. })
        ));
    }

    #[test]
    fn default_policy_denies_direct_grants() {
        let policy = CapabilityPolicy::default();
        let result =
            policy.ensure_direct_grants("raycast", &[String::from("aerospace.window.control")]);

        assert!(matches!(
            result,
            Err(SpindleError::CapabilityGrantDenied { .. })
        ));
    }

    #[test]
    fn user_policy_file_replaces_default_policy() -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        fs::write(
            dir.join(POLICY_FILE),
            r#"{"direct":{},"routes":{"workspace-indicator":[{"source":"aerospace","event":"aerospace.workspace.changed","capabilities":["aerospace.state.read"]}]}}"#,
        )?;

        let policy = CapabilityPolicy::load(&dir)?;
        policy.ensure_route_capabilities(
            "workspace-indicator",
            &route(
                "aerospace.workspace.changed",
                "aerospace",
                &["aerospace.state.read"],
            ),
        )?;
        let denied_source = policy.ensure_route_capabilities(
            "workspace-indicator",
            &route(
                "aerospace.workspace.changed",
                "sketchybar",
                &["aerospace.state.read"],
            ),
        );
        let denied_event = policy.ensure_route_capabilities(
            "workspace-indicator",
            &route(
                "sketchybar.workspace.clicked",
                "aerospace",
                &["aerospace.state.read"],
            ),
        );

        assert!(matches!(
            denied_source,
            Err(SpindleError::CapabilityGrantDenied { .. })
        ));
        assert!(matches!(
            denied_event,
            Err(SpindleError::CapabilityGrantDenied { .. })
        ));
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn policy_rejects_unknown_top_level_fields() -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        fs::write(
            dir.join(POLICY_FILE),
            r#"{"emits":{},"direct":{},"routes":{},"typo":{}}"#,
        )?;

        let result = CapabilityPolicy::load(&dir);

        assert!(result.is_err());
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn policy_rejects_grantor_wildcard() -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        fs::write(
            dir.join(POLICY_FILE),
            r#"{"emits":{"*":["test.changed"]},"direct":{},"routes":{}}"#,
        )?;

        let result = CapabilityPolicy::load(&dir);

        assert!(matches!(result, Err(SpindleError::InvalidField { .. })));
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn policy_allows_value_wildcard() {
        let policy = CapabilityPolicy {
            direct: BTreeMap::from([(String::from("unit"), vec![String::from(WILDCARD)])]),
            routes: BTreeMap::from([(
                String::from("workflow"),
                vec![RouteGrantPolicy {
                    source: String::from(WILDCARD),
                    event: String::from(WILDCARD),
                    capabilities: vec![String::from(WILDCARD)],
                }],
            )]),
            ..CapabilityPolicy::default()
        };

        assert!(
            policy
                .ensure_direct_grants("unit", &[String::from("anything.allowed")])
                .is_ok()
        );
        assert!(
            policy
                .ensure_route_capabilities(
                    "workflow",
                    &route("anything.happened", "anything", &["anything.allowed"]),
                )
                .is_ok()
        );
    }

    #[test]
    fn policy_rejects_legacy_route_grant_shape() -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        fs::write(
            dir.join(POLICY_FILE),
            r#"{"routes":{"workspace-indicator":["aerospace.state.read"]}}"#,
        )?;

        let result = CapabilityPolicy::load(&dir);

        assert!(matches!(
            result,
            Err(SpindleError::LegacyRouteGrantPolicy { .. })
        ));
        fs::remove_dir_all(dir)?;
        Ok(())
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

        let result = validate_extension_routes(
            &consumer,
            &[provider, consumer.clone()],
            &CapabilityPolicy::default(),
        );

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

        let result = validate_extension_routes(
            &consumer,
            &[provider, consumer.clone()],
            &CapabilityPolicy::default(),
        );

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

        let result = validate_extension_routes(
            &consumer,
            &[provider, consumer.clone()],
            &CapabilityPolicy::default(),
        );

        assert!(matches!(
            result,
            Err(SpindleError::UnknownRouteEvent { .. })
        ));
    }

    #[test]
    fn validate_policy_against_extensions_rejects_orphan_grant() {
        let provider = registered_extension(
            "aerospace",
            &["aerospace.workspace.changed", "aerospace.other.changed"],
            &[],
            &[],
            &[],
        );
        let consumer = registered_extension(
            "workspace-indicator",
            &[],
            &[],
            &[("workspace-indicator.render", ExtensionAction::default())],
            &[ExtensionRoute {
                event: String::from("aerospace.workspace.changed"),
                source: Some(String::from("aerospace")),
                action: String::from("workspace-indicator.render"),
                capabilities: vec![String::from("aerospace.state.read")],
                args: serde_json::json!({}),
            }],
        );
        let policy = CapabilityPolicy {
            routes: BTreeMap::from([(
                String::from("workspace-indicator"),
                vec![RouteGrantPolicy {
                    source: String::from("aerospace"),
                    event: String::from("aerospace.other.changed"),
                    capabilities: vec![String::from("aerospace.state.read")],
                }],
            )]),
            ..CapabilityPolicy::default()
        };

        let result = policy.validate_against_extensions(&[provider, consumer]);

        assert!(matches!(
            result,
            Err(SpindleError::OrphanPolicyGrant { .. })
        ));
    }

    fn route(event: &str, source: &str, capabilities: &[&str]) -> ExtensionRoute {
        ExtensionRoute {
            event: String::from(event),
            source: Some(String::from(source)),
            action: String::from("test.action"),
            capabilities: capabilities
                .iter()
                .map(|capability| String::from(*capability))
                .collect(),
            args: serde_json::json!({}),
        }
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
            manifest_path: PathBuf::from(format!("/tmp/{id}.json")),
            runtime: ExtensionRuntime::StdioJsonl,
            entrypoint: Some(String::from("./bin/extension")),
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
