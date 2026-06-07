#![warn(clippy::pedantic)]
#![warn(clippy::nursery)]
#![warn(clippy::cargo)]

use std::{
    collections::BTreeSet,
    env,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use serde::Deserialize;
use serde_json::json;
use spindle_extension_sdk::{
    ActionContext, ActionInvocation, ActionOutput, ContinuationContext, ExtensionContext,
    ExtensionRegistration, RegistrationAction, RegistrationRoute, serve_stdio_jsonl_host,
};
use spindle_workspace_indicator_extension::{
    OUTPUT_EVENT, WorkspaceSnapshot, build_status_message, build_workspace_message,
};

const DEFAULT_WORKSPACES: &str = "1,2,3,4,5,6,7,8,9,10";
const WORKSPACE_DEBOUNCE_MS: u64 = 50;
const ACTION_SCHEDULE_WORKSPACES: &str = "workspace-indicator.workspaces.schedule";
const ACTION_RENDER_WORKSPACES: &str = "workspace-indicator.workspaces.render";
const ACTION_RENDER_STATUS: &str = "workspace-indicator.status.render";
const OUTPUT_ACTION: &str = "sketchybar.message.send";
const SKETCHYBAR_WORKSPACE_CLICKED_EVENT: &str = "sketchybar.workspace.clicked";
const SKETCHYBAR_UI_WRITE_CAPABILITY: &str = "sketchybar.ui.write";
const AEROSPACE_STATE_READ_CAPABILITY: &str = "aerospace.state.read";
const AEROSPACE_WINDOW_CONTROL_CAPABILITY: &str = "aerospace.window.control";
const AEROSPACE_WORKSPACE_CHANGED_EVENT: &str = "aerospace.workspace.changed";
const AEROSPACE_MONITOR_CHANGED_EVENT: &str = "aerospace.monitor.changed";
const AEROSPACE_MODE_CHANGED_EVENT: &str = "aerospace.mode.changed";
const AEROSPACE_FOCUS_CHANGED_EVENT: &str = "aerospace.focus.changed";
const AEROSPACE_LAYOUT_CHANGED_EVENT: &str = "aerospace.layout.changed";
const AEROSPACE_WORKSPACE_SNAPSHOT_ACTION: &str = "aerospace.workspace.snapshot";
const AEROSPACE_MODE_SNAPSHOT_ACTION: &str = "aerospace.mode.snapshot";
const AEROSPACE_LAYOUT_SNAPSHOT_ACTION: &str = "aerospace.layout.snapshot";
const AEROSPACE_WORKSPACE_FOCUS_ACTION: &str = "aerospace.workspace.focus";
const MODE_ITEM: &str = "aerospace.mode";
const LAYOUT_ITEM: &str = "aerospace.layout";

fn main() -> Result<()> {
    Cli::parse().run()
}

#[derive(Debug, Parser)]
#[command(name = "spindle-workspace-indicator", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<CliCommand>,
}

impl Cli {
    fn run(self) -> Result<()> {
        let Some(command) = self.command else {
            return serve_host();
        };
        match command {
            CliCommand::Register => {
                print_registration(&registration())?;
            }
            CliCommand::RenderWorkspaces(args) => {
                let context = cli_context(
                    ACTION_RENDER_WORKSPACES,
                    json!({
                        "active": &args.active,
                        "occupied": &args.occupied,
                        "workspaces": &args.workspaces,
                    }),
                    args.extension_context,
                )?;
                print_output(&render_workspaces_action(
                    args.workspaces.as_deref(),
                    &context,
                )?)?;
            }
            CliCommand::RenderStatus(args) => {
                let context = cli_context(
                    ACTION_RENDER_STATUS,
                    json!({
                        "item": &args.item,
                        "name": &args.name,
                        "mode": &args.mode,
                        "window_info": &args.window_info,
                    }),
                    args.extension_context,
                )?;
                let output = render_status_action(args.item, &context)?;
                print_output(&output)?;
            }
        }

        Ok(())
    }
}

fn serve_host() -> Result<()> {
    let registration = registration();
    let scheduler = SchedulerState::default();
    Ok(serve_stdio_jsonl_host(&registration, move |context| {
        route_host_action(&scheduler, context)
    })?)
}

#[derive(Debug, Clone, Default)]
struct SchedulerState {
    generation: Arc<AtomicU64>,
}

