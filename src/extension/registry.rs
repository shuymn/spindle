use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{BufReader, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    ExtensionAction, ExtensionManifest, ExtensionRoute, ExtensionRuntime,
    surface::ensure_surface_ownership,
};
use crate::{
    CapabilityPolicy, ExtensionRuntimeHost, SpindleError, lock::SidecarLock,
    store::ensure_private_state_parent,
};

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
    /// Event types this extension's actions can produce.
    pub produces: Vec<String>,
    /// Exposed actions.
    pub actions: BTreeMap<String, ExtensionAction>,
    /// Routes contributed by this extension package.
    pub routes: Vec<ExtensionRoute>,
    /// Trusted runtime snapshot captured during dynamic registration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_trust: Option<RegisteredRuntimeTrust>,
}

/// Trusted runtime metadata captured at dynamic registration time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisteredRuntimeTrust {
    /// Canonical entrypoint path.
    pub entrypoint_path: PathBuf,
    /// SHA-256 hash of the entrypoint at registration time.
    pub entrypoint_sha256: String,
    /// Registration time in milliseconds since Unix epoch.
    pub registered_at_unix_ms: u128,
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
            #[serde(default)]
            produces: Vec<String>,
            actions: BTreeMap<String, StoredExtensionAction>,
            routes: Vec<ExtensionRoute>,
            #[serde(default)]
            runtime_trust: Option<RegisteredRuntimeTrust>,
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
            produces: stored.produces,
            actions: stored
                .actions
                .into_iter()
                .map(|(name, action)| (name, action.into_current()))
                .collect(),
            routes: stored.routes,
            runtime_trust: stored.runtime_trust,
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
    fn from_manifest(
        manifest: &ExtensionManifest,
        manifest_path: PathBuf,
        runtime_trust: Option<RegisteredRuntimeTrust>,
    ) -> Self {
        Self {
            id: manifest.id.clone(),
            version: manifest.version.clone(),
            manifest_path,
            runtime: manifest.runtime,
            entrypoint: manifest.entrypoint.clone(),
            capabilities: manifest.capabilities.clone(),
            emits: manifest.emits.clone(),
            produces: manifest.produces.clone(),
            actions: manifest.actions.clone(),
            routes: manifest.routes.clone(),
            runtime_trust,
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
        let manifest = ExtensionManifest::from_path(manifest_path)?;
        self.register_manifest_surface(manifest_path, &manifest, None)
    }

    /// Register or replace an extension manifest using trusted runtime surface.
    ///
    /// # Errors
    ///
    /// Returns an error if the manifest cannot be loaded or registry state
    /// cannot be read or written.
    pub fn register_manifest_trusting_runtime(
        &self,
        manifest_path: &Path,
        runtime: &ExtensionRuntimeHost,
    ) -> Result<RegisteredExtension, SpindleError> {
        let mut manifest = ExtensionManifest::from_path(manifest_path)?;
        let runtime_trust = RuntimeTrustSnapshot::for_manifest(manifest_path, &manifest)?;
        manifest.apply_runtime_registration(manifest_path, runtime)?;
        manifest.validate()?;
        let runtime_trust = RuntimeTrustSnapshot::verify(runtime_trust, &manifest.id)?;
        let registered = self.register_manifest_surface(manifest_path, &manifest, runtime_trust)?;
        runtime.invalidate_extension(&registered.id);
        Ok(registered)
    }

    fn register_manifest_surface(
        &self,
        manifest_path: &Path,
        manifest: &ExtensionManifest,
        runtime_trust: Option<RegisteredRuntimeTrust>,
    ) -> Result<RegisteredExtension, SpindleError> {
        ensure_static_surface(manifest)?;
        let canonical_path = fs::canonicalize(manifest_path)?;
        let registered =
            RegisteredExtension::from_manifest(manifest, canonical_path, runtime_trust);
        let policy = CapabilityPolicy::load(self.state_dir())?;
        policy.ensure_route_grants(&registered)?;

        ensure_private_state_parent(&self.path)?;
        let _lock = SidecarLock::acquire(&self.path)?;
        let mut entries = self.list()?;
        entries.retain(|entry| entry.id != registered.id);
        ensure_surface_ownership(&registered, &entries)?;
        entries.push(registered.clone());
        entries.sort_by(|left, right| left.id.cmp(&right.id));
        self.write_entries(&entries)?;

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
        runtime: &ExtensionRuntimeHost,
    ) -> Result<RegisteredExtension, SpindleError> {
        self.register_manifest_trusting_runtime(manifest_path, runtime)
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
        let temp_path = temporary_registry_path(&self.path);
        if let Err(error) = write_registry_file(&temp_path, entries) {
            let _ = fs::remove_file(&temp_path);
            return Err(error);
        }
        if let Err(error) = fs::rename(&temp_path, &self.path) {
            let _ = fs::remove_file(&temp_path);
            return Err(error.into());
        }
        sync_parent_dir(&self.path)?;
        Ok(())
    }

    /// Return the state directory that owns this registry.
    #[must_use]
    pub fn state_dir(&self) -> &Path {
        self.path.parent().unwrap_or_else(|| Path::new("."))
    }
}

