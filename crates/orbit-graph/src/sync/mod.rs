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
use crate::{GraphError, SyncMode, SyncObserver, SyncOutcome, SyncReport};

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
    if !diff.has_changes() {
        let duration = started.elapsed();
        return Ok(SyncReport {
            files_indexed: count_indexed_files(db_path)?,
            files_changed: 0,
            files_removed: 0,
            duration,
        });
    }
    let before = definitions_before_pass1(db_path, mode, &diff)?;
    let pass1 = pass1::run(db_path, worktree_root, mode, &diff, None)?;
    let total_files = pass1.total_files;
    let last_touched_path = pass1.last_touched_path.clone();
    pass2::run(
        db_path,
        mode,
        pass1.refs,
        &before,
        None,
        total_files,
        last_touched_path,
    )?;
    let duration = started.elapsed();

    Ok(SyncReport {
        files_indexed: pass1.files_indexed,
        files_changed: pass1.files_written,
        files_removed: pass1.files_removed,
        duration,
    })
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
    if !diff.has_changes() {
        let duration = started.elapsed();
        return Ok(SyncOutcome::Completed(SyncReport {
            files_indexed: count_indexed_files(db_path)?,
            files_changed: 0,
            files_removed: 0,
            duration,
        }));
    }
    let before = definitions_before_pass1(db_path, mode, &diff)?;
    let pass1 = pass1::run(db_path, worktree_root, mode, &diff, Some(observer))?;
    if pass1.cancelled {
        let duration = started.elapsed();
        return Ok(SyncOutcome::Cancelled(SyncReport {
            files_indexed: pass1.files_indexed,
            files_changed: pass1.files_written,
            files_removed: pass1.files_removed,
            duration,
        }));
    }
    let total_files = pass1.total_files;
    let last_touched_path = pass1.last_touched_path.clone();
    pass2::run(
        db_path,
        mode,
        pass1.refs,
        &before,
        Some(observer),
        total_files,
        last_touched_path,
    )?;
    let duration = started.elapsed();

    Ok(SyncOutcome::Completed(SyncReport {
        files_indexed: pass1.files_indexed,
        files_changed: pass1.files_written,
        files_removed: pass1.files_removed,
        duration,
    }))
}

/// What the files an incremental sync is about to rewrite or remove define
/// now. Pass 2 compares it with what they define afterwards to find the refs
/// elsewhere that must be re-resolved; a full sync rewrites every ref, so it
/// needs none.
fn definitions_before_pass1(
    db_path: &Path,
    mode: SyncMode,
    diff: &scanner::Diff,
) -> Result<pass2::Definitions, GraphError> {
    if mode == SyncMode::Full {
        return Ok(pass2::Definitions::default());
    }
    let paths = diff
        .modified
        .iter()
        .chain(&diff.deleted)
        .map(|path| scanner::normalize_path(path))
        .collect::<Vec<_>>();
    pass2::Definitions::load(db_path, paths.iter().map(String::as_str))
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
