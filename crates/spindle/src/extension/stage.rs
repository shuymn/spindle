use std::{
    fs,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use super::{ExtensionManifest, ExtensionRuntime};
use crate::{SpindleError, runtime::resolve_package_binary};

/// Installed extension package filename.
pub const MANIFEST_FILE: &str = "extension.json";

/// Materialized extension package under a spindle state directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedPackage {
    /// Root directory of the staged package.
    pub package_root: PathBuf,
}

/// Resolve a source package directory and manifest path from install input.
///
/// # Errors
///
/// Returns an error when the input path does not identify a readable package.
pub fn resolve_source_package(input: &Path) -> Result<(PathBuf, PathBuf), SpindleError> {
    if !input.try_exists()? {
        return Err(SpindleError::InvalidField {
            field: "package",
            reason: "path does not exist",
        });
    }

    if !input.is_dir() {
        return Err(SpindleError::InvalidField {
            field: "package",
            reason: "input must be a directory",
        });
    }

    let package_root = input.to_path_buf();
    let manifest_path = package_root.join(MANIFEST_FILE);

    if !manifest_path.is_file() {
        return Err(SpindleError::InvalidField {
            field: "package",
            reason: "extension package must contain extension.json",
        });
    }

    Ok((
        fs::canonicalize(package_root)?,
        fs::canonicalize(manifest_path)?,
    ))
}

/// Copy a source package into the spindle state directory.
///
/// # Errors
///
/// Returns an error when the source entrypoint is missing or staging fails.
pub fn materialize_package(
    state_dir: &Path,
    source_package: &Path,
    manifest: &ExtensionManifest,
) -> Result<StagedPackage, SpindleError> {
    let source_entrypoint = if manifest.runtime == ExtensionRuntime::Recipe {
        None
    } else {
        let entrypoint = resolve_package_binary(source_package, &manifest.id);
        ensure_entrypoint_ready(&manifest.id, &entrypoint)?;
        Some(entrypoint)
    };

    let package_root = staged_package_root(state_dir, &manifest.id);
    if package_root.exists() {
        fs::remove_dir_all(&package_root)?;
    }
    fs::create_dir_all(&package_root)?;

    write_staged_manifest(&package_root, manifest)?;

    if let Some(source_entrypoint) = source_entrypoint {
        let staged_entrypoint = resolve_package_binary(&package_root, &manifest.id);
        if let Some(parent) = staged_entrypoint.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&source_entrypoint, &staged_entrypoint)?;
        set_executable(&staged_entrypoint)?;
        let source_config = source_entrypoint.with_extension("json");
        if source_config.is_file() {
            fs::copy(&source_config, staged_entrypoint.with_extension("json"))?;
        }
    }

    Ok(StagedPackage { package_root })
}

/// Return the staged package directory for one extension id.
#[must_use]
pub fn staged_package_root(state_dir: &Path, extension_id: &str) -> PathBuf {
    state_dir.join("extensions").join(extension_id)
}

fn write_staged_manifest(
    package_root: &Path,
    manifest: &ExtensionManifest,
) -> Result<(), SpindleError> {
    write_manifest_file(&package_root.join(MANIFEST_FILE), manifest, true)
}

/// Rewrite the staged manifest after trusted runtime registration.
///
/// # Errors
///
/// Returns an error when the manifest cannot be written.
pub(super) fn refresh_staged_manifest(
    package_root: &Path,
    manifest: &ExtensionManifest,
) -> Result<(), SpindleError> {
    write_manifest_file(&package_root.join(MANIFEST_FILE), manifest, false)
}

fn write_manifest_file(
    manifest_path: &Path,
    manifest: &ExtensionManifest,
    create_new: bool,
) -> Result<(), SpindleError> {
    let mut options = fs::OpenOptions::new();
    options.write(true).mode(0o600);
    if create_new {
        options.create_new(true);
    } else {
        options.create(true).truncate(true);
    }
    let mut file = options.open(manifest_path)?;
    serde_json::to_writer_pretty(&mut file, manifest)?;
    writeln!(file)?;
    file.flush()?;
    file.sync_all()?;
    Ok(())
}

/// Remove a staged package directory after a failed install.
pub(super) fn remove_staged_package(package_root: &Path) {
    let _ = fs::remove_dir_all(package_root);
}

fn ensure_entrypoint_ready(extension_id: &str, path: &Path) -> Result<(), SpindleError> {
    if !path.is_file() {
        return Err(SpindleError::EntrypointNotFound {
            extension: String::from(extension_id),
            path: path.to_path_buf(),
        });
    }

    let metadata = fs::metadata(path)?;
    if metadata.permissions().mode() & 0o111 == 0 {
        return Err(SpindleError::EntrypointNotExecutable {
            extension: String::from(extension_id),
            path: path.to_path_buf(),
        });
    }

    Ok(())
}

fn set_executable(path: &Path) -> Result<(), SpindleError> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}
