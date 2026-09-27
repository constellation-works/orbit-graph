//! Background watcher for long-lived graph handles.
//!
//! Overflow policy (`STD-03 §R2`): file events reach the sync thread through a
//! channel of [`EVENT_CHANNEL_CAPACITY`] events. When it is full, the notify
//! callback drops the event and counts it, and the sync thread then schedules
//! a sync. Every watcher sync rescans the whole worktree, so a dropped event
//! loses no change. Dropping the watcher waits at most [`JOIN_TIMEOUT`] for the
//! thread, then detaches it (`STD-03 §R22`).
//!
//! Only an event that can change the worktree schedules a sync. The notify
//! backend also reports reads (on Linux, inotify's `IN_OPEN` for every file
//! and directory a sync opens), so honouring them made each sync schedule the
//! next one: an idle watched graph rescanned itself every debounce, forever
//! (ORB-13169).

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use notify::event::{AccessKind, AccessMode};
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::{GraphError, SyncMode};

const IDLE_POLL: Duration = Duration::from_millis(50);
/// Most file events buffered between the notify callback and the sync thread.
const EVENT_CHANNEL_CAPACITY: usize = 1024;
/// Longest the watcher waits for its thread to report startup.
const START_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest dropping the watcher waits for its thread to stop.
const JOIN_TIMEOUT: Duration = Duration::from_secs(5);
const IGNORED_TOP_LEVEL_DIRS: &[&str] = &[
    ".git",
    ".orbit-graph",
    ".orbit",
    "build",
    "dist",
    "node_modules",
    "target",
    "venv",
    ".venv",
    "__pycache__",
];

#[must_use = "dropping the watcher stops it"]
pub(crate) struct SyncWatcher {
    stop: Arc<AtomicBool>,
    finished: Arc<Finished>,
    handle: Option<JoinHandle<()>>,
}

impl SyncWatcher {
    pub(crate) fn start(
        db_path: PathBuf,
        worktree_root: PathBuf,
        debounce: Duration,
    ) -> Result<Self, GraphError> {
        let stop = Arc::new(AtomicBool::new(false));
        let ready = Arc::new((Mutex::new(None), Condvar::new()));
        let thread_stop = Arc::clone(&stop);
        let thread_ready = Arc::clone(&ready);
        let (finished, handle) = spawn_stoppable("orbit-graph-sync-watcher", move || {
            watcher_thread(db_path, worktree_root, debounce, thread_stop, thread_ready);
        })
        .map_err(|source| {
            GraphError::invalid_data("spawn graph sync watcher", source.to_string())
        })?;

        let start_result = wait_for_start(ready.as_ref(), START_TIMEOUT);
        if let Err(reason) = start_result {
            stop_and_join(&stop, &finished, handle, JOIN_TIMEOUT);
            return Err(GraphError::invalid_data("start graph sync watcher", reason));
        }

        Ok(Self {
            stop,
            finished,
            handle: Some(handle),
        })
    }
}

impl Drop for SyncWatcher {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            stop_and_join(&self.stop, &self.finished, handle, JOIN_TIMEOUT);
        }
    }
}

/// Set once a stoppable thread's body has returned or unwound.
type Finished = (Mutex<bool>, Condvar);

/// Marks its thread finished on drop, including during unwind.
struct FinishedGuard(Arc<Finished>);

impl Drop for FinishedGuard {
    fn drop(&mut self) {
        let (lock, cvar) = self.0.as_ref();
        *lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        cvar.notify_all();
    }
}

fn spawn_stoppable(
    name: &str,
    body: impl FnOnce() + Send + 'static,
) -> std::io::Result<(Arc<Finished>, JoinHandle<()>)> {
    let finished = Arc::new((Mutex::new(false), Condvar::new()));
    let guard = FinishedGuard(Arc::clone(&finished));
    let handle = thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            let _finished = guard;
            body();
        })?;
    Ok((finished, handle))
}

/// Asks the thread to stop and joins it if it finishes within `timeout`;
/// otherwise detaches it with a warning. Returns whether it was joined.
fn stop_and_join(
    stop: &AtomicBool,
    finished: &Finished,
    handle: JoinHandle<()>,
    timeout: Duration,
) -> bool {
    stop.store(true, Ordering::Relaxed);
    let (lock, cvar) = finished;
    let done = lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (done, _) = cvar
        .wait_timeout_while(done, timeout, |done| !*done)
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !*done {
        tracing::warn!(
            timeout_ms = timeout.as_millis(),
            "graph sync watcher thread did not stop in time; detaching it"
        );
        return false;
    }
    drop(done);
    if handle.join().is_err() {
        tracing::warn!("graph sync watcher thread panicked during shutdown");
    }
    true
}

