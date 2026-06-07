use std::{
    env,
    io::{self, Write},
    path::PathBuf,
};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use serde_json::Value;

use crate::{
    EventFilter, EventLog, ExtensionManifest, ExtensionRegistry, ExtensionRuntimeHost, HubRequest,
    SpindleError, execute_request, send_request, serve,
};

/// Run the spindle command-line interface.
///
/// # Errors
///
/// Returns an error when arguments are invalid, state cannot be read or written,
/// JSON parsing fails, or manifest validation fails.
pub fn run() -> Result<()> {
    let cli = Cli::parse();
    run_cli(cli)
}

#[derive(Debug, Parser)]
#[command(
    name = "spindle",
    about = "Minimal local automation harness",
    version,
    propagate_version = true
)]
struct Cli {
    #[arg(long, global = true, value_name = "DIR")]
    state_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the local spindle daemon.
    Daemon(ServeArgs),
    /// Serve JSONL hub requests over a Unix domain socket.
    Serve(ServeArgs),
    /// Install an extension package or manifest.
    Install(InstallArgs),
    /// Send one JSONL hub request to a running server.
    Send(SendArgs),
    /// Append an event to the local log.
    Emit(EmitArgs),
    /// Query local state.
    Query(QueryCommand),
    /// Dispatch an installed action.
    Invoke(InvokeArgs),
    /// Work with extension manifests.
    Extension(ExtensionCommand),
}

#[derive(Debug, Args)]
struct ServeArgs {
    #[arg(long, value_name = "SOCKET")]
    socket: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct SendArgs {
    #[arg(long, value_name = "SOCKET")]
    socket: Option<PathBuf>,
    #[arg(long, value_name = "JSON")]
    request: String,
}

#[derive(Debug, Args)]
struct InstallArgs {
    #[arg(value_name = "EXTENSION")]
    extension: PathBuf,
}

#[derive(Debug, Args)]
struct EmitArgs {
    #[arg(long = "type", value_name = "TYPE")]
    kind: String,
    #[arg(long, value_name = "SOURCE")]
    source: String,
    #[arg(long, value_name = "SUBJECT")]
    subject: Option<String>,
    #[arg(long, value_name = "JSON", default_value = "{}")]
    data: String,
}

#[derive(Debug, Args)]
struct InvokeArgs {
    #[arg(long, value_name = "ACTION")]
    action: String,
    #[arg(long, value_name = "SOURCE")]
    source: String,
    #[arg(long = "capability", value_name = "CAPABILITY")]
    capabilities: Vec<String>,
    #[arg(long, value_name = "JSON", default_value = "{}")]
    args: String,
}

#[derive(Debug, Args)]
struct QueryCommand {
    #[command(subcommand)]
    command: QuerySubcommand,
}

#[derive(Debug, Subcommand)]
enum QuerySubcommand {
    /// Read events from the local log.
    Events(QueryEventsArgs),
}

#[derive(Debug, Args)]
struct QueryEventsArgs {
    #[arg(long = "type", value_name = "TYPE")]
    kind: Option<String>,
    #[arg(long, value_name = "SOURCE")]
    source: Option<String>,
    #[arg(long, value_name = "N")]
    limit: Option<usize>,
}

#[derive(Debug, Args)]
struct ExtensionCommand {
    #[command(subcommand)]
    command: ExtensionSubcommand,
}

#[derive(Debug, Subcommand)]
enum ExtensionSubcommand {
    /// Validate a JSON extension manifest.
    Validate(ValidateExtensionArgs),
    /// Register or replace an extension manifest.
    Register(RegisterExtensionArgs),
    /// List registered extensions.
    List,
}

#[derive(Debug, Args)]
struct ValidateExtensionArgs {
    #[arg(value_name = "MANIFEST")]
    manifest: PathBuf,
}

#[derive(Debug, Args)]
struct RegisterExtensionArgs {
    #[arg(value_name = "MANIFEST")]
    manifest: PathBuf,
}

fn run_cli(cli: Cli) -> Result<()> {
    let state_dir = resolve_state_dir(cli.state_dir)?;
    let log = EventLog::in_dir(&state_dir);
    let registry = ExtensionRegistry::in_dir(&state_dir);
    let mut runtime = ExtensionRuntimeHost::new();

    match cli.command {
        Command::Daemon(args) | Command::Serve(args) => {
            let socket = resolve_socket_path(&state_dir, args.socket);
            serve(&socket, &log)?;
        }
        Command::Install(args) => {
            let manifest = resolve_install_manifest(&args.extension);
            let registered = registry.install_manifest_with_runtime(&manifest, &mut runtime)?;
            write_json(&registered)?;
        }
        Command::Send(args) => {
            let socket = resolve_socket_path(&state_dir, args.socket);
            let request = serde_json::from_str::<HubRequest>(&args.request)
                .context("failed to parse --request as JSON")?;
            let response = send_request(&socket, &request)?;
            write_json(&response)?;
        }
        Command::Emit(args) => {
            let data = parse_json(&args.data).context("failed to parse --data as JSON")?;
            let response = execute_request(
                HubRequest::Emit {
                    kind: args.kind,
                    source: args.source,
                    subject: args.subject,
                    data,
                },
                &log,
                &registry,
                &mut runtime,
            )?;
            write_json(&response)?;
        }
        Command::Query(QueryCommand {
            command: QuerySubcommand::Events(args),
        }) => {
            let events = log.read(&EventFilter {
                kind: args.kind,
                source: args.source,
                limit: args.limit,
            })?;
            write_json(&events)?;
        }
        Command::Invoke(args) => {
            let action_args = parse_json(&args.args).context("failed to parse --args as JSON")?;
            let response = execute_request(
                HubRequest::Invoke {
                    action: args.action,
                    source: args.source,
                    capabilities: args.capabilities,
                    args: action_args,
                },
                &log,
                &registry,
                &mut runtime,
            )?;
            write_json(&response)?;
        }
        Command::Extension(ExtensionCommand {
            command: ExtensionSubcommand::Validate(args),
        }) => {
            let manifest = ExtensionManifest::from_path(&args.manifest)?;
            write_json(&manifest)?;
        }
        Command::Extension(ExtensionCommand {
            command: ExtensionSubcommand::Register(args),
        }) => {
            let registered =
                registry.register_manifest_with_runtime(&args.manifest, &mut runtime)?;
            write_json(&registered)?;
        }
        Command::Extension(ExtensionCommand {
            command: ExtensionSubcommand::List,
        }) => {
            let extensions = registry.list()?;
            write_json(&extensions)?;
        }
    }

    Ok(())
}

fn parse_json(input: &str) -> Result<Value> {
    Ok(serde_json::from_str(input)?)
}

fn write_json<T>(value: &T) -> Result<()>
where
    T: serde::Serialize,
{
    let mut stdout = io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, value)?;
    writeln!(stdout)?;
    Ok(())
}

