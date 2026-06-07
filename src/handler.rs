use serde_json::Value;

use crate::{
    ActionRequest, Dispatcher, Event, EventFilter, EventLog, ExtensionManifest, ExtensionRegistry,
    ExtensionRuntimeHost, HubRequest, SpindleError, policy::CapabilityPolicy,
};

/// Execute one hub request against local kernel state.
///
/// # Errors
///
/// Returns an error if validation, policy checks, I/O, extension dispatch, or
/// JSON serialization fails.
pub fn execute_request(
    request: HubRequest,
    log: &EventLog,
    registry: &ExtensionRegistry,
    runtime: &mut ExtensionRuntimeHost,
) -> Result<Value, SpindleError> {
    let mut state = HandlerState {
        log,
        registry,
        runtime,
    };
    match request {
        HubRequest::Emit {
            kind,
            source,
            subject,
            data,
        } => emit_event(kind, source, subject, data, &mut state),
        HubRequest::QueryEvents {
            kind,
            source,
            limit,
        } => query_events(kind, source, limit, log),
        HubRequest::Invoke {
            action,
            source,
            capabilities,
            args,
        } => invoke_action(&action, source, capabilities, &args, &mut state),
        HubRequest::ValidateExtension { manifest } => {
            let manifest = ExtensionManifest::from_path(&manifest)?;
            Ok(serde_json::to_value(manifest)?)
        }
        HubRequest::RegisterExtension { manifest } => {
            let registered = registry.register_manifest_with_runtime(&manifest, runtime)?;
            Ok(serde_json::to_value(registered)?)
        }
        HubRequest::ListExtensions => Ok(serde_json::to_value(registry.list()?)?),
    }
}

struct HandlerState<'a> {
    log: &'a EventLog,
    registry: &'a ExtensionRegistry,
    runtime: &'a mut ExtensionRuntimeHost,
}

fn emit_event(
    kind: String,
    source: String,
    subject: Option<String>,
    data: Value,
    state: &mut HandlerState<'_>,
) -> Result<Value, SpindleError> {
    let event = Event::builder(kind, source)
        .subject(subject)
        .data(data)
        .build()?;
    state.log.append(&event)?;
    let dispatches =
        Dispatcher::with_log(state.registry, state.log, state.runtime).dispatch_event(&event)?;
    Ok(serde_json::json!({
        "event": event,
        "dispatches": dispatches
    }))
}

fn query_events(
    kind: Option<String>,
    source: Option<String>,
    limit: Option<usize>,
    log: &EventLog,
) -> Result<Value, SpindleError> {
    let events = log.read(&EventFilter {
        kind,
        source,
        limit,
    })?;
    Ok(serde_json::to_value(events)?)
}

fn invoke_action(
    action: &str,
    source: String,
    capabilities: Vec<String>,
    args: &Value,
    state: &mut HandlerState<'_>,
) -> Result<Value, SpindleError> {
    let policy = CapabilityPolicy::load(state_dir_for_log(state.log))?;
    policy.ensure_direct_grants(&source, &capabilities)?;
    let granted_capabilities = capabilities.clone();
    let event = ActionRequest {
        action: String::from(action),
        requested_by: source,
        capabilities,
        args: args.clone(),
    }
    .into_event()?;
    state.log.append(&event)?;
    let dispatches = Dispatcher::with_log(state.registry, state.log, state.runtime)
        .dispatch_action(action, args, &granted_capabilities)?;
    Ok(serde_json::json!({
        "event": event,
        "dispatches": dispatches
    }))
}

