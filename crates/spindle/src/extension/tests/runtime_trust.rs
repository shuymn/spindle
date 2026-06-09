use std::os::unix::fs::symlink;

use spindle_extension_sdk::{ExtensionRegistration, RegistrationAction};
use spindle_test_host::{StartupAction, TestHostConfig};

use super::*;

#[test]
fn registry_accepts_installed_route_grants_without_policy() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let workflow = write_static_manifest_with_surface(
        &dir,
        "workflow",
        StaticManifestSurface {
            action_names: &[],
            emits: &[],
            produces: &[],
            capabilities: &[],
        },
    )?;
    fs::write(
        workflow.join("extension.json"),
        r#"{
          "id": "workflow",
          "version": "0.1.0",
          "runtime": "recipe",
          "routes": [
            {
              "event": "provider.changed",
              "source": "provider",
              "action": "provider.snapshot",
              "capabilities": ["provider.read"]
            }
          ]
        }"#,
    )?;
    let provider = write_static_manifest_with_surface(
        &dir,
        "provider",
        StaticManifestSurface {
            action_names: &["provider.snapshot"],
            emits: &["provider.changed"],
            produces: &[],
            capabilities: &[],
        },
    )?;
    fs::write(
        provider.join("extension.json"),
        r#"{
          "id": "provider",
          "version": "0.1.0",
          "runtime": "stdio-jsonl",
          "emits": ["provider.changed"],
          "actions": {
            "provider.snapshot": {
              "capabilities": []
            }
          }
        }"#,
    )?;
    let registry = ExtensionRegistry::in_dir(&dir);
    registry.install_manifest(&provider)?;
    let registered = registry.install_manifest(&workflow)?;

    assert_eq!(
        registered.routes[0].capabilities,
        vec![String::from("provider.read")]
    );
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn static_registration_accepts_route_capabilities_without_policy() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let workflow = write_static_manifest_with_surface(
        &dir,
        "workflow",
        StaticManifestSurface {
            action_names: &[],
            emits: &[],
            produces: &[],
            capabilities: &[],
        },
    )?;
    fs::write(
        workflow.join("extension.json"),
        r#"{
          "id": "workflow",
          "version": "0.1.0",
          "runtime": "recipe",
          "routes": [
            {
              "event": "provider.changed",
              "source": "provider",
              "action": "provider.snapshot",
              "capabilities": ["provider.read"]
            }
          ]
        }"#,
    )?;

    let registered = ExtensionRegistry::in_dir(&dir).install_manifest(&workflow)?;

    assert_eq!(
        registered.routes[0].capabilities,
        vec![String::from("provider.read")]
    );
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn trusted_runtime_registration_accepts_route_capabilities_without_policy()
-> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let host = dir.join("route-host.sh");
    crate::store::tests_support::write_executable(
        &host,
        r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"type":"register"'*)
      printf '%s\n' '{"type":"registration","registration":{"capabilities":["provider.read"],"actions":{"provider.snapshot":{"capabilities":["provider.read"]}},"routes":[{"event":"provider.changed","source":"provider","action":"provider.snapshot","capabilities":["provider.read"]}]}}'
      ;;
    *'"type":"shutdown"'*)
      printf '%s\n' '{"type":"shutdown"}'
      exit 0
      ;;
  esac
