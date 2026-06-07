use std::{
    cell::RefCell,
    collections::BTreeMap,
    io::{Read, Write},
    os::unix::net::UnixListener,
    path::PathBuf,
    thread,
};

use serde_json::json;
use spindle_aerospace_extension::{
    AerospaceClient, AerospaceError, AerospaceIpcClient, AerospaceResponse, focus_workspace_args,
    read_layout_with_client, read_mode_with_client, read_workspace_snapshot_with_client,
};

#[derive(Default)]
struct FakeAerospace {
    responses: BTreeMap<String, String>,
    commands: RefCell<Vec<Vec<String>>>,
}

impl FakeAerospace {
    fn new() -> Self {
        Self::default()
    }

    fn with_response(mut self, command: &str, stdout: &str) -> Self {
        self.responses
            .insert(String::from(command), String::from(stdout));
        self
    }

    fn commands(&self) -> Vec<Vec<String>> {
        self.commands.borrow().clone()
    }
}

impl AerospaceClient for FakeAerospace {
    fn send_command(
        &self,
        command: &str,
        args: &[&str],
    ) -> Result<AerospaceResponse, AerospaceError> {
        let mut recorded = Vec::with_capacity(args.len() + 1);
        recorded.push(String::from(command));
        recorded.extend(args.iter().map(|arg| String::from(*arg)));
        self.commands.borrow_mut().push(recorded);

        Ok(AerospaceResponse::success(
            self.responses.get(command).map_or("", String::as_str),
        ))
    }
}

struct FailingAerospace {
    command: &'static str,
}

impl AerospaceClient for FailingAerospace {
    fn send_command(
        &self,
        command: &str,
        args: &[&str],
    ) -> Result<AerospaceResponse, AerospaceError> {
        if command == self.command {
            return Err(AerospaceError::CommandFailed {
                command: command.to_owned(),
                exit_code: 1,
                stderr: String::from("AeroSpace failed"),
            });
        }

        Ok(AerospaceResponse::success(args.join("\n")))
    }
}

fn test_socket_path() -> anyhow::Result<(tempfile::TempDir, PathBuf)> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("aerospace.sock");
    Ok((dir, path))
}

#[test]
fn aerospace_client_sends_json_over_unix_socket() -> anyhow::Result<()> {
    let (_dir, socket_path) = test_socket_path()?;
    let listener = UnixListener::bind(&socket_path)?;
    let handle = thread::spawn(move || -> anyhow::Result<String> {
        let (mut stream, _address) = listener.accept()?;
        let mut buffer = [0_u8; 4096];
        let read = stream.read(&mut buffer)?;
        let request = String::from_utf8(buffer[..read].to_vec())?;
        stream.write_all(
            serde_json::to_string(&json!({
                "serverVersionAndHash": "0.20.0-Beta abc",
                "stdout": "main\n",
                "stderr": "",
                "exitCode": 0
            }))?
            .as_bytes(),
        )?;
        Ok(request)
    });

    let client = AerospaceIpcClient::new(socket_path.clone());
    let response = client.send_command("list-modes", &["--current"])?;
    let request = handle
        .join()
        .map_err(|_payload| anyhow::anyhow!("server thread panicked"))??;

    assert_eq!(response.stdout(), "main\n");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&request)?,
        json!({
            "command": "",
            "args": ["list-modes", "--current"],
            "stdin": "",
            "windowId": null,
            "workspace": null
        })
    );

    Ok(())
}

#[test]
fn aerospace_client_uses_first_socket_response() -> anyhow::Result<()> {
    let (_dir, socket_path) = test_socket_path()?;
    let listener = UnixListener::bind(&socket_path)?;
    let handle = thread::spawn(move || -> anyhow::Result<()> {
        let (mut stream, _address) = listener.accept()?;
        let mut buffer = [0_u8; 4096];
        let _read = stream.read(&mut buffer)?;
        let success = serde_json::to_string(&json!({
            "serverVersionAndHash": "0.20.0-Beta abc",
            "stdout": "1",
            "stderr": "",
            "exitCode": 0
        }))?;
        let empty_request = serde_json::to_string(&json!({
            "serverVersionAndHash": "0.20.0-Beta abc",
            "stdout": "",
            "stderr": "Empty request",
            "exitCode": 1
        }))?;
        stream.write_all(success.as_bytes())?;
        stream.write_all(empty_request.as_bytes())?;
        Ok(())
    });

    let client = AerospaceIpcClient::new(socket_path.clone());
    let response = client.send_command("list-workspaces", &["--focused"])?;

    assert_eq!(response.stdout(), "1");
    handle
        .join()
        .map_err(|_payload| anyhow::anyhow!("server thread panicked"))??;
    Ok(())
}

