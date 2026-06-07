use serde_json::Value;

use crate::{
    ActionRequest, ContinuationAudit, ContinuationConfig, Dispatcher, Event, EventFilter, EventLog,
    ExtensionManifest, ExtensionRegistry, ExtensionRuntimeHost, HubRequest, SpindleError,
    dispatch::ensure_produced_event, policy::CapabilityPolicy,
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
    runtime: &ExtensionRuntimeHost,
) -> Result<Value, SpindleError> {
    execute_request_with_continuations(request, log, registry, runtime, None)
}

/// Execute one hub request with continuation support enabled.
///
/// # Errors
///
/// Returns an error if validation, policy checks, I/O, extension dispatch, or
/// JSON serialization fails.
pub fn execute_request_with_continuations(
    request: HubRequest,
    log: &EventLog,
    registry: &ExtensionRegistry,
    runtime: &ExtensionRuntimeHost,
    continuation: Option<&ContinuationConfig>,
) -> Result<Value, SpindleError> {
    let state = HandlerState {
        log,
        registry,
        runtime,
        continuation,
    };
    match request {
        HubRequest::Emit {
            kind,
            source,
            subject,
            data,
        } => emit_event(kind, source, subject, data, &state),
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
        } => invoke_action(&action, source, capabilities, &args, &state),
        HubRequest::ContinuationInvoke {
            continuation,
            action,
            args,
        } => continuation_invoke(&continuation, &action, &args, &state),
        HubRequest::ContinuationEmit {
            continuation,
            kind,
            subject,
            data,
        } => continuation_emit(&continuation, kind, subject, data, &state),
        HubRequest::ValidateExtension { manifest } => {
            let manifest = ExtensionManifest::from_path(&manifest)?;
            Ok(serde_json::to_value(manifest)?)
        }
        HubRequest::RegisterExtension {
            manifest,
            trust_runtime,
        } => {
            let registered = if trust_runtime {
                registry.register_manifest_trusting_runtime(&manifest, runtime)?
            } else {
                registry.register_manifest(&manifest)?
            };
            Ok(serde_json::to_value(registered)?)
        }
        HubRequest::ListExtensions => Ok(serde_json::to_value(registry.list()?)?),
    }
}

struct HandlerState<'a> {
    log: &'a EventLog,
    registry: &'a ExtensionRegistry,
    runtime: &'a ExtensionRuntimeHost,
    continuation: Option<&'a ContinuationConfig>,
}

fn emit_event(
    kind: String,
    source: String,
    subject: Option<String>,
    data: Value,
    state: &HandlerState<'_>,
) -> Result<Value, SpindleError> {
    let policy = CapabilityPolicy::load(state_dir_for_log(state.log))?;
    policy.ensure_emit(&source, &kind)?;
    let event = Event::builder(kind, source)
        .subject(subject)
        .data(data)
        .build()?;
    append_and_dispatch_event(&event, state.continuation, state)
}

