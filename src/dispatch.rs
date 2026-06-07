use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use spindle_extension_sdk::{
    ActionDescriptor, ActionInvocation, ActionOutputEvent, EventContext, EventDescriptor,
    ExtensionContext,
};

use crate::{
    CapabilityPolicy, ContinuationConfig, Event, EventLog, ExtensionAction, ExtensionRegistry,
    ExtensionRuntime, ExtensionRuntimeHost, RegisteredExtension, SpindleError,
};

const MAX_DISPATCH_DEPTH: usize = 16;

/// Result of dispatching one action to one installed extension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchReport {
    /// Action that was dispatched.
    pub action: String,
    /// Extension that handled the action.
    pub extension: String,
}

/// Dispatches events and actions through installed extension manifests.
#[derive(Debug)]
pub struct Dispatcher<'a> {
    registry: &'a ExtensionRegistry,
    log: Option<&'a EventLog>,
    runtime: &'a ExtensionRuntimeHost,
    continuation: Option<&'a ContinuationConfig>,
}

#[derive(Debug, Clone, Copy)]
struct ActionDispatchScope<'a> {
    surface: &'a InstalledSurface,
    capabilities: &'a [String],
    event: Option<&'a Event>,
    depth: usize,
}

#[derive(Debug)]
struct InstalledSurface {
    extensions: Vec<RegisteredExtension>,
    policy: CapabilityPolicy,
    visible_context: ExtensionVisibleContext,
    action_owners: BTreeMap<String, usize>,
    event_routes: BTreeMap<String, Vec<RouteHandler>>,
}

#[derive(Debug, Clone, Copy)]
struct RouteHandler {
    extension: usize,
    route: usize,
}

#[derive(Debug, Clone)]
struct ExtensionVisibleContext {
    events: Vec<EventDescriptor>,
    actions: Vec<ActionDescriptor>,
    capabilities: Vec<String>,
}

impl<'a> Dispatcher<'a> {
    /// Create a dispatcher backed by an extension registry.
    #[must_use]
    pub const fn new(registry: &'a ExtensionRegistry, runtime: &'a ExtensionRuntimeHost) -> Self {
        Self {
            registry,
            log: None,
            runtime,
            continuation: None,
        }
    }

    /// Create a dispatcher that can append action-emitted events.
    #[must_use]
    pub const fn with_log(
        registry: &'a ExtensionRegistry,
        log: &'a EventLog,
        runtime: &'a ExtensionRuntimeHost,
    ) -> Self {
        Self {
            registry,
            log: Some(log),
            runtime,
            continuation: None,
        }
    }

    /// Attach continuation configuration used to pass deferred-work handles to extension actions.
    #[must_use]
    pub const fn with_continuation_config(
        mut self,
        continuation: Option<&'a ContinuationConfig>,
    ) -> Self {
        self.continuation = continuation;
        self
    }

    /// Dispatch routes matching an event.
    ///
    /// # Errors
    ///
    /// Returns an error when installed extension state cannot be read or when a
    /// matched action cannot be executed.
    pub fn dispatch_event(&self, event: &Event) -> Result<Vec<DispatchReport>, SpindleError> {
        let surface = InstalledSurface::load(self.registry)?;
        self.dispatch_event_at_depth(&surface, event, 0)
    }

    fn dispatch_event_at_depth(
        &self,
        surface: &InstalledSurface,
        event: &Event,
        depth: usize,
    ) -> Result<Vec<DispatchReport>, SpindleError> {
        if depth > MAX_DISPATCH_DEPTH {
            return Err(SpindleError::DispatchDepthExceeded);
        }

        let mut reports = Vec::new();
        for (extension, route) in surface.routes_for_event(event) {
            surface.ensure_route_capabilities(extension, route)?;
            let args = merge_args(&event.data, &route.args);
            reports.extend(self.dispatch_action_from(
                &route.action,
                &args,
                ActionDispatchScope {
                    surface,
                    capabilities: &route.capabilities,
                    event: Some(event),
                    depth,
                },
            )?);
        }
        Ok(reports)
    }

