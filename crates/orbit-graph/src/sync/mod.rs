//! Graph synchronization orchestration.

pub(crate) mod pass1;
pub(crate) mod pass2;
pub(crate) mod scanner;
pub(crate) mod watcher;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rusqlite::Connection;

use crate::lock::{self, LockHolder};
use crate::{GraphError, SyncFailure, SyncMode, SyncObserver, SyncOutcome, SyncReport, SyncSkip};

pub(crate) fn run(
    db_path: &Path,
    worktree_root: &Path,
    mode: SyncMode,
) -> Result<SyncReport, GraphError> {
    let timeout = lock::lock_timeout()?;
    coalesced(db_path, timeout, || {
        run_once(db_path, worktree_root, mode, timeout)
    })
}

fn run_once(
    db_path: &Path,
    worktree_root: &Path,
    mode: SyncMode,
    lock_timeout: Duration,
) -> Result<SyncReport, GraphError> {
    let started = Instant::now();
    let _lock = scanner::DbLockGuard::acquire_within(db_path, lock_timeout)?;
    let diff = scanner::scan_diff_with_lock_held(db_path, worktree_root, mode)?;
    maybe_fail_after_scan(db_path)?;
    maybe_wait_after_scan(db_path);
    maybe_panic_after_scan(db_path);
    Ok(
        match write(db_path, worktree_root, mode, diff, None, started)? {
            SyncOutcome::Completed(report) | SyncOutcome::Cancelled(report) => report,
        },
    )
}

/// Observer-driven sync used by [`crate::Graph::sync_with_observer`].
///
/// Deliberately does not go through [`coalesced`]: it is intended for a
/// single dedicated indexing thread that owns its database exclusively, so
/// the concurrent-caller dedup that `run` needs does not apply here.
pub(crate) fn run_with_observer(
    db_path: &Path,
    worktree_root: &Path,
    mode: SyncMode,
    observer: &dyn SyncObserver,
) -> Result<SyncOutcome, GraphError> {
    let started = Instant::now();
    let _lock = scanner::DbLockGuard::acquire(db_path)?;
    let diff = scanner::scan_diff_with_lock_held(db_path, worktree_root, mode)?;
    maybe_fail_after_scan(db_path)?;
    maybe_wait_after_scan(db_path);
    write(db_path, worktree_root, mode, diff, Some(observer), started)
}

/// Writes what the scan found: pass 1, then pass 2, whose commit makes the
/// written files current. A sync that finds an earlier one unfinished runs
/// pass 2 even with nothing changed, re-resolving every stored ref.
fn write(
    db_path: &Path,
    worktree_root: &Path,
    mode: SyncMode,
    diff: scanner::Diff,
    observer: Option<&dyn SyncObserver>,
    started: Instant,
) -> Result<SyncOutcome, GraphError> {
    let recovering = sync_pending(db_path)?;
    let mut report = SyncReport {
        files_indexed: 0,
        files_changed: 0,
        files_removed: 0,
        duration: Duration::ZERO,
        failed: diff.failed.clone(),
        skipped: skips(&diff.oversize),
        database_path: db_path.to_path_buf(),
        branch: String::new(),
    };
    if !diff.has_changes() && !recovering {
        report.files_indexed = count_indexed_files(db_path)?;
        report.duration = started.elapsed();
        return Ok(SyncOutcome::Completed(report));
    }
    let before = if recovering || mode == SyncMode::Full {
        None
    } else {
        Some(definitions_before_pass1(db_path, &diff)?)
    };
    let pass1 = pass1::run(db_path, worktree_root, mode, &diff, observer)?;
    report.files_indexed = pass1.files_indexed;
    report.files_changed = pass1.files_written;
    report.files_removed = pass1.files_removed;
    report.failed.extend(pass1.failed);
    report
        .failed
        .sort_by(|left, right| left.path.cmp(&right.path));
    report.skipped.extend(skips(&pass1.oversize));
    if pass1.cancelled {
        // Pass 2 does not run, so no file pass 1 wrote becomes current; the
        // next sync extracts them again (`STD-03 §R8`).
        report.duration = started.elapsed();
        return Ok(SyncOutcome::Cancelled(report));
    }
    inject_fault(FaultPoint::AfterPass1);
    let reresolve = match &before {
        Some(before) => pass2::Reresolve::Dependents(before),
        None => pass2::Reresolve::All,
    };
    pass2::run(
        db_path,
        mode,
        pass1.refs,
        reresolve,
        observer,
        pass1.total_files,
        pass1.last_touched_path,
    )?;
    report.duration = started.elapsed();
    Ok(SyncOutcome::Completed(report))
}

