#![warn(clippy::pedantic)]
#![warn(clippy::nursery)]
#![warn(clippy::cargo)]

use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use serde::Deserialize;
use spindle_extension_sdk::{
    ActionContext, ActionHandler, ActionInvocation, ActionOutput, ExtensionRegistration,
    RegistrationAction, serve_stdio_jsonl_actions,
};
use spindle_sketchybar_extension::send_args;

const ACTION_SEND_MESSAGE: &str = "sketchybar.message.send";
const EVENT_WORKSPACE_CLICKED: &str = "sketchybar.workspace.clicked";
const CAPABILITY_UI_WRITE: &str = "sketchybar.ui.write";

fn main() -> Result<()> {
    Cli::parse().run()
}

#[derive(Debug, Parser)]
#[command(name = "spindle-sketchybar", version)]
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
            CliCommand::SendMessage(args) => {
                let context = cli_context(
                    ACTION_SEND_MESSAGE,
                    serde_json::json!({
                        "cache_key": args.cache_key,
                        "cache_value": args.cache_value,
                    }),
                );
                send_message_action(args.state_dir, args.args, &context)?;
            }
        }
        Ok(())
    }
}

fn serve_host() -> Result<()> {
    let registration = registration();
    Ok(serve_stdio_jsonl_actions(&registration, ACTION_HANDLERS)?)
}

const ACTION_HANDLERS: &[ActionHandler<anyhow::Error>] = &[ActionHandler::new(
    ACTION_SEND_MESSAGE,
    send_message_host_action,
)];

fn send_message_host_action(context: &ActionContext) -> Result<ActionOutput> {
    send_message_action(None, Vec::new(), context)?;
    Ok(ActionOutput::empty())
}

#[derive(Debug, Subcommand)]
enum CliCommand {
    /// Register this extension's spindle surface.
    Register,
    /// Send raw `SketchyBar` command arguments.
    SendMessage(SendMessageArgs),
}

#[derive(Debug, Args)]
struct SendMessageArgs {
    #[arg(long, value_name = "DIR")]
    state_dir: Option<PathBuf>,
    #[arg(long, value_name = "KEY")]
    cache_key: Option<String>,
    #[arg(long, value_name = "VALUE")]
    cache_value: Option<String>,
    #[arg(value_name = "ARG")]
    args: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SendMessageActionArgs {
    args: Vec<String>,
    cache_key: Option<String>,
    cache_value: Option<String>,
}

fn resolve_message_args(cli_args: Vec<String>, action_args: Vec<String>) -> Result<Vec<String>> {
    let args = if cli_args.is_empty() {
        action_args
    } else {
        cli_args
    };
    if args.is_empty() {
        return Err(anyhow::anyhow!("no SketchyBar arguments were provided"));
    }
    if let Some(arg) = args.iter().find(|arg| arg.chars().any(char::is_control)) {
        anyhow::bail!("SketchyBar argument contains control characters: {arg:?}");
    }
    Ok(args)
}

fn send_message_action(
    state_dir: Option<PathBuf>,
    cli_args: Vec<String>,
    context: &ActionContext,
) -> Result<()> {
    let action_args = context.args::<SendMessageActionArgs>()?;
    let cache = action_args.cache_key.zip(action_args.cache_value);
    let sketchybar_args = resolve_message_args(cli_args, action_args.args)?;
    send_message(state_dir, &sketchybar_args, cache.as_ref())?;
    Ok(())
}

fn send_message(
    state_dir: Option<PathBuf>,
    args: &[String],
    cache: Option<&(String, String)>,
) -> Result<()> {
    let Some((cache_key, cache_value)) = cache else {
        send_args(args)?;
        return Ok(());
    };

    validate_cache_key(cache_key)?;
    let cache_path = resolve_state_dir(state_dir).join(format!("{cache_key}.state"));
    if cache_is_current(&cache_path, cache_value)? {
        return Ok(());
    }

    send_args(args)?;
    write_cache(&cache_path, cache_value)?;
    Ok(())
}

fn validate_cache_key(cache_key: &str) -> Result<()> {
    if cache_key.trim().is_empty() {
        anyhow::bail!("cache key must not be empty");
    }
    if cache_key
        .chars()
        .any(|character| character.is_control() || character == '/')
    {
        anyhow::bail!("cache key contains invalid characters: {cache_key:?}");
    }
    Ok(())
}

fn resolve_state_dir(explicit: Option<PathBuf>) -> PathBuf {
    explicit.unwrap_or_else(default_state_dir)
}

fn default_state_dir() -> PathBuf {
    env_path("SPINDLE_SKETCHYBAR_STATE_DIR")
        .or_else(|| env_path("SPINDLE_WORKSPACE_INDICATOR_STATE_DIR"))
        .or_else(|| env_path("AEROSPACE_STATE_DIR"))
        .unwrap_or_else(|| tmp_dir().join("sketchybar-aerospace"))
}

fn env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn tmp_dir() -> PathBuf {
    env::var_os("TMPDIR")
        .filter(|value| !value.is_empty())
        .map_or_else(|| Path::new("/tmp").to_path_buf(), PathBuf::from)
}

fn cache_is_current(path: &Path, value: &str) -> Result<bool> {
    match fs::read_to_string(path) {
        Ok(previous) => Ok(previous == value),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn write_cache(path: &Path, value: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let tmp_path = path.with_extension(format!("tmp.{}", std::process::id()));
    fs::write(&tmp_path, value)?;
    fs::rename(tmp_path, path)?;
    Ok(())
}

fn print_registration(registration: &ExtensionRegistration) -> Result<()> {
    println!("{}", registration.to_json_string()?);
    Ok(())
}

fn cli_context(action: &str, args: serde_json::Value) -> ActionContext {
    ActionContext::from_invocation(ActionInvocation::new(action, args))
}

fn registration() -> ExtensionRegistration {
    ExtensionRegistration::new()
        .emit(EVENT_WORKSPACE_CLICKED)
        .capability(CAPABILITY_UI_WRITE)
        .action(
            ACTION_SEND_MESSAGE,
            RegistrationAction::new().capability(CAPABILITY_UI_WRITE),
        )
}
