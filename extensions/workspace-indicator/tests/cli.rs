use std::process::Command;

const SURFACE_CONTEXT: &str = r#"{
  "id": "workspace-indicator",
  "events": [
    {
      "type": "sketchybar.message.requested",
      "source_extension": "workspace-indicator"
    }
  ],
  "actions": [
    {
      "name": "sketchybar.message.send",
      "extension": "sketchybar",
      "capabilities": ["sketchybar.ui.write"]
    }
  ],
  "capabilities": ["sketchybar.ui.write"]
}"#;

#[test]
fn cli_uses_registered_surface_before_emitting_message_request() -> anyhow::Result<()> {
    let output = Command::new(env!("CARGO_BIN_EXE_spindle-workspace-indicator"))
        .arg("render-status")
        .arg("--extension-context")
        .arg(SURFACE_CONTEXT)
        .arg("--item")
        .arg("aerospace.mode")
        .arg("--mode")
        .arg("service")
        .output()?;

    assert!(
        output.status.success(),
        "render-status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains(r#""type":"sketchybar.message.requested""#));
    assert!(stdout.contains(r#""args":["--set","aerospace.mode""#));
    Ok(())
}

#[test]
fn cli_rejects_missing_registered_output_action() -> anyhow::Result<()> {
    let output = Command::new(env!("CARGO_BIN_EXE_spindle-workspace-indicator"))
        .arg("render-status")
        .arg("--extension-context")
        .arg(
            r#"{
              "id": "workspace-indicator",
              "events": [
                {
                  "type": "sketchybar.message.requested",
                  "source_extension": "workspace-indicator"
                }
              ],
              "actions": [],
              "capabilities": []
            }"#,
        )
        .arg("--item")
        .arg("aerospace.mode")
        .arg("--mode")
        .arg("service")
        .output()?;

    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)?
            .contains("required action is not available: sketchybar.message.send")
    );
    Ok(())
}

#[test]
fn cli_register_ignores_unrelated_spindle_env() -> anyhow::Result<()> {
    let output = Command::new(env!("CARGO_BIN_EXE_spindle-workspace-indicator"))
        .arg("register")
        .env("SPINDLE_UNUSED_TEST_ENV", "{not json")
        .output()?;

    assert!(
        output.status.success(),
        "register failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains(r#""workspace-indicator.workspaces.render""#));
    Ok(())
}