fn route_host_action(scheduler: &SchedulerState, context: &ActionContext) -> Result<ActionOutput> {
    match context.action() {
        Some(ACTION_SCHEDULE_WORKSPACES) => schedule_workspaces_action(scheduler, context),
        Some(ACTION_RENDER_WORKSPACES) => render_workspaces_action(None, context),
        Some(ACTION_RENDER_STATUS) => render_status_action(None, context),
        Some(action) => anyhow::bail!("unknown action: {action}"),
        None => anyhow::bail!("missing action"),
    }
}

#[derive(Debug, Subcommand)]
enum CliCommand {
    /// Register this extension's spindle surface.
    Register,
    /// Project workspace state into a generic `SketchyBar` message request.
    RenderWorkspaces(RenderWorkspacesArgs),
    /// Project mode or layout state into a generic `SketchyBar` message request.
    RenderStatus(RenderStatusArgs),
}

#[derive(Debug, Args)]
struct RenderWorkspacesArgs {
    #[arg(long, value_name = "CSV")]
    workspaces: Option<String>,
    #[arg(long, value_name = "WORKSPACE")]
    active: Option<String>,
    #[arg(long, value_name = "WORKSPACE")]
    occupied: Vec<String>,
    #[arg(long, value_name = "JSON")]
    extension_context: Option<String>,
}

