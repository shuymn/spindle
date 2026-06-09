use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use serde_json::Value;

use crate::{
    EventFilter, EventLog, ExtensionManifest, ExtensionRegistry, ExtensionRuntimeHost, HubRequest,
    SpindleError, execute_request, extension::MANIFEST_FILE, send_request, serve,
    server::prepare_socket, store::ensure_private_parent, validate_installed_registry,
    validate_json_object,
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
    /// Prepare state before running the local spindle daemon.
    Bootstrap(BootstrapArgs),
    /// Run the local spindle daemon.
    Daemon(ServeArgs),
    /// Serve JSONL hub requests over a Unix domain socket.
    Serve(ServeArgs),
    /// Install an extension package.
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
struct BootstrapArgs {
    #[arg(
        long,
        help = "execute extension entrypoints during bootstrap to discover dynamic registration surface"
    )]
    trust_runtime: bool,
    #[arg(long, value_name = "DIR")]
    extension_dir: Vec<PathBuf>,
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
    #[arg(
        long,
        help = "execute the extension entrypoint during install to discover dynamic registration surface"
    )]
    trust_runtime: bool,
    #[arg(value_name = "PACKAGE")]
    package: PathBuf,
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
    /// Show runtime-discovered extension surface without registering it.
    Surface(SurfaceExtensionArgs),
    /// List installed extensions.
    List,
}

#[derive(Debug, Args)]
struct ValidateExtensionArgs {
    #[arg(value_name = "MANIFEST")]
    manifest: PathBuf,
}

#[derive(Debug, Args)]
struct SurfaceExtensionArgs {
    #[arg(
        long,
        help = "execute the extension entrypoint to inspect dynamic registration surface without installing"
    )]
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
        Command::Bootstrap(args) => {
            bootstrap_state(&state_dir, args, &registry, &runtime)?;
        }
        Command::Daemon(args) | Command::Serve(args) => {
            let socket = resolve_socket_path(&state_dir, args.socket);
            serve(&socket, &log)?;
        }
        Command::Install(args) => {
            let registered =
                install_extension_package(&registry, &runtime, &args.package, args.trust_runtime)?;
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
            command: ExtensionSubcommand::List,
        }) => {
            let extensions = registry.list()?;
            write_json(&extensions)?;
        }
    }

    Ok(())
}

fn bootstrap_state(
    state_dir: &Path,
    args: BootstrapArgs,
    registry: &ExtensionRegistry,
    runtime: &ExtensionRuntimeHost,
) -> Result<()> {
    ensure_private_parent(
        &state_dir.join(".bootstrap"),
        "state_dir",
        "state directory must be private",
    )?;

    remove_legacy_policy_file(state_dir)?;

    for package in extension_packages(&args.extension_dir)? {
        install_extension_package(registry, runtime, &package, args.trust_runtime)?;
    }

    let socket = resolve_socket_path(state_dir, args.socket);
    prepare_socket(&socket)?;
    validate_installed_registry(registry)?;
    Ok(())
}

fn install_extension_package(
    registry: &ExtensionRegistry,
    runtime: &ExtensionRuntimeHost,
    package: &Path,
    trust_runtime: bool,
) -> Result<crate::RegisteredExtension, SpindleError> {
    if trust_runtime {
        registry.install_manifest_with_runtime(package, runtime)
    } else {
        let registered = registry.install_manifest(package)?;
        runtime.invalidate_extension(&registered.id);
        Ok(registered)
    }
}

fn extension_packages(inputs: &[PathBuf]) -> Result<Vec<PathBuf>, SpindleError> {
    let mut packages = Vec::new();
    for input in inputs {
        if is_extension_package(input) {
            packages.push(input.clone());
            continue;
        }
        if !input.is_dir() {
            return Err(SpindleError::InvalidField {
                field: "extension-dir",
                reason: "path must be a package directory or directory containing packages",
            });
        }
        let mut children = fs::read_dir(input)?.try_fold(Vec::new(), |mut packages, entry| {
            let path = entry?.path();
            if is_extension_package(&path) {
                packages.push(path);
            }
            Ok::<_, std::io::Error>(packages)
        })?;
        children.sort();
        packages.extend(children);
    }
    Ok(packages)
}

