use std::path::Path;

use super::*;

#[test]
fn resolve_source_package_rejects_missing_path() {
    let result = resolve_source_package(Path::new("/nonexistent/spindle-package"));

    assert!(matches!(
        result,
        Err(SpindleError::InvalidField {
            field: "package",
            reason: "path does not exist",
        })
    ));
}

#[test]
fn resolve_source_package_rejects_non_directory() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let binary = dir.join("host");
    fs::write(&binary, b"#!/bin/sh\n")?;

    let result = resolve_source_package(&binary);

    assert!(matches!(
        result,
        Err(SpindleError::InvalidField {
            field: "package",
            reason: "input must be a directory",
        })
    ));
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn resolve_source_package_rejects_manifest_file_path() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let manifest = dir.join("extension.json");
    fs::write(&manifest, r#"{"id":"pkg","version":"0.1.0"}"#)?;

    let result = resolve_source_package(&manifest);

    assert!(matches!(
        result,
        Err(SpindleError::InvalidField {
            field: "package",
            reason: "input must be a directory",
        })
    ));
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn install_failure_does_not_leave_staged_directory() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let package = dir.join("empty-surface");
    fs::create_dir_all(package.join("bin"))?;
    crate::store::tests_support::write_executable(
        &package.join("bin").join("empty-surface"),
        "#!/bin/sh\nexit 0\n",
    )?;
    fs::write(
        package.join("extension.json"),
        r#"{
          "id": "empty-surface",
          "version": "0.1.0",
          "runtime": "stdio-jsonl"
        }"#,
    )?;
    let registry = ExtensionRegistry::in_dir(&dir);

    let result = registry.install_manifest(&package);

    assert!(matches!(
        result,
        Err(SpindleError::RuntimeTrustRequired { extension }) if extension == "empty-surface"
    ));
    assert!(!dir.join("extensions").join("empty-surface").exists());
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn manifest_rejects_entrypoint_field() {
    let result = serde_json::from_str::<ExtensionManifest>(
        r#"{
          "id": "legacy-entrypoint",
          "version": "0.1.0",
          "entrypoint": "bin/host",
          "runtime": "stdio-jsonl"
        }"#,
    );

    assert!(result.is_err());
}

#[test]
fn install_requires_bin_matching_extension_id() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let package = dir.join("missing-bin");
    fs::create_dir_all(package.join("bin"))?;
    fs::write(
        package.join("extension.json"),
        r#"{
          "id": "missing-bin",
          "version": "0.1.0",
          "runtime": "stdio-jsonl",
          "actions": {
            "missing.action": {}
          }
        }"#,
    )?;
    let registry = ExtensionRegistry::in_dir(&dir);

    let result = registry.install_manifest(&package);

    assert!(matches!(
        result,
        Err(SpindleError::EntrypointNotFound { extension, .. }) if extension == "missing-bin"
    ));
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn install_stages_package_into_state_dir() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let source = write_static_manifest(&dir, "staged", &["staged.action"], &[], &[])?;
    let registry = ExtensionRegistry::in_dir(&dir);

    let registered = registry.install_manifest(&source)?;

    assert_eq!(
        registered.package_root,
        dir.join("extensions").join("staged")
    );
    assert!(registered.package_root.join("bin/staged").is_file());
    assert!(registered.runtime_trust.is_some());
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn install_from_readonly_source_stages_into_state_dir() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let source = write_static_manifest(&dir, "readonly", &["readonly.action"], &[], &[])?;
    let mut permissions = fs::metadata(&source)?.permissions();
    permissions.set_mode(0o555);
    fs::set_permissions(&source, permissions)?;
    let state_dir = dir.join("state");
    fs::create_dir_all(&state_dir)?;
    fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o700))?;
    let registry = ExtensionRegistry::in_dir(&state_dir);

    let registered = registry.install_manifest(&source)?;

    assert!(registered.package_root.join("bin/readonly").is_file());
    fs::set_permissions(&source, fs::Permissions::from_mode(0o700))?;
    fs::remove_dir_all(dir)?;
    Ok(())
}
