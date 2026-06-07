use std::{
    fs,
    io::{BufReader, BufWriter, Write},
    net::Shutdown,
    os::unix::{
        fs::{FileTypeExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::Path,
    sync::Arc,
    thread,
    time::Duration,
};

use crate::{
    EventLog, ExtensionRegistry, ExtensionRuntimeHost, HubRequest, HubResponse, SpindleError,
    execute_request,
    protocol::{DEFAULT_JSONL_MESSAGE_LIMIT, read_limited_jsonl_line},
    store::ensure_private_parent,
};

const DEFAULT_STREAM_READ_IDLE_TIMEOUT: Duration = Duration::from_secs(5);

/// Serve spindle JSONL requests on a Unix domain socket.
///
/// # Errors
///
/// Returns an error if the socket cannot be prepared, bound, or used.
pub fn serve(socket_path: &Path, log: &EventLog) -> Result<(), SpindleError> {
    prepare_socket(socket_path)?;
    let listener = UnixListener::bind(socket_path)?;
    fs::set_permissions(socket_path, fs::Permissions::from_mode(0o600))?;
    let state = Arc::new(ServerState::new(log.clone()));

    for stream in listener.incoming() {
        spawn_stream_handler(stream?, Arc::clone(&state));
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
    send_request_inner(socket_path, request, None)
}

/// Send one request with read/write timeouts.
///
/// # Errors
///
/// Returns an error if the socket cannot be reached, the request cannot be
/// serialized, the response cannot be parsed, or the timeout expires.
pub fn send_request_with_timeout(
    socket_path: &Path,
    request: &HubRequest,
    timeout: Duration,
) -> Result<HubResponse, SpindleError> {
    send_request_inner(socket_path, request, Some(timeout))
}

fn send_request_inner(
    socket_path: &Path,
    request: &HubRequest,
    timeout: Option<Duration>,
) -> Result<HubResponse, SpindleError> {
    let mut stream = UnixStream::connect(socket_path)?;
    stream.set_read_timeout(timeout)?;
    stream.set_write_timeout(timeout)?;
    serde_json::to_writer(&mut stream, request)?;
    writeln!(stream)?;
    stream.shutdown(Shutdown::Write)?;

    let mut reader = BufReader::new(stream);
    let line = read_limited_jsonl_line(&mut reader, DEFAULT_JSONL_MESSAGE_LIMIT)?
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::UnexpectedEof))?;
    Ok(serde_json::from_str(&line)?)
}

#[derive(Debug)]
struct ServerState {
    log: EventLog,
    registry: ExtensionRegistry,
    runtime: Arc<ExtensionRuntimeHost>,
}

impl ServerState {
    fn new(log: EventLog) -> Self {
        Self {
            registry: registry_for_log(&log),
            log,
            runtime: Arc::new(ExtensionRuntimeHost::new()),
        }
    }

    fn execute(&self, request: HubRequest) -> Result<serde_json::Value, SpindleError> {
        execute_request(request, &self.log, &self.registry, &self.runtime)
    }
}

fn handle_stream(stream: UnixStream, state: &ServerState) -> Result<(), SpindleError> {
    handle_stream_with_timeout(stream, state, DEFAULT_STREAM_READ_IDLE_TIMEOUT)
}

fn handle_stream_with_timeout(
    stream: UnixStream,
    state: &ServerState,
    read_idle_timeout: Duration,
) -> Result<(), SpindleError> {
    stream.set_read_timeout(Some(read_idle_timeout))?;
    let reader_stream = stream.try_clone()?;
    let mut reader = BufReader::new(reader_stream);
    let mut writer = BufWriter::new(stream);

    loop {
        let line = match read_limited_jsonl_line(&mut reader, DEFAULT_JSONL_MESSAGE_LIMIT) {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(error) => {
                let response = HubResponse::Error {
                    error: error.to_string(),
                };
                serde_json::to_writer(&mut writer, &response)?;
                writeln!(writer)?;
                writer.flush()?;
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<HubRequest>(&line) {
            Ok(request) => HubResponse::from(state.execute(request)),
            Err(error) => HubResponse::Error {
                error: error.to_string(),
            },
        };
        serde_json::to_writer(&mut writer, &response)?;
        writeln!(writer)?;
        writer.flush()?;
    }

    Ok(())
}

fn spawn_stream_handler(stream: UnixStream, state: Arc<ServerState>) {
    let _handler = thread::spawn(move || {
        let _result = handle_stream(stream, &state);
    });
}

fn registry_for_log(log: &EventLog) -> ExtensionRegistry {
    ExtensionRegistry::in_dir(log.state_dir())
}

fn prepare_socket(socket_path: &Path) -> Result<(), SpindleError> {
    let Some(_parent) = socket_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Err(SpindleError::InvalidField {
            field: "socket",
            reason: "socket path must have a parent directory",
        });
    };
    ensure_private_parent(
        socket_path,
        "socket",
        "socket parent directory must be private",
    )?;

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
    use std::{fs, io::Read, os::unix::fs::PermissionsExt, thread};

    use serde_json::json;

    use super::*;
    use crate::{EventFilter, HubRequest};

    #[test]
    fn prepare_socket_does_not_chmod_existing_parent_directory() -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o500))?;

        prepare_socket(&dir.join("spindle.sock"))?;

        assert_eq!(fs::metadata(&dir)?.permissions().mode() & 0o777, 0o500);
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn prepare_socket_rejects_public_existing_parent_directory() -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755))?;

        let result = prepare_socket(&dir.join("spindle.sock"));

        assert!(matches!(
            result,
            Err(SpindleError::InvalidField {
                field: "socket",
                ..
            })
        ));
        assert_eq!(fs::metadata(&dir)?.permissions().mode() & 0o777, 0o755);
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn prepare_socket_rejects_parent_path_that_is_not_directory() -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        let parent_file = dir.join("not-a-directory");
        fs::write(&parent_file, b"not a directory")?;

        let result = prepare_socket(&parent_file.join("spindle.sock"));

        assert!(matches!(
            result,
            Err(SpindleError::InvalidField {
                field: "socket",
                reason: "parent path must be a directory",
            })
        ));
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn prepare_socket_creates_missing_parent_private() -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        let socket_parent = dir.join("state");

        prepare_socket(&socket_parent.join("spindle.sock"))?;

        assert_eq!(
            fs::metadata(&socket_parent)?.permissions().mode() & 0o777,
            0o700
        );
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn send_request_round_trips_over_unix_socket() -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        let socket = dir.join("spindle.sock");
        let log = EventLog::in_dir(&dir);
        crate::store::tests_support::write_capability_policy(
            &dir,
            r#"{"emits":{"pi":["agent.status.changed"]},"direct":{},"routes":{}}"#,
        )?;
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
    fn server_rejects_non_utf8_request() -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        let state = ServerState::new(EventLog::in_dir(&dir));
        let (mut client, server) = UnixStream::pair()?;
        let handle = thread::spawn(move || handle_stream(server, &state));

        client.write_all(b"{\"command\":\"list-extensions\",\"bad\":\"")?;
        client.write_all(&[0xff, b'"', b'}', b'\n'])?;
        client.shutdown(Shutdown::Write)?;

        let mut reader = std::io::BufReader::new(&mut client);
        let response = read_response_line(&mut reader)?;
        let parsed = serde_json::from_str::<HubResponse>(&response)?;
        assert!(matches!(parsed, HubResponse::Error { .. }));
        handle
            .join()
            .map_err(|_payload| SpindleError::InvalidField {
                field: "server_thread",
                reason: "panicked",
            })??;
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn oversized_request_returns_one_error_then_closes() -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        let state = ServerState::new(EventLog::in_dir(&dir));
        let (mut client, server) = UnixStream::pair()?;
        let handle = thread::spawn(move || handle_stream(server, &state));

        client.write_all(&vec![b'a'; DEFAULT_JSONL_MESSAGE_LIMIT + 1])?;
        client.write_all(b"\n")?;
        client.shutdown(Shutdown::Write)?;

        let mut reader = std::io::BufReader::new(&mut client);
        let response = read_response_line(&mut reader)?;
        let parsed = serde_json::from_str::<HubResponse>(&response)?;
        assert!(matches!(parsed, HubResponse::Error { .. }));
        let mut trailing = String::new();
        reader.read_to_string(&mut trailing)?;
        assert!(trailing.is_empty());
        handle
            .join()
            .map_err(|_payload| SpindleError::InvalidField {
                field: "server_thread",
                reason: "panicked",
            })??;
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn malformed_json_response_keeps_connection_alive() -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        let state = ServerState::new(EventLog::in_dir(&dir));
        let (mut client, server) = UnixStream::pair()?;
        let handle = thread::spawn(move || handle_stream(server, &state));

        client.write_all(b"{not-json}\n")?;
        client.write_all(b"{\"command\":\"list-extensions\"}\n")?;
        client.shutdown(Shutdown::Write)?;

        let mut reader = std::io::BufReader::new(&mut client);
        let first = serde_json::from_str::<HubResponse>(&read_response_line(&mut reader)?)?;
        let second = serde_json::from_str::<HubResponse>(&read_response_line(&mut reader)?)?;
        assert!(matches!(first, HubResponse::Error { .. }));
        assert!(matches!(second, HubResponse::Ok { .. }));
        handle
            .join()
            .map_err(|_payload| SpindleError::InvalidField {
                field: "server_thread",
                reason: "panicked",
            })??;
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    fn read_response_line(reader: &mut impl std::io::BufRead) -> Result<String, SpindleError> {
        read_limited_jsonl_line(reader, DEFAULT_JSONL_MESSAGE_LIMIT)?
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into())
    }

    #[test]
    fn partial_request_returns_protocol_error() -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        let state = ServerState::new(EventLog::in_dir(&dir));
        let (mut client, server) = UnixStream::pair()?;
        let handle = thread::spawn(move || handle_stream(server, &state));

        client.write_all(b"{\"command\":\"list-extensions\"")?;
        client.shutdown(Shutdown::Write)?;

        let mut reader = std::io::BufReader::new(&mut client);
        let response = read_response_line(&mut reader)?;
        let parsed = serde_json::from_str::<HubResponse>(&response)?;
        assert!(matches!(parsed, HubResponse::Error { .. }));
        handle
            .join()
            .map_err(|_payload| SpindleError::InvalidField {
                field: "server_thread",
                reason: "panicked",
            })??;
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn live_partial_peer_times_out_with_protocol_error() -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        let state = ServerState::new(EventLog::in_dir(&dir));
        let (mut client, server) = UnixStream::pair()?;
        let handle = thread::spawn(move || {
            handle_stream_with_timeout(server, &state, Duration::from_millis(50))
        });

        client.write_all(b"{\"command\":\"list-extensions\"")?;
        client.flush()?;

        let mut reader = std::io::BufReader::new(&mut client);
        let response = read_response_line(&mut reader)?;
        let parsed = serde_json::from_str::<HubResponse>(&response)?;
        assert!(matches!(parsed, HubResponse::Error { .. }));
        handle
            .join()
            .map_err(|_payload| SpindleError::InvalidField {
                field: "server_thread",
                reason: "panicked",
            })??;
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn daemon_reuses_stdio_hosts_across_client_connections() -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        crate::store::tests_support::write_capability_policy(
            &dir,
            r#"{"emits":{},"direct":{"test-client":["test.write"]},"routes":{}}"#,
        )?;
        let host = dir.join("host.sh");
        crate::store::tests_support::write_executable(
            &host,
            r#"#!/bin/sh
count=0
while IFS= read -r line; do
  case "$line" in
    *'"type":"register"'*)
      printf '%s\n' '{"type":"registration","registration":{"produces":["test.rendered"],"capabilities":["test.write"],"actions":{"test.render":{"capabilities":["test.write"]}}}}'
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
            send_request(
                &socket,
                &HubRequest::RegisterExtension {
                    manifest,
                    trust_runtime: true,
                },
            )?,
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
