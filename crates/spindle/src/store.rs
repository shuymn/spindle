use std::{
    collections::VecDeque,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, ErrorKind, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use crate::{Event, EventFilter, SpindleError, lock::SidecarLock};

pub fn ensure_private_state_parent(path: &Path) -> Result<(), SpindleError> {
    ensure_private_parent(path, "state_dir", "state directory must be private")
}

pub fn ensure_private_parent(
    path: &Path,
    field: &'static str,
    reason: &'static str,
) -> Result<(), SpindleError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };

    if parent.exists() {
        return ensure_private_existing_dir(parent, field, reason);
    }

    create_private_dir_all(parent)?;
    ensure_private_existing_dir(parent, field, reason)
}

fn create_private_dir_all(path: &Path) -> Result<(), SpindleError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        if current.as_os_str().is_empty() || current.exists() {
            continue;
        }
        match fs::DirBuilder::new().mode(0o700).create(&current) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn ensure_private_existing_dir(
    path: &Path,
    field: &'static str,
    reason: &'static str,
) -> Result<(), SpindleError> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_dir() {
        return Err(SpindleError::InvalidField {
            field,
            reason: "parent path must be a directory",
        });
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(SpindleError::InvalidField { field, reason });
    }
    Ok(())
}

/// Append-only JSONL event log.
#[derive(Debug, Clone)]
pub struct EventLog {
    path: PathBuf,
}

impl EventLog {
    /// Create a log located under a spindle state directory.
    #[must_use]
    pub fn in_dir(state_dir: &Path) -> Self {
        Self {
            path: state_dir.join("events.jsonl"),
        }
    }

    /// Return the backing JSONL path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Return the state directory that owns this log.
    #[must_use]
    pub fn state_dir(&self) -> &Path {
        self.path.parent().unwrap_or_else(|| Path::new("."))
    }

    /// Append an event to the log.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created, the log cannot be
    /// opened, or the event cannot be serialized.
    pub fn append(&self, event: &Event) -> Result<(), SpindleError> {
        ensure_private_state_parent(&self.path)?;

        let _lock = SidecarLock::acquire(&self.path)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&self.path)?;
        fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))?;
        serde_json::to_writer(&mut file, event)?;
        writeln!(file)?;
        file.flush()?;
        file.sync_data()?;
        Ok(())
    }

    /// Read events matching a filter.
    ///
    /// # Errors
    ///
    /// Returns an error if the log cannot be read or contains malformed JSON.
    pub fn read(&self, filter: &EventFilter) -> Result<Vec<Event>, SpindleError> {
        if filter.limit == Some(0) {
            return Ok(Vec::new());
        }
        let _lock = SidecarLock::acquire(&self.path)?;
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let reader = BufReader::new(file);
        let mut events = TailEvents::new(filter.limit);

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let event = serde_json::from_str::<Event>(&line)?;
            if filter.matches(&event) {
                events.push(event);
            }
        }

        Ok(events.into_vec())
    }
}

#[derive(Debug)]
enum TailEvents {
    All(Vec<Event>),
    Limited {
        limit: usize,
        events: VecDeque<Event>,
    },
}

impl TailEvents {
    fn new(limit: Option<usize>) -> Self {
        limit.map_or_else(
            || Self::All(Vec::new()),
            |limit| Self::Limited {
                limit,
                events: VecDeque::with_capacity(limit),
            },
        )
    }

    fn push(&mut self, event: Event) {
        match self {
            Self::All(events) => events.push(event),
            Self::Limited { limit, events } => {
                if *limit == 0 {
                    return;
                }
                if events.len() == *limit {
                    let _dropped = events.pop_front();
                }
                events.push_back(event);
            }
        }
    }

