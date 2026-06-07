use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Barrier},
    thread,
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
    let runtime = ExtensionRuntimeHost::new();

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
        produces: registration.produces,
        actions: registration
            .actions
            .into_iter()
            .map(|(name, action)| (name, crate::ExtensionAction::from(action)))
            .collect(),
        routes: Vec::new(),
        runtime_trust: None,
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
    let runtime = ExtensionRuntimeHost::with_timeout(Duration::from_millis(50));

    let result = runtime.load_registration(&manifest, &dir.join("extension.json"));

    assert!(matches!(
        result,
        Err(SpindleError::ExtensionHostTimedOut { .. })
    ));
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn stdio_host_rejects_oversized_response_line() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let host = dir.join("oversized-host.sh");
    crate::store::tests_support::write_executable(
        &host,
        r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"type":"register"'*)
      head -c 1048577 /dev/zero | tr '\000' a
      printf '\n'
      ;;
  esac
done
"#,
    )?;

    let manifest = stdio_manifest("oversized-host", &host);
    let runtime = ExtensionRuntimeHost::new();
    let result = runtime.load_registration(&manifest, &dir.join("extension.json"));

    assert!(matches!(result, Err(SpindleError::MessageTooLarge { .. })));
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn registration_protocol_error_drops_and_terminates_temporary_session() -> Result<(), SpindleError>
{
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let pid_file = dir.join("host.pid");
    let host = dir.join("invalid-registration-host.sh");
    crate::store::tests_support::write_executable(
        &host,
        &format!(
            r#"#!/bin/sh
echo $$ > {pid_file}
while IFS= read -r line; do
  case "$line" in
    *register*)
      printf '%s\n' '{{not-json}}'
      while :; do sleep 1; done
      ;;
  esac
done
"#,
            pid_file = pid_file.display()
        ),
    )?;
    let manifest = stdio_manifest("invalid-registration-host", &host);
    let runtime = ExtensionRuntimeHost::new();

    let result = runtime.load_registration(&manifest, &dir.join("extension.json"));

    assert!(matches!(
        result,
        Err(SpindleError::ExtensionHostProtocolInvalid { .. })
    ));
    wait_for_process_exit(&pid_file, Duration::from_secs(2))?;
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn registration_oversized_response_drops_and_terminates_temporary_session()
-> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let pid_file = dir.join("host.pid");
    let host = dir.join("oversized-registration-host.sh");
    crate::store::tests_support::write_executable(
        &host,
        &format!(
            r#"#!/bin/sh
echo $$ > {pid_file}
while IFS= read -r line; do
  case "$line" in
    *register*)
      head -c 1048577 /dev/zero | tr '\000' a
      while :; do sleep 1; done
      ;;
  esac
done
"#,
            pid_file = pid_file.display()
        ),
    )?;
    let manifest = stdio_manifest("oversized-registration-host", &host);
    let runtime = ExtensionRuntimeHost::new();

    let result = runtime.load_registration(&manifest, &dir.join("extension.json"));

    assert!(matches!(result, Err(SpindleError::MessageTooLarge { .. })));
    wait_for_process_exit(&pid_file, Duration::from_secs(2))?;
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
    let runtime = ExtensionRuntimeHost::with_timeout(Duration::from_secs(10));

    let started = Instant::now();
    let registration = runtime.load_registration(&manifest, &dir.join("extension.json"))?;

    assert!(registration.is_some());
    assert!(started.elapsed() < Duration::from_secs(15));
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn slow_extension_does_not_block_other_extension() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let slow_host = dir.join("slow-host.sh");
    let fast_host = dir.join("fast-host.sh");
    let slow_started = dir.join("slow-started");
    crate::store::tests_support::write_executable(
        &slow_host,
        &format!(
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *shutdown*)
      printf '%s\n' '{{"type":"shutdown"}}'
      exit 0
      ;;
    *)
      touch {slow_started}
      sleep 4
      printf '%s\n' '{{"type":"action-output","output":{{}}}}'
      ;;
  esac
done
"#,
            slow_started = slow_started.display()
        ),
    )?;
    crate::store::tests_support::write_executable(
        &fast_host,
        r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *shutdown*)
      printf '%s\n' '{"type":"shutdown"}'
      exit 0
      ;;
    *)
      printf '%s\n' '{"type":"action-output","output":{}}'
      ;;
  esac
