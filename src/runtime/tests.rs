use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use serde_json::json;

use super::*;

#[test]
fn relative_manifest_path_resolves_from_manifest_directory() {
    assert_eq!(
        resolve_manifest_path(
            Path::new("/tmp/spindle/extensions/sketchybar/extension.json"),
            "../../target/release/spindle-sketchybar",
        ),
        PathBuf::from("/tmp/spindle/target/release/spindle-sketchybar")
    );
}

#[test]
fn relative_entrypoint_preserves_leading_parent_segments() {
    assert_eq!(
        resolve_manifest_path(Path::new("extension.json"), "../bin/spindle-test"),
        PathBuf::from("../bin/spindle-test")
    );
}

#[test]
fn stdio_host_registers_and_invokes_without_respawning() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let host = dir.join("host.sh");
    crate::store::tests_support::write_executable(
        &host,
        r#"#!/bin/sh
count=0
while IFS= read -r line; do
  case "$line" in
    *'"type":"register"'*)
      printf '%s\n' '{"type":"registration","registration":{"emits":["test.changed"],"capabilities":["test.write"],"actions":{"test.render":{}},"routes":[]}}'
      ;;
    *'"type":"invoke"'*)
      count=$((count + 1))
      printf '%s\n' '{"type":"action-output","output":{"events":[{"type":"test.rendered","source":"test-host","data":{"count":'"$count"'}}]}}'
      ;;
    *'"type":"shutdown"'*)
      printf '%s\n' '{"type":"shutdown"}'
      exit 0
      ;;
  esac
done
"#,
    )?;

    let manifest = stdio_manifest("test-host", &host);
    let mut runtime = ExtensionRuntimeHost::new();

    let registration = runtime
        .load_registration(&manifest, &dir.join("extension.json"))?
        .ok_or(SpindleError::InvalidField {
            field: "registration",
            reason: "missing",
        })?;
    assert_eq!(registration.emits, vec![String::from("test.changed")]);

    let registered = RegisteredExtension {
        id: String::from("test-host"),
        version: String::from("0.1.0"),
        manifest_path: dir.join("extension.json"),
        runtime: ExtensionRuntime::StdioJsonl,
        entrypoint: Some(host.to_string_lossy().into_owned()),
        capabilities: registration.capabilities,
        emits: registration.emits,
        actions: registration
            .actions
            .into_iter()
            .map(|(name, action)| (name, crate::ExtensionAction::from(action)))
            .collect(),
        routes: Vec::new(),
    };
    registered
        .actions
        .get("test.render")
        .ok_or_else(|| SpindleError::ActionNotInstalled {
            action: String::from("test.render"),
        })?;

    let first = runtime.invoke_action(
        &registered,
        "test.render",
        &ActionInvocation::new("test.render", json!({})),
    )?;
    let second = runtime.invoke_action(
        &registered,
        "test.render",
        &ActionInvocation::new("test.render", json!({})),
    )?;

    assert_eq!(first.emitted_events()[0].data, json!({ "count": 1 }));
    assert_eq!(second.emitted_events()[0].data, json!({ "count": 2 }));
    runtime.shutdown()?;
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn stdio_host_registration_times_out() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let host = dir.join("silent-host.sh");
    crate::store::tests_support::write_executable(
        &host,
        r"#!/bin/sh
while IFS= read -r _line; do
  sleep 10
done
",
    )?;

    let manifest = stdio_manifest("silent-host", &host);
    let mut runtime = ExtensionRuntimeHost::with_timeout(Duration::from_millis(50));

    let result = runtime.load_registration(&manifest, &dir.join("extension.json"));

    assert!(matches!(
        result,
        Err(SpindleError::ExtensionHostTimedOut { .. })
    ));
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn stdio_host_shutdown_kills_host_that_acknowledges_without_exiting() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let host = dir.join("slow-shutdown-host.sh");
    crate::store::tests_support::write_executable(
        &host,
        r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"type":"register"'*)
      /bin/echo '{"type":"registration","registration":{"actions":{}}}'
      ;;
    *'"type":"shutdown"'*)
      /bin/echo '{"type":"shutdown"}'
      sleep 10
      exit 0
      ;;
  esac
done
"#,
    )?;

    let manifest = stdio_manifest("slow-shutdown-host", &host);
    let mut runtime = ExtensionRuntimeHost::with_timeout(Duration::from_secs(2));

    let started = Instant::now();
    let registration = runtime.load_registration(&manifest, &dir.join("extension.json"))?;

    assert!(registration.is_some());
    assert!(started.elapsed() < Duration::from_secs(5));
    fs::remove_dir_all(dir)?;
    Ok(())
}

fn stdio_manifest(id: &str, host: &Path) -> ExtensionManifest {
    ExtensionManifest {
        id: String::from(id),
        version: String::from("0.1.0"),
        entrypoint: Some(host.to_string_lossy().into_owned()),
        runtime: ExtensionRuntime::StdioJsonl,
        emits: Vec::new(),
        capabilities: Vec::new(),
        actions: BTreeMap::new(),
        routes: Vec::new(),
    }
}
