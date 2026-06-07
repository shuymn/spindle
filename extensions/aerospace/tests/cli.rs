use std::{
    io::{Read, Write},
    os::unix::net::UnixListener,
    path::PathBuf,
    process::Command,
    thread,
};

use serde_json::json;

fn test_socket_path() -> anyhow::Result<(tempfile::TempDir, PathBuf)> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("aerospace.sock");
    Ok((dir, path))
}

#[test]
fn cli_focus_workspace_uses_aerospace_socket() -> anyhow::Result<()> {
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
                "stdout": "",
                "stderr": "",
                "exitCode": 0
            }))?
            .as_bytes(),
        )?;
        Ok(request)
    });

    let focus = Command::new(env!("CARGO_BIN_EXE_spindle-aerospace"))
        .arg("focus-workspace")
        .arg("2")
        .env("AEROSPACESOCK", &socket_path)
        .output()?;

    assert!(
        focus.status.success(),
        "focus-workspace failed: {}",
        String::from_utf8_lossy(&focus.stderr)
    );
    let request = handle
        .join()
        .map_err(|_payload| anyhow::anyhow!("server thread panicked"))??;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&request)?,
        json!({
            "command": "",
            "args": ["workspace", "2"],
            "stdin": "",
            "windowId": null,
            "workspace": null
        })
    );

    Ok(())
}

#[test]
fn cli_register_ignores_unrelated_spindle_env() -> anyhow::Result<()> {
    let output = Command::new(env!("CARGO_BIN_EXE_spindle-aerospace"))
        .arg("register")
        .env("SPINDLE_UNUSED_TEST_ENV", "{not json")
        .output()?;

    assert!(
        output.status.success(),
        "register failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains(r#""aerospace.workspace.snapshot""#));
    Ok(())
}