fn skips(paths: &[PathBuf]) -> Vec<SyncSkip> {
    paths
        .iter()
        .map(|path| SyncSkip {
            path: scanner::normalize_path(path),
            reason: "oversize".to_string(),
        })
        .collect()
}

/// What the files an incremental sync is about to rewrite or remove define
/// now. Pass 2 compares it with what they define afterwards to find the refs
/// elsewhere that must be re-resolved; a full sync rewrites every ref, so it
/// needs none.
fn definitions_before_pass1(
    db_path: &Path,
    diff: &scanner::Diff,
) -> Result<pass2::Definitions, GraphError> {
    let paths = diff
        .modified
        .iter()
        .chain(&diff.deleted)
        .map(|path| scanner::normalize_path(path))
        .collect::<Vec<_>>();
    pass2::Definitions::load(db_path, paths.iter().map(String::as_str))
}

/// `meta` key present while a sync has written pass 1 rows that its pass 2
/// has not yet committed.
const SYNC_PENDING_KEY: &str = "sync_pending";

/// Records, before pass 1's first write, that the database holds an
/// unfinished sync. Pass 2 clears it in the transaction that commits.
pub(crate) fn mark_sync_pending(conn: &mut Connection) -> Result<(), GraphError> {
    conn.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, '1')",
        [SYNC_PENDING_KEY],
    )
    .map_err(|source| GraphError::sqlite("mark graph sync pending", source))?;
    Ok(())
}

pub(crate) fn clear_sync_pending(tx: &rusqlite::Transaction<'_>) -> Result<(), GraphError> {
    tx.execute("DELETE FROM meta WHERE key = ?1", [SYNC_PENDING_KEY])
        .map_err(|source| GraphError::sqlite("clear graph sync pending", source))?;
    Ok(())
}

/// Whether an earlier sync stopped between pass 1 and pass 2's commit.
fn sync_pending(db_path: &Path) -> Result<bool, GraphError> {
    let conn = Connection::open(db_path)
        .map_err(|source| GraphError::sqlite("open graph database for sync state", source))?;
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM meta WHERE key = ?1)",
        [SYNC_PENDING_KEY],
        |row| row.get::<_, bool>(0),
    )
    .map_err(|source| GraphError::sqlite("read graph sync state", source))
}

/// A failure of `operation` on the worktree-relative `path`.
pub(crate) fn io_failure(path: &Path, operation: &str, error: &std::io::Error) -> SyncFailure {
    SyncFailure {
        path: display_rel_path(path),
        operation: operation.to_string(),
        error_kind: io_error_kind(error).to_string(),
        message: error.to_string(),
    }
}

/// A failure on the worktree-relative `path` from a graph error.
pub(crate) fn graph_failure(path: &Path, error: &GraphError) -> SyncFailure {
    let (operation, error_kind) = match error {
        GraphError::Io { operation, .. } => (*operation, "io"),
        GraphError::Sqlite { operation, .. } => (*operation, "sqlite"),
        GraphError::InvalidData { operation, .. } => (*operation, "invalid_data"),
        GraphError::IndexMissing { .. } => ("open graph index", "index_missing"),
        GraphError::IndexIncompatible { .. } => ("open graph index", "index_incompatible"),
        GraphError::Unimplemented => ("sync", "unimplemented"),
    };
    SyncFailure {
        path: display_rel_path(path),
        operation: operation.to_string(),
        error_kind: error_kind.to_string(),
        message: error.to_string(),
    }
}

/// Stable class of an I/O error, reported as [`SyncFailure::error_kind`].
pub(crate) fn io_error_kind(error: &std::io::Error) -> &'static str {
    match error.kind() {
        std::io::ErrorKind::PermissionDenied => "permission_denied",
        std::io::ErrorKind::NotFound => "not_found",
        std::io::ErrorKind::InvalidData => "invalid_data",
        _ => "io",
    }
}

fn display_rel_path(path: &Path) -> String {
    let path = scanner::normalize_path(path);
    if path.is_empty() {
        ".".to_string()
    } else {
        path
    }
}

