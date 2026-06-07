use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
};

use spindle_extension_sdk::ActionInvocation;

use super::*;
use crate::{ExtensionRuntimeHost, SpindleError};

#[test]
fn manifest_requires_action_capabilities_to_be_declared() {
    let mut actions = BTreeMap::new();
    actions.insert(
        String::from("sketchybar.render"),
        ExtensionAction {
            capabilities: vec![String::from("sketchybar.ui.write")],
        },
    );

    let manifest = ExtensionManifest {
        id: String::from("sketchybar-agent-status"),
        version: String::from("0.1.0"),
        entrypoint: Some(String::from("./bin/extension")),
        runtime: ExtensionRuntime::StdioJsonl,
        emits: Vec::new(),
        capabilities: Vec::new(),
        actions,
        routes: Vec::new(),
    };

    assert!(manifest.validate().is_err());
}

#[test]
fn manifest_rejects_legacy_action_command_metadata() {
    let result = serde_json::from_str::<ExtensionManifest>(
        r#"{
          "id": "legacy-host",
          "version": "0.1.0",
          "entrypoint": "./bin/extension",
          "actions": {
            "legacy.render": {
              "capabilities": [],
              "command": ["render"]
            }
          }
        }"#,
    );

    assert!(result.is_err());
}

#[test]
fn registry_reads_legacy_entries_with_action_command_metadata() -> Result<(), SpindleError> {
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
            "entrypoint": "./bin/extension",
            "capabilities": ["legacy.write"],
            "emits": [],
            "actions": {
              "legacy.render": {
                "capabilities": ["legacy.write"],
                "command": ["render"]
              }
            },
            "routes": []
          }
        ]"#,
    )?;

    let entries = ExtensionRegistry::in_dir(&dir).list()?;

    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].actions["legacy.render"].capabilities,
        ["legacy.write"]
    );
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn registry_registers_manifest() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let manifest_path = dir.join("extension.json");
    fs::write(
        &manifest_path,
        r#"{
              "id": "sketchybar-agent-status",
              "version": "0.1.0",
              "runtime": "recipe",
              "capabilities": ["sketchybar.ui.write"],
              "actions": {
                "sketchybar.agentStatus.render": {
                  "capabilities": ["sketchybar.ui.write"]
                }
              }
            }"#,
    )?;

    let registry = ExtensionRegistry::in_dir(&dir);
    let registered = registry.register_manifest(&manifest_path)?;

    assert_eq!(registered.id, "sketchybar-agent-status");
    assert_eq!(registered.entrypoint, None);
    assert_eq!(registry.list()?, vec![registered]);
    fs::remove_dir_all(dir)?;
    Ok(())
}

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

#[test]
fn registry_authorizes_installed_route_grants() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let manifest_path = dir.join("extension.json");
    fs::write(
        &manifest_path,
        r#"{
          "id": "workflow",
          "version": "0.1.0",
          "runtime": "recipe",
          "routes": [
            {
              "event": "provider.changed",
              "action": "provider.snapshot",
              "capabilities": ["provider.read"]
            }
          ]
        }"#,
    )?;

    let registry = ExtensionRegistry::in_dir(&dir);
    let registered = registry.register_manifest(&manifest_path)?;
    let policy = crate::CapabilityPolicy::load(&dir)?;

    policy.ensure_route_grants(&registered)?;
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn recipe_manifest_can_contribute_routes_without_entrypoint() -> Result<(), SpindleError> {
    let manifest = ExtensionManifest {
        id: String::from("workspace-indicator"),
        version: String::from("0.1.0"),
        entrypoint: None,
        runtime: ExtensionRuntime::Recipe,
        emits: Vec::new(),
        capabilities: Vec::new(),
        actions: BTreeMap::new(),
        routes: vec![ExtensionRoute {
            event: String::from("aerospace.workspace.changed"),
            action: String::from("aerospace.workspace.snapshot"),
            capabilities: Vec::new(),
            args: serde_json::json!({ "settle_ms": 50 }),
        }],
    };

    manifest.validate()?;
    Ok(())
}