fn state_dir_for_log(log: &EventLog) -> &std::path::Path {
    log.state_dir()
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt};

    use serde_json::json;

    use super::*;

    fn write_executable(path: &std::path::Path, contents: &str) -> Result<(), SpindleError> {
        fs::write(path, contents)?;
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)?;
        Ok(())
    }

    fn write_capability_policy(
        dir: &std::path::Path,
        direct: &[(&str, &[&str])],
        routes: &[(&str, &[&str])],
    ) -> Result<(), SpindleError> {
        let direct = direct
            .iter()
            .map(|(grantor, capabilities)| {
                (
                    String::from(*grantor),
                    capabilities
                        .iter()
                        .map(|capability| String::from(*capability))
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let routes = routes
            .iter()
            .map(|(grantor, capabilities)| {
                (
                    String::from(*grantor),
                    capabilities
                        .iter()
                        .map(|capability| String::from(*capability))
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        fs::write(
            dir.join("capabilities.json"),
            serde_json::to_string_pretty(&json!({
                "direct": direct,
                "routes": routes
            }))?,
        )?;
        Ok(())
    }

    #[test]
    fn execute_emit_appends_event() -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        let log = EventLog::in_dir(&dir);
        let registry = ExtensionRegistry::in_dir(&dir);
        let mut runtime = ExtensionRuntimeHost::new();

        let response = execute_request(
            HubRequest::Emit {
                kind: String::from("agent.status.changed"),
                source: String::from("codex-hook"),
                subject: None,
                data: json!({ "state": "testing" }),
            },
            &log,
            &registry,
            &mut runtime,
        )?;

        assert_eq!(response["event"]["type"], "agent.status.changed");
        assert_eq!(response["dispatches"], json!([]));
        assert_eq!(log.read(&EventFilter::default())?.len(), 1);
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn execute_emit_dispatches_installed_route() -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        let host = dir.join("adapter-host.sh");
        write_executable(
            &host,
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"type":"register"'*)
      printf '%s\n' '{"type":"registration","registration":{"capabilities":["test.write"],"actions":{"test.render":{"capabilities":["test.write"]}}}}'
      ;;
    *'"type":"invoke"'*)
      printf '%s\n' '{"type":"action-output","output":{}}'
      ;;
    *'"type":"shutdown"'*)
      printf '%s\n' '{"type":"shutdown"}'
      exit 0
      ;;
  esac
done
"#,
        )?;
        let adapter_manifest = dir.join("adapter.json");
        fs::write(
            &adapter_manifest,
            serde_json::to_string_pretty(&json!({
                "id": "test-adapter",
                "version": "0.1.0",
                "entrypoint": host,
                "runtime": "stdio-jsonl"
            }))?,
        )?;
        let recipe_manifest = dir.join("recipe.json");
        fs::write(
            &recipe_manifest,
            r#"{
              "id": "test-recipe",
              "version": "0.1.0",
              "runtime": "recipe",
              "routes": [
                {
                  "event": "test.changed",
                  "action": "test.render",
                  "capabilities": ["test.write"]
                }
              ]
            }"#,
        )?;

        let log = EventLog::in_dir(&dir);
        let registry = ExtensionRegistry::in_dir(&dir);
        write_capability_policy(&dir, &[], &[("test-recipe", &["test.write"])])?;
        registry.install_manifest(&adapter_manifest)?;
        registry.install_manifest(&recipe_manifest)?;
        let mut runtime = ExtensionRuntimeHost::new();

        let response = execute_request(
            HubRequest::Emit {
                kind: String::from("test.changed"),
                source: String::from("test"),
                subject: None,
                data: json!({ "value": 1 }),
            },
            &log,
            &registry,
            &mut runtime,
        )?;

        assert_eq!(response["dispatches"][0]["action"], "test.render");
        assert_eq!(response["dispatches"][0]["extension"], "test-adapter");
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn execute_invoke_requires_policy_for_granted_capabilities() -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        let host = dir.join("policy-host.sh");
        write_executable(
            &host,
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"type":"register"'*)
      printf '%s\n' '{"type":"registration","registration":{"capabilities":["test.write"],"actions":{"test.render":{"capabilities":["test.write"]}}}}'
      ;;
    *'"type":"invoke"'*)
      printf '%s\n' '{"type":"action-output","output":{}}'
      ;;
    *'"type":"shutdown"'*)
      printf '%s\n' '{"type":"shutdown"}'
      exit 0
      ;;
  esac
done
"#,
        )?;
        let manifest = dir.join("extension.json");
        fs::write(
            &manifest,
            serde_json::to_string_pretty(&json!({
                "id": "policy-host",
                "version": "0.1.0",
                "entrypoint": host,
                "runtime": "stdio-jsonl"
            }))?,
        )?;
        let log = EventLog::in_dir(&dir);
        let registry = ExtensionRegistry::in_dir(&dir);
        registry.install_manifest(&manifest)?;
        let mut runtime = ExtensionRuntimeHost::new();

        let denied = execute_request(
            HubRequest::Invoke {
                action: String::from("test.render"),
                source: String::from("unit"),
                capabilities: vec![String::from("test.write")],
                args: json!({}),
            },
            &log,
            &registry,
            &mut runtime,
        );

        assert!(matches!(
            denied,
            Err(SpindleError::CapabilityGrantDenied { .. })
        ));

        write_capability_policy(&dir, &[("unit", &["test.write"])], &[])?;
        let response = execute_request(
            HubRequest::Invoke {
                action: String::from("test.render"),
                source: String::from("unit"),
                capabilities: vec![String::from("test.write")],
                args: json!({}),
            },
            &log,
            &registry,
            &mut runtime,
        )?;

        assert_eq!(response["dispatches"][0]["action"], "test.render");
        fs::remove_dir_all(dir)?;
        Ok(())
    }
}