/// Environment variable naming a point where the sync process aborts, for
/// tests that interrupt a sync through the real `orbit-graph` binary:
/// `after-pass1` aborts once pass 1 has committed, `mid-pass2` halfway
/// through pass 2's refs, before its commit. An abort stands in for a kill.
/// Unset, or any other value, injects nothing.
pub(crate) const FAULT_INJECT_ENV: &str = "ORBIT_GRAPH_FAULT_INJECT";

/// A point [`FAULT_INJECT_ENV`] can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FaultPoint {
    AfterPass1,
    MidPass2,
}

impl FaultPoint {
    fn name(self) -> &'static str {
        match self {
            Self::AfterPass1 => "after-pass1",
            Self::MidPass2 => "mid-pass2",
        }
    }
}

/// Aborts the process when [`FAULT_INJECT_ENV`] names `point`.
pub(crate) fn inject_fault(point: FaultPoint) {
    if std::env::var_os(FAULT_INJECT_ENV).is_some_and(|value| value == point.name()) {
        std::process::abort();
    }
}

type SyncResult = Result<SyncReport, GraphError>;

struct InFlightSync {
    result: Mutex<Option<SyncResult>>,
    ready: Condvar,
    /// The leader, named in a follower's timeout error.
    leader: LockHolder,
}

/// Runs `run` once per database at a time within this process.
///
/// The first caller for a database leads and runs `run`; callers arriving
/// while it runs follow and share its result. A follower waits at most
/// `timeout`, the same bound as the database lock, and then fails naming the
/// leader (`STD-03 §R7`, `§R22`). The leader's [`LeaderGuard`] publishes a
/// failure and clears the in-flight entry even when `run` panics, so neither
/// followers nor later syncs wait on a dead leader (`STD-03 §R4`).
fn coalesced<F>(db_path: &Path, timeout: Duration, run: F) -> SyncResult
where
    F: FnOnce() -> SyncResult,
{
    let key = db_path.to_path_buf();
    let (state, leader) = {
        let mut in_flight = in_flight_syncs()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(state) = in_flight.get(&key) {
            (Arc::clone(state), false)
        } else {
            let state = Arc::new(InFlightSync {
                result: Mutex::new(None),
                ready: Condvar::new(),
                leader: LockHolder::current(coalesced_leader_label()),
            });
            in_flight.insert(key.clone(), Arc::clone(&state));
            (state, true)
        }
    };

    if leader {
        let guard = LeaderGuard {
            key: key.as_path(),
            state: state.as_ref(),
        };
        note_sync_leader_started(key.as_path());
        let result = run();
        guard.publish(result.clone());
        result
    } else {
        note_sync_follower_waiting(key.as_path());
        let slot = state
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (slot, _) = state
            .ready
            .wait_timeout_while(slot, timeout, |slot| slot.is_none())
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        slot.as_ref().map_or_else(
            || {
                Err(lock::timeout_error(
                    "wait for in-flight graph sync",
                    key.as_path(),
                    timeout,
                    Some(&state.leader),
                ))
            },
            Clone::clone,
        )
    }
}

fn coalesced_leader_label() -> String {
    let thread = std::thread::current();
    lock::holder_label(&format!(
        "graph sync on thread {}",
        thread.name().unwrap_or("<unnamed>")
    ))
}

/// Publishes the leader's outcome and clears its in-flight entry on drop.
///
/// Dropped without [`LeaderGuard::publish`], as when the leader panics, it
/// publishes a failure, so it defaults to the failure outcome and never
/// panics itself.
#[must_use = "the guard publishes the leader's result when dropped"]
struct LeaderGuard<'a> {
    key: &'a Path,
    state: &'a InFlightSync,
}

impl LeaderGuard<'_> {
    fn publish(self, result: SyncResult) {
        self.set_result(result);
    }

    fn set_result(&self, result: SyncResult) {
        let mut slot = self
            .state
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.is_none() {
            *slot = Some(result);
        }
        self.state.ready.notify_all();
    }
}

impl Drop for LeaderGuard<'_> {
    fn drop(&mut self) {
        self.set_result(Err(GraphError::invalid_data(
            "run graph sync",
            "the coalesced sync leader panicked before publishing a result",
        )));
        let mut in_flight = in_flight_syncs()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if in_flight
            .get(self.key)
            .is_some_and(|entry| std::ptr::eq(entry.as_ref(), self.state))
        {
            in_flight.remove(self.key);
        }
    }
}