#[test]
fn static_manifest_validation_does_not_execute_extension_host() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let sentinel = dir.join("executed");
    let host = dir.join("host.sh");
    fs::write(
        &host,
        format!(
            "#!/bin/sh\nprintf touched > {}\nexit 65\n",
            sentinel.display()
        ),
    )?;
    let mut permissions = fs::metadata(&host)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&host, permissions)?;

    let manifest_path = dir.join("extension.json");
    fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&serde_json::json!({
            "id": "static-only",
            "version": "0.1.0",
            "entrypoint": host,
            "runtime": "stdio-jsonl"
        }))?,
    )?;

    let manifest = ExtensionManifest::from_path(&manifest_path)?;

    assert_eq!(manifest.id, "static-only");
    assert!(!sentinel.exists());
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn registry_replacement_invalidates_live_stdio_session() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let first_host = write_marker_stdio_host(&dir, "first-host", "v1")?;
    let second_host = write_marker_stdio_host(&dir, "second-host", "v2")?;
    let first_manifest = write_stdio_manifest(&dir, "first.json", "replace-me", &first_host)?;
    let second_manifest = write_stdio_manifest(&dir, "second.json", "replace-me", &second_host)?;
    let registry = ExtensionRegistry::in_dir(&dir);
    let mut runtime = ExtensionRuntimeHost::new();

    let first = registry.register_manifest_with_runtime(&first_manifest, &mut runtime)?;
    first
        .actions
        .get("test.render")
        .ok_or_else(|| SpindleError::ActionNotInstalled {
            action: String::from("test.render"),
        })?;
    let first_output = runtime.invoke_action(
        &first,
        "test.render",
        &ActionInvocation::new("test.render", serde_json::json!({})),
    )?;

    let second = registry.register_manifest_with_runtime(&second_manifest, &mut runtime)?;
    second
        .actions
        .get("test.render")
        .ok_or_else(|| SpindleError::ActionNotInstalled {
            action: String::from("test.render"),
        })?;
    let second_output = runtime.invoke_action(
        &second,
        "test.render",
        &ActionInvocation::new("test.render", serde_json::json!({})),
    )?;

    assert_eq!(
        first_output.emitted_events()[0].data,
        serde_json::json!({ "marker": "v1" })
    );
    assert_eq!(
        second_output.emitted_events()[0].data,
        serde_json::json!({ "marker": "v2" })
    );
    runtime.shutdown()?;
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn runtime_respawns_stdio_session_when_registered_metadata_changes() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let first_host = write_marker_stdio_host(&dir, "first-direct-host", "v1")?;
    let second_host = write_marker_stdio_host(&dir, "second-direct-host", "v2")?;
    let first = registered_stdio_extension(&dir, "replace-me", &first_host);
    let second = registered_stdio_extension(&dir, "replace-me", &second_host);
    let mut runtime = ExtensionRuntimeHost::new();

    let first_output = runtime.invoke_action(
        &first,
        "test.render",
        &ActionInvocation::new("test.render", serde_json::json!({})),
    )?;
    let second_output = runtime.invoke_action(
        &second,
        "test.render",
        &ActionInvocation::new("test.render", serde_json::json!({})),
    )?;

    assert_eq!(
        first_output.emitted_events()[0].data,
        serde_json::json!({ "marker": "v1" })
    );
    assert_eq!(
        second_output.emitted_events()[0].data,
        serde_json::json!({ "marker": "v2" })
    );
    runtime.shutdown()?;
    fs::remove_dir_all(dir)?;
    Ok(())
}

fn write_static_manifest(
    dir: &Path,
    id: &str,
    action_names: &[&str],
    emits: &[&str],
    capabilities: &[&str],
) -> Result<PathBuf, SpindleError> {
    let actions = action_names
        .iter()
        .map(|name| {
            (
                String::from(*name),
                ExtensionAction {
                    capabilities: Vec::new(),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let manifest = ExtensionManifest {
        id: String::from(id),
        version: String::from("0.1.0"),
        entrypoint: None,
        runtime: ExtensionRuntime::Recipe,
        emits: emits.iter().map(|event| String::from(*event)).collect(),
        capabilities: capabilities
            .iter()
            .map(|capability| String::from(*capability))
            .collect(),
        actions,
        routes: Vec::new(),
    };
    let path = dir.join(format!("{id}.json"));
    fs::write(&path, serde_json::to_string_pretty(&manifest)?)?;
    Ok(path)
}

fn registered_stdio_extension(dir: &Path, id: &str, host: &Path) -> RegisteredExtension {
    RegisteredExtension {
        id: String::from(id),
        version: String::from("0.1.0"),
        manifest_path: dir.join("extension.json"),
        runtime: ExtensionRuntime::StdioJsonl,
        entrypoint: Some(host.to_string_lossy().into_owned()),
        capabilities: Vec::new(),
        emits: Vec::new(),
        actions: BTreeMap::new(),
        routes: Vec::new(),
    }
}

fn write_marker_stdio_host(dir: &Path, name: &str, marker: &str) -> Result<PathBuf, SpindleError> {
    let host = dir.join(format!("{name}.sh"));
    fs::write(
        &host,
        format!(
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"type":"register"'*)
      printf '%s\n' '{{"type":"registration","registration":{{"actions":{{"test.render":{{}}}}}}}}'
      ;;
    *'"type":"invoke"'*)
      printf '%s\n' '{{"type":"action-output","output":{{"events":[{{"type":"test.rendered","source":"replace-me","data":{{"marker":"{marker}"}}}}]}}}}'
      ;;
    *'"type":"shutdown"'*)
      printf '%s\n' '{{"type":"shutdown"}}'
      exit 0
      ;;
  esac
done
"#
        ),
    )?;
    let mut permissions = fs::metadata(&host)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&host, permissions)?;
    Ok(host)
}

fn write_stdio_manifest(
    dir: &Path,
    file_name: &str,
    id: &str,
    host: &Path,
) -> Result<PathBuf, SpindleError> {
    let manifest = ExtensionManifest {
        id: String::from(id),
        version: String::from("0.1.0"),
        entrypoint: Some(host.to_string_lossy().into_owned()),
        runtime: ExtensionRuntime::StdioJsonl,
        emits: Vec::new(),
        capabilities: Vec::new(),
        actions: BTreeMap::new(),
        routes: Vec::new(),
    };
    let path = dir.join(file_name);
    fs::write(&path, serde_json::to_string_pretty(&manifest)?)?;
    Ok(path)
}

fn assert_surface_conflict(
    error: SpindleError,
    expected_surface: &'static str,
    expected_name: &str,
    expected_existing_extension: &str,
    expected_new_extension: &str,
) -> Result<(), SpindleError> {
    let SpindleError::SurfaceConflict {
        surface,
        name,
        existing_extension,
        new_extension,
    } = error
    else {
        return Err(error);
    };

    assert_eq!(surface, expected_surface);
    assert_eq!(name, expected_name);
    assert_eq!(existing_extension, expected_existing_extension);
    assert_eq!(new_extension, expected_new_extension);
    Ok(())
}
