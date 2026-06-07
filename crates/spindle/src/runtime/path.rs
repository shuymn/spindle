use std::path::{Path, PathBuf};

/// Resolve the conventional stdio binary path inside an extension package.
#[must_use]
pub fn resolve_package_binary(package_root: &Path, extension_id: &str) -> PathBuf {
    package_root.join("bin").join(extension_id)
}