fn in_flight_syncs() -> &'static Mutex<HashMap<PathBuf, Arc<InFlightSync>>> {
    static IN_FLIGHT_SYNCS: OnceLock<Mutex<HashMap<PathBuf, Arc<InFlightSync>>>> = OnceLock::new();
    IN_FLIGHT_SYNCS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn count_indexed_files(db_path: &Path) -> Result<usize, GraphError> {
    let conn = Connection::open(db_path)
        .map_err(|source| GraphError::sqlite("open graph database for sync count", source))?;
    let count = conn
        .query_row("SELECT count(*) FROM files", [], |row| row.get::<_, i64>(0))
        .map_err(|source| GraphError::sqlite("count indexed graph files", source))?;
    usize::try_from(count)
        .map_err(|source| GraphError::invalid_data("count indexed graph files", source.to_string()))
}

#[cfg(test)]
fn maybe_fail_after_scan(db_path: &Path) -> Result<(), GraphError> {
    let mut paths = fail_after_scan_paths()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if paths.remove(db_path) {
        return Err(GraphError::invalid_data(
            "run graph sync",
            "injected sync failure after scan",
        ));
    }
    Ok(())
}

#[cfg(not(test))]
fn maybe_fail_after_scan(_db_path: &Path) -> Result<(), GraphError> {
    Ok(())
}

#[cfg(test)]
fn maybe_wait_after_scan(db_path: &Path) {
    if let Some(gate) = sync_after_scan_gate(db_path) {
        gate.mark_started();
        gate.wait_released();
    }
}

#[cfg(not(test))]
fn maybe_wait_after_scan(_db_path: &Path) {}

/// Fault hook: the next sync of a database registered with
/// [`panic_next_sync_after_scan`] waits for its gate, then panics while it
/// leads and holds the database lock.
#[cfg(test)]
fn maybe_panic_after_scan(db_path: &Path) {
    let gate = panic_after_scan_gates()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(db_path);
    if let Some(gate) = gate {
        gate.mark_started();
        gate.wait_released();
        panic!("injected graph sync leader panic");
    }
}

#[cfg(not(test))]
fn maybe_panic_after_scan(_db_path: &Path) {}

#[cfg(test)]
pub(crate) fn panic_next_sync_after_scan(db_path: &Path, gate: Arc<SyncLeaderGate>) {
    panic_after_scan_gates()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(db_path.to_path_buf(), gate);
}

#[cfg(test)]
fn panic_after_scan_gates() -> &'static Mutex<HashMap<PathBuf, Arc<SyncLeaderGate>>> {
    static PANIC_AFTER_SCAN: OnceLock<Mutex<HashMap<PathBuf, Arc<SyncLeaderGate>>>> =
        OnceLock::new();
    PANIC_AFTER_SCAN.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(test)]
fn note_sync_follower_waiting(db_path: &Path) {
    let (counts, changed) = sync_follower_counts();
    let mut counts = counts
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *counts.entry(db_path.to_path_buf()).or_insert(0) += 1;
    changed.notify_all();
}

#[cfg(not(test))]
fn note_sync_follower_waiting(_db_path: &Path) {}

/// Waits until `count` followers have joined syncs of `db_path`.
#[cfg(test)]
pub(crate) fn wait_for_sync_followers(
    db_path: &Path,
    count: usize,
    timeout: std::time::Duration,
) -> bool {
    let (counts, changed) = sync_follower_counts();
    let counts = counts
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (counts, _) = changed
        .wait_timeout_while(counts, timeout, |counts| {
            counts.get(db_path).copied().unwrap_or(0) < count
        })
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    counts.get(db_path).copied().unwrap_or(0) >= count
}

#[cfg(test)]
fn sync_follower_counts() -> &'static (Mutex<HashMap<PathBuf, usize>>, Condvar) {
    static SYNC_FOLLOWERS: OnceLock<(Mutex<HashMap<PathBuf, usize>>, Condvar)> = OnceLock::new();
    SYNC_FOLLOWERS.get_or_init(|| (Mutex::new(HashMap::new()), Condvar::new()))
}

#[cfg(test)]
pub(crate) fn fail_next_sync_after_scan(db_path: &Path) {
    // L-0051: scope injected sync failures by DB path because orbit-graph tests run in parallel.
    fail_after_scan_paths()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(db_path.to_path_buf());
}