#[derive(Debug, Args)]
struct RenderStatusArgs {
    #[arg(long, value_name = "ITEM")]
    item: Option<String>,
    #[arg(long, value_name = "NAME")]
    name: Option<String>,
    #[arg(long, value_name = "MODE")]
    mode: Option<String>,
    #[arg(long, value_name = "WINDOW")]
    window_info: Option<String>,
    #[arg(long, value_name = "JSON")]
    extension_context: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RenderWorkspacesActionArgs {
    active: Option<String>,
    occupied: Vec<String>,
    workspaces: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RenderStatusActionArgs {
    item: Option<String>,
    name: Option<String>,
    mode: Option<String>,
    window_info: Option<String>,
}

fn parse_workspaces(input: &str) -> Result<Vec<String>> {
    let workspaces = input
        .split(',')
        .map(str::trim)
        .filter(|workspace| !workspace.is_empty())
        .map(String::from)
        .collect::<Vec<_>>();

    if workspaces.is_empty() {
        anyhow::bail!("--workspaces must contain at least one workspace");
    }

    Ok(workspaces)
}

fn resolve_item(explicit: Option<String>, action_args: &RenderStatusActionArgs) -> Result<String> {
    explicit
        .or_else(|| action_args.item.clone())
        .or_else(|| action_args.name.clone())
        .or_else(|| env::var("NAME").ok())
        .context("NAME is not set and --item was not provided")
}

fn schedule_workspaces_action(
    scheduler: &SchedulerState,
    context: &ActionContext,
) -> Result<ActionOutput> {
    ensure_scheduler_surface(context.extension())?;
    let continuation = context
        .continuation()
        .cloned()
        .context("continuation is required for workspace scheduling")?;
    let generation = scheduler.generation.fetch_add(1, Ordering::AcqRel) + 1;
    let scheduler = scheduler.clone();
    let _worker = thread::spawn(move || {
        thread::sleep(Duration::from_millis(WORKSPACE_DEBOUNCE_MS));
        if scheduler.generation.load(Ordering::Acquire) != generation {
            return;
        }
        if let Err(error) = invoke_workspace_snapshot(&continuation) {
            eprintln!("[workspace-indicator] scheduler snapshot failed: {error:#}");
        }
    });
    Ok(ActionOutput::empty())
}

fn invoke_workspace_snapshot(continuation: &ContinuationContext) -> Result<()> {
    let mut stream = UnixStream::connect(&continuation.socket).with_context(|| {
        format!(
            "failed to connect to spindle socket: {}",
            continuation.socket
        )
    })?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let request = json!({
        "command": "continuation-invoke",
        "continuation": continuation.id,
        "action": AEROSPACE_WORKSPACE_SNAPSHOT_ACTION,
        "args": {}
    });
    serde_json::to_writer(&mut stream, &request)?;
    writeln!(stream)?;
    stream.shutdown(std::net::Shutdown::Write)?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let response = serde_json::from_str::<serde_json::Value>(&line)?;
    if response.get("status").and_then(serde_json::Value::as_str) == Some("ok") {
        return Ok(());
    }
    anyhow::bail!("continuation invoke failed: {response}")
}

fn render_workspaces_action(
    explicit_workspaces: Option<&str>,
    context: &ActionContext,
) -> Result<ActionOutput> {
    ensure_output_surface(context.extension())?;
    let action_args = context.args::<RenderWorkspacesActionArgs>()?;
    let workspaces = parse_workspaces(
        explicit_workspaces
            .or(action_args.workspaces.as_deref())
            .unwrap_or(DEFAULT_WORKSPACES),
    )?;
    let snapshot = WorkspaceSnapshot::new(
        action_args.active,
        action_args.occupied.into_iter().collect::<BTreeSet<_>>(),
    );
    let request = build_workspace_message(&workspaces, &snapshot)?;
    Ok(ActionOutput::event(request.into_event()))
}

fn render_status_action(
    explicit_item: Option<String>,
    context: &ActionContext,
) -> Result<ActionOutput> {
    ensure_output_surface(context.extension())?;
    let action_args = context.args::<RenderStatusActionArgs>()?;
    let item = resolve_item(explicit_item, &action_args)?;
    let output = build_status_message(
        &item,
        action_args.mode.as_deref(),
        action_args.window_info.as_deref(),
    )
    .map_or_else(ActionOutput::empty, |request| {
        ActionOutput::event(request.into_event())
    });
    Ok(output)
}

fn print_output(output: &ActionOutput) -> Result<()> {
    println!("{}", output.to_json_string()?);
    Ok(())
}

fn print_registration(registration: &ExtensionRegistration) -> Result<()> {
    println!("{}", registration.to_json_string()?);
    Ok(())
}

fn cli_context(
    action: &str,
    args: serde_json::Value,
    extension_context: Option<String>,
) -> Result<ActionContext> {
    let extension = extension_context
        .map(|raw| serde_json::from_str::<ExtensionContext>(&raw))
        .transpose()?;
    Ok(ActionContext::from_invocation(
        ActionInvocation::new(action, args).with_extension(extension),
    ))
}

fn ensure_scheduler_surface(extension: Option<&ExtensionContext>) -> Result<()> {
    ensure_output_surface(extension)?;
    let Some(extension) = extension else {
        return Ok(());
    };

    if !extension.has_action(AEROSPACE_WORKSPACE_SNAPSHOT_ACTION) {
        anyhow::bail!("required action is not available: {AEROSPACE_WORKSPACE_SNAPSHOT_ACTION}");
    }
    Ok(())
}

fn ensure_output_surface(extension: Option<&ExtensionContext>) -> Result<()> {
    let Some(extension) = extension else {
        return Ok(());
    };

    if !extension.has_event(OUTPUT_EVENT) {
        anyhow::bail!("required event is not available: {OUTPUT_EVENT}");
    }
    if !extension.has_action(OUTPUT_ACTION) {
        anyhow::bail!("required action is not available: {OUTPUT_ACTION}");
    }
    Ok(())
}

fn registration() -> ExtensionRegistration {
    ExtensionRegistration::new()
        .produce(OUTPUT_EVENT)
        .action(
            ACTION_SCHEDULE_WORKSPACES,
            RegistrationAction::new().capability(AEROSPACE_STATE_READ_CAPABILITY),
        )
        .route(state_route(
            AEROSPACE_WORKSPACE_CHANGED_EVENT,
            ACTION_SCHEDULE_WORKSPACES,
        ))
        .route(state_route(
            AEROSPACE_MONITOR_CHANGED_EVENT,
            ACTION_SCHEDULE_WORKSPACES,
        ))
        .on_from(
            "aerospace",
            AEROSPACE_WORKSPACE_SNAPSHOT_ACTION,
            ACTION_RENDER_WORKSPACES,
            RegistrationAction::new(),
        )
        .route(
            RegistrationRoute::new(OUTPUT_EVENT, OUTPUT_ACTION)
                .source("workspace-indicator")
                .capability(SKETCHYBAR_UI_WRITE_CAPABILITY),
        )
        .route(state_route(
            AEROSPACE_MODE_CHANGED_EVENT,
            AEROSPACE_MODE_SNAPSHOT_ACTION,
        ))
        .on_with_args_from(
            ("aerospace", AEROSPACE_MODE_SNAPSHOT_ACTION),
            ACTION_RENDER_STATUS,
            RegistrationAction::new(),
            json!({ "item": MODE_ITEM }),
        )
        .route(state_route(
            AEROSPACE_FOCUS_CHANGED_EVENT,
            AEROSPACE_LAYOUT_SNAPSHOT_ACTION,
        ))
        .route(state_route(
            AEROSPACE_LAYOUT_CHANGED_EVENT,
            AEROSPACE_LAYOUT_SNAPSHOT_ACTION,
        ))
        .on_with_args_from(
            ("aerospace", AEROSPACE_LAYOUT_SNAPSHOT_ACTION),
            ACTION_RENDER_STATUS,
            RegistrationAction::new(),
            json!({ "item": LAYOUT_ITEM }),
        )
        .route(
            RegistrationRoute::new(
                SKETCHYBAR_WORKSPACE_CLICKED_EVENT,
                AEROSPACE_WORKSPACE_FOCUS_ACTION,
            )
            .source("sketchybar")
            .capability(AEROSPACE_WINDOW_CONTROL_CAPABILITY),
        )
}

fn state_route(event: &str, action: &str) -> RegistrationRoute {
    RegistrationRoute::new(event, action)
        .source("aerospace")
        .capability(AEROSPACE_STATE_READ_CAPABILITY)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixListener,
        sync::mpsc,
        time::Duration,
    };

    use spindle_extension_sdk::{
        ActionDescriptor, ContinuationContext, EventDescriptor, ExtensionContext,
    };

    use super::*;

    fn scheduler_context(socket: &std::path::Path) -> ActionContext {
        ActionContext::from_invocation(
            ActionInvocation::new(ACTION_SCHEDULE_WORKSPACES, json!({}))
                .with_extension(Some(ExtensionContext {
                    id: String::from("workspace-indicator"),
                    events: vec![EventDescriptor {
                        kind: String::from(OUTPUT_EVENT),
                        source_extension: String::from("workspace-indicator"),
                    }],
                    actions: vec![
                        ActionDescriptor {
                            name: String::from(AEROSPACE_WORKSPACE_SNAPSHOT_ACTION),
                            extension: String::from("aerospace"),
                            capabilities: vec![String::from(AEROSPACE_STATE_READ_CAPABILITY)],
                        },
                        ActionDescriptor {
                            name: String::from(OUTPUT_ACTION),
                            extension: String::from("sketchybar"),
                            capabilities: vec![String::from(SKETCHYBAR_UI_WRITE_CAPABILITY)],
                        },
                    ],
                    capabilities: vec![
                        String::from(AEROSPACE_STATE_READ_CAPABILITY),
                        String::from(SKETCHYBAR_UI_WRITE_CAPABILITY),
                    ],
                }))
                .with_continuation(Some(ContinuationContext::new(
                    "continuation-1",
                    socket.display().to_string(),
                    9_999_999_999,
                ))),
        )
    }

    #[test]
    fn schedule_action_returns_without_rendering_synchronously() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let socket = dir.path().join("spindle.sock");
        let context = scheduler_context(&socket);
        let scheduler = SchedulerState::default();

        let output = schedule_workspaces_action(&scheduler, &context)?;

        assert!(output.emitted_events().is_empty());
        Ok(())
    }

