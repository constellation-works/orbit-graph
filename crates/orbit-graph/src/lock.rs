//! Deadline-bounded advisory file locks with a diagnostic holder record.
//!
//! Every exclusive lock the graph takes on shared on-disk state goes through
//! [`FileLockGuard::acquire`] (`STD-03 §R6`, `§R7`):
//!
//! - the lock is a kernel advisory lock (`flock` on Unix) on a sidecar file
//!   that std opens close-on-exec, so a crashed holder never wedges it;
//! - acquisition polls `try_lock` until a deadline instead of blocking, so a
//!   stuck holder produces an error rather than a silent hang;
//! - just after acquiring, the holder writes its PID, acquisition time and a
//!   label into the lock file. The record only explains a timeout: a waiter
//!   may briefly read the previous holder's record or none, and nothing
//!   decides ownership from it. A refusal with no readable record is still
//!   contention, never a free lock.

use std::fmt::{Display, Formatter};
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::GraphError;

/// Default wait for a graph lock before failing (`STD-03 §R7`).
pub(crate) const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(30);

/// Environment variable overriding [`DEFAULT_LOCK_TIMEOUT`], in whole
/// milliseconds. `0` tries the lock once without waiting.
pub(crate) const LOCK_TIMEOUT_ENV: &str = "ORBIT_GRAPH_LOCK_TIMEOUT_MS";

const FIRST_POLL_INTERVAL: Duration = Duration::from_millis(1);
const MAX_POLL_INTERVAL: Duration = Duration::from_millis(50);
/// Upper bound on the holder record a waiter reads back.
const HOLDER_RECORD_LIMIT: u64 = 4096;

/// Who holds a lock, as recorded by the holder just after acquiring it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LockHolder {
    /// Process ID of the holder.
    pub(crate) pid: u32,
    /// UTC RFC 3339 time the holder acquired the lock.
    pub(crate) acquired_at: String,
    /// What the holder is doing, such as `orbit-graph graph sync`.
    pub(crate) label: String,
}

impl LockHolder {
    /// A record for the current process, acquired now.
    pub(crate) fn current(label: impl Into<String>) -> Self {
        Self {
            pid: std::process::id(),
            acquired_at: crate::recommend::current_observation_cutoff()
                .unwrap_or_else(|_| "unknown time".to_string()),
            label: label.into(),
        }
    }
}

impl Display for LockHolder {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "held by pid {} since {} ({})",
            self.pid, self.acquired_at, self.label
        )
    }
}

/// An exclusive advisory lock, released when the guard drops.
#[must_use = "the lock is released as soon as the guard is dropped"]
#[derive(Debug)]
pub(crate) struct FileLockGuard {
    _file: File,
}

impl FileLockGuard {
    /// Takes the exclusive lock on `lock_path`, waiting at most `timeout`.
    ///
    /// On timeout the error names the current holder from its record, or
    /// says that no readable record exists.
    pub(crate) fn acquire(
        lock_path: &Path,
        label: &str,
        timeout: Duration,
        operation: &'static str,
    ) -> Result<Self, GraphError> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)
            .map_err(|source| GraphError::io(operation, lock_path, source))?;
        let started = Instant::now();
        let mut interval = FIRST_POLL_INTERVAL;
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) => {}
                Err(TryLockError::Error(source)) => {
                    return Err(GraphError::io(operation, lock_path, source));
                }
            }
            let waited = started.elapsed();
            let Some(remaining) = timeout.checked_sub(waited).filter(|left| !left.is_zero()) else {
                return Err(timeout_error(
                    operation,
                    lock_path,
                    timeout,
                    read_holder(lock_path).as_ref(),
                ));
            };
            thread::sleep(interval.min(remaining));
            interval = (interval * 2).min(MAX_POLL_INTERVAL);
        }

        if let Err(error) = write_holder(&file, &LockHolder::current(label)) {
            // The record is diagnostic only; the kernel lock is the authority.
            tracing::warn!(
                path = %lock_path.display(),
                error = %error,
                "could not record lock holder"
            );
        }
        Ok(Self { _file: file })
    }
}

/// The lock wait configured by [`LOCK_TIMEOUT_ENV`], or the default.
pub(crate) fn lock_timeout() -> Result<Duration, GraphError> {
    match std::env::var(LOCK_TIMEOUT_ENV) {
        Ok(value) => value
            .trim()
            .parse::<u64>()
            .map(Duration::from_millis)
            .map_err(|error| {
                GraphError::invalid_data(
                    "read graph lock timeout",
                    format!("{LOCK_TIMEOUT_ENV} must be whole milliseconds: {error}"),
                )
            }),
        Err(std::env::VarError::NotPresent) => Ok(DEFAULT_LOCK_TIMEOUT),
        Err(error) => Err(GraphError::invalid_data(
            "read graph lock timeout",
            format!("{LOCK_TIMEOUT_ENV}: {error}"),
        )),
    }
}

/// A holder label naming this executable and `activity`.
pub(crate) fn holder_label(activity: &str) -> String {
    let program = std::env::args_os()
        .next()
        .and_then(|arg0| {
            Path::new(&arg0)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "orbit-graph".to_string());
    format!("{program} {activity}")
}

/// The error for a wait on `path` that outlived `timeout`.
pub(crate) fn timeout_error(
    operation: &'static str,
    path: &Path,
    timeout: Duration,
    holder: Option<&LockHolder>,
) -> GraphError {
    let holder = holder.map_or_else(
        || "holder unknown: no readable holder record".to_string(),
        ToString::to_string,
    );
    GraphError::Io {
        operation,
        path: PathBuf::from(path),
        reason: format!(
            "timed out after {} ms waiting for the lock; {holder}",
            timeout.as_millis()
        ),
    }
}

fn write_holder(mut file: &File, holder: &LockHolder) -> std::io::Result<()> {
    let record = serde_json::to_vec(holder).map_err(std::io::Error::other)?;
    file.set_len(0)?;
    file.rewind()?;
    file.write_all(&record)?;
    file.flush()
}

/// Reads the holder record, or `None` when it is missing or unreadable.
pub(crate) fn read_holder(lock_path: &Path) -> Option<LockHolder> {
    let mut bytes = Vec::new();
    File::open(lock_path)
        .ok()?
        .take(HOLDER_RECORD_LIMIT)
        .read_to_end(&mut bytes)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[cfg(test)]
#[path = "tests/lock.rs"]
mod tests;
