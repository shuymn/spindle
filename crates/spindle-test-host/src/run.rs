use std::{
    fs,
    io::{self, BufRead, BufReader, BufWriter, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process, thread,
    time::Duration,
};

use serde_json::{Map, Value, json};
use spindle_extension_sdk::{ActionOutput, ActionOutputEvent, HostResponse};

use crate::config::{
    CountEventTemplate, FailResponse, InvokeEffect, InvokeRule, RegisterResponse, ResponseTemplate,
    StartupAction, TestHostConfig,
};

const DEFAULT_OVERSIZED_BYTES: usize = 1_048_577;

/// Resolve the JSON config path for a copied test-host executable.
#[must_use]
pub fn config_path_for_executable(executable: &Path) -> PathBuf {
    fs::canonicalize(executable)
        .unwrap_or_else(|_| executable.to_path_buf())
        .with_extension("json")
}

/// Load config from `{executable}.json`.
///
/// # Errors
///
/// Returns an error when the config file cannot be read or parsed.
pub fn load_config(executable: &Path) -> Result<TestHostConfig, io::Error> {
    let mut candidates = Vec::new();
    if let Ok(path) = std::env::var("SPINDLE_TEST_HOST_CONFIG") {
        candidates.push(PathBuf::from(path));
    }
    candidates.push(executable.with_extension("json"));
    if let Ok(canonical) = fs::canonicalize(executable) {
        let canonical_config = canonical.with_extension("json");
        if !candidates.iter().any(|path| path == &canonical_config) {
            candidates.push(canonical_config);
        }
    }

    for config_path in candidates {
        let Ok(contents) = fs::read_to_string(&config_path) else {
            continue;
        };
        return serde_json::from_str(&contents).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "invalid test host config at {}: {error}",
                    config_path.display()
                ),
            )
        });
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!(
            "missing test host config for executable {}",
            executable.display()
        ),
    ))
}

/// Run the configured stdio JSONL host until stdin closes or shutdown is received.
///
/// # Errors
///
/// Returns an error when startup actions or I/O fail.
pub fn run_stdio_host(config: &TestHostConfig, executable: &Path) -> Result<(), io::Error> {
    let config_path = config_path_for_executable(executable);
    let mut state = HostState::new(&config_path, &config.startup);
    run_startup(&config.startup)?;
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = BufWriter::new(stdout.lock());
    let mut line = String::new();
    while reader.read_line(&mut line)? != 0 {
        if line.trim().is_empty() {
            line.clear();
            continue;
        }
        if line.contains(r#""type":"register""#) {
            handle_register_request(config, executable, &mut writer)?;
        } else if line.contains(r#""type":"invoke""#) {
            handle_invoke_request(config, &mut state, &line, &mut writer)?;
        } else if line.contains(r#""type":"shutdown""#) {
            write_response(&mut writer, &HostResponse::Shutdown)?;
            if config.shutdown.sleep_ms > 0 {
                thread::sleep(Duration::from_millis(config.shutdown.sleep_ms));
            }
            break;
        } else {
            write_response(
                &mut writer,
                &HostResponse::Error {
                    error: String::from("unexpected request"),
                },
            )?;
        }
        line.clear();
    }
    Ok(())
}

#[derive(Debug)]
struct HostState {
    session_marker: PathBuf,
    invoke_count: u64,
    count: u64,
}

impl HostState {
    fn new(config_path: &Path, startup: &StartupAction) -> Self {
        let session_marker = startup
            .session_marker
            .clone()
            .unwrap_or_else(|| TestHostConfig::session_marker_path(config_path));
        Self {
            session_marker,
            invoke_count: 0,
            count: 0,
        }
    }

    fn session_index(&self) -> u64 {
        if self.session_marker.exists() { 2 } else { 1 }
    }

    fn mark_session_seen(&self) -> Result<(), io::Error> {
        if let Some(parent) = self.session_marker.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&self.session_marker, b"1")
    }
}

