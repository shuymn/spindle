//! Typed contract helpers for spindle extension hosts.

#![warn(clippy::pedantic)]
#![warn(clippy::nursery)]
#![warn(clippy::cargo)]

mod action;
mod context;
mod error;
mod host;
mod registration;

pub use action::{ActionInvocation, ActionOutput, ActionOutputEvent};
pub use context::{
    ActionContext, ActionDescriptor, EventContext, EventDescriptor, ExtensionContext,
};
pub use error::ExtensionSdkError;
pub use host::{
    ActionHandler, HostRequest, HostResponse, serve_stdio_jsonl, serve_stdio_jsonl_actions,
    serve_stdio_jsonl_host,
};
pub use registration::{ExtensionRegistration, RegistrationAction, RegistrationRoute};
use serde_json::{Map, Value};

pub(crate) fn empty_object() -> Value {
    Value::Object(Map::new())
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct Args {
        workspace: String,
    }

    #[test]
    fn typed_args_decode_from_value() -> Result<(), ExtensionSdkError> {
        let context = ActionContext::from_invocation(ActionInvocation::new(
            "aerospace.workspace.focus",
            serde_json::json!({ "workspace": "2" }),
        ));

        assert_eq!(
            context.args::<Args>()?,
            Args {
                workspace: String::from("2")
            }
        );
        Ok(())
    }

    #[test]
    fn extension_context_queries_visible_surface() {
        let context = ExtensionContext {
            id: String::from("workspace-indicator"),
            events: vec![EventDescriptor {
                kind: String::from("sketchybar.message.requested"),
                source_extension: String::from("workspace-indicator"),
            }],
            actions: vec![ActionDescriptor {
                name: String::from("sketchybar.message.send"),
                extension: String::from("sketchybar"),
                capabilities: vec![String::from("sketchybar.ui.write")],
            }],
            capabilities: vec![String::from("sketchybar.ui.write")],
        };

        assert!(context.has_event("sketchybar.message.requested"));
        assert!(context.has_action("sketchybar.message.send"));
        assert!(context.has_capability("sketchybar.ui.write"));
        assert!(!context.has_action("aerospace.workspace.focus"));
    }

    #[test]
    fn extension_registration_serializes_declared_surface() -> Result<(), ExtensionSdkError> {
        let registration = ExtensionRegistration::new()
            .emit("aerospace.workspace.changed")
            .capability("aerospace.state.read")
            .action(
                "aerospace.workspace.snapshot",
                RegistrationAction::new().capability("aerospace.state.read"),
            )
            .route(
                RegistrationRoute::new(
                    "aerospace.workspace.snapshot",
                    "workspace-indicator.workspaces.render",
                )
                .with_args(serde_json::json!({ "workspaces": "1,2" })),
            );

        assert_eq!(
            registration.to_json_string()?,
            r#"{"emits":["aerospace.workspace.changed"],"capabilities":["aerospace.state.read"],"actions":{"aerospace.workspace.snapshot":{"capabilities":["aerospace.state.read"]}},"routes":[{"event":"aerospace.workspace.snapshot","action":"workspace-indicator.workspaces.render","args":{"workspaces":"1,2"}}]}"#
        );
        Ok(())
    }

    #[test]
    fn extension_registration_on_adds_handler_action_and_route() {
        let registration = ExtensionRegistration::new().on(
            "session_start",
            "session-title.on-start",
            RegistrationAction::new(),
        );

        assert!(registration.actions.contains_key("session-title.on-start"));
        assert_eq!(registration.routes[0].event, "session_start");
        assert_eq!(registration.routes[0].action, "session-title.on-start");
    }

    #[test]
    fn extension_registration_on_copies_action_capabilities_to_route() {
        let registration = ExtensionRegistration::new().on(
            "session_start",
            "session-title.on-start",
            RegistrationAction::new().capability("session.write"),
        );

        assert_eq!(
            registration.routes[0].capabilities,
            vec![String::from("session.write")]
        );
    }

    #[test]
    fn extension_registration_on_with_args_adds_handler_route_args() {
        let registration = ExtensionRegistration::new().on_with_args(
            "aerospace.mode.snapshot",
            "workspace-indicator.status.render",
            RegistrationAction::new(),
            serde_json::json!({ "item": "aerospace.mode" }),
        );

        assert!(
            registration
                .actions
                .contains_key("workspace-indicator.status.render")
        );
        assert_eq!(
            registration.routes[0].args,
            serde_json::json!({ "item": "aerospace.mode" })
        );
    }

    #[test]
    fn stdio_jsonl_host_serves_register_invoke_and_shutdown() -> Result<(), ExtensionSdkError> {
        let input = [
            r#"{"type":"register"}"#,
            r#"{"type":"invoke","invocation":{"action":"test.render","args":{"item":"mode"},"event":null,"extension":null}}"#,
            r#"{"type":"shutdown"}"#,
            "",
        ]
        .join("\n");
        let mut output = Vec::new();
        let registration =
            ExtensionRegistration::new().action("test.render", RegistrationAction::new());

        serve_stdio_jsonl(
            std::io::Cursor::new(input),
            &mut output,
            &registration,
            |context| -> Result<ActionOutput, ExtensionSdkError> {
                let args = context.args::<serde_json::Value>()?;
                Ok(ActionOutput::event(
                    ActionOutputEvent::new("test.rendered", "test-host").with_data(args),
                ))
            },
        )?;

        let lines = String::from_utf8_lossy(&output);
        let responses = lines.lines().collect::<Vec<_>>();
        assert!(responses[0].contains(r#""type":"registration""#));
        assert!(responses[1].contains(r#""type":"action-output""#));
        assert!(responses[1].contains(r#""item":"mode""#));
        assert_eq!(responses[2], r#"{"type":"shutdown"}"#);
        Ok(())
    }

    #[test]
    fn host_request_invoke_json_shape_ignores_internal_boxing() -> Result<(), ExtensionSdkError> {
        let request = HostRequest::Invoke {
            invocation: Box::new(ActionInvocation::new("test.render", serde_json::json!({}))),
        };

        assert_eq!(
            serde_json::to_string(&request)?,
            r#"{"type":"invoke","invocation":{"action":"test.render","args":{},"event":null,"extension":null}}"#
        );
        Ok(())
    }

    #[test]
    fn action_output_serializes_emitted_events() -> Result<(), ExtensionSdkError> {
        let output = ActionOutput::event(
            ActionOutputEvent::new("aerospace.workspace.snapshot", "aerospace")
                .with_data(serde_json::json!({ "active": "2", "occupied": ["1", "2"] })),
        );

        assert_eq!(
            output.to_json_string()?,
            r#"{"events":[{"type":"aerospace.workspace.snapshot","source":"aerospace","data":{"active":"2","occupied":["1","2"]}}]}"#
        );
        Ok(())
    }

    #[test]
    fn action_output_builds_multiple_events() {
        let first = ActionOutputEvent::new("first.changed", "test");
        let second = ActionOutputEvent::new("second.changed", "test");

        let mut output = ActionOutput::events([first]).with_event(second);
        output.push_event(ActionOutputEvent::new("third.changed", "test"));

        assert_eq!(
            output
                .emitted_events()
                .iter()
                .map(|event| event.kind.as_str())
                .collect::<Vec<_>>(),
            vec!["first.changed", "second.changed", "third.changed"]
        );
    }
}