    /// Dispatch an action directly.
    ///
    /// # Errors
    ///
    /// Returns an error when installed extension state cannot be read, no
    /// installed extension exposes the action, or the action process fails.
    pub fn dispatch_action(
        &self,
        action: &str,
        args: &Value,
        capabilities: &[String],
    ) -> Result<Vec<DispatchReport>, SpindleError> {
        let surface = InstalledSurface::load(self.registry)?;
        self.dispatch_action_from(
            action,
            args,
            ActionDispatchScope {
                surface: &surface,
                capabilities,
                event: None,
                depth: 0,
            },
        )
    }

    fn dispatch_action_from(
        &self,
        action: &str,
        args: &Value,
        scope: ActionDispatchScope<'_>,
    ) -> Result<Vec<DispatchReport>, SpindleError> {
        if scope.depth > MAX_DISPATCH_DEPTH {
            return Err(SpindleError::DispatchDepthExceeded);
        }

        let Some((extension, definition)) = scope.surface.action_handler(action) else {
            return Err(SpindleError::ActionNotInstalled {
                action: String::from(action),
            });
        };
        ensure_capabilities(action, definition, scope.capabilities)?;

        let continuation =
            self.create_continuation_for_action(action, extension, definition, scope)?;
        let invocation = ActionInvocation::new(action, args.clone())
            .with_event(
                scope
                    .event
                    .map(|event| EventContext::new(event.kind.clone(), event.data.clone())),
            )
            .with_extension(Some(scope.surface.context_for(extension)))
            .with_continuation(continuation);
        let output = self.runtime.invoke_action(extension, action, &invocation)?;

        let mut reports = vec![DispatchReport {
            action: String::from(action),
            extension: extension.id.clone(),
        }];

        for output_event in output.emitted_events() {
            ensure_produced_event(extension, action, &output_event.kind)?;
            let event = action_output_event_to_event(output_event, &extension.id)?;
            if let Some(log) = self.log {
                log.append(&event)?;
            }
            reports.extend(self.dispatch_event_at_depth(scope.surface, &event, scope.depth + 1)?);
        }

        Ok(reports)
    }

    fn create_continuation_for_action(
        &self,
        action: &str,
        extension: &RegisteredExtension,
        definition: &ExtensionAction,
        scope: ActionDispatchScope<'_>,
    ) -> Result<Option<spindle_extension_sdk::ContinuationContext>, SpindleError> {
        let Some(config) = self.continuation else {
            return Ok(None);
        };
        if scope.depth != 0 {
            return Ok(None);
        }

        let granted_capabilities = scope
            .capabilities
            .iter()
            .filter(|capability| definition.capabilities.contains(capability))
            .cloned()
            .collect::<Vec<_>>();
        config
            .store
            .create(&extension.id, action, &granted_capabilities, &config.socket)
            .map(Some)
    }
}

impl InstalledSurface {
    fn load(registry: &ExtensionRegistry) -> Result<Self, SpindleError> {
        let extensions = registry.list()?;
        let policy = CapabilityPolicy::load(registry.state_dir())?;
        let visible_context = ExtensionVisibleContext::from_extensions(&extensions);
        let action_owners = action_owners(&extensions);
        let event_routes = event_routes(&extensions);
        Ok(Self {
            extensions,
            policy,
            visible_context,
            action_owners,
            event_routes,
        })
    }