    fn into_vec(self) -> Vec<Event> {
        match self {
            Self::All(events) => events,
            Self::Limited { events, .. } => events.into_iter().collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt, sync::Arc, thread};

    use super::*;

    #[test]
    fn event_log_appends_and_filters_events() -> Result<(), SpindleError> {
        let dir = tests_support::test_dir()?;
        let log = EventLog::in_dir(&dir);
        let first =
            Event::builder(String::from("agent.status.changed"), String::from("codex")).build()?;
        let second =
            Event::builder(String::from("agent.status.changed"), String::from("pi")).build()?;

        log.append(&first)?;
        log.append(&second)?;

        let events = log.read(&EventFilter {
            kind: Some(String::from("agent.status.changed")),
            source: Some(String::from("pi")),
            limit: None,
        })?;

        assert_eq!(events, vec![second]);
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn event_log_file_is_created_private() -> Result<(), SpindleError> {
        let dir = tests_support::test_dir()?;
        let log = EventLog::in_dir(&dir);
        let event =
            Event::builder(String::from("agent.status.changed"), String::from("codex")).build()?;

        log.append(&event)?;

        assert_eq!(
            fs::metadata(log.path())?.permissions().mode() & 0o777,
            0o600
        );
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn event_log_creates_new_state_dir_private() -> Result<(), SpindleError> {
        let root = tests_support::test_dir()?;
        let dir = root.join("nested").join("state");
        let log = EventLog::in_dir(&dir);
        let event =
            Event::builder(String::from("agent.status.changed"), String::from("codex")).build()?;

        log.append(&event)?;

        assert_eq!(fs::metadata(&root)?.permissions().mode() & 0o777, 0o700);
        assert_eq!(
            fs::metadata(root.join("nested"))?.permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(fs::metadata(&dir)?.permissions().mode() & 0o777, 0o700);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn event_log_rejects_public_existing_state_dir() -> Result<(), SpindleError> {
        let dir = tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755))?;
        let log = EventLog::in_dir(&dir);
        let event =
            Event::builder(String::from("agent.status.changed"), String::from("codex")).build()?;

        let result = log.append(&event);

        assert!(matches!(
            result,
            Err(SpindleError::InvalidField {
                field: "state_dir",
                ..
            })
        ));
        assert_eq!(fs::metadata(&dir)?.permissions().mode() & 0o777, 0o755);
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn event_log_rejects_parent_path_that_is_not_directory() -> Result<(), SpindleError> {
        let dir = tests_support::test_dir()?;
        let parent_file = dir.join("not-a-directory");
        fs::write(&parent_file, b"not a directory")?;
        let log = EventLog {
            path: parent_file.join("events.jsonl"),
        };
        let event =
            Event::builder(String::from("agent.status.changed"), String::from("codex")).build()?;

        let result = log.append(&event);

        assert!(matches!(
            result,
            Err(SpindleError::InvalidField {
                field: "state_dir",
                reason: "parent path must be a directory",
            })
        ));
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn event_log_does_not_chmod_existing_private_state_dir() -> Result<(), SpindleError> {
        let dir = tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        let log = EventLog::in_dir(&dir);
        let event =
            Event::builder(String::from("agent.status.changed"), String::from("codex")).build()?;

        log.append(&event)?;

        assert_eq!(fs::metadata(&dir)?.permissions().mode() & 0o777, 0o700);
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn event_log_limit_keeps_tail_of_matched_events() -> Result<(), SpindleError> {
        let dir = tests_support::test_dir()?;
        let log = EventLog::in_dir(&dir);
        for index in 0..5 {
            let source = if index % 2 == 0 { "codex" } else { "pi" };
            let event = Event::builder(String::from("agent.status.changed"), String::from(source))
                .subject(Some(index.to_string()))
                .build()?;
            log.append(&event)?;
        }

        let events = log.read(&EventFilter {
            kind: Some(String::from("agent.status.changed")),
            source: Some(String::from("codex")),
            limit: Some(2),
        })?;
        let subjects = events
            .iter()
            .map(|event| event.subject.clone())
            .collect::<Vec<_>>();

        assert_eq!(
            subjects,
            vec![Some(String::from("2")), Some(String::from("4"))]
        );
        assert_eq!(
            log.read(&EventFilter {
                kind: None,
                source: None,
                limit: Some(0),
            })?,
            Vec::new()
        );
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn event_log_serializes_concurrent_appends() -> Result<(), SpindleError> {
        let dir = tests_support::test_dir()?;
        let log = Arc::new(EventLog::in_dir(&dir));
        let mut handles = Vec::new();

        for index in 0..16 {
            let log = Arc::clone(&log);
            handles.push(thread::spawn(move || {
                let event =
                    Event::builder(String::from("agent.status.changed"), String::from("test"))
                        .subject(Some(index.to_string()))
                        .build()?;
                log.append(&event)
            }));
        }

        for handle in handles {
            handle
                .join()
                .map_err(|_payload| SpindleError::InvalidField {
                    field: "thread",
                    reason: "panicked",
                })??;
        }

        let events = log.read(&EventFilter::default())?;

        assert_eq!(events.len(), 16);
        fs::remove_dir_all(dir)?;
        Ok(())
    }
}

#[cfg(test)]
pub mod tests_support {
    use std::{
        env, fs,
        os::unix::fs::PermissionsExt,
        path::PathBuf,
        process,
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use crate::SpindleError;

    static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

    pub fn test_dir() -> Result<PathBuf, SpindleError> {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let counter = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
        let dir =
            env::temp_dir().join(format!("spindle-test-{}-{suffix}-{counter}", process::id()));
        fs::create_dir_all(&dir)?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        Ok(dir)
    }

    pub fn write_executable(path: &std::path::Path, contents: &str) -> Result<(), SpindleError> {
        fs::write(path, contents)?;
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)?;
        Ok(())
    }

    /// Copy the `spindle-test-host` binary into `dir` and write its JSON config.
    ///
    /// The returned path is suitable for use as a stdio JSONL manifest entrypoint.
    fn test_host_source() -> Result<PathBuf, SpindleError> {
        if let Ok(path) = std::env::var("CARGO_BIN_EXE_spindle-test-host") {
            return Ok(PathBuf::from(path));
        }

        let profile = std::env::var("PROFILE").unwrap_or_else(|_error| String::from("debug"));
        let candidate = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("target")
            .join(profile)
            .join("spindle-test-host");
        if candidate.is_file() {
            return Ok(candidate);
        }

        Err(SpindleError::InvalidField {
            field: "test_host",
            reason: "spindle-test-host binary is missing; run cargo build -p spindle-test-host",
        })
    }

    pub fn test_host_with_write_render(
        dir: &std::path::Path,
        name: &str,
    ) -> Result<PathBuf, SpindleError> {
        use spindle_extension_sdk::{ExtensionRegistration, RegistrationAction};
        use spindle_test_host::TestHostConfig;

        let registration = ExtensionRegistration::new()
            .capability("test.write")
            .action(
                "test.render",
                RegistrationAction::new().capability("test.write"),
            );
        install_test_host(dir, name, &TestHostConfig::with_registration(registration))
    }

    pub fn install_test_host(
        dir: &std::path::Path,
        name: &str,
        config: &spindle_test_host::TestHostConfig,
    ) -> Result<PathBuf, SpindleError> {
        let host = dir.join(name);
        let source = test_host_source()?;
        fs::copy(&source, &host)?;
        let mut permissions = fs::metadata(&host)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&host, permissions)?;
        let config_json = serde_json::to_string(config)?;
        fs::write(host.with_extension("json"), &config_json)?;
        if let Ok(canonical) = fs::canonicalize(&host) {
            let canonical_config = canonical.with_extension("json");
            if canonical_config != host.with_extension("json") {
                fs::write(canonical_config, &config_json)?;
            }
        }
        Ok(host)
    }
}
