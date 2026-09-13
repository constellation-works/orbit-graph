#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

use crate::sync::{SyncLeaderGate, set_sync_after_scan_gate};
use crate::{
    EXTRACTOR_VERSION, Graph, SyncMode, SyncObserver, SyncOutcome, SyncPolicy, SyncProgress,
    resolve_db_path,
};

/// Records every [`SyncProgress`] event and cancels once `threshold` files
/// have been processed (the initial pre-loop event, which carries no path,
/// does not count).
struct CancelAfterObserver {
    threshold: usize,
    processed: AtomicUsize,
    events: Mutex<Vec<SyncProgress>>,
}

impl CancelAfterObserver {
    fn new(threshold: usize) -> Self {
        Self {
            threshold,
            processed: AtomicUsize::new(0),
            events: Mutex::new(Vec::new()),
        }
    }

    fn touched_paths(&self) -> Vec<String> {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter_map(|event| event.current_path.clone())
            .collect()
    }
}

impl SyncObserver for CancelAfterObserver {
    fn on_progress(&self, progress: &SyncProgress) {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(progress.clone());
        if progress.current_path.is_some() {
            self.processed.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn is_cancelled(&self) -> bool {
        self.processed.load(Ordering::SeqCst) >= self.threshold
    }
}

#[test]
fn observer_sees_every_file_a_full_sync_touches() {
    let worktree = TestWorktree::new("observer-sees-every-file");
    worktree.write("src/a.rs", "pub fn a() {}\n");
    worktree.write("src/b.rs", "pub fn b() {}\n");
    worktree.write("src/c.rs", "pub fn c() {}\n");
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");

    let observer = CancelAfterObserver::new(usize::MAX);
    let outcome = graph
        .sync_with_observer(SyncMode::Full, &observer)
        .expect("observed sync succeeds");
    let report = match outcome {
        SyncOutcome::Completed(report) => report,
        SyncOutcome::Cancelled(_) => panic!("sync should not have been cancelled"),
    };
    assert_eq!(report.files_changed, 3);

    let mut touched = observer.touched_paths();
    touched.sort();
    assert_eq!(touched, vec!["src/a.rs", "src/b.rs", "src/c.rs"]);

    let events = observer
        .events
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(events.first().is_some_and(|first| first.files_indexed == 0));
    let mut previous = 0;
    for event in events.iter() {
        assert!(event.files_indexed >= previous, "files_indexed regressed");
        assert_eq!(event.files_seen, 3);
        previous = event.files_indexed;
    }
    assert_eq!(events.last().expect("at least one event").files_indexed, 3);
}

#[test]
fn cancel_between_files_leaves_the_store_consistent_and_the_next_sync_completes() {
    let worktree = TestWorktree::new("cancel-between-files");
    worktree.write("src/a.rs", "pub fn a() {}\n");
    worktree.write("src/b.rs", "pub fn b() {}\n");
    worktree.write("src/c.rs", "pub fn c() {}\n");
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");

    let observer = CancelAfterObserver::new(1);
    let outcome = graph
        .sync_with_observer(SyncMode::Full, &observer)
        .expect("observed sync succeeds despite cancellation");
    let cancelled_report = match outcome {
        SyncOutcome::Cancelled(report) => report,
        SyncOutcome::Completed(_) => panic!("sync should have been cancelled"),
    };
    assert_eq!(cancelled_report.files_changed, 1);
    assert_eq!(observer.touched_paths().len(), 1);

    let conn = open_test_connection(worktree.path());
    assert_eq!(
        row_count(&conn, "files"),
        1,
        "only the one processed file is indexed"
    );
    drop(conn);

    // Incremental mode reports only what its diff scan finds: the file
    // already indexed before cancellation looks unchanged, and the two never
    // touched still look new.
    let report = graph
        .sync(SyncMode::Auto)
        .expect("next sync completes fully after a cancellation");
    assert_eq!(
        report.files_changed, 2,
        "the two untouched files are indexed now"
    );

    let conn = open_test_connection(worktree.path());
    assert_eq!(row_count(&conn, "files"), 3);
    assert_eq!(row_count(&conn, "symbols"), 3);
    assert_eq!(duplicate_file_row_groups(&conn), 0);
}

#[cfg(unix)]
#[test]
fn concurrent_syncs_hold_flock_across_scan_and_writes() {
    let worktree = TestWorktree::new("whole-sync-flock");
    worktree.write("src/lib.rs", "pub fn original() {}\n");
    let first_graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open first graph");
    first_graph
        .sync(SyncMode::Full)
        .expect("seed initial graph rows");
    let db_path = first_graph.db_path().path().to_path_buf();

    // L-0055: the symlink path bypasses in-process coalescing while sharing the same lock file.
    let link = symlink_to(worktree.path(), "whole-sync-flock-link");
    let second_graph = Graph::open(link.as_path(), SyncPolicy::Manual).expect("open second graph");

    fs::remove_file(worktree.path().join("src/lib.rs")).expect("remove indexed file");
    let gate = Arc::new(SyncLeaderGate::new());
    set_sync_after_scan_gate(db_path.clone(), Some(Arc::clone(&gate)));

    let first = thread::spawn(move || first_graph.sync(SyncMode::Auto));
    assert!(gate.wait_started(Duration::from_secs(2)));

    worktree.write("src/lib.rs", "pub fn recreated() {}\n");
    let second = thread::spawn(move || second_graph.sync(SyncMode::Auto));
    let _second_finished_before_release = wait_until_finished(&second, Duration::from_secs(1));
    gate.release();

    first
        .join()
        .expect("join first sync")
        .expect("first sync succeeds");
    second
        .join()
        .expect("join second sync")
        .expect("second sync succeeds");
    set_sync_after_scan_gate(db_path, None);
    fs::remove_file(link).expect("remove symlink");

    let conn = open_test_connection(worktree.path());
    assert_eq!(row_count(&conn, "files"), 1);
    assert_eq!(duplicate_file_row_groups(&conn), 0);
}

#[cfg(unix)]
fn symlink_to(target: &Path, name: &str) -> PathBuf {
    let mut link = std::env::temp_dir();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after epoch")
        .as_nanos();
    link.push(format!("orbit-graph-{name}-{}-{stamp}", std::process::id()));
    std::os::unix::fs::symlink(target, link.as_path()).expect("create symlink");
    link
}

fn wait_until_finished<T>(handle: &thread::JoinHandle<T>, timeout: Duration) -> bool {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if handle.is_finished() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    handle.is_finished()
}

fn open_test_connection(worktree: &Path) -> Connection {
    Connection::open(graph_db_path(worktree)).expect("open graph database")
}

fn graph_db_path(worktree: &Path) -> PathBuf {
    resolve_db_path(worktree, "HEAD", EXTRACTOR_VERSION)
        .path()
        .to_path_buf()
}

fn row_count(conn: &Connection, table: &str) -> i64 {
    let sql = format!("SELECT count(*) FROM {table}");
    conn.query_row(&sql, [], |row| row.get(0))
        .expect("count rows")
}

fn duplicate_file_row_groups(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT count(*) FROM (
            SELECT path FROM files GROUP BY path HAVING count(*) > 1
         )",
        [],
        |row| row.get(0),
    )
    .expect("count duplicate file row groups")
}

struct TestWorktree {
    path: PathBuf,
}

impl TestWorktree {
    fn new(name: &str) -> Self {
        let mut path = std::env::temp_dir();
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time after epoch")
            .as_nanos();
        path.push(format!(
            "orbit-graph-sync-{name}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create test worktree");
        Self { path }
    }

    fn path(&self) -> &Path {
        self.path.as_path()
    }

    fn write(&self, rel: &str, content: &str) {
        let path = self.path.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent directory");
        }
        fs::write(path, content).expect("write file");
    }
}

impl Drop for TestWorktree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