fn is_extension_package(path: &Path) -> bool {
    path.join(MANIFEST_FILE).is_file()
}

fn remove_legacy_policy_file(state_dir: &Path) -> Result<(), SpindleError> {
    let path = state_dir.join("capabilities.json");
    if path.exists() {
        fs::remove_file(path)?;
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

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt};

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
        let package = dir.join("surface-only");
        fs::create_dir_all(package.join("bin"))?;
        let staged_host = package.join("bin/surface-only");
        fs::copy(&host, &staged_host)?;
        let host_config = host.with_extension("json");
        if host_config.is_file() {
            fs::copy(&host_config, staged_host.with_extension("json"))?;
        }
        let mut permissions = fs::metadata(&staged_host)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&staged_host, permissions)?;
        let manifest = package.join("extension.json");
        fs::write(
            &manifest,
            serde_json::to_string_pretty(&serde_json::json!({
                "id": "surface-only",
                "version": "0.1.0",
                "runtime": "stdio-jsonl",
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
                args: String::from("[]"),
            }),
        });

        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn bootstrap_removes_stale_legacy_capabilities_json() -> Result<()> {
        let dir = crate::store::tests_support::test_dir()?;
        let source_dir = crate::store::tests_support::test_dir()?;
        let packages = source_dir.join("packages");
        fs::create_dir_all(&packages)?;
        fs::write(
            dir.join("capabilities.json"),
            r#"{"emits":{},"direct":{},"routes":{"legacy":[]}}"#,
        )?;

        run_cli(Cli {
            state_dir: Some(dir.clone()),
            command: Command::Bootstrap(BootstrapArgs {
                trust_runtime: false,
                extension_dir: vec![packages],
                socket: None,
            }),
        })?;

        assert!(!dir.join("capabilities.json").exists());
        fs::remove_dir_all(dir)?;
        fs::remove_dir_all(source_dir)?;
        Ok(())
    }

    #[test]
    fn bootstrap_rejects_custom_socket_with_public_parent() -> Result<()> {
        let dir = crate::store::tests_support::test_dir()?;
        let socket_dir = crate::store::tests_support::test_dir()?;
        fs::create_dir_all(&socket_dir)?;
        fs::set_permissions(&socket_dir, fs::Permissions::from_mode(0o755))?;

        let result = run_cli(Cli {
            state_dir: Some(dir.clone()),
            command: Command::Bootstrap(BootstrapArgs {
                trust_runtime: false,
                extension_dir: Vec::new(),
                socket: Some(socket_dir.join("spindle.sock")),
            }),
        });

        assert!(matches!(
            result,
            Err(error) if error.downcast_ref::<SpindleError>().is_some_and(|error| {
                matches!(
                    error,
                    SpindleError::InvalidField {
                        field: "socket",
                        ..
                    }
                )
            })
        ));
        fs::set_permissions(&socket_dir, fs::Permissions::from_mode(0o700))?;
        fs::remove_dir_all(dir)?;
        fs::remove_dir_all(socket_dir)?;
        Ok(())
    }

    #[test]
    fn bootstrap_fails_for_incomplete_extension_set() -> Result<()> {
        let dir = crate::store::tests_support::test_dir()?;
        let source_dir = crate::store::tests_support::test_dir()?;
        let packages = source_dir.join("packages");
        let workflow = packages.join("workflow");
        fs::create_dir_all(&workflow)?;
        fs::write(
            workflow.join("extension.json"),
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
        let result = run_cli(Cli {
            state_dir: Some(dir.clone()),
            command: Command::Bootstrap(BootstrapArgs {
                trust_runtime: false,
                extension_dir: vec![packages],
                socket: None,
            }),
        });

        assert!(result.is_err());
        assert!(dir.join("extensions.json").exists());
        fs::remove_dir_all(dir)?;
        fs::remove_dir_all(source_dir)?;
        Ok(())
    }
}
