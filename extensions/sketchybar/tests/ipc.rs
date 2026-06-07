use std::process::Command;

use spindle_sketchybar_extension::SketchybarMessage;

#[test]
fn sketchybar_message_matches_cli_wire_format() {
    let message = SketchybarMessage::from_args(&["--set", "status.item", "label=N"]);

    assert_eq!(message.as_bytes(), b"--set\0status.item\0label=N\0\0");
}

#[test]
fn cli_register_ignores_unrelated_spindle_env() -> anyhow::Result<()> {
    let output = Command::new(env!("CARGO_BIN_EXE_spindle-sketchybar"))
        .arg("register")
        .env("SPINDLE_UNUSED_TEST_ENV", "{not json")
        .output()?;

    assert!(
        output.status.success(),
        "register failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains(r#""sketchybar.message.send""#));
    Ok(())
}
