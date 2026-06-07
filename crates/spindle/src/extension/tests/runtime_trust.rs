use std::os::unix::fs::symlink;

use super::*;

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
              "source": "provider",
              "action": "provider.snapshot",
              "capabilities": ["provider.read"]
            }
          ]
        }"#,
    )?;
    crate::store::tests_support::write_capability_policy(
        &dir,
        r#"{"emits":{},"direct":{},"routes":{"workflow":[{"source":"provider","event":"provider.changed","capabilities":["provider.read"]}]}}"#,
    )?;

    let registry = ExtensionRegistry::in_dir(&dir);
    let registered = registry.register_manifest(&manifest_path)?;
    let policy = crate::CapabilityPolicy::load(&dir)?;

    policy.ensure_route_grants(&registered)?;
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn static_registration_rejects_ungranted_route_capabilities() -> Result<(), SpindleError> {
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
              "source": "provider",
              "action": "provider.snapshot",
              "capabilities": ["provider.read"]
            }
          ]
        }"#,
    )?;

    let result = ExtensionRegistry::in_dir(&dir).register_manifest(&manifest_path);

    assert!(matches!(
        result,
        Err(SpindleError::CapabilityGrantDenied { .. })
    ));
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn trusted_runtime_registration_rejects_ungranted_route_capabilities() -> Result<(), SpindleError> {
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
    let manifest = write_stdio_manifest(&dir, "route-host.json", "route-host", &host)?;
    let runtime = ExtensionRuntimeHost::new();

    let result =
        ExtensionRegistry::in_dir(&dir).register_manifest_trusting_runtime(&manifest, &runtime);

    assert!(matches!(
        result,
        Err(SpindleError::CapabilityGrantDenied { .. })
    ));
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
    let runtime = ExtensionRuntimeHost::new();

    let first = registry.register_manifest_trusting_runtime(&first_manifest, &runtime)?;
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

    let second = registry.register_manifest_trusting_runtime(&second_manifest, &runtime)?;
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
    let manifest = write_stdio_manifest(&dir, "trusted.json", "trusted", &host)?;
    let runtime = ExtensionRuntimeHost::new();

    let registered =
        ExtensionRegistry::in_dir(&dir).register_manifest_trusting_runtime(&manifest, &runtime)?;
    let trust = registered.runtime_trust.ok_or(SpindleError::InvalidField {
        field: "runtime_trust",
        reason: "missing",
    })?;

    assert_eq!(trust.entrypoint_path, fs::canonicalize(&host)?);
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
    let host = dir.join("mutating-host.sh");
    crate::store::tests_support::write_executable(
        &host,
        r#"#!/bin/sh
replacement="$0.replacement"
cat > "$replacement" <<'SCRIPT'
#!/bin/sh
exit 42
SCRIPT
chmod 755 "$replacement"
mv "$replacement" "$0"
while IFS= read -r line; do
  case "$line" in
    *'"type":"register"'*)
      printf '%s\n' '{"type":"registration","registration":{"actions":{"test.render":{}}}}'
      ;;
    *'"type":"shutdown"'*)
      printf '%s\n' '{"type":"shutdown"}'
      exit 0
      ;;
  esac
done
"#,
    )?;
    let manifest = write_stdio_manifest(&dir, "mutating.json", "mutating", &host)?;
    let runtime = ExtensionRuntimeHost::new();

    let result =
        ExtensionRegistry::in_dir(&dir).register_manifest_trusting_runtime(&manifest, &runtime);

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
fn static_registration_has_no_runtime_trust_metadata() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let manifest = write_static_manifest(&dir, "static", &["static.action"], &[], &[])?;

    let registered = ExtensionRegistry::in_dir(&dir).register_manifest(&manifest)?;

    assert!(registered.runtime_trust.is_none());
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn changed_trusted_entrypoint_is_rejected_before_spawn() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let host = write_marker_stdio_host(&dir, "trusted-host", "v1")?;
    let manifest = write_stdio_manifest(&dir, "trusted.json", "trusted", &host)?;
    let runtime = ExtensionRuntimeHost::new();
    let registered =
        ExtensionRegistry::in_dir(&dir).register_manifest_trusting_runtime(&manifest, &runtime)?;
    crate::store::tests_support::write_executable(
        &host,
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
    let trusted_host = write_marker_stdio_host(&dir, "trusted-real", "trusted")?;
    let malicious_host = write_marker_stdio_host(&dir, "malicious", "malicious")?;
    let entry_symlink = dir.join("entry.sh");
    symlink(&trusted_host, &entry_symlink)?;
    let manifest = write_stdio_manifest(&dir, "trusted.json", "trusted", &entry_symlink)?;
    let runtime = ExtensionRuntimeHost::new();
    let registered =
        ExtensionRegistry::in_dir(&dir).register_manifest_trusting_runtime(&manifest, &runtime)?;
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
fn trusted_registration_through_symlinked_manifest_uses_canonical_entrypoint()
-> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    let real_dir = dir.join("real");
    let linked_dir = dir.join("linked");
    fs::create_dir_all(&real_dir)?;
    symlink(&real_dir, &linked_dir)?;
    let host = write_marker_stdio_host(&real_dir, "trusted-host", "canonical")?;
    write_stdio_manifest(&real_dir, "extension.json", "trusted", &host)?;
    let linked_manifest = linked_dir.join("extension.json");
    let runtime = ExtensionRuntimeHost::new();

    let registered = ExtensionRegistry::in_dir(&dir)
        .register_manifest_trusting_runtime(&linked_manifest, &runtime)?;
    let trust = registered
        .runtime_trust
        .as_ref()
        .ok_or(SpindleError::InvalidField {
            field: "runtime_trust",
            reason: "missing",
        })?;

    assert_eq!(trust.entrypoint_path, fs::canonicalize(&host)?);
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
    let first_host = write_marker_stdio_host(&dir, "first-direct-host", "v1")?;
    let second_host = write_marker_stdio_host(&dir, "second-direct-host", "v2")?;
    let first = registered_stdio_extension(&dir, "replace-me", &first_host);
    let second = registered_stdio_extension(&dir, "replace-me", &second_host);
    let runtime = ExtensionRuntimeHost::new();

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