fn resolve_state_dir(explicit: Option<PathBuf>) -> Result<PathBuf, SpindleError> {
    if let Some(path) = explicit {
        return Ok(path);
    }

    if let Ok(path) = env::var("SPINDLE_STATE_DIR") {
        return Ok(PathBuf::from(path));
    }

    let home = env::var("HOME").map_err(|_err| SpindleError::MissingHome)?;
    Ok(PathBuf::from(home)
        .join(".local")
        .join("state")
        .join("spindle"))
}

fn resolve_socket_path(state_dir: &std::path::Path, explicit: Option<PathBuf>) -> PathBuf {
    explicit.unwrap_or_else(|| state_dir.join("spindle.sock"))
}

fn resolve_install_manifest(extension: &std::path::Path) -> PathBuf {
    if extension.is_dir() {
        return extension.join("extension.json");
    }

    if extension.exists() {
        return extension.to_path_buf();
    }

    PathBuf::from("extensions")
        .join(extension)
        .join("extension.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_state_dir_wins() -> Result<()> {
        let path = PathBuf::from("/tmp/spindle-explicit");
        assert_eq!(resolve_state_dir(Some(path.clone()))?, path);
        Ok(())
    }

    #[test]
    fn parse_json_accepts_objects() -> Result<()> {
        let value = parse_json(r#"{"state":"testing"}"#)?;
        assert_eq!(value["state"], "testing");
        Ok(())
    }

    #[test]
    fn install_name_resolves_to_builtin_extension_manifest() {
        assert_eq!(
            resolve_install_manifest(std::path::Path::new("aerospace")),
            PathBuf::from("extensions/aerospace/extension.json")
        );
    }
}
