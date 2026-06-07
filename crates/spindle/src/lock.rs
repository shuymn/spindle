use std::{
    ffi::OsString,
    fs::{self, File},
    io::{Error, ErrorKind, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{self, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant, SystemTime},
};

use crate::SpindleError;

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
const MALFORMED_STALE_AFTER: Duration = Duration::from_secs(1);

#[derive(Debug)]
pub struct SidecarLock {
    path: PathBuf,
    token: String,
}

impl SidecarLock {
    pub fn acquire(target_path: &Path) -> Result<Self, SpindleError> {
        Self::acquire_with_timeout(target_path, DEFAULT_TIMEOUT)
    }

    pub fn acquire_with_timeout(
        target_path: &Path,
        timeout: Duration,
    ) -> Result<Self, SpindleError> {
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let lock_path = sidecar_lock_path(target_path);
        let started = Instant::now();
        let token = next_lock_token();

        loop {
            match File::options()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(mut file) => {
                    writeln!(file, "pid={}", process::id())?;
                    writeln!(file, "token={token}")?;
                    file.flush()?;
                    return Ok(Self {
                        path: lock_path,
                        token,
                    });
                }
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                    let timed_out = started.elapsed() >= timeout;
                    let lock = read_lock(&lock_path)?;
                    if remove_recoverable_lock(&lock_path, &lock)? {
                        continue;
                    }
                    if timed_out {
                        return Err(lock_timeout_error(&lock_path, &lock).into());
                    }
                    thread::sleep(POLL_INTERVAL);
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
}

impl Drop for SidecarLock {
    fn drop(&mut self) {
        if lock_token_matches(&self.path, &self.token) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct LockOwner {
    pid: u32,
    token: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LockFileIdentity {
    dev: u64,
    ino: u64,
}

impl LockFileIdentity {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
        }
    }
}

#[derive(Debug)]
enum LockRead {
    Missing,
    Owner {
        owner: LockOwner,
        identity: LockFileIdentity,
    },
    Malformed {
        modified: Option<SystemTime>,
        identity: LockFileIdentity,
    },
}

fn remove_recoverable_lock(lock_path: &Path, lock: &LockRead) -> Result<bool, SpindleError> {
    match lock {
        LockRead::Missing => Ok(true),
        LockRead::Owner { owner, identity } if !process_is_alive(owner.pid) => {
            remove_lock_file(lock_path, *identity)
        }
        LockRead::Malformed { modified, identity } if malformed_lock_is_stale(*modified) => {
            remove_lock_file(lock_path, *identity)
        }
        LockRead::Owner { .. } | LockRead::Malformed { .. } => Ok(false),
    }
}

fn remove_lock_file(
    lock_path: &Path,
    expected_identity: LockFileIdentity,
) -> Result<bool, SpindleError> {
    match fs::metadata(lock_path) {
        Ok(metadata) if LockFileIdentity::from_metadata(&metadata) == expected_identity => {}
        Ok(_) | Err(_) => return Ok(false),
    }
    match fs::remove_file(lock_path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn read_lock(lock_path: &Path) -> Result<LockRead, SpindleError> {
    let metadata = match fs::metadata(lock_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(LockRead::Missing),
        Err(error) => return Err(error.into()),
    };
    let modified = metadata.modified().ok();
    let identity = LockFileIdentity::from_metadata(&metadata);
    let contents = match fs::read_to_string(lock_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(LockRead::Missing),
        Err(error) if error.kind() == ErrorKind::InvalidData => {
            return Ok(LockRead::Malformed { modified, identity });
        }
        Err(error) => return Err(error.into()),
    };
    let mut pid = None;
    let mut token = None;

    for line in contents.lines() {
        if let Some(value) = line.strip_prefix("pid=") {
            pid = value.parse::<u32>().ok();
        } else if let Some(value) = line.strip_prefix("token=") {
            token = Some(String::from(value));
        }
    }

    Ok(
        pid.map_or(LockRead::Malformed { modified, identity }, |pid| {
            LockRead::Owner {
                owner: LockOwner {
                    pid,
                    token: token.unwrap_or_default(),
                },
                identity,
            }
        }),
    )
}

fn read_lock_owner(lock_path: &Path) -> Result<Option<LockOwner>, SpindleError> {
    match read_lock(lock_path)? {
        LockRead::Owner { owner, .. } => Ok(Some(owner)),
        LockRead::Missing | LockRead::Malformed { .. } => Ok(None),
    }
}

fn malformed_lock_is_stale(modified: Option<SystemTime>) -> bool {
    modified
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age >= MALFORMED_STALE_AFTER)
}

fn lock_timeout_error(lock_path: &Path, lock: &LockRead) -> Error {
    let state = match lock {
        LockRead::Missing => "missing",
        LockRead::Owner { .. } => "owned",
        LockRead::Malformed { .. } => "malformed",
    };
    Error::new(
        ErrorKind::TimedOut,
        format!(
            "timed out waiting for sidecar lock {} ({state})",
            lock_path.display()
        ),
    )
}

fn lock_token_matches(lock_path: &Path, token: &str) -> bool {
    read_lock_owner(lock_path)
        .ok()
        .flatten()
        .is_some_and(|owner| owner.token == token)
}

fn process_is_alive(pid: u32) -> bool {
    if pid == process::id() {
        return true;
    }

    Command::new("/bin/kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

static NEXT_LOCK_TOKEN: AtomicU64 = AtomicU64::new(0);

fn next_lock_token() -> String {
    let counter = NEXT_LOCK_TOKEN.fetch_add(1, Ordering::Relaxed);
    format!("{}-{counter}", process::id())
}

fn sidecar_lock_path(target_path: &Path) -> PathBuf {
    let mut file_name = target_path.file_name().map_or_else(
        || OsString::from("spindle-state"),
        std::ffi::OsStr::to_os_string,
    );
    file_name.push(".lock");

    let mut path = target_path.to_path_buf();
    path.set_file_name(file_name);
    path
}

#[cfg(test)]
mod tests {
    use std::{fs, thread, time::Duration};

    use super::*;

    #[test]
    fn sidecar_lock_recovers_stale_lock_file() -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        let target = dir.join("events.jsonl");
        fs::write(
            dir.join("events.jsonl.lock"),
            "pid=999999999\ntoken=stale\n",
        )?;

        let _lock = SidecarLock::acquire_with_timeout(&target, Duration::ZERO)?;

        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn malformed_stale_lock_is_recovered() -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        let target = dir.join("events.jsonl");
        fs::write(dir.join("events.jsonl.lock"), "not a spindle lock\n")?;
        thread::sleep(MALFORMED_STALE_AFTER + Duration::from_millis(50));

        let _lock = SidecarLock::acquire_with_timeout(&target, Duration::ZERO)?;

        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn malformed_stale_recovery_does_not_remove_replaced_lock() -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        let lock_path = dir.join("events.jsonl.lock");
        fs::write(&lock_path, "not a spindle lock\n")?;
        thread::sleep(MALFORMED_STALE_AFTER + Duration::from_millis(50));
        let stale_read = read_lock(&lock_path)?;
        fs::remove_file(&lock_path)?;
        fs::write(&lock_path, format!("pid={}\ntoken=fresh\n", process::id()))?;

        assert!(!remove_recoverable_lock(&lock_path, &stale_read)?);
        assert!(lock_path.exists());
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn malformed_fresh_lock_is_not_immediately_removed() -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        let target = dir.join("events.jsonl");
        let lock_path = dir.join("events.jsonl.lock");
        fs::write(&lock_path, "not a spindle lock\n")?;

        let result = SidecarLock::acquire_with_timeout(&target, Duration::from_millis(20));

        assert!(result.is_err());
        assert!(lock_path.exists());
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn malformed_lock_timeout_reports_parse_state() -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        let target = dir.join("events.jsonl");
        let lock_path = dir.join("events.jsonl.lock");
        fs::write(&lock_path, "not a spindle lock\n")?;

        let error = SidecarLock::acquire_with_timeout(&target, Duration::from_millis(20))
            .err()
            .ok_or(SpindleError::InvalidField {
                field: "lock",
                reason: "expected timeout",
            })?;
        let message = error.to_string();

        assert!(message.contains("malformed"));
        assert!(message.contains(&lock_path.display().to_string()));
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn sidecar_lock_does_not_remove_live_lock() -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        let target = dir.join("events.jsonl");
        let lock_path = dir.join("events.jsonl.lock");

        let first_lock = SidecarLock::acquire_with_timeout(&target, Duration::ZERO)?;
        let second_lock = SidecarLock::acquire_with_timeout(&target, Duration::from_millis(20));

        assert!(second_lock.is_err());
        assert!(lock_path.exists());
        drop(first_lock);
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn sidecar_lock_drop_removes_only_owned_lock() -> Result<(), SpindleError> {
        let dir = crate::store::tests_support::test_dir()?;
        fs::create_dir_all(&dir)?;
        let target = dir.join("events.jsonl");
        let lock_path = dir.join("events.jsonl.lock");

        let lock = SidecarLock::acquire_with_timeout(&target, Duration::ZERO)?;
        fs::write(
            &lock_path,
            format!("pid={}\ntoken=other-owner\n", process::id()),
        )?;
        drop(lock);

        assert!(lock_path.exists());
        fs::remove_dir_all(dir)?;
        Ok(())
    }
}