done
"#,
    )?;
    let package = write_stdio_package(&dir, "route-host", &host)?;
    let runtime = ExtensionRuntimeHost::new();

    let registered =
        ExtensionRegistry::in_dir(&dir).install_manifest_with_runtime(&package, &runtime)?;

    assert_eq!(
        registered.routes[0].capabilities,
        vec![String::from("provider.read")]
    );
    runtime.shutdown()?;
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn recipe_manifest_can_contribute_routes_without_entrypoint() -> Result<(), SpindleError> {
    let manifest = ExtensionManifest {
        id: String::from("workspace-indicator"),
        version: String::from("0.1.0"),
        runtime: ExtensionRuntime::Recipe,
        emits: Vec::new(),
        produces: Vec::new(),
        capabilities: Vec::new(),
        actions: BTreeMap::new(),
        routes: vec![ExtensionRoute {
            event: String::from("aerospace.workspace.changed"),
            source: None,
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
    let package = dir.join("static-only");
    fs::create_dir_all(package.join("bin"))?;
    let sentinel = package.join("executed");
    let host = package.join("bin/static-only");
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
    fs::write(
        package.join("extension.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "id": "static-only",
            "version": "0.1.0",
            "runtime": "stdio-jsonl"
        }))?,
    )?;

    let manifest = ExtensionManifest::from_path(&package.join("extension.json"))?;

    assert_eq!(manifest.id, "static-only");
    assert!(!sentinel.exists());
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn registry_replacement_invalidates_live_stdio_session() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let first_host = write_marker_shell_host(&dir, "first-host", "v1")?;
    let second_host = write_marker_shell_host(&dir, "second-host", "v2")?;
    let first_package = write_stdio_package(&dir, "replace-me", &first_host)?;
    let registry = ExtensionRegistry::in_dir(&dir);
    let runtime = ExtensionRuntimeHost::new();

    let first = registry.install_manifest_with_runtime(&first_package, &runtime)?;
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

    let second_package = write_stdio_package(&dir, "replace-me", &second_host)?;
    let second = registry.install_manifest_with_runtime(&second_package, &runtime)?;
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
fn trusted_runtime_registration_records_entrypoint_hash() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let host = write_marker_stdio_host(&dir, "trusted-host", "v1")?;
    let package = write_stdio_package(&dir, "trusted", &host)?;
    let runtime = ExtensionRuntimeHost::new();

    let registered =
        ExtensionRegistry::in_dir(&dir).install_manifest_with_runtime(&package, &runtime)?;
    let trust = registered.runtime_trust.ok_or(SpindleError::InvalidField {
        field: "runtime_trust",
        reason: "missing",
    })?;

    assert_eq!(
        trust.entrypoint_path,
        fs::canonicalize(dir.join("extensions").join("trusted").join("bin/trusted"))?
    );
    assert_eq!(trust.entrypoint_sha256.len(), 64);
    assert!(trust.registered_at_unix_ms > 0);
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn trusted_runtime_registration_rejects_entrypoint_mutation_during_registration()
-> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let registration =
        ExtensionRegistration::new().action("test.render", RegistrationAction::new());
    let config = TestHostConfig {
        startup: StartupAction {
            mutate_on_register: true,
            ..StartupAction::default()
        },
        ..TestHostConfig::with_registration(registration)
    };
    let host = crate::store::tests_support::install_test_host(&dir, "mutating-host", &config)?;
    let package = write_stdio_package(&dir, "mutating", &host)?;
    let runtime = ExtensionRuntimeHost::new();

    let result = ExtensionRegistry::in_dir(&dir).install_manifest_with_runtime(&package, &runtime);

    assert!(matches!(
        result,
        Err(SpindleError::ExtensionTrustChanged { .. })
    ));
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn sha256_file_hashes_large_file_without_reading_all_at_once() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let path = dir.join("large.bin");
    let bytes = (0..(2 * 1024 * 1024))
        .map(|index| u8::try_from(index % 251).unwrap_or_default())
        .collect::<Vec<_>>();
    fs::write(&path, &bytes)?;
    let digest = sha2::Sha256::digest(&bytes);
    let mut expected = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut expected, "{byte:02x}").map_err(|_error| SpindleError::InvalidField {
            field: "sha256",
            reason: "failed to encode digest",
        })?;
    }

    assert_eq!(sha256_file(&path)?, expected);
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn static_registration_records_runtime_trust_metadata() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let package = write_static_manifest(&dir, "static", &["static.action"], &[], &[])?;

    let registered = ExtensionRegistry::in_dir(&dir).install_manifest(&package)?;

    assert!(registered.runtime_trust.is_some());
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn changed_trusted_entrypoint_is_rejected_before_spawn() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let host = write_marker_stdio_host(&dir, "trusted-host", "v1")?;
    let package = write_stdio_package(&dir, "trusted", &host)?;
    let runtime = ExtensionRuntimeHost::new();
    let registered =
        ExtensionRegistry::in_dir(&dir).install_manifest_with_runtime(&package, &runtime)?;
    crate::store::tests_support::write_executable(
        &registered
            .runtime_trust
            .as_ref()
            .ok_or(SpindleError::InvalidField {
                field: "runtime_trust",
                reason: "missing",
            })?
            .entrypoint_path,
        r"#!/bin/sh
exit 42
",
    )?;

    let result = runtime.invoke_action(
        &registered,
        "test.render",
        &ActionInvocation::new("test.render", serde_json::json!({})),
    );

    assert!(matches!(
        result,
        Err(SpindleError::ExtensionTrustChanged { .. })
    ));
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn trusted_invoke_executes_canonical_entrypoint_not_retargeted_symlink() -> Result<(), SpindleError>
{
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let trusted_host = write_marker_shell_host(&dir, "trusted-real", "trusted")?;
    let malicious_host = write_marker_shell_host(&dir, "malicious", "malicious")?;
    let package = dir.join("trusted");
    fs::create_dir_all(package.join("bin"))?;
    let entry_symlink = package.join("bin/trusted");
    symlink(&trusted_host, &entry_symlink)?;
    fs::write(
        package.join("extension.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "id": "trusted",
            "version": "0.1.0",
            "runtime": "stdio-jsonl"
        }))?,
    )?;
    let runtime = ExtensionRuntimeHost::new();
    let registered =
        ExtensionRegistry::in_dir(&dir).install_manifest_with_runtime(&package, &runtime)?;
    fs::remove_file(&entry_symlink)?;
    symlink(&malicious_host, &entry_symlink)?;

    let output = runtime.invoke_action(
        &registered,
        "test.render",
        &ActionInvocation::new("test.render", serde_json::json!({})),
    )?;

    assert_eq!(
        output.emitted_events()[0].data,
        serde_json::json!({ "marker": "trusted" })
    );
    runtime.shutdown()?;
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn trusted_registration_through_symlinked_package_uses_canonical_entrypoint()
-> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    let real_dir = dir.join("real");
    let linked_dir = dir.join("linked");
    fs::create_dir_all(&real_dir)?;
    symlink(&real_dir, &linked_dir)?;
    let host = write_marker_stdio_host(&real_dir, "trusted-host", "canonical")?;
    write_stdio_package(&real_dir, "trusted", &host)?;
    let runtime = ExtensionRuntimeHost::new();

    let registered = ExtensionRegistry::in_dir(&dir)
        .install_manifest_with_runtime(&linked_dir.join("trusted"), &runtime)?;
    let trust = registered
        .runtime_trust
        .as_ref()
        .ok_or(SpindleError::InvalidField {
            field: "runtime_trust",
            reason: "missing",
        })?;

    assert_eq!(
        trust.entrypoint_path,
        fs::canonicalize(dir.join("extensions/trusted/bin/trusted"))?
    );
    let output = runtime.invoke_action(
        &registered,
        "test.render",
        &ActionInvocation::new("test.render", serde_json::json!({})),
    )?;

    assert_eq!(
        output.emitted_events()[0].data,
        serde_json::json!({ "marker": "canonical" })
    );
    runtime.shutdown()?;
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn runtime_respawns_stdio_session_when_registered_metadata_changes() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let first_host = write_marker_shell_host(&dir, "first-direct-host", "v1")?;
    let second_host = write_marker_shell_host(&dir, "second-direct-host", "v2")?;
    let first = registered_stdio_extension(&dir, "replace-me", &first_host)?;
    let runtime = ExtensionRuntimeHost::new();

    let first_output = runtime.invoke_action(
        &first,
        "test.render",
        &ActionInvocation::new("test.render", serde_json::json!({})),
    )?;
    let second = registered_stdio_extension(&dir, "replace-me", &second_host)?;
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
fn bootstrap_install_order_does_not_require_provider_first() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let clock = write_clock_bootstrap_package(&dir)?;
    let sketchybar = write_sketchybar_bootstrap_package(&dir)?;
    let registry = ExtensionRegistry::in_dir(&dir);

    registry.install_manifest(&clock)?;
    registry.install_manifest(&sketchybar)?;

    assert_eq!(registry.list()?.len(), 2);
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn incomplete_bootstrap_fails_installed_registry_validation() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let clock = write_clock_bootstrap_package(&dir)?;
    let registry = ExtensionRegistry::in_dir(&dir);
    registry.install_manifest(&clock)?;

    let result = validate_installed_registry(&registry);

    let Err(SpindleError::RegistryValidationFailed { messages }) = result else {
        return Err(SpindleError::InvalidField {
            field: "validation",
            reason: "expected registry validation to fail",
        });
    };
    assert_eq!(messages.len(), 1);
    assert!(messages[0].starts_with("clock:"));
    assert!(messages[0].contains("sketchybar.message.send"));
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn incomplete_bootstrap_blocks_daemon_startup() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let clock = write_clock_bootstrap_package(&dir)?;
    let registry = ExtensionRegistry::in_dir(&dir);
    registry.install_manifest(&clock)?;

    let socket = dir.join("spindle.sock");
    let log = EventLog::in_dir(&dir);
    let result = serve(&socket, &log);

    assert!(matches!(
        result,
        Err(SpindleError::RegistryValidationFailed { .. })
    ));
    assert!(!socket.exists());
    fs::remove_dir_all(dir)?;
    Ok(())
}