done
"#,
    )?;
    let runtime = Arc::new(ExtensionRuntimeHost::with_timeout(Duration::from_secs(10)));
    let slow = registered_stdio_extension(&dir, "slow", &slow_host);
    let fast = registered_stdio_extension(&dir, "fast", &fast_host);
    let slow_runtime = Arc::clone(&runtime);
    let slow_handle = thread::spawn(move || {
        slow_runtime.invoke_action(
            &slow,
            "test.render",
            &ActionInvocation::new("test.render", json!({})),
        )
    });

    wait_for_file(&slow_started, Duration::from_secs(3))?;
    let started = Instant::now();
    let fast_output = runtime.invoke_action(
        &fast,
        "test.render",
        &ActionInvocation::new("test.render", json!({})),
    )?;

    assert!(fast_output.emitted_events().is_empty());
    assert!(started.elapsed() < Duration::from_secs(3));
    slow_handle
        .join()
        .map_err(|_payload| SpindleError::InvalidField {
            field: "thread",
            reason: "panicked",
        })??;
    runtime.shutdown()?;
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn same_extension_invocations_are_serialized() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let host = dir.join("serialized-host.sh");
    let first_started = dir.join("first-started");
    let second_started = dir.join("second-started");
    crate::store::tests_support::write_executable(
        &host,
        &format!(
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *shutdown*)
      printf '%s\n' '{{"type":"shutdown"}}'
      exit 0
      ;;
    *)
      if [ ! -f {first_started} ]; then
        touch {first_started}
        sleep 0.3
      else
        touch {second_started}
      fi
      printf '%s\n' '{{"type":"action-output","output":{{}}}}'
      ;;
  esac
done
"#,
            first_started = first_started.display(),
            second_started = second_started.display()
        ),
    )?;
    let runtime = Arc::new(ExtensionRuntimeHost::new());
    let first_runtime = Arc::clone(&runtime);
    let second_runtime = Arc::clone(&runtime);
    let first = registered_stdio_extension(&dir, "serialized", &host);
    let second = first.clone();

    let started = Instant::now();
    let first_handle = thread::spawn(move || {
        first_runtime.invoke_action(
            &first,
            "test.render",
            &ActionInvocation::new("test.render", json!({})),
        )
    });
    let second_handle = thread::spawn(move || {
        second_runtime.invoke_action(
            &second,
            "test.render",
            &ActionInvocation::new("test.render", json!({})),
        )
    });

    wait_for_file(&first_started, Duration::from_secs(10))?;
    thread::sleep(Duration::from_millis(100));
    assert!(!second_started.exists());
    first_handle
        .join()
        .map_err(|_payload| SpindleError::InvalidField {
            field: "thread",
            reason: "panicked",
        })??;
    second_handle
        .join()
        .map_err(|_payload| SpindleError::InvalidField {
            field: "thread",
            reason: "panicked",
        })??;
    assert!(second_started.exists());
    assert!(started.elapsed() >= Duration::from_millis(250));
    runtime.shutdown()?;
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn concurrent_first_invocations_spawn_one_stdio_session() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let host = dir.join("single-spawn-host.sh");
    let starts = dir.join("starts.log");
    crate::store::tests_support::write_executable(
        &host,
        &format!(
            r#"#!/bin/sh
echo $$ >> {starts}
while IFS= read -r line; do
  case "$line" in
    *shutdown*)
      printf '%s\n' '{{"type":"shutdown"}}'
      exit 0
      ;;
    *)
      printf '%s\n' '{{"type":"action-output","output":{{}}}}'
      ;;
  esac
done
"#,
            starts = starts.display()
        ),
    )?;
    let runtime = Arc::new(ExtensionRuntimeHost::new());
    let extension = registered_stdio_extension(&dir, "single-spawn", &host);
    let barrier = Arc::new(Barrier::new(3));
    let first_runtime = Arc::clone(&runtime);
    let first_extension = extension.clone();
    let first_barrier = Arc::clone(&barrier);
    let first = thread::spawn(move || {
        first_barrier.wait();
        first_runtime.invoke_action(
            &first_extension,
            "test.render",
            &ActionInvocation::new("test.render", json!({})),
        )
    });
    let second_runtime = Arc::clone(&runtime);
    let second_extension = extension;
    let second_barrier = Arc::clone(&barrier);
    let second = thread::spawn(move || {
        second_barrier.wait();
        second_runtime.invoke_action(
            &second_extension,
            "test.render",
            &ActionInvocation::new("test.render", json!({})),
        )
    });

    barrier.wait();
    first
        .join()
        .map_err(|_payload| SpindleError::InvalidField {
            field: "thread",
            reason: "panicked",
        })??;
    second
        .join()
        .map_err(|_payload| SpindleError::InvalidField {
            field: "thread",
            reason: "panicked",
        })??;

    let starts = fs::read_to_string(starts)?;
    assert_eq!(starts.lines().count(), 1);
    runtime.shutdown()?;
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn broken_stdio_sessions_are_removed_terminated_and_recovered() -> Result<(), SpindleError> {
    for scenario in [
        RecoveryScenario::ProtocolInvalid,
        RecoveryScenario::HostClosed,
        RecoveryScenario::Oversized,
        RecoveryScenario::NonUtf8,
    ] {
        scenario.assert_recovers()?;
    }
    Ok(())
}