    fn routes_for_event<'a>(
        &'a self,
        event: &'a Event,
    ) -> impl Iterator<Item = (&'a RegisteredExtension, &'a crate::ExtensionRoute)> + 'a {
        self.event_routes
            .get(&event.kind)
            .into_iter()
            .flatten()
            .filter(move |handler| {
                let route = &self.extensions[handler.extension].routes[handler.route];
                route
                    .source
                    .as_ref()
                    .is_none_or(|source| source == &event.source)
            })
            .map(|handler| {
                let extension = &self.extensions[handler.extension];
                (extension, &extension.routes[handler.route])
            })
    }

    fn action_handler<'a>(
        &'a self,
        action: &'a str,
    ) -> Option<(&'a RegisteredExtension, &'a crate::ExtensionAction)> {
        self.action_owners.get(action).map(|extension_index| {
            let extension = &self.extensions[*extension_index];
            (extension, &extension.actions[action])
        })
    }

    fn ensure_route_capabilities(
        &self,
        extension: &RegisteredExtension,
        route: &crate::ExtensionRoute,
    ) -> Result<(), SpindleError> {
        self.policy.ensure_route_capabilities(&extension.id, route)
    }

    fn context_for(&self, extension: &RegisteredExtension) -> ExtensionContext {
        self.visible_context.context_for(&extension.id)
    }
}

fn action_owners(extensions: &[RegisteredExtension]) -> BTreeMap<String, usize> {
    let mut owners = BTreeMap::new();
    for (extension_index, extension) in extensions.iter().enumerate() {
        if extension.runtime == ExtensionRuntime::Recipe {
            continue;
        }
        for action in extension.actions.keys() {
            owners.insert(action.clone(), extension_index);
        }
    }
    owners
}

fn event_routes(extensions: &[RegisteredExtension]) -> BTreeMap<String, Vec<RouteHandler>> {
    let mut routes = BTreeMap::<String, Vec<RouteHandler>>::new();
    for (extension_index, extension) in extensions.iter().enumerate() {
        for (route_index, route) in extension.routes.iter().enumerate() {
            routes
                .entry(route.event.clone())
                .or_default()
                .push(RouteHandler {
                    extension: extension_index,
                    route: route_index,
                });
        }
    }
    routes
}

impl ExtensionVisibleContext {
    fn from_extensions(extensions: &[RegisteredExtension]) -> Self {
        let mut events = BTreeSet::new();
        let mut actions = Vec::new();
        let mut capabilities = BTreeSet::new();

        for extension in extensions {
            for event in &extension.emits {
                events.insert((event.clone(), extension.id.clone()));
            }
            for event in &extension.produces {
                events.insert((event.clone(), extension.id.clone()));
            }
            for capability in &extension.capabilities {
                capabilities.insert(capability.clone());
            }
            if extension.runtime == ExtensionRuntime::Recipe {
                continue;
            }
            for (name, definition) in &extension.actions {
                actions.push(ActionDescriptor {
                    name: name.clone(),
                    extension: extension.id.clone(),
                    capabilities: definition.capabilities.clone(),
                });
            }
        }

        actions.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.extension.cmp(&right.extension))
        });

        Self {
            events: events
                .into_iter()
                .map(|(kind, source_extension)| EventDescriptor {
                    kind,
                    source_extension,
                })
                .collect(),
            actions,
            capabilities: capabilities.into_iter().collect(),
        }
    }

    fn context_for(&self, extension_id: &str) -> ExtensionContext {
        ExtensionContext {
            id: String::from(extension_id),
            events: self.events.clone(),
            actions: self.actions.clone(),
            capabilities: self.capabilities.clone(),
        }
    }
}

fn ensure_capabilities(
    action: &str,
    definition: &crate::ExtensionAction,
    granted: &[String],
) -> Result<(), SpindleError> {
    let granted = granted.iter().collect::<BTreeSet<_>>();
    for capability in &definition.capabilities {
        if !granted.contains(capability) {
            return Err(SpindleError::MissingActionCapability {
                action: String::from(action),
                capability: capability.clone(),
            });
        }
    }
    Ok(())
}