type StartState = (Mutex<Option<Result<(), String>>>, Condvar);

fn wait_for_start(ready: &StartState, timeout: Duration) -> Result<(), String> {
    let (lock, cvar) = ready;
    let state = lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (state, _) = cvar
        .wait_timeout_while(state, timeout, |state| state.is_none())
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.as_ref().cloned().unwrap_or_else(|| {
        Err(format!(
            "watcher did not report startup within {} ms",
            timeout.as_millis()
        ))
    })
}

fn watcher_thread(
    db_path: PathBuf,
    worktree_root: PathBuf,
    debounce: Duration,
    stop: Arc<AtomicBool>,
    ready: Arc<StartState>,
) {
    let (event_tx, event_rx) = mpsc::sync_channel(EVENT_CHANNEL_CAPACITY);
    let dropped = Arc::new(AtomicU64::new(0));
    let callback_dropped = Arc::clone(&dropped);
    let mut watcher = match RecommendedWatcher::new(
        move |event| forward_event(&event_tx, &callback_dropped, event),
        Config::default(),
    ) {
        Ok(watcher) => watcher,
        Err(error) => {
            notify_start(ready.as_ref(), Err(error.to_string()));
            return;
        }
    };
    if let Err(error) = watcher.watch(worktree_root.as_path(), RecursiveMode::Recursive) {
        notify_start(ready.as_ref(), Err(error.to_string()));
        return;
    }
    notify_start(ready.as_ref(), Ok(()));
    event_loop(
        &event_rx,
        &dropped,
        &stop,
        worktree_root.as_path(),
        debounce,
        &mut || run_background_sync(db_path.as_path(), worktree_root.as_path()),
    );
}