    #[test]
    fn rapid_schedule_actions_invoke_only_latest_snapshot() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let socket = dir.path().join("spindle.sock");
        if socket.exists() {
            fs::remove_file(&socket)?;
        }
        let listener = UnixListener::bind(&socket)?;
        listener.set_nonblocking(false)?;
        let (sender, receiver) = mpsc::channel();
        let server = std::thread::spawn(move || -> Result<()> {
            let (mut stream, _address) = listener.accept()?;
            let mut line = String::new();
            BufReader::new(stream.try_clone()?).read_line(&mut line)?;
            sender.send(line)?;
            stream.write_all(b"{\"status\":\"ok\",\"data\":{}}\n")?;
            stream.flush()?;
            Ok(())
        });
        let scheduler = SchedulerState::default();
        let first = scheduler_context(&socket);
        let second = scheduler_context(&socket);

        schedule_workspaces_action(&scheduler, &first)?;
        schedule_workspaces_action(&scheduler, &second)?;

        let request = receiver.recv_timeout(Duration::from_secs(1))?;
        assert!(request.contains(r#""command":"continuation-invoke""#));
        assert!(request.contains(r#""action":"aerospace.workspace.snapshot""#));
        server
            .join()
            .map_err(|_payload| anyhow::anyhow!("server thread panicked"))??;
        Ok(())
    }
}