fn append_and_dispatch_event(
    event: &Event,
    continuation: Option<&ContinuationConfig>,
    state: &HandlerState<'_>,
) -> Result<Value, SpindleError> {
    state.log.append(event)?;
    let reports = Dispatcher::with_log(state.registry, state.log, state.runtime)
        .with_continuation_config(continuation)
        .dispatch_event(event)?;
    Ok(serde_json::json!({
        "event": event,
        "dispatches": reports
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
    state: &HandlerState<'_>,
) -> Result<Value, SpindleError> {
    let policy = CapabilityPolicy::load(state_dir_for_log(state.log))?;
    policy.ensure_direct_grants(&source, &capabilities)?;
    let granted_capabilities = capabilities.clone();
    record_and_dispatch_action_request(
        ActionDispatchRequest {
            action,
            requested_by: source,
            capabilities,
            dispatch_capabilities: &granted_capabilities,
            args,
            continuation_audit: None,
            continuation: state.continuation,
        },
        state,
    )
}

struct ActionDispatchRequest<'a> {
    action: &'a str,
    requested_by: String,
    capabilities: Vec<String>,
    dispatch_capabilities: &'a [String],
    args: &'a Value,
    continuation_audit: Option<ContinuationAudit>,
    continuation: Option<&'a ContinuationConfig>,
}

fn record_and_dispatch_action_request(
    request: ActionDispatchRequest<'_>,
    state: &HandlerState<'_>,
) -> Result<Value, SpindleError> {
    let event = ActionRequest {
        action: String::from(request.action),
        requested_by: request.requested_by,
        capabilities: request.capabilities,
        args: request.args.clone(),
        continuation: request.continuation_audit,
    }
    .into_event()?;
    state.log.append(&event)?;
    let reports = Dispatcher::with_log(state.registry, state.log, state.runtime)
        .with_continuation_config(request.continuation)
        .dispatch_action(request.action, request.args, request.dispatch_capabilities)?;
    Ok(serde_json::json!({
        "event": event,
        "dispatches": reports
    }))
}

fn continuation_invoke(
    continuation_id: &str,
    action: &str,
    args: &Value,
    state: &HandlerState<'_>,
) -> Result<Value, SpindleError> {
    let Some(continuation) = state.continuation else {
        return Err(SpindleError::ContinuationInvalid);
    };
    let grant = continuation.store.validate(continuation_id)?;
    record_and_dispatch_action_request(
        ActionDispatchRequest {
            action,
            requested_by: format!("{}:continuation", grant.extension),
            capabilities: grant.capabilities.clone(),
            dispatch_capabilities: &grant.capabilities,
            args,
            continuation_audit: Some(ContinuationAudit {
                id: grant.id,
                extension: grant.extension,
                action: grant.action,
            }),
            continuation: Some(continuation),
        },
        state,
    )
}

fn continuation_emit(
    continuation_id: &str,
    kind: String,
    subject: Option<String>,
    data: Value,
    state: &HandlerState<'_>,
) -> Result<Value, SpindleError> {
    let Some(continuation) = state.continuation else {
        return Err(SpindleError::ContinuationInvalid);
    };
    let grant = continuation.store.validate(continuation_id)?;
    let policy = CapabilityPolicy::load(state_dir_for_log(state.log))?;
    policy.ensure_emit(&grant.extension, &kind)?;
    ensure_continuation_emit_allowed(state.registry, &grant.extension, &kind)?;
    let event = Event::builder(kind, grant.extension)
        .subject(subject)
        .data(data)
        .build()?;
    append_and_dispatch_event(&event, Some(continuation), state)
}

fn ensure_continuation_emit_allowed(
    registry: &ExtensionRegistry,
    extension_id: &str,
    kind: &str,
) -> Result<(), SpindleError> {
    let extension = registry
        .list()?
        .into_iter()
        .find(|extension| extension.id == extension_id)
        .ok_or(SpindleError::ContinuationInvalid)?;
    ensure_produced_event(&extension, "continuation.emit", kind)
}

fn state_dir_for_log(log: &EventLog) -> &std::path::Path {
    log.state_dir()
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs, time::Duration};

    use serde_json::json;

    use super::*;
    use crate::{ContinuationGrantRequest, ContinuationStore};

    fn write_capability_policy(
        dir: &std::path::Path,
        emits: &[(&str, &[&str])],
        direct: &[(&str, &[&str])],
        routes: &[(&str, &str, &str, &[&str])],
    ) -> Result<(), SpindleError> {
        fs::create_dir_all(dir)?;
        let emits = emits
            .iter()
            .map(|(grantor, events)| {
                (
                    String::from(*grantor),
                    events
                        .iter()
                        .map(|event| String::from(*event))
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<BTreeMap<_, _>>();
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
        let mut routes_by_grantor = BTreeMap::<String, Vec<serde_json::Value>>::new();
        for (grantor, source, event, capabilities) in routes {
            routes_by_grantor
                .entry(String::from(*grantor))
                .or_default()
                .push(json!({
                    "source": source,
                    "event": event,
                    "capabilities": capabilities
                }));
        }
        fs::write(
            dir.join("capabilities.json"),
            serde_json::to_string_pretty(&json!({
                "emits": emits,
                "direct": direct,
                "routes": routes_by_grantor
            }))?,
        )?;
        Ok(())
    }

    #[test]
    fn execute_emit_appends_event() -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        let log = EventLog::in_dir(&dir);
        let registry = ExtensionRegistry::in_dir(&dir);
        let runtime = ExtensionRuntimeHost::new();
        write_capability_policy(&dir, &[("codex-hook", &["agent.status.changed"])], &[], &[])?;

        let response = execute_request(
            HubRequest::Emit {
                kind: String::from("agent.status.changed"),
                source: String::from("codex-hook"),
                subject: None,
                data: json!({ "state": "testing" }),
            },
            &log,
            &registry,
            &runtime,
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
        crate::store::tests_support::write_executable(
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
                  "source": "test",
                  "action": "test.render",
                  "capabilities": ["test.write"]
                }
              ]
            }"#,
        )?;

        let log = EventLog::in_dir(&dir);
        let registry = ExtensionRegistry::in_dir(&dir);
        let runtime = ExtensionRuntimeHost::new();
        write_capability_policy(
            &dir,
            &[("test", &["test.changed"])],
            &[],
            &[("test-recipe", "test", "test.changed", &["test.write"])],
        )?;
        registry.install_manifest_with_runtime(&adapter_manifest, &runtime)?;
        registry.install_manifest(&recipe_manifest)?;

        let response = execute_request(
            HubRequest::Emit {
                kind: String::from("test.changed"),
                source: String::from("test"),
                subject: None,
                data: json!({ "value": 1 }),
            },
            &log,
            &registry,
            &runtime,
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
        crate::store::tests_support::write_executable(
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
        let runtime = ExtensionRuntimeHost::new();
        registry.install_manifest_with_runtime(&manifest, &runtime)?;

        let denied = execute_request(
            HubRequest::Invoke {
                action: String::from("test.render"),
                source: String::from("unit"),
                capabilities: vec![String::from("test.write")],
                args: json!({}),
            },
            &log,
            &registry,
            &runtime,
        );

        assert!(matches!(
            denied,
            Err(SpindleError::CapabilityGrantDenied { .. })
        ));

        write_capability_policy(&dir, &[], &[("unit", &["test.write"])], &[])?;
        let response = execute_request(
            HubRequest::Invoke {
                action: String::from("test.render"),
                source: String::from("unit"),
                capabilities: vec![String::from("test.write")],
                args: json!({}),
            },
            &log,
            &registry,
            &runtime,
        )?;

        assert_eq!(response["dispatches"][0]["action"], "test.render");
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn continuation_invoke_uses_original_capability_grant() -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        let host = dir.join("continuation-host.sh");
        crate::store::tests_support::write_executable(
            &host,
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"type":"register"'*)
      printf '%s\n' '{"type":"registration","registration":{"capabilities":["test.read","test.write"],"actions":{"test.read":{"capabilities":["test.read"]},"test.write":{"capabilities":["test.write"]}}}}'
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
                "id": "test-host",
                "version": "0.1.0",
                "entrypoint": host,
                "runtime": "stdio-jsonl"
            }))?,
        )?;
        let log = EventLog::in_dir(&dir);
        let registry = ExtensionRegistry::in_dir(&dir);
        let runtime = ExtensionRuntimeHost::new();
        registry.install_manifest_with_runtime(&manifest, &runtime)?;
        let continuation = ContinuationConfig {
            store: ContinuationStore::default(),
            socket: dir.join("spindle.sock"),
        };
        let handle = continuation.store.create(
            "workflow",
            "workflow.schedule",
            &[String::from("test.read")],
            &continuation.socket,
        )?;

        let response = execute_request_with_continuations(
            HubRequest::ContinuationInvoke {
                continuation: handle.id,
                action: String::from("test.read"),
                args: json!({}),
            },
            &log,
            &registry,
            &runtime,
            Some(&continuation),
        )?;

        assert_eq!(response["dispatches"][0]["action"], "test.read");
        assert_eq!(
            response["event"]["data"]["continuation"]["extension"],
            "workflow"
        );
        runtime.shutdown()?;
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn continuation_invoke_rejects_ungranted_capability() -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        let host = dir.join("continuation-deny-host.sh");
        crate::store::tests_support::write_executable(
            &host,
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"type":"register"'*)
      printf '%s\n' '{"type":"registration","registration":{"capabilities":["test.write"],"actions":{"test.write":{"capabilities":["test.write"]}}}}'
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
                "id": "test-host",
                "version": "0.1.0",
                "entrypoint": host,
                "runtime": "stdio-jsonl"
            }))?,
        )?;
        let log = EventLog::in_dir(&dir);
        let registry = ExtensionRegistry::in_dir(&dir);
        let runtime = ExtensionRuntimeHost::new();
        registry.install_manifest_with_runtime(&manifest, &runtime)?;
        let continuation = ContinuationConfig {
            store: ContinuationStore::default(),
            socket: dir.join("spindle.sock"),
        };
        let handle = continuation.store.create(
            "workflow",
            "workflow.schedule",
            &[String::from("test.read")],
            &continuation.socket,
        )?;

        let denied = execute_request_with_continuations(
            HubRequest::ContinuationInvoke {
                continuation: handle.id,
                action: String::from("test.write"),
                args: json!({}),
            },
            &log,
            &registry,
            &runtime,
            Some(&continuation),
        );

        assert!(matches!(
            denied,
            Err(SpindleError::MissingActionCapability { .. })
        ));
        runtime.shutdown()?;
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn expired_continuation_fails_closed() -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        let log = EventLog::in_dir(&dir);
        let registry = ExtensionRegistry::in_dir(&dir);
        let runtime = ExtensionRuntimeHost::new();
        let continuation = ContinuationConfig {
            store: ContinuationStore::default(),
            socket: dir.join("spindle.sock"),
        };
        let capabilities = [String::from("test.read")];
        let handle = continuation
            .store
            .create_with_lifetime(ContinuationGrantRequest {
                extension: "workflow",
                action: "workflow.schedule",
                capabilities: &capabilities,
                socket: &continuation.socket,
                ttl: Duration::from_millis(0),
            })?;

        let denied = execute_request_with_continuations(
            HubRequest::ContinuationInvoke {
                continuation: handle.id,
                action: String::from("test.read"),
                args: json!({}),
            },
            &log,
            &registry,
            &runtime,
            Some(&continuation),
        );

        assert!(matches!(denied, Err(SpindleError::ContinuationExpired)));
        fs::remove_dir_all(dir)?;
        Ok(())
    }
}