fn run_startup(startup: &StartupAction) -> Result<(), io::Error> {
    if let Some(code) = startup.exit_code {
        process::exit(code);
    }
    if startup.sleep_ms > 0 {
        thread::sleep(Duration::from_millis(startup.sleep_ms));
    }
    if let Some(path) = &startup.touch {
        touch_file(path)?;
    }
    if let Some(path) = &startup.write_pid {
        write_pid(path, false)?;
    }
    if let Some(path) = &startup.append_pid {
        write_pid(path, true)?;
    }
    Ok(())
}

fn handle_register_request(
    config: &TestHostConfig,
    executable: &Path,
    writer: &mut BufWriter<io::StdoutLock<'_>>,
) -> Result<(), io::Error> {
    match &config.register_response {
        RegisterResponse::Registration => {
            if config.startup.mutate_on_register {
                mutate_to_exit_stub(executable)?;
            }
            write_response(
                writer,
                &HostResponse::Registration {
                    registration: config.registration.clone(),
                },
            )
        }
        RegisterResponse::InvalidJson { line } => {
            write_raw_line(line)?;
            park_forever();
        }
        RegisterResponse::Oversized { bytes } => {
            write_oversized_line(*bytes)?;
            park_forever();
        }
        RegisterResponse::Sleep { ms } => {
            thread::sleep(Duration::from_millis(*ms));
            Ok(())
        }
    }
}

fn handle_invoke_request(
    config: &TestHostConfig,
    state: &mut HostState,
    request_line: &str,
    writer: &mut BufWriter<io::StdoutLock<'_>>,
) -> Result<(), io::Error> {
    state.invoke_count = state.invoke_count.saturating_add(1);
    let invoke_index = state.invoke_count;
    let session_index = config
        .invoke_rules
        .iter()
        .any(|rule| rule.when_session_index.is_some())
        .then(|| state.session_index());
    let effect = config
        .invoke_rules
        .iter()
        .find(|rule| rule_matches(rule, request_line, invoke_index, session_index))
        .map_or(&config.invoke_default, |rule| &rule.effect);
    let response = apply_invoke_effect(effect, &mut *state)?;
    if let Some(response) = response {
        write_response(writer, &response)?;
    }
    Ok(())
}

fn rule_matches(
    rule: &InvokeRule,
    request_line: &str,
    invoke_index: u64,
    session_index: Option<u64>,
) -> bool {
    if let Some(expected) = &rule.when_contains
        && !request_line.contains(expected)
    {
        return false;
    }
    if let Some(expected) = rule.when_invoke_index
        && invoke_index != expected
    {
        return false;
    }
    if let Some(expected) = rule.when_session_index
        && session_index != Some(expected)
    {
        return false;
    }
    true
}

fn apply_invoke_effect(
    effect: &InvokeEffect,
    state: &mut HostState,
) -> Result<Option<HostResponse>, io::Error> {
    if let Some(path) = &effect.touch {
        if path.exists() {
            if let Some(other) = &effect.touch_else {
                touch_file(other)?;
            }
        } else {
            touch_file(path)?;
        }
    }
    if effect.sleep_ms > 0 {
        thread::sleep(Duration::from_millis(effect.sleep_ms));
    }
    if let Some(path) = &effect.remove_file {
        let _ = fs::remove_file(path);
    }
    match &effect.response {
        ResponseTemplate::Failure(failure) => apply_failure(failure, &*state),
        ResponseTemplate::Error { error } => Ok(Some(HostResponse::Error {
            error: error.clone(),
        })),
        ResponseTemplate::RegistrationSurface { registration } => {
            state.mark_session_seen()?;
            Ok(Some(HostResponse::Registration {
                registration: registration.clone(),
            }))
        }
        ResponseTemplate::Output { output } => Ok(Some(HostResponse::ActionOutput {
            output: ActionOutput::from_json_value(output.clone())?,
        })),
        ResponseTemplate::EmptyOutput => {
            if let Some(template) = &effect.count_event {
                state.count = state.count.saturating_add(1);
                return Ok(Some(HostResponse::ActionOutput {
                    output: counting_output(template, state.count),
                }));
            }
            if let Some(marker) = &effect.marker {
                return Ok(Some(HostResponse::ActionOutput {
                    output: marker_output(marker),
                }));
            }
            Ok(Some(HostResponse::ActionOutput {
                output: ActionOutput::empty(),
            }))
        }
    }
}

