//! Minimal stdio JSONL extension host using `spindle-extension-sdk`.
//!
//! Run with:
//!
//! ```text
//! cargo run -p spindle-extension-sdk --example minimal_host
//! ```

use spindle_extension_sdk::{
    ActionOutput, ActionOutputEvent, ExtensionRegistration, ExtensionSdkError, RegistrationAction,
    serve_stdio_jsonl_host,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let registration = ExtensionRegistration::new()
        .produce("example.rendered")
        .action("example.render", RegistrationAction::new());

    serve_stdio_jsonl_host(
        &registration,
        |_context| -> Result<ActionOutput, ExtensionSdkError> {
            Ok(ActionOutput::event(ActionOutputEvent::new(
                "example.rendered",
            )))
        },
    )?;
    Ok(())
}
