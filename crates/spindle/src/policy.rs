use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::BufReader,
    path::Path,
};

use serde::{Deserialize, Serialize};

use crate::{ExtensionRoute, RegisteredExtension, SpindleError, validate_name};

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

        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let policy = serde_json::from_reader::<_, Self>(reader)?;
        policy.validate()?;
        Ok(policy)
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
    use std::fs;

    use super::*;

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
}
