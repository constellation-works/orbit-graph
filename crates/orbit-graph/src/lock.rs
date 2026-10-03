//! Deadline-bounded advisory file locks with a diagnostic holder record.
//!
//! Exclusive locks on shared on-disk state use [`FileLockGuard`] (`STD-03
//! §R6`, `§R7`). Sidecar locks use [`FileLockGuard::acquire`]:
//!
//! - the lock is a kernel advisory lock (`flock` on Unix) on a sidecar file
//!   that std opens close-on-exec, so a crashed holder never wedges it;
//! - the lock file is state like any other (`STD-05 §R7`–`§R9`): it is
//!   opened without following a symlink, refused when it is not a regular
//!   file the current user owns, and created or repaired to `0600`;
//! - acquisition polls `try_lock` until a deadline instead of blocking, so a
//!   stuck holder produces an error rather than a silent hang;
//! - just after acquiring, the holder writes its PID, acquisition time and a
//!   label into the lock file. The record only explains a timeout: a waiter
//!   may briefly read the previous holder's record or none, and nothing
//!   decides ownership from it. A refusal with no readable record is still
//!   contention, never a free lock.
//!
//! Identity prechecks use [`FileLockGuard::acquire_directory`] instead. They
//! cannot create or repair state before compatibility is known, so the existing
//! directory is locked without a holder record; a timeout reports it as unknown.

use std::fmt::{Display, Formatter};
use std::fs::{File, TryLockError};
use std::io::{Read, Seek, Write};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::GraphError;
use crate::state_dir::{StateAccess, open_state_file};

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
        let Some(file) = open_state_file(lock_path, StateAccess::Write)? else {
            return Err(GraphError::io(
                operation,
                lock_path,
                std::io::Error::from(std::io::ErrorKind::NotFound),
            ));
        };
        Self::wait(&file, lock_path, timeout, operation, true)?;

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

    /// Lock an already validated physical directory without creating files,
    /// repairing permissions, or writing a holder record. Used before graph
    /// identity is known, when even changing a lock sidecar is forbidden.
    pub(crate) fn acquire_directory(
        path: &Path,
        timeout: Duration,
        operation: &'static str,
    ) -> Result<Self, GraphError> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW);
        }
        let file = options
            .open(path)
            .map_err(|source| GraphError::io(operation, path, source))?;
        Self::wait(&file, path, timeout, operation, false)?;
        Ok(Self { _file: file })
    }

    fn wait(
        file: &File,
        lock_path: &Path,
        timeout: Duration,
        operation: &'static str,
        holder_record: bool,
    ) -> Result<(), GraphError> {
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
                    holder_record
                        .then(|| read_holder(lock_path))
                        .flatten()
                        .as_ref(),
                ));
            };
            thread::sleep(interval.min(remaining));
            interval = (interval * 2).min(MAX_POLL_INTERVAL);
        }

        Ok(())
    }
}

/// The lock wait the composition layer installed, or the default.
pub(crate) fn lock_timeout() -> Duration {
    crate::runtime::runtime().lock_timeout
}

/// A holder label naming this executable and `activity`.
pub(crate) fn holder_label(activity: &str) -> String {
    format!("{} {activity}", crate::runtime::runtime().program_name)
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
    GraphError::timeout(
        operation,
        timeout,
        format!("waiting for the lock at {}; {holder}", path.display()),
    )
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
    open_state_file(lock_path, StateAccess::Read)
        .ok()??
        .take(HOLDER_RECORD_LIMIT)
        .read_to_end(&mut bytes)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[cfg(test)]
#[path = "lock/tests/mod.rs"]
mod tests;
