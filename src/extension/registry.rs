use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, File},
    io::{BufReader, Write},
    path::{Path, PathBuf},
    process,
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};

use super::{
    ExtensionAction, ExtensionManifest, ExtensionRoute, ExtensionRuntime,
    surface::ensure_surface_ownership,
};
use crate::{CapabilityPolicy, ExtensionRuntimeHost, SpindleError, lock::SidecarLock};

/// Registered extension metadata stored by the spindle kernel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegisteredExtension {
    /// Extension identifier.
    pub id: String,
    /// Extension version string.
    pub version: String,
    /// Source manifest path.
    pub manifest_path: PathBuf,
    /// Runtime used to execute this extension.
    pub runtime: ExtensionRuntime,
    /// Executable or script path relative to the manifest file.
    pub entrypoint: Option<String>,
    /// Declared capabilities.
    pub capabilities: Vec<String>,
    /// Event types this extension can emit.
    pub emits: Vec<String>,
    /// Exposed actions.
    pub actions: BTreeMap<String, ExtensionAction>,
    /// Routes contributed by this extension package.
    pub routes: Vec<ExtensionRoute>,
}

impl<'de> Deserialize<'de> for RegisteredExtension {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireRegisteredExtension {
            id: String,
            version: String,
            manifest_path: PathBuf,
            runtime: ExtensionRuntime,
            entrypoint: Option<String>,
            capabilities: Vec<String>,
            emits: Vec<String>,
            actions: BTreeMap<String, StoredExtensionAction>,
            routes: Vec<ExtensionRoute>,
        }

        let stored = WireRegisteredExtension::deserialize(deserializer)?;
        Ok(Self {
            id: stored.id,
            version: stored.version,
            manifest_path: stored.manifest_path,
            runtime: stored.runtime,
            entrypoint: stored.entrypoint,
            capabilities: stored.capabilities,
            emits: stored.emits,
            actions: stored
                .actions
                .into_iter()
                .map(|(name, action)| (name, action.into_current()))
                .collect(),
            routes: stored.routes,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredExtensionAction {
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(default, rename = "command")]
    _legacy_command: Vec<String>,
}

impl StoredExtensionAction {
    fn into_current(self) -> ExtensionAction {
        ExtensionAction {
            capabilities: self.capabilities,
        }
    }
}

impl RegisteredExtension {
    fn from_manifest(manifest: &ExtensionManifest, manifest_path: PathBuf) -> Self {
        Self {
            id: manifest.id.clone(),
            version: manifest.version.clone(),
            manifest_path,
            runtime: manifest.runtime,
            entrypoint: manifest.entrypoint.clone(),
            capabilities: manifest.capabilities.clone(),
            emits: manifest.emits.clone(),
            actions: manifest.actions.clone(),
            routes: manifest.routes.clone(),
        }
    }
}

/// State-backed extension registry.
#[derive(Debug, Clone)]
pub struct ExtensionRegistry {
    path: PathBuf,
}

impl ExtensionRegistry {
    /// Create a registry located under a spindle state directory.
    #[must_use]
    pub fn in_dir(state_dir: &Path) -> Self {
        Self {
            path: state_dir.join("extensions.json"),
        }
    }

    /// Return the registry path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Register or replace an extension manifest.
    ///
    /// # Errors
    ///
    /// Returns an error if the manifest cannot be loaded or registry state
    /// cannot be read or written.
    pub fn register_manifest(
        &self,
        manifest_path: &Path,
    ) -> Result<RegisteredExtension, SpindleError> {
        let mut runtime = ExtensionRuntimeHost::new();
        self.register_manifest_with_runtime(manifest_path, &mut runtime)
    }

    /// Register or replace an extension manifest using an existing runtime host.
    ///
    /// # Errors
    ///
    /// Returns an error if the manifest cannot be loaded or registry state
    /// cannot be read or written.
    pub fn register_manifest_with_runtime(
        &self,
        manifest_path: &Path,
        runtime: &mut ExtensionRuntimeHost,
    ) -> Result<RegisteredExtension, SpindleError> {
        let manifest = ExtensionManifest::from_path_with_registration(manifest_path, runtime)?;
        let canonical_path = fs::canonicalize(manifest_path)?;
        let registered = RegisteredExtension::from_manifest(&manifest, canonical_path);

        let _lock = SidecarLock::acquire(&self.path)?;
        let mut entries = self.list()?;
        entries.retain(|entry| entry.id != registered.id);
        ensure_surface_ownership(&registered, &entries)?;
        CapabilityPolicy::authorize_route_grants(self.state_dir(), &registered)?;
        entries.push(registered.clone());
        entries.sort_by(|left, right| left.id.cmp(&right.id));
        self.write_entries(&entries)?;
        runtime.invalidate_extension(&registered.id);

        Ok(registered)
    }

    /// Install or replace an extension manifest.
    ///
    /// # Errors
    ///
    /// Returns an error if the manifest cannot be loaded or registry state
    /// cannot be read or written.
    pub fn install_manifest(
        &self,
        manifest_path: &Path,
    ) -> Result<RegisteredExtension, SpindleError> {
        self.register_manifest(manifest_path)
    }

    /// Install or replace an extension manifest using an existing runtime host.
    ///
    /// # Errors
    ///
    /// Returns an error if the manifest cannot be loaded or registry state
    /// cannot be read or written.
    pub fn install_manifest_with_runtime(
        &self,
        manifest_path: &Path,
        runtime: &mut ExtensionRuntimeHost,
    ) -> Result<RegisteredExtension, SpindleError> {
        self.register_manifest_with_runtime(manifest_path, runtime)
    }

    /// List registered extensions.
    ///
    /// # Errors
    ///
    /// Returns an error if registry state cannot be read.
    pub fn list(&self) -> Result<Vec<RegisteredExtension>, SpindleError> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }

        let file = File::open(&self.path)?;
        let reader = BufReader::new(file);
        Ok(serde_json::from_reader(reader)?)
    }

    fn write_entries(&self, entries: &[RegisteredExtension]) -> Result<(), SpindleError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }

        let temp_path = temporary_registry_path(&self.path);
        if let Err(error) = write_registry_file(&temp_path, entries) {
            let _ = fs::remove_file(&temp_path);
            return Err(error);
        }
        if let Err(error) = fs::rename(&temp_path, &self.path) {
            let _ = fs::remove_file(&temp_path);
            return Err(error.into());
        }
        Ok(())
    }

    /// Return the state directory that owns this registry.
    #[must_use]
    pub fn state_dir(&self) -> &Path {
        self.path.parent().unwrap_or_else(|| Path::new("."))
    }
}

fn write_registry_file(path: &Path, entries: &[RegisteredExtension]) -> Result<(), SpindleError> {
    let mut file = File::create(path)?;
    serde_json::to_writer_pretty(&mut file, entries)?;
    writeln!(file)?;
    file.flush()?;
    file.sync_all()?;
    Ok(())
}

static NEXT_REGISTRY_TEMP: AtomicU64 = AtomicU64::new(0);

fn temporary_registry_path(path: &Path) -> PathBuf {
    let mut file_name = path.file_name().map_or_else(
        || OsString::from("extensions.json"),
        std::ffi::OsStr::to_os_string,
    );
    let counter = NEXT_REGISTRY_TEMP.fetch_add(1, Ordering::Relaxed);
    file_name.push(".");
    file_name.push(process::id().to_string());
    file_name.push(".");
    file_name.push(counter.to_string());
    file_name.push(".tmp");

    let mut temp_path = path.to_path_buf();
    temp_path.set_file_name(file_name);
    temp_path
}