fn write_registry_file(path: &Path, entries: &[RegisteredExtension]) -> Result<(), SpindleError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    serde_json::to_writer_pretty(&mut file, entries)?;
    writeln!(file)?;
    file.flush()?;
    file.sync_all()?;
    Ok(())
}

fn sync_parent_dir(path: &Path) -> Result<(), SpindleError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn ensure_static_surface(manifest: &ExtensionManifest) -> Result<(), SpindleError> {
    if manifest.runtime == ExtensionRuntime::Recipe
        || !manifest.emits.is_empty()
        || !manifest.produces.is_empty()
        || !manifest.capabilities.is_empty()
        || !manifest.actions.is_empty()
        || !manifest.routes.is_empty()
    {
        return Ok(());
    }

    Err(SpindleError::RuntimeTrustRequired {
        extension: manifest.id.clone(),
    })
}

#[derive(Debug)]
struct RuntimeTrustSnapshot {
    entrypoint_path: PathBuf,
    entrypoint_sha256: String,
    registered_at_unix_ms: u128,
}

impl RuntimeTrustSnapshot {
    fn for_manifest(
        manifest_path: &Path,
        manifest: &ExtensionManifest,
    ) -> Result<Option<Self>, SpindleError> {
        if manifest.runtime == ExtensionRuntime::Recipe {
            return Ok(None);
        }
        let Some(entrypoint) = manifest.entrypoint.as_deref() else {
            return Ok(None);
        };
        let entrypoint_path = crate::runtime::resolve_manifest_path(manifest_path, entrypoint);
        let entrypoint_path = fs::canonicalize(entrypoint_path)?;
        Ok(Some(Self {
            entrypoint_sha256: sha256_file(&entrypoint_path)?,
            entrypoint_path,
            registered_at_unix_ms: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
        }))
    }

    fn verify(
        snapshot: Option<Self>,
        extension: &str,
    ) -> Result<Option<RegisteredRuntimeTrust>, SpindleError> {
        let Some(snapshot) = snapshot else {
            return Ok(None);
        };
        let current = sha256_file(&snapshot.entrypoint_path)?;
        if current != snapshot.entrypoint_sha256 {
            return Err(SpindleError::ExtensionTrustChanged {
                extension: extension.to_owned(),
                entrypoint: snapshot.entrypoint_path,
            });
        }
        Ok(Some(RegisteredRuntimeTrust {
            entrypoint_path: snapshot.entrypoint_path,
            entrypoint_sha256: snapshot.entrypoint_sha256,
            registered_at_unix_ms: snapshot.registered_at_unix_ms,
        }))
    }
}

pub fn sha256_file(path: &Path) -> Result<String, SpindleError> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
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
