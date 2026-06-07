use std::{
    io::{BufRead, BufReader, BufWriter, Write},
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc, Mutex,
        mpsc::{self, RecvTimeoutError, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use spindle_extension_sdk::{HostRequest, HostResponse};

use crate::{
    SpindleError,
    runtime::{resolve_manifest_path, timeout_millis, unexpected_host_response},
};

#[derive(Debug)]
pub(super) struct StdioJsonlSession {
    extension: String,
    child: Arc<Mutex<Child>>,
    requests: Option<Sender<StdioJsonlWorkerRequest>>,
    worker: Option<JoinHandle<()>>,
}

#[derive(Debug)]
struct StdioJsonlWorkerRequest {
    request: HostRequest,
    response: Sender<Result<HostResponse, SpindleError>>,
}

#[derive(Debug)]
struct StdioJsonlWorker {
    extension: String,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl StdioJsonlSession {
    pub(super) fn spawn(
        extension: &str,
        manifest_path: &Path,
        entrypoint: &str,
    ) -> Result<Self, SpindleError> {
        let mut child = Command::new(resolve_manifest_path(manifest_path, entrypoint))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| SpindleError::ExtensionHostClosed {
                extension: String::from(extension),
            })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| SpindleError::ExtensionHostClosed {
                extension: String::from(extension),
            })?;

        let child = Arc::new(Mutex::new(child));
        let (requests, worker_requests) = mpsc::channel::<StdioJsonlWorkerRequest>();
        let worker = StdioJsonlWorker {
            extension: String::from(extension),
            stdin: BufWriter::new(stdin),
            stdout: BufReader::new(stdout),
        };
        let worker = thread::spawn(move || worker.run(worker_requests));

        Ok(Self {
            extension: String::from(extension),
            child,
            requests: Some(requests),
            worker: Some(worker),
        })
    }

    pub(super) fn request(
        &mut self,
        request: &HostRequest,
        timeout: Duration,
    ) -> Result<HostResponse, SpindleError> {
        let Some(requests) = &self.requests else {
            return Err(SpindleError::ExtensionHostClosed {
                extension: self.extension.clone(),
            });
        };
        let (response_sender, response_receiver) = mpsc::channel();
        requests
            .send(StdioJsonlWorkerRequest {
                request: request.clone(),
                response: response_sender,
            })
            .map_err(|_send_error| SpindleError::ExtensionHostClosed {
                extension: self.extension.clone(),
            })?;

        match response_receiver.recv_timeout(timeout) {
            Ok(response) => response,
            Err(RecvTimeoutError::Timeout) => {
                self.terminate();
                Err(SpindleError::ExtensionHostTimedOut {
                    extension: self.extension.clone(),
                    timeout_ms: timeout_millis(timeout),
                })
            }
            Err(RecvTimeoutError::Disconnected) => Err(SpindleError::ExtensionHostClosed {
                extension: self.extension.clone(),
            }),
        }
    }

    pub(super) fn shutdown(&mut self, timeout: Duration) -> Result<(), SpindleError> {
        match self.request(&HostRequest::Shutdown, timeout)? {
            HostResponse::Shutdown => {
                self.join_worker();
                self.wait_for_child_exit_or_kill(timeout)?;
                Ok(())
            }
            response => Err(unexpected_host_response(&self.extension, &response)),
        }
    }

    pub(super) fn terminate(&mut self) {
        self.requests.take();
        if let Ok(mut child) = self.child.lock() {
            let _result = child.kill();
        }
        self.join_worker();
    }

    fn join_worker(&mut self) {
        self.requests.take();
        if let Some(worker) = self.worker.take() {
            let _result = worker.join();
        }
    }

    fn wait_for_child_exit_or_kill(&self, timeout: Duration) -> Result<(), SpindleError> {
        let started = Instant::now();

        loop {
            {
                let mut child =
                    self.child
                        .lock()
                        .map_err(|_error| SpindleError::ExtensionHostClosed {
                            extension: self.extension.clone(),
                        })?;
                if child.try_wait()?.is_some() {
                    drop(child);
                    return Ok(());
                }
                if started.elapsed() >= timeout {
                    let _result = child.kill();
                    let _result = child.wait();
                    drop(child);
                    return Ok(());
                }
                drop(child);
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl StdioJsonlWorker {
    fn run(mut self, requests: mpsc::Receiver<StdioJsonlWorkerRequest>) {
        for request in requests {
            let is_shutdown = request.request == HostRequest::Shutdown;
            let response = self.request(&request.request);
            let _result = request.response.send(response);
            if is_shutdown {
                break;
            }
        }
    }

    fn request(&mut self, request: &HostRequest) -> Result<HostResponse, SpindleError> {
        serde_json::to_writer(&mut self.stdin, request)?;
        writeln!(self.stdin)?;
        self.stdin.flush()?;

        let mut line = String::new();
        if self.stdout.read_line(&mut line)? == 0 {
            return Err(SpindleError::ExtensionHostClosed {
                extension: self.extension.clone(),
            });
        }
        serde_json::from_str::<HostResponse>(&line).map_err(|source| {
            SpindleError::ExtensionHostProtocolInvalid {
                extension: self.extension.clone(),
                source,
            }
        })
    }
}
