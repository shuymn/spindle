use serde_json::Value;

use crate::{
    ActionRequest, ContinuationAudit, ContinuationConfig, Dispatcher, Event, EventFilter, EventLog,
    ExtensionManifest, ExtensionRegistry, ExtensionRuntimeHost, HubRequest, SpindleError,
    dispatch::ensure_produced_event,
};

/// Execute one hub request against local kernel state.
///
/// # Errors
///
/// Returns an error if validation, I/O, extension dispatch, or JSON
/// serialization fails.
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
/// Returns an error if validation, I/O, extension dispatch, or JSON
/// serialization fails.
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
            args,
        } => invoke_action(&action, source, &args, &state),
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
        HubRequest::InstallExtension {
            package,
            trust_runtime,
        } => {
            let registered = if trust_runtime {
                registry.install_manifest_with_runtime(&package, runtime)?
            } else {
                let registered = registry.install_manifest(&package)?;
                runtime.invalidate_extension(&registered.id);
                registered
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
    args: &Value,
    state: &HandlerState<'_>,
) -> Result<Value, SpindleError> {
    record_and_dispatch_action_request(
        ActionDispatchRequest {
            action,
            requested_by: source,
            capabilities: Vec::new(),
            dispatch_capabilities: &[],
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

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        time::Duration,
    };

    use serde_json::json;
    use spindle_extension_sdk::{ExtensionRegistration, RegistrationAction};
    use spindle_test_host::TestHostConfig;

    use super::*;
    use crate::{ContinuationGrantRequest, ContinuationStore};

    fn prepare_stdio_install_package(
        dir: &Path,
        id: &str,
        host: &Path,
    ) -> Result<PathBuf, SpindleError> {
        let package = dir.join(id);
        fs::create_dir_all(package.join("bin"))?;
        let staged = package.join("bin").join(id);
        fs::copy(host, &staged)?;
        let host_config = host.with_extension("json");
        if host_config.is_file() {
            fs::copy(&host_config, staged.with_extension("json"))?;
        }
        let mut permissions = fs::metadata(&staged)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&staged, permissions)?;
        fs::write(
            package.join("extension.json"),
            serde_json::to_string_pretty(&json!({
                "id": id,
                "version": "0.1.0",
                "runtime": "stdio-jsonl"
            }))?,
        )?;
        Ok(package)
    }

    fn prepare_recipe_package(
        dir: &Path,
        id: &str,
        manifest: &str,
    ) -> Result<PathBuf, SpindleError> {
        let package = dir.join(id);
        fs::create_dir_all(&package)?;
        fs::write(package.join("extension.json"), manifest)?;
        Ok(package)
    }

    #[test]
    fn execute_emit_appends_event_without_policy() -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        let log = EventLog::in_dir(&dir);
        let registry = ExtensionRegistry::in_dir(&dir);
        let runtime = ExtensionRuntimeHost::new();

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
        let host = crate::store::tests_support::test_host_with_write_render(&dir, "adapter-host")?;
        let adapter_package = prepare_stdio_install_package(&dir, "test-adapter", &host)?;
        let recipe_package = prepare_recipe_package(
            &dir,
            "test-recipe",
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
        registry.install_manifest_with_runtime(&adapter_package, &runtime)?;
        registry.install_manifest(&recipe_package)?;

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
    fn direct_invoke_rejects_capability_requiring_action_without_continuation()
    -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        let host =
            crate::store::tests_support::test_host_with_write_render(&dir, "policy-host-bin")?;
        let package = prepare_stdio_install_package(&dir, "policy-host", &host)?;
        let log = EventLog::in_dir(&dir);
        let registry = ExtensionRegistry::in_dir(&dir);
        let runtime = ExtensionRuntimeHost::new();
        registry.install_manifest_with_runtime(&package, &runtime)?;

        let denied = execute_request(
            HubRequest::Invoke {
                action: String::from("test.render"),
                source: String::from("unit"),
                args: json!({}),
            },
            &log,
            &registry,
            &runtime,
        );

        assert!(matches!(
            denied,
            Err(SpindleError::MissingActionCapability { .. })
        ));
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn continuation_invoke_uses_original_capability_grant() -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        let registration = ExtensionRegistration::new()
            .capability("test.read")
            .capability("test.write")
            .action(
                "test.read",
                RegistrationAction::new().capability("test.read"),
            )
            .action(
                "test.write",
                RegistrationAction::new().capability("test.write"),
            );
        let host = crate::store::tests_support::install_test_host(
            &dir,
            "continuation-host",
            &TestHostConfig::with_registration(registration),
        )?;
        let package = prepare_stdio_install_package(&dir, "test-host", &host)?;
        let log = EventLog::in_dir(&dir);
        let registry = ExtensionRegistry::in_dir(&dir);
        let runtime = ExtensionRuntimeHost::new();
        registry.install_manifest_with_runtime(&package, &runtime)?;
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
        let registration = ExtensionRegistration::new()
            .capability("test.write")
            .action(
                "test.write",
                RegistrationAction::new().capability("test.write"),
            );
        let host = crate::store::tests_support::install_test_host(
            &dir,
            "continuation-deny-host",
            &TestHostConfig::with_registration(registration),
        )?;
        let package = prepare_stdio_install_package(&dir, "test-host", &host)?;
        let log = EventLog::in_dir(&dir);
        let registry = ExtensionRegistry::in_dir(&dir);
        let runtime = ExtensionRuntimeHost::new();
        registry.install_manifest_with_runtime(&package, &runtime)?;
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