/// Queues `event` without blocking the notify thread. When the channel is
/// full the event is dropped and counted; the sync thread turns the count
/// into a full rescan.
fn forward_event(
    event_tx: &SyncSender<notify::Result<Event>>,
    dropped: &AtomicU64,
    event: notify::Result<Event>,
) {
    match event_tx.try_send(event) {
        Ok(()) | Err(TrySendError::Disconnected(_)) => {}
        Err(TrySendError::Full(_)) => {
            dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn notify_start(ready: &StartState, result: Result<(), String>) {
    let (lock, cvar) = ready;
    let mut state = lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *state = Some(result);
    cvar.notify_all();
}

/// Runs `run_sync` once no relevant event has arrived for `debounce`.
///
/// Every wait is at most [`IDLE_POLL`], so `stop` is honoured promptly
/// whatever the debounce.
fn event_loop(
    event_rx: &Receiver<notify::Result<Event>>,
    dropped: &AtomicU64,
    stop: &AtomicBool,
    worktree_root: &Path,
    debounce: Duration,
    run_sync: &mut dyn FnMut(),
) {
    let mut sync_due: Option<Instant> = None;
    let schedule = |now: Instant| Some(now.checked_add(debounce).unwrap_or(now));
    while !stop.load(Ordering::Relaxed) {
        probe::note_idle(worktree_root, sync_due.is_some());
        let dropped_events = dropped.swap(0, Ordering::Relaxed);
        if dropped_events > 0 {
            tracing::warn!(
                target: "orbit.graph.sync",
                dropped_events,
                "graph sync watcher event queue overflowed; scheduling full rescan"
            );
            sync_due = schedule(Instant::now());
        }
        let wait = sync_due.map_or(IDLE_POLL, |due| {
            due.saturating_duration_since(Instant::now()).min(IDLE_POLL)
        });
        match event_rx.recv_timeout(wait) {
            Ok(Ok(event)) => {
                probe::note_event(worktree_root, &event);
                if event_requires_sync(worktree_root, &event) {
                    sync_due = schedule(Instant::now());
                }
            }
            Ok(Err(error)) => {
                tracing::warn!(
                    target: "orbit.graph.sync",
                    error = %error,
                    "graph sync watcher reported an error; scheduling full diff"
                );
                sync_due = schedule(Instant::now());
            }
            Err(RecvTimeoutError::Timeout) => {
                if sync_due.is_some_and(|due| Instant::now() >= due) {
                    sync_due = None;
                    run_sync();
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn run_background_sync(db_path: &Path, worktree_root: &Path) {
    if let Err(error) = super::run(db_path, worktree_root, SyncMode::Auto) {
        tracing::warn!(
            target: "orbit.graph.sync",
            error = %error,
            "background graph sync failed"
        );
    }
}

/// Whether `event` may have changed what a sync would index.
///
/// An access that is not the close of a write reads the worktree without
/// changing it, whatever its path; a sync's own reads are such accesses. Any
/// other event, including an unclassified one or one without paths (a
/// backend's rescan request), schedules a sync unless every path it names is
/// in an ignored top-level directory.
fn event_requires_sync(worktree_root: &Path, event: &Event) -> bool {
    if matches!(event.kind, EventKind::Access(kind) if kind != AccessKind::Close(AccessMode::Write))
    {
        return false;
    }
    event.paths.is_empty()
        || event
            .paths
            .iter()
            .any(|path| path_requires_sync(worktree_root, path.as_path()))
}

fn path_requires_sync(worktree_root: &Path, path: &Path) -> bool {
    let relative = path.strip_prefix(worktree_root).unwrap_or(path);
    !matches!(
        relative.components().next(),
        Some(Component::Normal(name)) if IGNORED_TOP_LEVEL_DIRS
            .iter()
            .any(|ignored| name == *ignored)
    )
}

/// Test hooks that observe the event loop of a watched worktree.
///
/// [`probe::wait_for_settled`] is the induced delay the watch-policy tests
/// use in place of sleeps: it holds a test until the loop has classified a
/// sentinel event and then reached a pass with no sync due, which is the state
/// a loaded machine reaches when a test's reads outlast the debounce. Every
/// event the worktree produced before the sentinel has been classified by
/// then, and any sync they scheduled has run.
#[cfg(test)]
pub(crate) mod probe {
    use std::collections::{HashMap, HashSet};
    use std::path::{Path, PathBuf};
    use std::sync::{Condvar, Mutex, OnceLock};
    use std::time::Duration;

    use notify::Event;

    #[derive(Default)]
    struct LoopState {
        /// Paths named by events the loop has classified.
        seen: HashSet<PathBuf>,
        /// Seen paths after which the loop reached a pass with no sync due.
        settled: HashSet<PathBuf>,
    }

    type Probes = (Mutex<HashMap<PathBuf, LoopState>>, Condvar);

    fn probes() -> &'static Probes {
        static PROBES: OnceLock<Probes> = OnceLock::new();
        PROBES.get_or_init(|| (Mutex::new(HashMap::new()), Condvar::new()))
    }

    pub(super) fn note_event(worktree_root: &Path, event: &Event) {
        let (states, changed) = probes();
        let mut states = states
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        states
            .entry(worktree_root.to_path_buf())
            .or_default()
            .seen
            .extend(event.paths.iter().cloned());
        changed.notify_all();
    }

    pub(super) fn note_idle(worktree_root: &Path, sync_due: bool) {
        if sync_due {
            return;
        }
        let (states, changed) = probes();
        let mut states = states
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(state) = states.get_mut(worktree_root) {
            let seen = std::mem::take(&mut state.seen);
            state.settled.extend(seen);
            changed.notify_all();
        }
    }

    /// Waits until the watcher of `worktree_root` has classified an event on
    /// `sentinel` and then passed through its loop with no sync due. Returns
    /// whether that happened within `timeout`.
    pub(crate) fn wait_for_settled(
        worktree_root: &Path,
        sentinel: &Path,
        timeout: Duration,
    ) -> bool {
        let (states, changed) = probes();
        let states = states
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (states, _) = changed
            .wait_timeout_while(states, timeout, |states| {
                !states
                    .get(worktree_root)
                    .is_some_and(|state| state.settled.contains(sentinel))
            })
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        states
            .get(worktree_root)
            .is_some_and(|state| state.settled.contains(sentinel))
    }
}

#[cfg(not(test))]
mod probe {
    use std::path::Path;

    use notify::Event;

    pub(super) fn note_event(_worktree_root: &Path, _event: &Event) {}

    pub(super) fn note_idle(_worktree_root: &Path, _sync_due: bool) {}
}

#[cfg(test)]
#[path = "tests/watcher.rs"]
mod tests;
