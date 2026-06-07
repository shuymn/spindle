#![warn(clippy::pedantic)]
#![warn(clippy::nursery)]
#![warn(clippy::cargo)]

use std::{collections::BTreeSet, env};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use serde::Deserialize;
use serde_json::json;
use spindle_extension_sdk::{
    ActionContext, ActionHandler, ActionInvocation, ActionOutput, ExtensionContext,
    ExtensionRegistration, RegistrationAction, RegistrationRoute, serve_stdio_jsonl_actions,
};
use spindle_workspace_indicator_extension::{
    OUTPUT_EVENT, WorkspaceSnapshot, build_status_message, build_workspace_message,
};

const DEFAULT_WORKSPACES: &str = "1,2,3,4,5,6,7,8,9,10";
const WORKSPACE_SETTLE_MS: u64 = 50;
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
    Ok(serve_stdio_jsonl_actions(&registration, ACTION_HANDLERS)?)
}

const ACTION_HANDLERS: &[ActionHandler<anyhow::Error>] = &[
    ActionHandler::new(ACTION_RENDER_WORKSPACES, render_workspaces_host_action),
    ActionHandler::new(ACTION_RENDER_STATUS, render_status_host_action),
];

fn render_workspaces_host_action(context: &ActionContext) -> Result<ActionOutput> {
    render_workspaces_action(None, context)
}

fn render_status_host_action(context: &ActionContext) -> Result<ActionOutput> {
    render_status_action(None, context)
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
        .emit(OUTPUT_EVENT)
        .route(workspace_snapshot_route(AEROSPACE_WORKSPACE_CHANGED_EVENT))
        .route(workspace_snapshot_route(AEROSPACE_MONITOR_CHANGED_EVENT))
        .on(
            AEROSPACE_WORKSPACE_SNAPSHOT_ACTION,
            ACTION_RENDER_WORKSPACES,
            RegistrationAction::new(),
        )
        .route(
            RegistrationRoute::new(OUTPUT_EVENT, OUTPUT_ACTION)
                .capability(SKETCHYBAR_UI_WRITE_CAPABILITY),
        )
        .route(state_route(
            AEROSPACE_MODE_CHANGED_EVENT,
            AEROSPACE_MODE_SNAPSHOT_ACTION,
        ))
        .on_with_args(
            AEROSPACE_MODE_SNAPSHOT_ACTION,
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
        .on_with_args(
            AEROSPACE_LAYOUT_SNAPSHOT_ACTION,
            ACTION_RENDER_STATUS,
            RegistrationAction::new(),
            json!({ "item": LAYOUT_ITEM }),
        )
        .route(
            RegistrationRoute::new(
                SKETCHYBAR_WORKSPACE_CLICKED_EVENT,
                AEROSPACE_WORKSPACE_FOCUS_ACTION,
            )
            .capability(AEROSPACE_WINDOW_CONTROL_CAPABILITY),
        )
}

fn state_route(event: &str, action: &str) -> RegistrationRoute {
    RegistrationRoute::new(event, action).capability(AEROSPACE_STATE_READ_CAPABILITY)
}

fn workspace_snapshot_route(event: &str) -> RegistrationRoute {
    state_route(event, AEROSPACE_WORKSPACE_SNAPSHOT_ACTION)
        .with_args(json!({ "settle_ms": WORKSPACE_SETTLE_MS }))
}
