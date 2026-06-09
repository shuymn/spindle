use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{BufReader, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process,
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    ExtensionAction, ExtensionManifest, ExtensionRoute, ExtensionRuntime, StagedPackage,
    materialize_package, resolve_source_package,
    stage::{refresh_staged_manifest, remove_staged_package},
    surface::ensure_surface_ownership,
};
use crate::{
    ExtensionRuntimeHost, SpindleError, lock::SidecarLock, runtime::resolve_package_binary,
    store::ensure_private_state_parent,
};

/// Registered extension metadata stored by the spindle kernel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisteredExtension {
    /// Extension identifier.
    pub id: String,
    /// Extension version string.
    pub version: String,
    /// Staged extension package root directory.
    pub package_root: PathBuf,
    /// Runtime used to execute this extension.
    pub runtime: ExtensionRuntime,
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
    /// Trusted runtime snapshot captured during installation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_trust: Option<RegisteredRuntimeTrust>,
}

/// Trusted runtime metadata captured at installation time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisteredRuntimeTrust {
    /// Canonical entrypoint path.
    pub entrypoint_path: PathBuf,
    /// SHA-256 hash of the entrypoint at registration time.
    pub entrypoint_sha256: String,
    /// Registration time in milliseconds since Unix epoch.
    pub registered_at_unix_ms: u64,
}

impl RegisteredExtension {
    fn from_manifest(
        manifest: &ExtensionManifest,
        package_root: PathBuf,
        runtime_trust: Option<RegisteredRuntimeTrust>,
    ) -> Self {
        Self {
            id: manifest.id.clone(),
            version: manifest.version.clone(),
            package_root,
            runtime: manifest.runtime,
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

    fn record_staged_extension(
        &self,
        staged: &StagedPackage,
        manifest: &ExtensionManifest,
        runtime_trust: Option<RegisteredRuntimeTrust>,
    ) -> Result<RegisteredExtension, SpindleError> {
        ensure_static_surface(manifest)?;
        let registered = RegisteredExtension::from_manifest(
            manifest,
            staged.package_root.clone(),
            runtime_trust,
        );
        ensure_private_state_parent(&self.path)?;
        let _lock = SidecarLock::acquire(&self.path)?;
        let mut entries = self.list()?;
        entries.retain(|entry| entry.id != registered.id);
        entries.push(registered.clone());
        ensure_surface_ownership(&registered, &entries[..entries.len() - 1])?;
        entries.sort_by(|left, right| left.id.cmp(&right.id));
        self.write_entries(&entries)?;

        Ok(registered)
    }

    /// Install or replace an extension package.
    ///
    /// # Errors
    ///
    /// Returns an error if the package cannot be loaded or registry state
    /// cannot be read or written.
    pub fn install_manifest(&self, input: &Path) -> Result<RegisteredExtension, SpindleError> {
        let (source_package, source_manifest) = resolve_source_package(input)?;
        let manifest = ExtensionManifest::from_path(&source_manifest)?;
        ensure_static_surface(&manifest)?;
        let staged = materialize_package(self.state_dir(), &source_package, &manifest)?;
        let runtime_trust = RuntimeTrustSnapshot::for_staged(&staged, &manifest)?;
        match self.record_staged_extension(&staged, &manifest, runtime_trust) {
            Ok(registered) => Ok(registered),
            Err(error) => {
                remove_staged_package(&staged.package_root);
                Err(error)
            }
        }
    }

    /// Install or replace an extension package using trusted runtime surface.
    ///
    /// # Errors
    ///
    /// Returns an error if the package cannot be loaded or registry state
    /// cannot be read or written.
    pub fn install_manifest_with_runtime(
        &self,
        input: &Path,
        runtime: &ExtensionRuntimeHost,
    ) -> Result<RegisteredExtension, SpindleError> {
        let (source_package, source_manifest) = resolve_source_package(input)?;
        let mut manifest = ExtensionManifest::from_path(&source_manifest)?;
        let staged = materialize_package(self.state_dir(), &source_package, &manifest)?;
        let snapshot = RuntimeTrustSnapshot::capture(&staged.package_root, &manifest)?;
        let result = (|| -> Result<RegisteredExtension, SpindleError> {
            manifest.apply_runtime_registration(&staged.package_root, runtime)?;
            manifest.validate()?;
            let runtime_trust = RuntimeTrustSnapshot::verify(snapshot, &manifest.id)?;
            refresh_staged_manifest(&staged.package_root, &manifest)?;
            let registered = self.record_staged_extension(&staged, &manifest, runtime_trust)?;
            runtime.invalidate_extension(&registered.id);
            Ok(registered)
        })();
        if result.is_err() {
            remove_staged_package(&staged.package_root);
        }
        result
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
}

impl RuntimeTrustSnapshot {
    fn capture(
        package_root: &Path,
        manifest: &ExtensionManifest,
    ) -> Result<Option<Self>, SpindleError> {
        if manifest.runtime == ExtensionRuntime::Recipe {
            return Ok(None);
        }
        let entrypoint_path = resolve_package_binary(package_root, &manifest.id);
        let entrypoint_path = fs::canonicalize(entrypoint_path)?;
        Ok(Some(Self {
            entrypoint_sha256: sha256_file(&entrypoint_path)?,
            entrypoint_path,
        }))
    }

    fn for_staged(
        staged: &StagedPackage,
        manifest: &ExtensionManifest,
    ) -> Result<Option<RegisteredRuntimeTrust>, SpindleError> {
        let Some(snapshot) = Self::capture(&staged.package_root, manifest)? else {
            return Ok(None);
        };
        Ok(Some(RegisteredRuntimeTrust {
            entrypoint_sha256: snapshot.entrypoint_sha256,
            entrypoint_path: snapshot.entrypoint_path,
            registered_at_unix_ms: crate::now_unix_ms()?,
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
            registered_at_unix_ms: crate::now_unix_ms()?,
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
