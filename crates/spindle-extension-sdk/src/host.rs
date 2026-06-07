use std::{
    fmt::{self, Display},
    io::{self, BufRead, BufReader, BufWriter, Write},
};

use serde::{Deserialize, Serialize};

use crate::{
    ActionContext, ActionInvocation, ActionOutput, ExtensionRegistration, ExtensionSdkError,
};

/// Static handler for one extension action.
#[derive(Clone, Copy)]
pub struct ActionHandler<E> {
    name: &'static str,
    handler: fn(&ActionContext) -> Result<ActionOutput, E>,
}

impl<E> ActionHandler<E> {
    /// Bind one action name to its handler.
    #[must_use]
    pub const fn new(
        name: &'static str,
        handler: fn(&ActionContext) -> Result<ActionOutput, E>,
    ) -> Self {
        Self { name, handler }
    }

    fn invoke(&self, context: &ActionContext) -> Result<ActionOutput, E> {
        (self.handler)(context)
    }
}

enum ActionRouterError<E> {
    MissingAction,
    UnknownAction,
    Handler(E),
}

impl<E: Display> Display for ActionRouterError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingAction => formatter.write_str("missing action"),
            Self::UnknownAction => formatter.write_str("unknown action"),
            Self::Handler(error) => error.fmt(formatter),
        }
    }
}

/// JSONL request accepted by a stdio extension host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum HostRequest {
    /// Return this extension's registered surface.
    Register,
    /// Invoke one installed action.
    Invoke {
        /// Action invocation supplied by spindle.
        invocation: ActionInvocation,
    },
    /// Ask the extension host to terminate cleanly.
    Shutdown,
}

/// JSONL response returned by a stdio extension host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum HostResponse {
    /// Extension surface registration.
    Registration {
        /// Registered surface.
        registration: ExtensionRegistration,
    },
    /// Output from one action invocation.
    ActionOutput {
        /// Structured action output.
        output: ActionOutput,
    },
    /// Host-side action or registration failure.
    Error {
        /// Human-readable error message.
        error: String,
    },
    /// Clean shutdown acknowledgement.
    Shutdown,
}

/// Serve a stdio JSONL extension host.
///
/// # Errors
///
/// Returns an error when stdin cannot be read or stdout cannot be written.
pub fn serve_stdio_jsonl<R, W, F, E>(
    reader: R,
    writer: W,
    registration: &ExtensionRegistration,
    mut handler: F,
) -> Result<(), ExtensionSdkError>
where
    R: BufRead,
    W: Write,
    F: FnMut(&ActionContext) -> Result<ActionOutput, E>,
    E: Display,
{
    let mut reader = reader;
    let mut writer = writer;
    let mut line = String::new();
    while reader.read_line(&mut line)? != 0 {
        if line.trim().is_empty() {
            line.clear();
            continue;
        }
        let request = serde_json::from_str::<HostRequest>(&line).map_err(|source| {
            ExtensionSdkError::Json {
                name: "HostRequest",
                source,
            }
        })?;
        let should_shutdown = matches!(request, HostRequest::Shutdown);
        let response = match request {
            HostRequest::Register => HostResponse::Registration {
                registration: registration.clone(),
            },
            HostRequest::Invoke { invocation } => {
                let context = ActionContext::from_invocation(invocation);
                match handler(&context) {
                    Ok(output) => HostResponse::ActionOutput { output },
                    Err(error) => HostResponse::Error {
                        error: error.to_string(),
                    },
                }
            }
            HostRequest::Shutdown => HostResponse::Shutdown,
        };
        serde_json::to_writer(&mut writer, &response).map_err(|source| {
            ExtensionSdkError::Json {
                name: "HostResponse",
                source,
            }
        })?;
        writeln!(writer)?;
        writer.flush()?;
        if should_shutdown {
            break;
        }
        line.clear();
    }
    Ok(())
}

/// Serve a stdio JSONL extension host on process stdin/stdout.
///
/// # Errors
///
/// Returns an error when stdin cannot be read or stdout cannot be written.
pub fn serve_stdio_jsonl_host<F, E>(
    registration: &ExtensionRegistration,
    handler: F,
) -> Result<(), ExtensionSdkError>
where
    F: FnMut(&ActionContext) -> Result<ActionOutput, E>,
    E: Display,
{
    let stdin = io::stdin();
    let stdout = io::stdout();
    serve_stdio_jsonl(
        BufReader::new(stdin.lock()),
        BufWriter::new(stdout.lock()),
        registration,
        handler,
    )
}

/// Serve a stdio JSONL host from a static action handler table.
///
/// # Errors
///
/// Returns an error when stdin cannot be read or stdout cannot be written.
pub fn serve_stdio_jsonl_actions<E>(
    registration: &ExtensionRegistration,
    handlers: &'static [ActionHandler<E>],
) -> Result<(), ExtensionSdkError>
where
    E: Display,
{
    serve_stdio_jsonl_host(registration, move |context: &ActionContext| {
        let action = context.action().ok_or(ActionRouterError::MissingAction)?;
        let handler = handlers
            .iter()
            .find(|handler| handler.name == action)
            .ok_or(ActionRouterError::UnknownAction)?;
        handler.invoke(context).map_err(ActionRouterError::Handler)
    })
}