pub fn ensure_produced_event(
    extension: &RegisteredExtension,
    action: &str,
    kind: &str,
) -> Result<(), SpindleError> {
    if extension.produces.iter().any(|produced| produced == kind) {
        return Ok(());
    }

    Err(SpindleError::UndeclaredProducedEvent {
        extension: extension.id.clone(),
        action: String::from(action),
        kind: String::from(kind),
    })
}

fn action_output_event_to_event(
    event: &ActionOutputEvent,
    extension_id: &str,
) -> Result<Event, SpindleError> {
    Event::builder(event.kind.clone(), String::from(extension_id))
        .subject(event.subject.clone())
        .data(event.data.clone())
        .build()
}

fn merge_args(event_data: &Value, route_args: &Value) -> Value {
    let mut merged = match event_data {
        Value::Object(object) => object.clone(),
        _ => serde_json::Map::new(),
    };

    if let Value::Object(object) = route_args {
        for (key, value) in object {
            merged.insert(key.clone(), value.clone());
        }
    }

    Value::Object(merged)
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use serde_json::json;
    use spindle_extension_sdk::ActionOutput;

    use super::*;

    #[test]
    fn route_args_override_event_data() {
        let merged = merge_args(
            &json!({ "item": "event", "workspace": "2" }),
            &json!({ "item": "route" }),
        );

        assert_eq!(merged, json!({ "item": "route", "workspace": "2" }));
    }

    #[test]
    fn dispatch_action_reports_missing_action() -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        let registry = ExtensionRegistry::in_dir(&dir);
        let runtime = ExtensionRuntimeHost::new();
        let dispatcher = Dispatcher::new(&registry, &runtime);

        let result = dispatcher.dispatch_action("missing.action", &json!({}), &[]);

        assert!(matches!(
            result,
            Err(SpindleError::ActionNotInstalled { .. })
        ));
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn empty_action_output_does_not_emit_events() {
        assert!(ActionOutput::empty().emitted_events().is_empty());
    }

    #[test]
    fn action_output_becomes_events() -> Result<(), SpindleError> {
        let output = ActionOutput::event(
            ActionOutputEvent::new("aerospace.workspace.snapshot")
                .with_data(json!({ "active": "2", "occupied": ["1", "2"] })),
        );
        let events = output
            .emitted_events()
            .iter()
            .map(|event| action_output_event_to_event(event, "aerospace"))
            .collect::<Result<Vec<_>, _>>()?;

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "aerospace.workspace.snapshot");
        assert_eq!(
            events[0].data,
            json!({ "active": "2", "occupied": ["1", "2"] })
        );
        Ok(())
    }

    #[test]
    fn recursive_dispatch_reuses_top_level_policy_snapshot() -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        let policy_path = dir.join("capabilities.json");
        crate::store::tests_support::write_capability_policy(
            &dir,
            r#"{"emits":{"unit":["test.initial"]},"direct":{},"routes":{"test-recipe":[{"source":"unit","event":"test.initial","capabilities":["test.write"]},{"source":"test-host","event":"test.followup","capabilities":["test.write"]}]}}"#,
        )?;
        let host = dir.join("snapshot-host.sh");
        crate::store::tests_support::write_executable(
            &host,
            &format!(
                r#"#!/bin/sh
policy_path='{}'
while IFS= read -r line; do
  case "$line" in
    *'"type":"register"'*)
      printf '%s\n' '{{"type":"registration","registration":{{"produces":["test.followup"],"capabilities":["test.write"],"actions":{{"test.start":{{"capabilities":["test.write"]}},"test.finish":{{"capabilities":["test.write"]}}}}}}}}'
      ;;
    *'"action":"test.start"'*)
      rm -f "$policy_path"
      printf '%s\n' '{{"type":"action-output","output":{{"events":[{{"type":"test.followup","source":"test-host","data":{{}}}}]}}}}'
      ;;
    *'"action":"test.finish"'*)
      printf '%s\n' '{{"type":"action-output","output":{{}}}}'
      ;;
    *'"type":"shutdown"'*)
      printf '%s\n' '{{"type":"shutdown"}}'
      exit 0
      ;;
  esac
