use std::{
    ffi::OsString,
    fs::{self, File},
    io::{Error, ErrorKind, Write},
    path::{Path, PathBuf},
    process::{self, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

use crate::SpindleError;

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

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
                    if remove_dead_owner_lock(&lock_path)? {
                        continue;
                    }
                    if started.elapsed() >= timeout {
                        return Err(Error::new(
                            ErrorKind::TimedOut,
                            "timed out waiting for sidecar lock",
                        )
                        .into());
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

fn remove_dead_owner_lock(lock_path: &Path) -> Result<bool, SpindleError> {
    let Some(owner) = read_lock_owner(lock_path)? else {
        return Ok(false);
    };

    if process_is_alive(owner.pid) {
        return Ok(false);
    }

    match fs::remove_file(lock_path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error.into()),
    }
}

fn read_lock_owner(lock_path: &Path) -> Result<Option<LockOwner>, SpindleError> {
    let contents = match fs::read_to_string(lock_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
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

    Ok(pid.map(|pid| LockOwner {
        pid,
        token: token.unwrap_or_default(),
    }))
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
    use std::{fs, time::Duration};

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