#[test]
fn timed_out_session_is_removed_and_terminated() -> Result<(), SpindleError> {
    RecoveryScenario::Timeout.assert_recovers()
}

#[test]
fn shutdown_races_with_first_session_creation() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let started = dir.join("started");
    let host = dir.join("slow-start-host.sh");
    crate::store::tests_support::write_executable(
        &host,
        &format!(
            r#"#!/bin/sh
touch {started}
while IFS= read -r line; do
  case "$line" in
    *shutdown*)
      printf '%s\n' '{{"type":"shutdown"}}'
      exit 0
      ;;
    *)
      sleep 2
      printf '%s\n' '{{"type":"action-output","output":{{}}}}'
      ;;
  esac
done
"#,
            started = started.display()
        ),
    )?;
    let runtime = Arc::new(ExtensionRuntimeHost::new());
    let extension = registered_stdio_extension(&dir, "slow-start", &host);
    let invoke_runtime = Arc::clone(&runtime);
    let invoke_extension = extension.clone();
    let handle = thread::spawn(move || {
        invoke_runtime.invoke_action(
            &invoke_extension,
            "test.render",
            &ActionInvocation::new("test.render", json!({})),
        )
    });

    wait_for_file(&started, Duration::from_secs(3))?;
    runtime.shutdown()?;
    let _first = handle
        .join()
        .map_err(|_payload| SpindleError::InvalidField {
            field: "thread",
            reason: "panicked",
        })?;

    let second = runtime.invoke_action(
        &extension,
        "test.render",
        &ActionInvocation::new("test.render", json!({})),
    )?;
    assert!(second.emitted_events().is_empty());
    runtime.shutdown()?;
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn stale_session_removal_does_not_evict_replacement_session() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let marker = dir.join("seen");
    let pid_file = dir.join("host.pid");
    let host = dir.join("replacement-safe-host.sh");
    crate::store::tests_support::write_executable(
        &host,
        &format!(
            r#"#!/bin/sh
echo $$ > {pid_file}
if [ -f {marker} ]; then
  first=0
else
  touch {marker}
  first=1
fi
while IFS= read -r line; do
  case "$line" in
    *shutdown*)
      printf '%s\n' '{{"type":"shutdown"}}'
      exit 0
      ;;
    *)
      if [ "$first" -eq 1 ]; then
        first=0
        printf '%s\n' '{{not-json}}'
        exit 0
      fi
      printf '%s\n' '{{"type":"action-output","output":{{"events":[{{"type":"test.rendered","source":"replacement-safe","data":{{"recovered":true}}}}]}}}}'
      ;;
  esac
done
"#,
            marker = marker.display(),
            pid_file = pid_file.display()
        ),
    )?;
    let runtime = ExtensionRuntimeHost::new();
    let extension = registered_stdio_extension(&dir, "replacement-safe", &host);

    let first = runtime.invoke_action(
        &extension,
        "test.render",
        &ActionInvocation::new("test.render", json!({})),
    );
    assert!(matches!(
        first,
        Err(SpindleError::ExtensionHostProtocolInvalid { .. })
    ));
    wait_for_process_exit(&pid_file, Duration::from_secs(2))?;

    let second = runtime.invoke_action(
        &extension,
        "test.render",
        &ActionInvocation::new("test.render", json!({})),
    )?;
    let replacement_pid = fs::read_to_string(&pid_file)?;
    assert_eq!(
        second.emitted_events()[0].data,
        json!({ "recovered": true })
    );

    let third = runtime.invoke_action(
        &extension,
        "test.render",
        &ActionInvocation::new("test.render", json!({})),
    )?;
    assert_eq!(fs::read_to_string(&pid_file)?, replacement_pid);
    assert_eq!(third.emitted_events()[0].data, json!({ "recovered": true }));
    runtime.shutdown()?;
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn unexpected_registration_response_evicts_stdio_session() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let marker = dir.join("seen");
    let host = dir.join("unexpected-response-host.sh");
    crate::store::tests_support::write_executable(
        &host,
        &format!(
            r#"#!/bin/sh
if [ -f {marker} ]; then
  first=0
else
  touch {marker}
  first=1
fi
while IFS= read -r line; do
  case "$line" in
    *shutdown*)
      printf '%s\n' '{{"type":"shutdown"}}'
      exit 0
      ;;
    *)
      if [ "$first" -eq 1 ]; then
        first=0
        printf '%s\n' '{{"type":"registration","registration":{{"actions":{{}}}}}}'
      else
        printf '%s\n' '{{"type":"action-output","output":{{"events":[{{"type":"test.rendered","source":"unexpected-response","data":{{"recovered":true}}}}]}}}}'
      fi
      ;;
  esac
