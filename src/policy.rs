use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufReader, Write},
    path::Path,
};

use serde::{Deserialize, Serialize};

use crate::{RegisteredExtension, SpindleError, validate_token};

const POLICY_FILE: &str = "capabilities.json";
const WILDCARD: &str = "*";

/// Local allow policy for capability grants.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityPolicy {
    /// Capabilities direct clients may grant by request source.
    #[serde(default)]
    pub direct: BTreeMap<String, Vec<String>>,
    /// Capabilities route-owning extensions may grant.
    #[serde(default)]
    pub routes: BTreeMap<String, Vec<String>>,
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

    /// Persist exact route grants required by an installed extension.
    ///
    /// # Errors
    ///
    /// Returns an error when the policy cannot be written or the resulting
    /// grant names are invalid.
    pub fn authorize_route_grants(
        state_dir: &Path,
        extension: &RegisteredExtension,
    ) -> Result<Self, SpindleError> {
        let mut policy = Self::load(state_dir)?;
        policy.add_route_grants(extension);
        policy.validate()?;
        policy.save(state_dir)?;
        Ok(policy)
    }

    /// Ensure a route-owning extension may grant all route capabilities.
    ///
    /// # Errors
    ///
    /// Returns an error if any route grants a capability not allowed for this
    /// extension id.
    pub fn ensure_route_grants(&self, extension: &RegisteredExtension) -> Result<(), SpindleError> {
        for route in &extension.routes {
            self.ensure_route_capabilities(&extension.id, &route.capabilities)?;
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
        capabilities: &[String],
    ) -> Result<(), SpindleError> {
        ensure_grants("route extension", extension_id, capabilities, &self.routes)
    }

    fn add_route_grants(&mut self, extension: &RegisteredExtension) {
        let required = extension
            .routes
            .iter()
            .flat_map(|route| route.capabilities.iter())
            .collect::<BTreeSet<_>>();
        if required.is_empty() {
            return;
        }

        let grants = self.routes.entry(extension.id.clone()).or_default();
        for capability in required {
            if !grants.contains(capability) {
                grants.push(capability.clone());
            }
        }
    }

    fn save(&self, state_dir: &Path) -> Result<(), SpindleError> {
        fs::create_dir_all(state_dir)?;
        let mut file = File::create(state_dir.join(POLICY_FILE))?;
        serde_json::to_writer_pretty(&mut file, self)?;
        writeln!(file)?;
        Ok(())
    }

    fn validate(&self) -> Result<(), SpindleError> {
        validate_policy_map("direct", &self.direct)?;
        validate_policy_map("routes", &self.routes)
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

fn validate_policy_map(
    field: &'static str,
    policy: &BTreeMap<String, Vec<String>>,
) -> Result<(), SpindleError> {
    for (grantor, capabilities) in policy {
        validate_token(field, grantor)?;
        let mut seen = BTreeSet::new();
        for capability in capabilities {
            validate_token(field, capability)?;
            if !seen.insert(capability) {
                return Err(SpindleError::InvalidField {
                    field,
                    reason: "must not contain duplicate capabilities",
                });
            }
        }
    }
    Ok(())
}

fn is_allowed(allowed: &BTreeMap<String, Vec<String>>, grantor: &str, capability: &str) -> bool {
    allowed.get(grantor).is_some_and(|capabilities| {
        capabilities
            .iter()
            .any(|allowed| allowed == capability || allowed == WILDCARD)
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn default_policy_denies_route_grants() {
        let policy = CapabilityPolicy::default();

        let result = policy.ensure_route_capabilities(
            "workspace-indicator",
            &[
                String::from("aerospace.state.read"),
                String::from("sketchybar.ui.write"),
            ],
        );

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
            r#"{"direct":{},"routes":{"workspace-indicator":["aerospace.state.read"]}}"#,
        )?;

        let policy = CapabilityPolicy::load(&dir)?;
        policy.ensure_route_capabilities(
            "workspace-indicator",
            &[String::from("aerospace.state.read")],
        )?;
        let result = policy.ensure_route_capabilities(
            "workspace-indicator",
            &[String::from("sketchybar.ui.write")],
        );

        assert!(matches!(
            result,
            Err(SpindleError::CapabilityGrantDenied { .. })
        ));
        fs::remove_dir_all(dir)?;
        Ok(())
    }
}
