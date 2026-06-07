use std::path::{Component, Path, PathBuf};

pub(super) fn resolve_manifest_path(manifest_path: &Path, entrypoint: &str) -> PathBuf {
    let path = PathBuf::from(entrypoint);
    if path.is_absolute() {
        path
    } else {
        normalize_path(
            &manifest_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(path),
        )
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                if should_preserve_parent_dir(&normalized) {
                    normalized.push(component.as_os_str());
                } else {
                    normalized.pop();
                }
            }
            Component::CurDir => {}
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn should_preserve_parent_dir(path: &Path) -> bool {
    path.as_os_str().is_empty()
        || path
            .components()
            .next_back()
            .is_some_and(|component| matches!(component, Component::ParentDir))
}