fn apply_failure(
    failure: &FailResponse,
    state: &HostState,
) -> Result<Option<HostResponse>, io::Error> {
    state.mark_session_seen()?;
    match failure {
        FailResponse::InvalidJson { line } => {
            write_raw_line(line)?;
            process::exit(0);
        }
        FailResponse::Oversized { bytes } => {
            write_oversized_line(*bytes)?;
            park_forever();
        }
        FailResponse::NonUtf8 => {
            let stdout = io::stdout();
            let mut writer = BufWriter::new(stdout.lock());
            writer.write_all(&[0xff, b'\n'])?;
            writer.flush()?;
            park_forever();
        }
        FailResponse::Sleep { ms } => {
            thread::sleep(Duration::from_millis(*ms));
            Ok(Some(HostResponse::Error {
                error: String::from("invoke sleep"),
            }))
        }
        FailResponse::Exit { code } => process::exit(*code),
    }
}

fn counting_output(template: &CountEventTemplate, count: u64) -> ActionOutput {
    let mut data = Map::new();
    data.insert(template.field.clone(), Value::Number(count.into()));
    ActionOutput::event(
        ActionOutputEvent::new(template.event_type.clone()).with_data(Value::Object(data)),
    )
}

fn marker_output(marker: &str) -> ActionOutput {
    ActionOutput::event(
        ActionOutputEvent::new(String::from("test.rendered"))
            .with_data(json!({ "marker": marker })),
    )
}

fn write_response(
    writer: &mut BufWriter<io::StdoutLock<'_>>,
    response: &HostResponse,
) -> Result<(), io::Error> {
    serde_json::to_writer(&mut *writer, response)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    writeln!(writer)?;
    writer.flush()
}

#[allow(clippy::infinite_loop)]
fn park_forever() -> ! {
    loop {
        thread::sleep(Duration::from_secs(3600));
    }
}

fn write_raw_line(line: &str) -> Result<(), io::Error> {
    let stdout = io::stdout();
    let mut writer = BufWriter::new(stdout.lock());
    writeln!(writer, "{line}")?;
    writer.flush()
}

fn write_oversized_line(bytes: usize) -> Result<(), io::Error> {
    let size = if bytes == 0 {
        DEFAULT_OVERSIZED_BYTES
    } else {
        bytes
    };
    let stdout = io::stdout();
    let mut writer = BufWriter::new(stdout.lock());
    writer.write_all(&vec![b'a'; size])?;
    writeln!(writer)?;
    writer.flush()
}

fn touch_file(path: &Path) -> Result<(), io::Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, b"")
}

fn write_pid(path: &Path, append: bool) -> Result<(), io::Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let pid = process::id().to_string();
    if append {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        writeln!(file, "{pid}")?;
    } else {
        fs::write(path, pid)?;
    }
    Ok(())
}

fn mutate_to_exit_stub(executable: &Path) -> Result<(), io::Error> {
    let replacement = executable.with_extension("replacement");
    fs::write(&replacement, "#!/bin/sh\nexit 42\n")?;
    let mut permissions = fs::metadata(&replacement)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&replacement, permissions)?;
    fs::rename(&replacement, executable)
}

trait ActionOutputJson {
    fn from_json_value(value: Value) -> Result<ActionOutput, io::Error>;
}

impl ActionOutputJson for ActionOutput {
    fn from_json_value(value: Value) -> Result<Self, io::Error> {
        serde_json::from_value(value)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))
    }
}