done
"#,
            marker = marker.display()
        ),
    )?;
    let runtime = ExtensionRuntimeHost::new();
    let extension = registered_stdio_extension(&dir, "unexpected-response", &host);

    let first = runtime.invoke_action(
        &extension,
        "test.render",
        &ActionInvocation::new("test.render", json!({})),
    );
    assert!(matches!(
        first,
        Err(SpindleError::ExtensionHostError { .. })
    ));

    let second = runtime.invoke_action(
        &extension,
        "test.render",
        &ActionInvocation::new("test.render", json!({})),
    )?;
    assert_eq!(
        second.emitted_events()[0].data,
        json!({ "recovered": true })
    );
    runtime.shutdown()?;
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn action_error_response_does_not_evict_stdio_session() -> Result<(), SpindleError> {
    let dir = crate::store::tests_support::test_dir()?;
    fs::create_dir_all(&dir)?;
    let host = dir.join("action-error-host.sh");
    crate::store::tests_support::write_executable(
        &host,
        r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *shutdown*)
      printf '%s\n' '{"type":"shutdown"}'
      exit 0
      ;;
    *)
      printf '%s\n' '{"type":"error","error":"action failed"}'
      ;;
  esac
done
"#,
    )?;
    let runtime = ExtensionRuntimeHost::new();
    let extension = registered_stdio_extension(&dir, "action-error", &host);

    let first = runtime.invoke_action(
        &extension,
        "test.render",
        &ActionInvocation::new("test.render", json!({})),
    );
    assert!(matches!(
        first,
        Err(SpindleError::ExtensionActionFailed { .. })
    ));

    let second = runtime.invoke_action(
        &extension,
        "test.render",
        &ActionInvocation::new("test.render", json!({})),
    );
    assert!(matches!(
        second,
        Err(SpindleError::ExtensionActionFailed { .. })
    ));
    runtime.shutdown()?;
    fs::remove_dir_all(dir)?;
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum RecoveryScenario {
    ProtocolInvalid,
    Timeout,
    HostClosed,
    Oversized,
    NonUtf8,
}