done
"#,
                policy_path.display()
            ),
        )?;
        let host_manifest = dir.join("host.json");
        fs::write(
            &host_manifest,
            serde_json::to_string_pretty(&json!({
                "id": "test-host",
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
                  "event": "test.initial",
                  "source": "unit",
                  "action": "test.start",
                  "capabilities": ["test.write"]
                },
                {
                  "event": "test.followup",
                  "source": "test-host",
                  "action": "test.finish",
                  "capabilities": ["test.write"]
                }
              ]
            }"#,
        )?;
        let registry = ExtensionRegistry::in_dir(&dir);
        let runtime = ExtensionRuntimeHost::new();
        registry.install_manifest_with_runtime(&host_manifest, &runtime)?;
        registry.install_manifest(&recipe_manifest)?;
        let event = Event::builder(String::from("test.initial"), String::from("unit"))
            .data(json!({}))
            .build()?;
        let dispatcher = Dispatcher::new(&registry, &runtime);

        let reports = dispatcher.dispatch_event(&event)?;

        assert_eq!(
            reports,
            vec![
                DispatchReport {
                    action: String::from("test.start"),
                    extension: String::from("test-host")
                },
                DispatchReport {
                    action: String::from("test.finish"),
                    extension: String::from("test-host")
                }
            ]
        );
        runtime.shutdown()?;
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn extension_context_lists_provided_events_without_route_subscriptions() {
        let provider = RegisteredExtension {
            id: String::from("aerospace"),
            version: String::from("0.1.0"),
            manifest_path: PathBuf::from("/tmp/aerospace/extension.json"),
            runtime: ExtensionRuntime::StdioJsonl,
            entrypoint: Some(String::from("provider")),
            capabilities: vec![String::from("aerospace.state.read")],
            emits: vec![String::from("aerospace.workspace.changed")],
            produces: vec![String::from("aerospace.workspace.snapshot")],
            actions: [(
                String::from("aerospace.workspace.snapshot"),
                crate::ExtensionAction {
                    capabilities: vec![String::from("aerospace.state.read")],
                },
            )]
            .into(),
            routes: Vec::new(),
            runtime_trust: None,
        };
        let workflow = RegisteredExtension {
            id: String::from("workspace-indicator"),
            version: String::from("0.1.0"),
            manifest_path: PathBuf::from("/tmp/workspace-indicator/extension.json"),
            runtime: ExtensionRuntime::StdioJsonl,
            entrypoint: Some(String::from("workflow")),
            capabilities: Vec::new(),
            emits: Vec::new(),
            produces: vec![String::from(
                "workspace-indicator.sketchybar.message.requested",
            )],
            actions: std::collections::BTreeMap::new(),
            routes: vec![crate::ExtensionRoute {
                event: String::from("aerospace.route.only"),
                source: Some(String::from("aerospace")),
                action: String::from("workspace-indicator.workspaces.render"),
                capabilities: Vec::new(),
                args: json!({}),
            }],
            runtime_trust: None,
        };

        let visible_context =
            ExtensionVisibleContext::from_extensions(&[provider, workflow.clone()]);
        let context = visible_context.context_for(&workflow.id);

        assert_eq!(context.id, "workspace-indicator");
        assert!(
            context
                .events
                .iter()
                .any(|event| event.kind == "aerospace.workspace.changed")
        );
        assert!(
            context
                .events
                .iter()
                .any(|event| event.kind == "aerospace.workspace.snapshot")
        );
        assert!(
            !context
                .events
                .iter()
                .any(|event| event.kind == "aerospace.route.only")
        );
        assert!(
            context
                .actions
                .iter()
                .any(|action| action.name == "aerospace.workspace.snapshot")
        );
        assert_eq!(
            context.capabilities,
            vec![String::from("aerospace.state.read")]
        );
    }
}
