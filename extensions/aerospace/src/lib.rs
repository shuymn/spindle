//! `AeroSpace` IPC primitives and spindle actions.

#![warn(clippy::pedantic)]
#![warn(clippy::nursery)]
#![warn(clippy::cargo)]

use std::{
    collections::BTreeSet,
    env, io,
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::{Mutex, PoisonError},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const AEROSPACE_SOCKET_ENV: &str = "AEROSPACESOCK";
const AEROSPACE_WINDOW_ID_ENV: &str = "AEROSPACE_WINDOW_ID";
const AEROSPACE_WORKSPACE_ENV: &str = "AEROSPACE_WORKSPACE";

/// Error returned by the `AeroSpace` extension.
#[derive(Debug, Error)]
pub enum AerospaceError {
    /// A workspace value cannot safely be passed to `AeroSpace`.
    #[error("invalid workspace: {reason}")]
    InvalidWorkspace {
        /// Validation failure reason.
        reason: &'static str,
    },

    /// An `AeroSpace` command exited unsuccessfully.
    #[error("aerospace command failed: {command} exited {exit_code}: {stderr}")]
    CommandFailed {
        /// Command and arguments.
        command: String,
        /// `AeroSpace` exit code.
        exit_code: i32,
        /// Standard error text returned by `AeroSpace`.
        stderr: String,
    },

    /// `AeroSpace` socket path could not be resolved.
    #[error("AEROSPACESOCK was not set and USER was not available")]
    MissingSocketUser,

    /// The reusable `AeroSpace` socket connection state is unavailable.
    #[error("aerospace ipc client state is unavailable")]
    ClientState,

    /// Filesystem, stream, or socket I/O failed.
    #[error(transparent)]
    Io(#[from] io::Error),

    /// JSON parsing or serialization failed.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// Low-level `AeroSpace` client.
pub trait AerospaceClient {
    /// Send an `AeroSpace` command without spawning the `aerospace` CLI.
    ///
    /// # Errors
    ///
    /// Returns an error when the socket cannot be reached, the request or
    /// response cannot be serialized, or `AeroSpace` reports a failure.
    fn send_command(
        &self,
        command: &str,
        args: &[&str],
    ) -> Result<AerospaceResponse, AerospaceError>;
}

/// Response returned by `AeroSpace` over its Unix socket.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AerospaceResponse {
    #[serde(rename = "serverVersionAndHash")]
    server_version: String,
    stderr: String,
    stdout: String,
    #[serde(rename = "exitCode")]
    exit_code: i32,
}

/// Snapshot of `AeroSpace` workspace state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkspaceSnapshot {
    /// Focused workspace, when available.
    pub active: Option<String>,
    /// Workspaces with at least one window.
    pub occupied: Vec<String>,
}

impl AerospaceResponse {
    /// Create a successful response value.
    #[must_use]
    pub fn success(stdout: impl Into<String>) -> Self {
        Self {
            server_version: String::new(),
            stderr: String::new(),
            stdout: stdout.into(),
            exit_code: 0,
        }
    }

    /// Return the `AeroSpace` standard output payload.
    #[must_use]
    pub fn stdout(&self) -> &str {
        &self.stdout
    }

    /// Return the `AeroSpace` standard error payload.
    #[must_use]
    pub fn stderr(&self) -> &str {
        &self.stderr
    }

    /// Return the `AeroSpace` exit code.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        self.exit_code
    }

    fn ensure_success(self, command: &str, args: &[&str]) -> Result<Self, AerospaceError> {
        if self.exit_code == 0 {
            return Ok(self);
        }

        let command = command_line(command, args);
        Err(AerospaceError::CommandFailed {
            command,
            exit_code: self.exit_code,
            stderr: self.stderr,
        })
    }
}

/// `AeroSpace` Unix socket client.
#[derive(Debug)]
pub struct AerospaceIpcClient {
    socket_path: PathBuf,
    stream: Mutex<Option<UnixStream>>,
}

impl AerospaceIpcClient {
    /// Create a client for an explicit socket path.
    #[must_use]
    pub const fn new(socket_path: PathBuf) -> Self {
        Self {
            socket_path,
            stream: Mutex::new(None),
        }
    }

    /// Create a client from `AEROSPACESOCK` or the default socket path.
    ///
    /// # Errors
    ///
    /// Returns an error when neither `AEROSPACESOCK` nor `USER` can resolve the
    /// socket path.
    pub fn from_env() -> Result<Self, AerospaceError> {
        Ok(Self::new(resolve_socket_path()?))
    }

    /// Send an `AeroSpace` command over the Unix socket.
    ///
    /// # Errors
    ///
    /// Returns an error when the socket cannot be reached, the request or
    /// response cannot be serialized, or `AeroSpace` reports a failure.
    pub fn send_command(
        &self,
        command: &str,
        args: &[&str],
    ) -> Result<AerospaceResponse, AerospaceError> {
        <Self as AerospaceClient>::send_command(self, command, args)
    }
}

