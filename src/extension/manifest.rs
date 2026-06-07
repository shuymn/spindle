use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::BufReader,
    path::Path,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use spindle_extension_sdk::{ExtensionRegistration, RegistrationAction, RegistrationRoute};

use crate::{ExtensionRuntimeHost, SpindleError, validate_token};

/// Supported extension execution modes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExtensionRuntime {
    /// Communicate with a child process through JSON lines over stdio.
    #[default]
    StdioJsonl,
    /// Declarative package that contributes routes but runs no process.
    Recipe,
}

/// Action exposed by an extension.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionAction {
    /// Capabilities required to invoke this action.
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// Declarative event-to-action route contributed by an extension or recipe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionRoute {
    /// Event type matched by this route.
    pub event: String,
    /// Action to invoke when the event matches.
    pub action: String,
    /// Capabilities granted by this route.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Static arguments merged into the event payload before invoking.
    #[serde(default = "empty_object")]
    pub args: Value,
}

impl From<RegistrationAction> for ExtensionAction {
    fn from(action: RegistrationAction) -> Self {
        Self {
            capabilities: action.capabilities,
        }
    }
}

impl From<RegistrationRoute> for ExtensionRoute {
    fn from(route: RegistrationRoute) -> Self {
        Self {
            event: route.event,
            action: route.action,
            capabilities: route.capabilities,
            args: route.args,
        }
    }
}

/// Static extension manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionManifest {
    /// Extension identifier.
    pub id: String,
    /// Extension version string.
    pub version: String,
    /// Executable or script path relative to the manifest file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
    /// Runtime used to launch the extension.
    #[serde(default)]
    pub runtime: ExtensionRuntime,
    /// Event types this extension can emit.
    #[serde(default)]
    pub emits: Vec<String>,
    /// Capabilities declared by this extension.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Actions exposed by this extension.
    #[serde(default)]
    pub actions: BTreeMap<String, ExtensionAction>,
    /// Routes contributed by this extension package.
    #[serde(default)]
    pub routes: Vec<ExtensionRoute>,
}

impl ExtensionManifest {
    /// Load a manifest from JSON and validate it.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read, cannot be parsed as JSON,
    /// or violates spindle's manifest rules.
    pub fn from_path(path: &Path) -> Result<Self, SpindleError> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let manifest: Self = serde_json::from_reader(reader)?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Load a manifest and trusted extension-owned registration.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read, cannot be parsed as JSON,
    /// violates spindle's manifest rules, or registration fails.
    pub fn from_path_with_registration(
        path: &Path,
        runtime: &mut ExtensionRuntimeHost,
    ) -> Result<Self, SpindleError> {
        let mut manifest = Self::from_path(path)?;
        manifest.apply_runtime_registration(path, runtime)?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Validate static manifest fields.
    ///
    /// # Errors
    ///
    /// Returns an error when required fields are missing, names contain control
    /// characters, or an action requires an undeclared capability.
    pub fn validate(&self) -> Result<(), SpindleError> {
        validate_token("id", &self.id)?;
        validate_token("version", &self.version)?;
        self.validate_runtime_fields()?;

        for event in &self.emits {
            validate_token("emits", event)?;
        }
        validate_unique_values("emits", &self.emits)?;

        for capability in &self.capabilities {
            validate_token("capability", capability)?;
        }
        validate_unique_values("capabilities", &self.capabilities)?;

        let declared = self.capabilities.iter().collect::<BTreeSet<_>>();
        for (action, definition) in &self.actions {
            validate_token("action", action)?;
            for capability in &definition.capabilities {
                validate_token("capability", capability)?;
                if !declared.contains(capability) {
                    return Err(SpindleError::UndeclaredCapability {
                        action: action.clone(),
                        capability: capability.clone(),
                    });
                }
            }
        }

        for route in &self.routes {
            validate_token("route.event", &route.event)?;
            validate_token("route.action", &route.action)?;
            for capability in &route.capabilities {
                validate_token("route.capability", capability)?;
            }
        }

        Ok(())
    }

    fn validate_runtime_fields(&self) -> Result<(), SpindleError> {
        match self.runtime {
            ExtensionRuntime::Recipe => validate_optional_entrypoint(self.entrypoint.as_deref()),
            ExtensionRuntime::StdioJsonl => {
                validate_required_entrypoint(self.entrypoint.as_deref())
            }
        }
    }

    fn apply_runtime_registration(
        &mut self,
        manifest_path: &Path,
        runtime: &mut ExtensionRuntimeHost,
    ) -> Result<(), SpindleError> {
        let Some(registration) = runtime.load_registration(self, manifest_path)? else {
            return Ok(());
        };
        self.merge_registration(registration)?;
        Ok(())
    }

    fn merge_registration(
        &mut self,
        registration: ExtensionRegistration,
    ) -> Result<(), SpindleError> {
        for event in registration.emits {
            if !self.emits.contains(&event) {
                self.emits.push(event);
            }
        }
        for capability in registration.capabilities {
            if !self.capabilities.contains(&capability) {
                self.capabilities.push(capability);
            }
        }
        for (name, action) in registration.actions {
            let action = ExtensionAction::from(action);
            if let Some(existing) = self.actions.get(&name) {
                if existing == &action {
                    continue;
                }
                return Err(SpindleError::InvalidField {
                    field: "actions",
                    reason: "must not conflict with static manifest actions",
                });
            }
            self.actions.insert(name, action);
        }
        for route in registration.routes {
            let route = ExtensionRoute::from(route);
            if !self.routes.contains(&route) {
                self.routes.push(route);
            }
        }
        Ok(())
    }
}

fn validate_optional_entrypoint(entrypoint: Option<&str>) -> Result<(), SpindleError> {
    if let Some(entrypoint) = entrypoint {
        validate_token("entrypoint", entrypoint)?;
    }
    Ok(())
}

fn validate_required_entrypoint(entrypoint: Option<&str>) -> Result<(), SpindleError> {
    let Some(entrypoint) = entrypoint else {
        return Err(SpindleError::InvalidField {
            field: "entrypoint",
            reason: "is required unless runtime is recipe",
        });
    };
    validate_token("entrypoint", entrypoint)
}

fn validate_unique_values(field: &'static str, values: &[String]) -> Result<(), SpindleError> {
    let mut seen = BTreeSet::new();
    for value in values {
        if !seen.insert(value) {
            return Err(SpindleError::InvalidField {
                field,
                reason: "must not contain duplicate values",
            });
        }
    }
    Ok(())
}

fn empty_object() -> Value {
    Value::Object(serde_json::Map::new())
}