impl RecoveryScenario {
    fn id(self) -> &'static str {
        match self {
            Self::ProtocolInvalid => "invalid",
            Self::Timeout => "timeout",
            Self::HostClosed => "closed",
            Self::Oversized => "oversized",
            Self::NonUtf8 => "non-utf8",
        }
    }

    fn first_failure_script(self) -> &'static str {
        match self {
            Self::ProtocolInvalid => "printf '%s\\n' '{{not-json}}'; while :; do sleep 1; done",
            Self::Timeout => "sleep 5",
            Self::HostClosed => "exit 0",
            Self::Oversized => {
                "head -c 1048577 /dev/zero | tr '\\000' a; while :; do sleep 1; done"
            }
            Self::NonUtf8 => "printf '\\377\\n'; while :; do sleep 1; done",
        }
    }

    fn expected_error(self, result: &Result<ActionOutput, SpindleError>) -> bool {
        match self {
            Self::ProtocolInvalid => {
                matches!(
                    result,
                    Err(SpindleError::ExtensionHostProtocolInvalid { .. })
                )
            }
            Self::Timeout => matches!(result, Err(SpindleError::ExtensionHostTimedOut { .. })),
            Self::HostClosed => matches!(result, Err(SpindleError::ExtensionHostClosed { .. })),
            Self::Oversized => matches!(result, Err(SpindleError::MessageTooLarge { .. })),
            Self::NonUtf8 => matches!(
                result,
                Err(SpindleError::Io(error)) if error.kind() == std::io::ErrorKind::InvalidData
            ),
        }
    }

    fn runtime(self) -> ExtensionRuntimeHost {
        match self {
            Self::Timeout => ExtensionRuntimeHost::with_timeout(Duration::from_secs(3)),
            Self::ProtocolInvalid | Self::HostClosed | Self::Oversized | Self::NonUtf8 => {
                ExtensionRuntimeHost::new()
            }
        }
    }

    fn assert_recovers(self) -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        let host = self.write_host(&dir)?;
        let runtime = self.runtime();
        let extension = registered_stdio_extension(&dir, self.id(), &host);

        let first = runtime.invoke_action(
            &extension,
            "test.render",
            &ActionInvocation::new("test.render", json!({})),
        );
        assert!(self.expected_error(&first), "unexpected result: {first:?}");
        wait_for_process_exit(&dir.join("host.pid"), Duration::from_secs(2))?;
        let second = runtime.invoke_action(
            &extension,
            "test.render",
            &ActionInvocation::new("test.render", json!({})),
        )?;

        assert!(second.emitted_events().is_empty());
        runtime.shutdown()?;
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    fn write_host(self, dir: &Path) -> Result<PathBuf, SpindleError> {
        let marker = dir.join("seen");
        let pid_file = dir.join("host.pid");
        let host = dir.join(format!("{}-then-valid-host.sh", self.id()));
        crate::store::tests_support::write_executable(
            &host,
            &format!(
                r#"#!/bin/sh
echo $$ > {pid_file}
if [ -f {marker} ]; then
  first=0
else
  touch {marker}
  first=1
fi
while IFS= read -r line; do
  case "$line" in
    *shutdown*)
      printf '%s\n' '{{"type":"shutdown"}}'
      exit 0
      ;;
    *)
      if [ "$first" -eq 1 ]; then
        first=0
        {first_failure}
      fi
      printf '%s\n' '{{"type":"action-output","output":{{}}}}'
      ;;
  esac
done
"#,
                marker = marker.display(),
                pid_file = pid_file.display(),
                first_failure = self.first_failure_script()
            ),
        )?;
        Ok(host)
    }
}

fn wait_for_file(path: &Path, timeout: Duration) -> Result<(), SpindleError> {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if path.exists() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(10));
    }
    Err(SpindleError::InvalidField {
        field: "file",
        reason: "timed out waiting for file",
    })
}

fn wait_for_process_exit(pid_file: &Path, timeout: Duration) -> Result<(), SpindleError> {
    wait_for_file(pid_file, timeout)?;
    let pid = fs::read_to_string(pid_file)?
        .trim()
        .parse::<u32>()
        .map_err(|_error| SpindleError::InvalidField {
            field: "pid",
            reason: "must be a process id",
        })?;
    let started = Instant::now();
    while started.elapsed() < timeout {
        if !process_is_alive(pid)? {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(10));
    }
    Err(SpindleError::InvalidField {
        field: "pid",
        reason: "process did not exit",
    })
}

fn process_is_alive(pid: u32) -> Result<bool, SpindleError> {
    Ok(Command::new("/bin/kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?
        .success())
}

fn stdio_manifest(id: &str, host: &Path) -> ExtensionManifest {
    ExtensionManifest {
        id: String::from(id),
        version: String::from("0.1.0"),
        entrypoint: Some(host.to_string_lossy().into_owned()),
        runtime: ExtensionRuntime::StdioJsonl,
        emits: Vec::new(),
        produces: Vec::new(),
        capabilities: Vec::new(),
        actions: BTreeMap::new(),
        routes: Vec::new(),
    }
}

fn registered_stdio_extension(dir: &Path, id: &str, host: &Path) -> RegisteredExtension {
    let action = crate::ExtensionAction {
        capabilities: Vec::new(),
    };
    RegisteredExtension {
        id: String::from(id),
        version: String::from("0.1.0"),
        manifest_path: dir.join(format!("{id}.json")),
        runtime: ExtensionRuntime::StdioJsonl,
        entrypoint: Some(host.to_string_lossy().into_owned()),
        capabilities: Vec::new(),
        emits: Vec::new(),
        produces: Vec::new(),
        actions: BTreeMap::from([(String::from("test.render"), action)]),
        routes: Vec::new(),
        runtime_trust: None,
    }
}
