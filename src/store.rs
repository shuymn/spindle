use std::{
    collections::VecDeque,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, ErrorKind, Write},
    path::{Path, PathBuf},
};

use crate::{Event, EventFilter, SpindleError, lock::SidecarLock};

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
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }

        let _lock = SidecarLock::acquire(&self.path)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
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
    use std::{fs, sync::Arc, thread};

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
        Ok(env::temp_dir().join(format!("spindle-test-{}-{suffix}-{counter}", process::id())))
    }

    pub fn write_executable(path: &std::path::Path, contents: &str) -> Result<(), SpindleError> {
        fs::write(path, contents)?;
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)?;
        Ok(())
    }

    pub fn write_capability_policy(
        state_dir: &std::path::Path,
        contents: &str,
    ) -> Result<(), SpindleError> {
        fs::write(state_dir.join("capabilities.json"), contents)?;
        Ok(())
    }
}
