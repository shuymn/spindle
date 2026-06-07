use std::{
    fs,
    io::{BufRead, BufReader, BufWriter, Write},
    net::Shutdown,
    os::unix::{
        fs::FileTypeExt,
        net::{UnixListener, UnixStream},
    },
    path::Path,
    sync::{Arc, Mutex},
    thread,
};

use crate::{
    EventLog, ExtensionRegistry, ExtensionRuntimeHost, HubRequest, HubResponse, SpindleError,
    execute_request,
};

/// Serve spindle JSONL requests on a Unix domain socket.
///
/// # Errors
///
/// Returns an error if the socket cannot be prepared, bound, or used.
pub fn serve(socket_path: &Path, log: &EventLog) -> Result<(), SpindleError> {
    prepare_socket(socket_path)?;
    let listener = UnixListener::bind(socket_path)?;
    let state = ServerState::new(log.clone());

    for stream in listener.incoming() {
        spawn_stream_handler(stream?, state.clone());
    }

    Ok(())
}

/// Send one request to a running spindle server.
///
/// # Errors
///
/// Returns an error if the socket cannot be reached, the request cannot be
/// serialized, or the response cannot be parsed.
pub fn send_request(socket_path: &Path, request: &HubRequest) -> Result<HubResponse, SpindleError> {
    let mut stream = UnixStream::connect(socket_path)?;
    serde_json::to_writer(&mut stream, request)?;
    writeln!(stream)?;
    stream.shutdown(Shutdown::Write)?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    Ok(serde_json::from_str(&line)?)
}

#[derive(Clone, Debug)]
struct ServerState {
    log: EventLog,
    registry: ExtensionRegistry,
    runtime: Arc<Mutex<ExtensionRuntimeHost>>,
}

impl ServerState {
    fn new(log: EventLog) -> Self {
        Self {
            registry: registry_for_log(&log),
            log,
            runtime: Arc::new(Mutex::new(ExtensionRuntimeHost::new())),
        }
    }

    fn execute(&self, request: HubRequest) -> Result<serde_json::Value, SpindleError> {
        if !request_needs_runtime(&request) {
            let mut runtime = ExtensionRuntimeHost::new();
            return execute_request(request, &self.log, &self.registry, &mut runtime);
        }

        let mut runtime = self
            .runtime
            .lock()
            .map_err(|_error| SpindleError::InvalidField {
                field: "runtime",
                reason: "lock poisoned",
            })?;
        execute_request(request, &self.log, &self.registry, &mut runtime)
    }
}

const fn request_needs_runtime(request: &HubRequest) -> bool {
    matches!(
        request,
        HubRequest::Emit { .. } | HubRequest::Invoke { .. } | HubRequest::RegisterExtension { .. }
    )
}

fn handle_stream(stream: UnixStream, state: &ServerState) -> Result<(), SpindleError> {
    let reader_stream = stream.try_clone()?;
    let mut reader = BufReader::new(reader_stream);
    let mut writer = BufWriter::new(stream);
    let mut line = String::new();

    while reader.read_line(&mut line)? != 0 {
        let response = match serde_json::from_str::<HubRequest>(&line) {
            Ok(request) => HubResponse::from(state.execute(request)),
            Err(error) => HubResponse::Error {
                error: error.to_string(),
            },
        };
        serde_json::to_writer(&mut writer, &response)?;
        writeln!(writer)?;
        writer.flush()?;
        line.clear();
    }

    Ok(())
}

fn spawn_stream_handler(stream: UnixStream, state: ServerState) {
    let _handler = thread::spawn(move || {
        let _result = handle_stream(stream, &state);
    });
}

fn registry_for_log(log: &EventLog) -> ExtensionRegistry {
    ExtensionRegistry::in_dir(log.state_dir())
}

