use std::{
    env,
    io::{self, Write},
    path::PathBuf,
};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use serde_json::Value;

use crate::{
    CapabilityPolicy, EventFilter, EventLog, ExtensionManifest, ExtensionRegistry,
    ExtensionRuntimeHost, HubRequest, SpindleError, execute_request, send_request, serve,
    validate_extension_routes, validate_json_object,
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
    /// Work with capability policy.
    Policy(PolicyCommand),
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
    #[arg(long)]
    trust_runtime: bool,
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

#[derive(Debug, Args)]
struct PolicyCommand {
    #[command(subcommand)]
    command: PolicySubcommand,
}

#[derive(Debug, Subcommand)]
enum PolicySubcommand {
    /// Validate capability policy against installed extensions.
    Validate,
}

#[derive(Debug, Subcommand)]
enum ExtensionSubcommand {
    /// Validate a JSON extension manifest.
    Validate(ValidateExtensionArgs),
    /// Show runtime-discovered extension surface without registering it.
    Surface(SurfaceExtensionArgs),
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
    #[arg(long)]
    trust_runtime: bool,
    #[arg(value_name = "MANIFEST")]
    manifest: PathBuf,
}

#[derive(Debug, Args)]
struct SurfaceExtensionArgs {
    #[arg(long)]
    trust_runtime: bool,
    #[arg(value_name = "MANIFEST")]
    manifest: PathBuf,
}

fn run_cli(cli: Cli) -> Result<()> {
    let state_dir = resolve_state_dir(cli.state_dir)?;
    let log = EventLog::in_dir(&state_dir);
    let registry = ExtensionRegistry::in_dir(&state_dir);
    let runtime = ExtensionRuntimeHost::new();

    match cli.command {
        Command::Daemon(args) | Command::Serve(args) => {
            let socket = resolve_socket_path(&state_dir, args.socket);
            serve(&socket, &log)?;
        }
        Command::Install(args) => {
            let manifest = resolve_install_manifest(&args.extension);
            let registered = if args.trust_runtime {
                registry.install_manifest_with_runtime(&manifest, &runtime)?
            } else {
                registry.install_manifest(&manifest)?
            };
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
            let data = parse_json_object("data", &args.data)
                .context("failed to parse --data as JSON object")?;
            let response = execute_request(
                HubRequest::Emit {
                    kind: args.kind,
                    source: args.source,
                    subject: args.subject,
                    data,
                },
                &log,
                &registry,
                &runtime,
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
            let action_args = parse_json_object("args", &args.args)
                .context("failed to parse --args as JSON object")?;
            let response = execute_request(
                HubRequest::Invoke {
                    action: args.action,
                    source: args.source,
                    capabilities: args.capabilities,
                    args: action_args,
                },
                &log,
                &registry,
                &runtime,
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
            command: ExtensionSubcommand::Surface(args),
        }) => {
            if !args.trust_runtime {
                return Err(SpindleError::RuntimeTrustRequired {
                    extension: args.manifest.display().to_string(),
                }
                .into());
            }
            let manifest =
                ExtensionManifest::from_path_with_registration(&args.manifest, &runtime)?;
            write_json(&manifest)?;
        }
        Command::Extension(ExtensionCommand {
            command: ExtensionSubcommand::Register(args),
        }) => {
            let registered = if args.trust_runtime {
                registry.register_manifest_trusting_runtime(&args.manifest, &runtime)?
            } else {
                registry.register_manifest(&args.manifest)?
            };
            write_json(&registered)?;
        }
        Command::Extension(ExtensionCommand {
            command: ExtensionSubcommand::List,
        }) => {
            let extensions = registry.list()?;
            write_json(&extensions)?;
        }
        Command::Policy(PolicyCommand {
            command: PolicySubcommand::Validate,
        }) => {
            let policy = CapabilityPolicy::load(&state_dir)?;
            let extensions = registry.list()?;
            for extension in &extensions {
                validate_extension_routes(extension, &extensions, &policy)?;
            }
            policy.validate_against_extensions(&extensions)?;
            write_json(&policy)?;
        }
    }

    Ok(())
}

fn parse_json_object(field: &'static str, input: &str) -> Result<Value> {
    let value = serde_json::from_str(input)?;
    validate_json_object(field, &value)?;
    Ok(value)
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

    extension.to_path_buf()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use spindle_extension_sdk::{ExtensionRegistration, RegistrationAction};
    use spindle_test_host::TestHostConfig;

    use super::*;

    #[test]
    fn explicit_state_dir_wins() -> Result<()> {
        let path = PathBuf::from("/tmp/spindle-explicit");
        assert_eq!(resolve_state_dir(Some(path.clone()))?, path);
        Ok(())
    }

    #[test]
    fn parse_json_object_accepts_objects() -> Result<()> {
        let value = parse_json_object("data", r#"{"state":"testing"}"#)?;
        assert_eq!(value["state"], "testing");
        Ok(())
    }

    #[test]
    fn parse_json_object_rejects_arrays() {
        let result = parse_json_object("data", r#"["testing"]"#);
        assert!(result.is_err());
    }

    #[test]
    fn surface_trust_runtime_does_not_write_registry() -> Result<()> {
        let dir = crate::store::tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        let registration = ExtensionRegistration::new()
            .produce("test.rendered")
            .action("test.render", RegistrationAction::new());
        let host = crate::store::tests_support::install_test_host(
            &dir,
            "host",
            &TestHostConfig::with_registration(registration),
        )?;
        let manifest = dir.join("extension.json");
        fs::write(
            &manifest,
            serde_json::to_string_pretty(&serde_json::json!({
                "id": "surface-only",
                "version": "0.1.0",
                "runtime": "stdio-jsonl",
                "entrypoint": host,
                "capabilities": [],
                "actions": {}
            }))?,
        )?;

        run_cli(Cli {
            state_dir: Some(dir.clone()),
            command: Command::Extension(ExtensionCommand {
                command: ExtensionSubcommand::Surface(SurfaceExtensionArgs {
                    trust_runtime: true,
                    manifest,
                }),
            }),
        })?;

        assert!(!dir.join("extensions.json").exists());
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn emit_rejects_non_object_data_from_cli() -> Result<()> {
        let dir = crate::store::tests_support::test_dir()?;
        let result = run_cli(Cli {
            state_dir: Some(dir),
            command: Command::Emit(EmitArgs {
                kind: String::from("test.changed"),
                source: String::from("test"),
                subject: None,
                data: String::from("[]"),
            }),
        });

        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn invoke_rejects_non_object_args_from_cli() -> Result<()> {
        let dir = crate::store::tests_support::test_dir()?;
        let result = run_cli(Cli {
            state_dir: Some(dir),
            command: Command::Invoke(InvokeArgs {
                action: String::from("test.render"),
                source: String::from("test"),
                capabilities: Vec::new(),
                args: String::from("[]"),
            }),
        });

        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn policy_validate_cli_succeeds_for_installed_extensions() -> Result<()> {
        let dir = crate::store::tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        let workflow_manifest = dir.join("workflow.json");
        fs::write(
            &workflow_manifest,
            r#"{
          "id": "workflow",
          "version": "0.1.0",
          "runtime": "recipe",
          "routes": [
            {
              "event": "provider.changed",
              "source": "provider",
              "action": "provider.snapshot",
              "capabilities": ["provider.read"]
            }
          ]
        }"#,
        )?;
        let provider_host = dir.join("provider-host.sh");
        crate::store::tests_support::write_executable(&provider_host, "#!/bin/sh\nexit 0\n")?;
        let provider_manifest = dir.join("provider.json");
        fs::write(
            &provider_manifest,
            format!(
                r#"{{
          "id": "provider",
          "version": "0.1.0",
          "entrypoint": "{}",
          "runtime": "stdio-jsonl",
          "emits": ["provider.changed"],
          "actions": {{
            "provider.snapshot": {{
              "capabilities": []
            }}
          }}
        }}"#,
                provider_host.display()
            ),
        )?;
        crate::store::tests_support::write_capability_policy(
            &dir,
            r#"{"emits":{"provider":["provider.changed"]},"direct":{},"routes":{"workflow":[{"source":"provider","event":"provider.changed","capabilities":["provider.read"]}]}}"#,
        )?;

        let registry = ExtensionRegistry::in_dir(&dir);
        registry.register_manifest(&provider_manifest)?;
        registry.register_manifest(&workflow_manifest)?;

        run_cli(Cli {
            state_dir: Some(dir.clone()),
            command: Command::Policy(PolicyCommand {
                command: PolicySubcommand::Validate,
            }),
        })?;

        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn install_directory_resolves_to_extension_manifest() -> Result<()> {
        let dir = crate::store::tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        assert_eq!(
            resolve_install_manifest(dir.as_path()),
            dir.join("extension.json")
        );
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn install_manifest_path_is_used_as_is() {
        let path = PathBuf::from("/tmp/my-extension/extension.json");
        assert_eq!(resolve_install_manifest(path.as_path()), path);
    }
}