impl AerospaceClient for AerospaceIpcClient {
    fn send_command(
        &self,
        command: &str,
        args: &[&str],
    ) -> Result<AerospaceResponse, AerospaceError> {
        let request = AerospaceRequest::from_env(command, args);
        let request = serde_json::to_vec(&request)?;
        let response = {
            let mut stream = self.stream.lock()?;
            if stream.is_none() {
                *stream = Some(connect_socket(&self.socket_path)?);
            }

            let Some(active_stream) = stream.as_mut() else {
                return Err(AerospaceError::ClientState);
            };
            match send_request(active_stream, &request) {
                Ok(response) => {
                    drop(stream);
                    response
                }
                Err(error) => {
                    *stream = None;
                    drop(stream);
                    return Err(error);
                }
            }
        };

        response.ensure_success(command, args)
    }
}

/// Build the `AeroSpace` command used when a workspace item is clicked.
///
/// # Errors
///
/// Returns an error when the workspace is empty or contains control characters.
pub fn focus_workspace_args(workspace: &str) -> Result<[String; 2], AerospaceError> {
    validate_workspace(workspace)?;
    Ok([String::from("workspace"), String::from(workspace)])
}

/// Focus an `AeroSpace` workspace.
///
/// # Errors
///
/// Returns an error when the workspace is invalid, the socket cannot be
/// reached, or `AeroSpace` rejects the command.
pub fn focus_workspace(workspace: &str) -> Result<(), AerospaceError> {
    let aerospace = AerospaceIpcClient::from_env()?;
    focus_workspace_with_client(workspace, &aerospace)
}

/// Focus an `AeroSpace` workspace through an explicit IPC client.
///
/// # Errors
///
/// Returns an error when the workspace is invalid, the socket cannot be
/// reached, or `AeroSpace` rejects the command.
pub fn focus_workspace_with_client<A>(workspace: &str, aerospace: &A) -> Result<(), AerospaceError>
where
    A: AerospaceClient,
{
    let args = focus_workspace_args(workspace)?;
    aerospace.send_command("workspace", &[args[1].as_str()])?;
    Ok(())
}

/// Read the current workspace snapshot.
///
/// # Errors
///
/// Returns an error when the `AeroSpace` IPC transport fails.
pub fn read_workspace_snapshot(
    focused_workspace: Option<&str>,
) -> Result<WorkspaceSnapshot, AerospaceError> {
    let aerospace = AerospaceIpcClient::from_env()?;
    read_workspace_snapshot_with_client(focused_workspace, &aerospace)
}

/// Read the current workspace snapshot with an explicit client.
///
/// # Errors
///
/// Returns an error when the `AeroSpace` IPC transport fails.
pub fn read_workspace_snapshot_with_client<A>(
    focused_workspace: Option<&str>,
    aerospace: &A,
) -> Result<WorkspaceSnapshot, AerospaceError>
where
    A: AerospaceClient,
{
    let active = match focused_workspace.filter(|workspace| !workspace.trim().is_empty()) {
        Some(workspace) => Some(String::from(workspace)),
        None => read_focused_workspace(aerospace)?,
    };
    let occupied = read_occupied_workspaces(aerospace)?.into_iter().collect();
    Ok(WorkspaceSnapshot { active, occupied })
}

/// Read the current `AeroSpace` mode.
///
/// # Errors
///
/// Returns an error when the `AeroSpace` IPC transport fails.
pub fn read_mode() -> Result<Option<String>, AerospaceError> {
    let aerospace = AerospaceIpcClient::from_env()?;
    read_mode_with_client(&aerospace)
}

/// Read the current `AeroSpace` mode with an explicit client.
///
/// # Errors
///
/// Returns an error when the `AeroSpace` IPC transport fails.
pub fn read_mode_with_client<A>(aerospace: &A) -> Result<Option<String>, AerospaceError>
where
    A: AerospaceClient,
{
    Ok(stdout_optional(aerospace, "list-modes", &["--current"])?
        .as_deref()
        .and_then(first_non_empty_line)
        .map(String::from))
}

/// Read focused window layout info for status rendering.
///
/// # Errors
///
/// Returns an error when the `AeroSpace` IPC transport fails.
pub fn read_layout() -> Result<Option<String>, AerospaceError> {
    let aerospace = AerospaceIpcClient::from_env()?;
    read_layout_with_client(&aerospace)
}

