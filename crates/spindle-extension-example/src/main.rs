#![warn(clippy::pedantic)]
#![warn(clippy::nursery)]
#![warn(clippy::cargo)]

use anyhow::Result;
use spindle_extension_sdk::{
    ActionOutput, ActionOutputEvent, ExtensionRegistration, ExtensionSdkError, RegistrationAction,
    serve_stdio_jsonl_host,
};

fn main() -> Result<()> {
    let registration = ExtensionRegistration::new()
        .emit("example.item.changed")
        .produce("example.item.echoed")
        .capability("example.read")
        .action(
            "example.echo",
            RegistrationAction::new().capability("example.read"),
        );

    serve_stdio_jsonl_host(
        &registration,
        |context| -> Result<ActionOutput, ExtensionSdkError> {
            let args = context.args::<serde_json::Value>()?;
            Ok(ActionOutput::event(
                ActionOutputEvent::new("example.item.echoed").with_data(args),
            ))
        },
    )?;
    Ok(())
}