#[cfg(test)]
fn fail_after_scan_paths() -> &'static Mutex<std::collections::BTreeSet<PathBuf>> {
    static FAIL_AFTER_SCAN: OnceLock<Mutex<std::collections::BTreeSet<PathBuf>>> = OnceLock::new();
    FAIL_AFTER_SCAN.get_or_init(|| Mutex::new(std::collections::BTreeSet::new()))
}

#[cfg(test)]
fn note_sync_leader_started(db_path: &Path) {
    let mut counts = sync_leader_counts()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *counts.entry(db_path.to_path_buf()).or_insert(0) += 1;
    drop(counts);

    if let Some(gate) = sync_leader_gate() {
        gate.mark_started();
        gate.wait_released();
    }
}

#[cfg(not(test))]
fn note_sync_leader_started(_db_path: &Path) {}

#[cfg(test)]
pub(crate) fn sync_leader_count(db_path: &Path) -> usize {
    sync_leader_counts()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(db_path)
        .copied()
        .unwrap_or(0)
}

#[cfg(test)]
fn sync_leader_counts() -> &'static Mutex<std::collections::BTreeMap<PathBuf, usize>> {
    static SYNC_LEADER_COUNTS: OnceLock<Mutex<std::collections::BTreeMap<PathBuf, usize>>> =
        OnceLock::new();
    SYNC_LEADER_COUNTS.get_or_init(|| Mutex::new(std::collections::BTreeMap::new()))
}

#[cfg(test)]
pub(crate) struct SyncLeaderGate {
    started: (Mutex<bool>, Condvar),
    release: (Mutex<bool>, Condvar),
}

#[cfg(test)]
impl SyncLeaderGate {
    pub(crate) fn new() -> Self {
        Self {
            started: (Mutex::new(false), Condvar::new()),
            release: (Mutex::new(false), Condvar::new()),
        }
    }

    fn mark_started(&self) {
        let (lock, ready) = &self.started;
        let mut started = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *started = true;
        ready.notify_all();
    }

    pub(crate) fn wait_started(&self, timeout: std::time::Duration) -> bool {
        let (lock, ready) = &self.started;
        let started = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (started, _) = ready
            .wait_timeout_while(started, timeout, |started| !*started)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *started
    }

    fn wait_released(&self) {
        let (lock, ready) = &self.release;
        let released = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _released = ready
            .wait_while(released, |released| !*released)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }

    pub(crate) fn release(&self) {
        let (lock, ready) = &self.release;
        let mut released = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *released = true;
        ready.notify_all();
    }
}

#[cfg(test)]
pub(crate) fn set_sync_leader_gate(gate: Option<Arc<SyncLeaderGate>>) {
    let mut slot = sync_leader_gate_slot()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *slot = gate;
}

#[cfg(test)]
pub(crate) fn set_sync_after_scan_gate(db_path: PathBuf, gate: Option<Arc<SyncLeaderGate>>) {
    let mut slot = sync_after_scan_gate_slot()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *slot = gate.map(|gate| (db_path, gate));
}

#[cfg(test)]
fn sync_leader_gate() -> Option<Arc<SyncLeaderGate>> {
    sync_leader_gate_slot()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

#[cfg(test)]
fn sync_leader_gate_slot() -> &'static Mutex<Option<Arc<SyncLeaderGate>>> {
    static SYNC_LEADER_GATE: OnceLock<Mutex<Option<Arc<SyncLeaderGate>>>> = OnceLock::new();
    SYNC_LEADER_GATE.get_or_init(|| Mutex::new(None))
}

#[cfg(test)]
fn sync_after_scan_gate(db_path: &Path) -> Option<Arc<SyncLeaderGate>> {
    sync_after_scan_gate_slot()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .filter(|(gate_path, _gate)| gate_path == db_path)
        .map(|(_gate_path, gate)| Arc::clone(gate))
}

#[cfg(test)]
type SyncAfterScanGateSlot = Mutex<Option<(PathBuf, Arc<SyncLeaderGate>)>>;

#[cfg(test)]
fn sync_after_scan_gate_slot() -> &'static SyncAfterScanGateSlot {
    static SYNC_AFTER_SCAN_GATE: OnceLock<SyncAfterScanGateSlot> = OnceLock::new();
    SYNC_AFTER_SCAN_GATE.get_or_init(|| Mutex::new(None))
}

#[cfg(test)]
#[path = "tests/mod.rs"]
mod tests;