/// Read focused window layout info with an explicit client.
///
/// # Errors
///
/// Returns an error when the `AeroSpace` IPC transport fails.
pub fn read_layout_with_client<A>(aerospace: &A) -> Result<Option<String>, AerospaceError>
where
    A: AerospaceClient,
{
    Ok(stdout_optional(
        aerospace,
        "list-windows",
        &[
            "--focused",
            "--format",
            "%{window-is-fullscreen}|%{window-layout}",
        ],
    )?
    .as_deref()
    .and_then(first_non_empty_line)
    .map(String::from))
}

/// Return stdout for an `AeroSpace` command.
///
/// # Errors
///
/// Returns an error when the IPC transport fails or `AeroSpace` rejects the
/// command.
pub fn stdout_optional<A>(
    aerospace: &A,
    command: &str,
    args: &[&str],
) -> Result<Option<String>, AerospaceError>
where
    A: AerospaceClient,
{
    Ok(Some(aerospace.send_command(command, args)?.stdout))
}

impl<T> From<PoisonError<T>> for AerospaceError {
    fn from(_error: PoisonError<T>) -> Self {
        Self::ClientState
    }
}

#[derive(Debug, Clone, Serialize)]
struct AerospaceRequest {
    command: &'static str,
    args: Vec<String>,
    stdin: &'static str,
    #[serde(rename = "windowId")]
    window_id: Option<u64>,
    workspace: Option<String>,
}

impl AerospaceRequest {
    fn from_env(command: &str, args: &[&str]) -> Self {
        let mut full_args = Vec::with_capacity(args.len() + 1);
        full_args.push(String::from(command));
        full_args.extend(args.iter().map(|arg| String::from(*arg)));
        let window_id = env::var(AEROSPACE_WINDOW_ID_ENV)
            .ok()
            .and_then(|value| value.parse::<u64>().ok());
        let workspace = env::var(AEROSPACE_WORKSPACE_ENV).ok();
        Self {
            command: "",
            args: full_args,
            stdin: "",
            window_id,
            workspace,
        }
    }
}

fn connect_socket(socket_path: &Path) -> Result<UnixStream, AerospaceError> {
    let stream = UnixStream::connect(socket_path)?;
    stream.set_read_timeout(Some(Duration::from_millis(1_000)))?;
    Ok(stream)
}

fn send_request(
    stream: &mut UnixStream,
    request: &[u8],
) -> Result<AerospaceResponse, AerospaceError> {
    stream.write_all(request)?;

    let mut response = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        response.extend_from_slice(&buffer[..read]);

        let mut decoder = serde_json::Deserializer::from_slice(&response);
        match AerospaceResponse::deserialize(&mut decoder) {
            Ok(response) => return Ok(response),
            Err(error) if error.is_eof() => {}
            Err(error) => return Err(AerospaceError::Json(error)),
        }
    }

    let mut decoder = serde_json::Deserializer::from_slice(&response);
    Ok(AerospaceResponse::deserialize(&mut decoder)?)
}

fn validate_workspace(workspace: &str) -> Result<(), AerospaceError> {
    if workspace.trim().is_empty() {
        return Err(AerospaceError::InvalidWorkspace {
            reason: "must not be empty",
        });
    }

    if workspace.chars().any(char::is_control) {
        return Err(AerospaceError::InvalidWorkspace {
            reason: "must not contain control characters",
        });
    }

    Ok(())
}

fn read_focused_workspace<A>(aerospace: &A) -> Result<Option<String>, AerospaceError>
where
    A: AerospaceClient,
{
    let Some(output) = stdout_optional(aerospace, "list-workspaces", &["--focused"])? else {
        return Ok(None);
    };
    Ok(first_non_empty_line(&output).map(String::from))
}

fn read_occupied_workspaces<A>(aerospace: &A) -> Result<BTreeSet<String>, AerospaceError>
where
    A: AerospaceClient,
{
    let Some(output) = stdout_optional(
        aerospace,
        "list-windows",
        &["--all", "--format", "%{workspace}"],
    )?
    else {
        return Ok(BTreeSet::new());
    };

    Ok(output
        .lines()
        .map(str::trim)
        .filter(|workspace| !workspace.is_empty())
        .map(String::from)
        .collect())
}

fn first_non_empty_line(output: &str) -> Option<&str> {
    output.lines().map(str::trim).find(|line| !line.is_empty())
}

fn resolve_socket_path() -> Result<PathBuf, AerospaceError> {
    if let Some(path) = env::var_os(AEROSPACE_SOCKET_ENV).filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }

    let user = env::var("USER").map_err(|_err| AerospaceError::MissingSocketUser)?;
    Ok(PathBuf::from(format!("/tmp/bobko.aerospace-{user}.sock")))
}

fn command_line(command: &str, args: &[&str]) -> String {
    let mut line = String::from(command);
    for arg in args {
        line.push(' ');
        line.push_str(arg);
    }
    line
}