fn prepare_socket(socket_path: &Path) -> Result<(), SpindleError> {
    if let Some(parent) = socket_path.parent() {
        fs::create_dir_all(parent)?;
    }

    if !socket_path.exists() {
        return Ok(());
    }

    let metadata = fs::metadata(socket_path)?;
    if !metadata.file_type().is_socket() {
        return Err(SpindleError::InvalidField {
            field: "socket",
            reason: "path already exists and is not a socket",
        });
    }

    if UnixStream::connect(socket_path).is_ok() {
        return Err(SpindleError::SocketInUse {
            path: socket_path.to_path_buf(),
        });
    }

    fs::remove_file(socket_path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, thread};

    use serde_json::json;

    use super::*;
    use crate::{EventFilter, HubRequest};

    #[test]
    fn request_runtime_need_is_limited_to_dispatch_and_registration() {
        assert!(request_needs_runtime(&HubRequest::Emit {
            kind: String::from("test.changed"),
            source: String::from("unit"),
            subject: None,
            data: json!({})
        }));
        assert!(request_needs_runtime(&HubRequest::Invoke {
            action: String::from("test.render"),
            source: String::from("unit"),
            capabilities: Vec::new(),
            args: json!({})
        }));
        assert!(request_needs_runtime(&HubRequest::RegisterExtension {
            manifest: "extension.json".into()
        }));
        assert!(!request_needs_runtime(&HubRequest::QueryEvents {
            kind: None,
            source: None,
            limit: None
        }));
        assert!(!request_needs_runtime(&HubRequest::ValidateExtension {
            manifest: "extension.json".into()
        }));
        assert!(!request_needs_runtime(&HubRequest::ListExtensions));
    }

    #[test]
    fn send_request_round_trips_over_unix_socket() -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        let socket = dir.join("spindle.sock");
        let log = EventLog::in_dir(&dir);
        let server_state = ServerState::new(log.clone());
        let listener = UnixListener::bind(&socket)?;

        let handle = thread::spawn(move || -> Result<(), SpindleError> {
            let (stream, _address) = listener.accept()?;
            handle_stream(stream, &server_state)
        });

        let response = send_request(
            &socket,
            &HubRequest::Emit {
                kind: String::from("agent.status.changed"),
                source: String::from("pi"),
                subject: None,
                data: json!({ "state": "working" }),
            },
        )?;

        let server_result = handle
            .join()
            .map_err(|_payload| SpindleError::InvalidField {
                field: "server_thread",
                reason: "panicked",
            })?;
        server_result?;

        assert!(matches!(response, HubResponse::Ok { .. }));
        assert_eq!(log.read(&EventFilter::default())?.len(), 1);
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn daemon_reuses_stdio_hosts_across_client_connections() -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        crate::store::tests_support::write_capability_policy(
            &dir,
            r#"{"direct":{"test-client":["test.write"]},"routes":{}}"#,
        )?;
        let host = dir.join("host.sh");
        crate::store::tests_support::write_executable(
            &host,
            r#"#!/bin/sh
count=0
while IFS= read -r line; do
  case "$line" in
    *'"type":"register"'*)
      printf '%s\n' '{"type":"registration","registration":{"capabilities":["test.write"],"actions":{"test.render":{"capabilities":["test.write"]}}}}'
      ;;
    *'"type":"invoke"'*)
      count=$((count + 1))
      printf '%s\n' '{"type":"action-output","output":{"events":[{"type":"test.rendered","source":"test-host","data":{"count":'"$count"'}}]}}'
      ;;
    *'"type":"shutdown"'*)
      printf '%s\n' '{"type":"shutdown"}'
      exit 0
      ;;
  esac
done
"#,
        )?;
        let manifest = dir.join("extension.json");
        fs::write(
            &manifest,
            serde_json::to_string_pretty(&json!({
                "id": "test-host",
                "version": "0.1.0",
                "entrypoint": host,
                "runtime": "stdio-jsonl"
            }))?,
        )?;

        let socket = dir.join("spindle.sock");
        let log = EventLog::in_dir(&dir);
        let server_log = log.clone();
        let server_state = ServerState::new(server_log);
        let listener = UnixListener::bind(&socket)?;

        let handle = thread::spawn(move || -> Result<(), SpindleError> {
            for _request in 0..3 {
                let (stream, _address) = listener.accept()?;
                handle_stream(stream, &server_state)?;
            }
            Ok(())
        });

        assert!(matches!(
            send_request(&socket, &HubRequest::RegisterExtension { manifest })?,
            HubResponse::Ok { .. }
        ));
        let first = send_request(
            &socket,
            &HubRequest::Invoke {
                action: String::from("test.render"),
                source: String::from("test-client"),
                capabilities: vec![String::from("test.write")],
                args: json!({}),
            },
        )?;
        let second = send_request(
            &socket,
            &HubRequest::Invoke {
                action: String::from("test.render"),
                source: String::from("test-client"),
                capabilities: vec![String::from("test.write")],
                args: json!({}),
            },
        )?;

        handle
            .join()
            .map_err(|_payload| SpindleError::InvalidField {
                field: "server_thread",
                reason: "panicked",
            })??;

        assert!(matches!(first, HubResponse::Ok { .. }));
        assert!(matches!(second, HubResponse::Ok { .. }));
        let rendered = log.read(&EventFilter {
            kind: Some(String::from("test.rendered")),
            source: Some(String::from("test-host")),
            limit: None,
        })?;
        assert_eq!(rendered.len(), 2);
        assert_eq!(rendered[0].data, json!({ "count": 1 }));
        assert_eq!(rendered[1].data, json!({ "count": 2 }));
        fs::remove_dir_all(dir)?;
        Ok(())
    }
}
