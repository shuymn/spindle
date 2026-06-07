use super::*;

#[test]
fn registry_rejects_action_surface_conflicts() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let first = write_static_manifest(&dir, "first", &["shared.action"], &[], &[])?;
    let second = write_static_manifest(&dir, "second", &["shared.action"], &[], &[])?;
    let registry = ExtensionRegistry::in_dir(&dir);

    registry.register_manifest(&first)?;
    let error = registry
        .register_manifest(&second)
        .err()
        .ok_or(SpindleError::InvalidField {
            field: "surface",
            reason: "conflict was not rejected",
        })?;

    assert_surface_conflict(error, "action", "shared.action", "first", "second")?;
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn registry_rejects_event_surface_conflicts() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let first = write_static_manifest(&dir, "first", &[], &["shared.changed"], &[])?;
    let second = write_static_manifest(&dir, "second", &[], &["shared.changed"], &[])?;
    let registry = ExtensionRegistry::in_dir(&dir);

    registry.register_manifest(&first)?;
    let error = registry
        .register_manifest(&second)
        .err()
        .ok_or(SpindleError::InvalidField {
            field: "surface",
            reason: "conflict was not rejected",
        })?;

    assert_surface_conflict(error, "event", "shared.changed", "first", "second")?;
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn surface_conflict_detects_emit_produce_overlap() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let first = write_static_manifest(&dir, "first", &[], &["shared.changed"], &[])?;
    let second = write_static_manifest_with_surface(
        &dir,
        "second",
        StaticManifestSurface {
            action_names: &[],
            emits: &[],
            produces: &["shared.changed"],
            capabilities: &[],
        },
    )?;
    let registry = ExtensionRegistry::in_dir(&dir);

    registry.register_manifest(&first)?;
    let error = registry
        .register_manifest(&second)
        .err()
        .ok_or(SpindleError::InvalidField {
            field: "surface",
            reason: "conflict was not rejected",
        })?;

    assert_surface_conflict(error, "event", "shared.changed", "first", "second")?;
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn surface_conflict_detects_produce_produce_overlap() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let first = write_static_manifest_with_surface(
        &dir,
        "first",
        StaticManifestSurface {
            action_names: &[],
            emits: &[],
            produces: &["shared.changed"],
            capabilities: &[],
        },
    )?;
    let second = write_static_manifest_with_surface(
        &dir,
        "second",
        StaticManifestSurface {
            action_names: &[],
            emits: &[],
            produces: &["shared.changed"],
            capabilities: &[],
        },
    )?;
    let registry = ExtensionRegistry::in_dir(&dir);

    registry.register_manifest(&first)?;
    let error = registry
        .register_manifest(&second)
        .err()
        .ok_or(SpindleError::InvalidField {
            field: "surface",
            reason: "conflict was not rejected",
        })?;

    assert_surface_conflict(error, "event", "shared.changed", "first", "second")?;
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn registry_rejects_capability_surface_conflicts() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let first = write_static_manifest(&dir, "first", &[], &[], &["shared.write"])?;
    let second = write_static_manifest(&dir, "second", &[], &[], &["shared.write"])?;
    let registry = ExtensionRegistry::in_dir(&dir);

    registry.register_manifest(&first)?;
    let error = registry
        .register_manifest(&second)
        .err()
        .ok_or(SpindleError::InvalidField {
            field: "surface",
            reason: "conflict was not rejected",
        })?;

    assert_surface_conflict(error, "capability", "shared.write", "first", "second")?;
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn registry_allows_consumer_action_capabilities_from_provider() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;

    let provider = ExtensionManifest {
        id: String::from("aerospace"),
        version: String::from("0.1.0"),
        entrypoint: None,
        runtime: ExtensionRuntime::Recipe,
        emits: Vec::new(),
        produces: Vec::new(),
        capabilities: vec![String::from("aerospace.state.read")],
        actions: BTreeMap::new(),
        routes: Vec::new(),
    };
    let mut consumer_actions = BTreeMap::new();
    consumer_actions.insert(
        String::from("workspace-indicator.render"),
        ExtensionAction {
            capabilities: vec![
                String::from("aerospace.state.read"),
                String::from("aerospace.window.control"),
                String::from("sketchybar.ui.write"),
            ],
        },
    );
    let consumer = ExtensionManifest {
        id: String::from("workspace-indicator"),
        version: String::from("0.1.0"),
        entrypoint: Some(String::from("./bin/extension")),
        runtime: ExtensionRuntime::StdioJsonl,
        emits: Vec::new(),
        produces: Vec::new(),
        capabilities: Vec::new(),
        actions: consumer_actions,
        routes: Vec::new(),
    };

    let provider_path = dir.join("aerospace.json");
    let consumer_path = dir.join("workspace-indicator.json");
    fs::write(&provider_path, serde_json::to_string_pretty(&provider)?)?;
    fs::write(&consumer_path, serde_json::to_string_pretty(&consumer)?)?;

    let registry = ExtensionRegistry::in_dir(&dir);
    registry.register_manifest(&provider_path)?;
    registry.register_manifest(&consumer_path)?;

    let extensions = registry.list()?;
    assert_eq!(extensions.len(), 2);
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn registry_serializes_concurrent_installs() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let registry = Arc::new(ExtensionRegistry::in_dir(&dir));
    let mut manifests = Vec::new();

    for index in 0..8 {
        let id = format!("extension-{index}");
        let action = format!("test.action.{index}");
        let event = format!("test.event.{index}");
        let capability = format!("test.capability.{index}");
        manifests.push(write_static_manifest(
            &dir,
            &id,
            &[action.as_str()],
            &[event.as_str()],
            &[capability.as_str()],
        )?);
    }

    let mut handles = Vec::new();
    for manifest in manifests {
        let registry = Arc::clone(&registry);
        handles.push(thread::spawn(move || {
            registry.register_manifest(&manifest).map(|_registered| ())
        }));
    }

    for handle in handles {
        handle
            .join()
            .map_err(|_payload| SpindleError::InvalidField {
                field: "thread",
                reason: "panicked",
            })??;
    }

    assert_eq!(registry.list()?.len(), 8);
    fs::remove_dir_all(dir)?;
    Ok(())
}