#[test]
fn aerospace_client_reuses_socket_connection() -> anyhow::Result<()> {
    let (_dir, socket_path) = test_socket_path()?;
    let listener = UnixListener::bind(&socket_path)?;
    let handle = thread::spawn(move || -> anyhow::Result<Vec<String>> {
        let (mut stream, _address) = listener.accept()?;
        let mut requests = Vec::new();
        for stdout in ["2", "1\n2\n"] {
            let mut buffer = [0_u8; 4096];
            let read = stream.read(&mut buffer)?;
            requests.push(String::from_utf8(buffer[..read].to_vec())?);
            stream.write_all(
                serde_json::to_string(&json!({
                    "serverVersionAndHash": "0.20.0-Beta abc",
                    "stdout": stdout,
                    "stderr": "",
                    "exitCode": 0
                }))?
                .as_bytes(),
            )?;
        }
        Ok(requests)
    });

    let client = AerospaceIpcClient::new(socket_path.clone());
    let focused = client.send_command("list-workspaces", &["--focused"])?;
    let windows = client.send_command("list-windows", &["--all", "--format", "%{workspace}"])?;
    let requests = handle
        .join()
        .map_err(|_payload| anyhow::anyhow!("server thread panicked"))??;

    assert_eq!(focused.stdout(), "2");
    assert_eq!(windows.stdout(), "1\n2\n");
    assert_eq!(requests.len(), 2);
    Ok(())
}

#[test]
fn focus_workspace_args_builds_aerospace_command() -> anyhow::Result<()> {
    let args = focus_workspace_args("10")?;

    assert_eq!(args, [String::from("workspace"), String::from("10")]);
    Ok(())
}

#[test]
fn workspace_snapshot_reads_aerospace_state() -> anyhow::Result<()> {
    let aerospace = FakeAerospace::new()
        .with_response("list-workspaces", "2\n")
        .with_response("list-windows", "1\n2\n2\n");

    let snapshot = read_workspace_snapshot_with_client(None, &aerospace)?;

    assert_eq!(snapshot.active, Some(String::from("2")));
    assert_eq!(
        snapshot.occupied,
        vec![String::from("1"), String::from("2")]
    );
    assert_eq!(
        aerospace.commands(),
        vec![
            vec!["list-workspaces", "--focused"],
            vec!["list-windows", "--all", "--format", "%{workspace}"],
        ]
    );
    Ok(())
}

#[test]
fn workspace_snapshot_prefers_event_workspace() -> anyhow::Result<()> {
    let aerospace = FakeAerospace::new().with_response("list-windows", "1\n2\n");

    let snapshot = read_workspace_snapshot_with_client(Some("3"), &aerospace)?;

    assert_eq!(snapshot.active, Some(String::from("3")));
    assert_eq!(
        aerospace.commands(),
        vec![vec!["list-windows", "--all", "--format", "%{workspace}"]]
    );
    Ok(())
}

#[test]
fn workspace_snapshot_propagates_required_command_failure() {
    let aerospace = FailingAerospace {
        command: "list-windows",
    };

    let error = read_workspace_snapshot_with_client(Some("3"), &aerospace)
        .err()
        .map_or_else(String::new, |error| error.to_string());

    assert!(error.contains("aerospace command failed: list-windows exited 1"));
}

#[test]
fn mode_and_layout_snapshot_read_aerospace_state() -> anyhow::Result<()> {
    let aerospace = FakeAerospace::new()
        .with_response("list-modes", "service\n")
        .with_response("list-windows", "false|h_tiles\n");

    assert_eq!(
        read_mode_with_client(&aerospace)?,
        Some(String::from("service"))
    );
    assert_eq!(
        read_layout_with_client(&aerospace)?,
        Some(String::from("false|h_tiles"))
    );
    Ok(())
}
