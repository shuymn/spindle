use super::*;

#[test]
fn registry_rejects_legacy_extensions_json_shape() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    fs::write(
        dir.join("extensions.json"),
        r#"[
          {
            "id": "legacy-host",
            "version": "0.1.0",
            "manifest_path": "/tmp/legacy-host/extension.json",
            "runtime": "stdio-jsonl",
            "entrypoint": "bin/extension",
            "capabilities": [],
            "emits": [],
            "actions": {},
            "routes": []
          }
        ]"#,
    )?;

    let result = ExtensionRegistry::in_dir(&dir).list();

    assert!(result.is_err());
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn sdk_registration_surface_matches_core_manifest_shape() -> Result<(), SpindleError> {
    let sdk_route = RegistrationRoute::new("provider.changed", "provider.snapshot")
        .source("provider")
        .capability("provider.read")
        .with_args(serde_json::json!({ "settle_ms": 50 }));
    let sdk_action = RegistrationAction::new().capability("provider.read");

    let core_route = ExtensionRoute::from(sdk_route);
    let core_action = ExtensionAction::from(sdk_action);

    assert_eq!(
        serde_json::to_value(&core_route)?,
        serde_json::json!({
            "event": "provider.changed",
            "source": "provider",
            "action": "provider.snapshot",
            "capabilities": ["provider.read"],
            "args": { "settle_ms": 50 }
        })
    );
    assert_eq!(
        serde_json::to_value(&core_action)?,
        serde_json::json!({
            "capabilities": ["provider.read"]
        })
    );
    Ok(())
}

#[test]
fn registry_registers_manifest() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let package = write_static_manifest_with_surface(
        &dir,
        "sketchybar-agent-status",
        StaticManifestSurface {
            action_names: &["sketchybar.agentStatus.render"],
            emits: &[],
            produces: &[],
            capabilities: &["sketchybar.ui.write"],
        },
    )?;
    fs::write(
        package.join("extension.json"),
        r#"{
          "id": "sketchybar-agent-status",
          "version": "0.1.0",
          "runtime": "stdio-jsonl",
          "capabilities": ["sketchybar.ui.write"],
          "actions": {
            "sketchybar.agentStatus.render": {
              "capabilities": ["sketchybar.ui.write"]
            }
          }
        }"#,
    )?;

    let registry = ExtensionRegistry::in_dir(&dir);
    let registered = registry.install_manifest(&package)?;

    assert_eq!(registered.id, "sketchybar-agent-status");
    assert_eq!(
        registered.package_root,
        dir.join("extensions").join("sketchybar-agent-status")
    );
    assert_eq!(registry.list()?, vec![registered]);
    assert_eq!(
        fs::metadata(dir.join("extensions.json"))?
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn registry_file_is_created_private() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let package = write_static_manifest(&dir, "private-registry", &["test.action"], &[], &[])?;
    let registry = ExtensionRegistry::in_dir(&dir);

    registry.install_manifest(&package)?;

    assert_eq!(
        fs::metadata(dir.join("extensions.json"))?
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn registry_rejects_public_existing_state_dir() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let package = write_static_manifest(&dir, "public-registry", &["test.action"], &[], &[])?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755))?;
    let registry = ExtensionRegistry::in_dir(&dir);

    let result = registry.install_manifest(&package);

    assert!(matches!(
        result,
        Err(SpindleError::InvalidField {
            field: "state_dir",
            ..
        })
    ));
    assert_eq!(fs::metadata(&dir)?.permissions().mode() & 0o777, 0o755);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn registry_rejects_parent_path_that_is_not_directory() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    let package = write_static_manifest(&dir, "non-directory-parent", &["test.action"], &[], &[])?;
    let parent_file = dir.join("not-a-directory");
    fs::write(&parent_file, b"not a directory")?;
    let registry = ExtensionRegistry::in_dir(&parent_file);

    let result = registry.install_manifest(&package);

    assert!(result.is_err());
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn registry_does_not_chmod_existing_private_state_dir() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    let package = write_static_manifest(&dir, "private-state", &["test.action"], &[], &[])?;
    let registry = ExtensionRegistry::in_dir(&dir);

    registry.install_manifest(&package)?;

    assert_eq!(fs::metadata(&dir)?.permissions().mode() & 0o777, 0o700);
    fs::remove_dir_all(dir)?;
    Ok(())
}
