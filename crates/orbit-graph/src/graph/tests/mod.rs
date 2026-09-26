use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, params};

use crate::sync::{fail_next_sync_after_scan, scanner::scan_count, sync_leader_count};
use crate::{EXTRACTOR_VERSION, Graph, GraphError, SearchQuery, SyncPolicy, resolve_db_path};

#[test]
fn manual_policy_ensure_synced_is_noop() {
    let worktree = TestWorktree::new("manual-noop");
    worktree.write("src/lib.rs", "pub fn manual() {}\n");
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    let db_path = graph_db_path(worktree.path());

    graph.ensure_synced().expect("manual ensure");

    let conn = open_test_connection(worktree.path());
    assert_eq!(sync_leader_count(db_path.as_path()), 0);
    assert_eq!(row_count(&conn, "files"), 0);
    assert_eq!(meta_value(&conn, "last_incremental_at"), 0);
}

#[test]
fn on_read_policy_ensure_synced_syncs_on_every_call() {
    let worktree = TestWorktree::new("on-read");
    worktree.write("src/lib.rs", "pub fn on_read() {}\n");
    let graph = Graph::open(worktree.path(), SyncPolicy::OnRead).expect("open graph");
    let db_path = graph_db_path(worktree.path());

    graph.ensure_synced().expect("first on-read ensure");
    graph.ensure_synced().expect("second on-read ensure");

    let conn = open_test_connection(worktree.path());
    assert_eq!(sync_leader_count(db_path.as_path()), 2);
    assert_eq!(row_count(&conn, "files"), 1);
    assert!(meta_value(&conn, "last_incremental_at") > 0);
}

#[test]
fn windowed_policy_respects_recent_and_expired_sync_windows() {
    let worktree = TestWorktree::new("windowed");
    worktree.write("src/lib.rs", "pub fn windowed() {}\n");
    let graph = Graph::open(
        worktree.path(),
        SyncPolicy::Windowed {
            window: Duration::from_millis(500),
        },
    )
    .expect("open graph");
    let db_path = graph_db_path(worktree.path());

    graph.ensure_synced().expect("initial windowed ensure");
    graph.ensure_synced().expect("recent windowed ensure");
    assert_eq!(sync_leader_count(db_path.as_path()), 1);

    thread::sleep(Duration::from_millis(600));
    graph.ensure_synced().expect("expired windowed ensure");

    assert_eq!(sync_leader_count(db_path.as_path()), 2);
}

#[test]
fn windowed_policy_uses_process_local_success_timestamp_between_checks() {
    let worktree = TestWorktree::new("windowed-local-timestamp");
    worktree.write("src/lib.rs", "pub fn stale_meta() {}\n");
    let graph = Graph::open(
        worktree.path(),
        SyncPolicy::Windowed {
            window: Duration::from_millis(500),
        },
    )
    .expect("open graph");
    let db_path = graph_db_path(worktree.path());

    graph.ensure_synced().expect("initial windowed ensure");
    set_meta_value(worktree.path(), "last_incremental_at", 1);
    graph
        .ensure_synced()
        .expect("recent windowed ensure after out-of-band metadata update");

    assert_eq!(sync_leader_count(db_path.as_path()), 1);

    thread::sleep(Duration::from_millis(600));
    graph
        .ensure_synced()
        .expect("expired windowed ensure after local timestamp elapses");

    assert_eq!(sync_leader_count(db_path.as_path()), 2);
}

#[test]
fn windowed_policy_retries_after_sync_failure_without_advancing_timestamp() {
    let worktree = TestWorktree::new("windowed-retry");
    worktree.write("src/lib.rs", "pub fn retry_after_failure() {}\n");
    let graph = Graph::open(
        worktree.path(),
        SyncPolicy::Windowed {
            window: Duration::from_millis(500),
        },
    )
    .expect("open graph");
    let db_path = graph_db_path(worktree.path());

    fail_next_sync_after_scan(db_path.as_path());
    let result = graph.ensure_synced();

    assert!(matches!(
        result,
        Err(GraphError::InvalidData {
            operation: "run graph sync",
            ..
        })
    ));
    assert_eq!(sync_leader_count(db_path.as_path()), 1);
    assert_eq!(
        meta_value(
            &open_test_connection(worktree.path()),
            "last_incremental_at"
        ),
        0
    );

    graph.ensure_synced().expect("retry windowed ensure");

    assert_eq!(sync_leader_count(db_path.as_path()), 2);
    assert!(
        meta_value(
            &open_test_connection(worktree.path()),
            "last_incremental_at"
        ) > 0
    );
}

#[test]
fn watch_policy_repeated_reads_do_not_rescan_without_file_events() {
    let worktree = TestWorktree::new("watch-read-no-rescan");
    worktree.write("src/lib.rs", "pub fn watched_read_marker() {}\n");
    let graph = Graph::open(
        worktree.path(),
        SyncPolicy::Watch {
            // Keep the background debounce beyond these reads even when the
            // rest of the test suite runs concurrently.
            debounce: Duration::from_millis(250),
        },
    )
    .expect("open graph");

    for _ in 0..10 {
        let result = graph
            .search(&SearchQuery::new("watched_read_marker"))
            .expect("watch-backed search");
        assert_eq!(result.matches.len(), 1);
    }

    assert!(
        scan_count(worktree.path()) <= 1,
        "watch-backed reads should not trigger scanner walks: {}",
        scan_count(worktree.path())
    );
}

#[test]
fn watch_policy_refreshes_query_results_after_file_edit() {
    let worktree = TestWorktree::new("watch-freshness");
    worktree.write("src/lib.rs", "pub fn before_edit() {}\n");
    let graph = Graph::open(
        worktree.path(),
        SyncPolicy::Watch {
            debounce: Duration::from_millis(25),
        },
    )
    .expect("open graph");

    let before = graph
        .search(&SearchQuery::new("before_edit"))
        .expect("initial watched search");
    assert_eq!(before.matches.len(), 1);

    worktree.write("src/lib.rs", "pub fn fresh_after_edit() {}\n");

    assert!(
        wait_until(Duration::from_secs(5), || {
            graph
                .search(&SearchQuery::new("fresh_after_edit"))
                .expect("watched search after edit")
                .matches
                .len()
                == 1
        }),
        "watch-backed graph query did not observe edited file within freshness window"
    );
}

#[test]
fn clean_auto_sync_skips_pass_writes_and_preserves_persisted_sync_timestamp() {
    let worktree = TestWorktree::new("clean-auto-sync");
    worktree.write("src/lib.rs", "pub fn clean_auto_sync() {}\n");
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");

    graph
        .sync(crate::SyncMode::Auto)
        .expect("initial auto sync");
    let conn = open_test_connection(worktree.path());
    let initial_incremental_at = meta_value(&conn, "last_incremental_at");
    drop(conn);

    let report = graph.sync(crate::SyncMode::Auto).expect("clean auto sync");

    assert_eq!(report.files_changed, 0);
    assert_eq!(report.files_removed, 0);
    assert_eq!(
        meta_value(
            &open_test_connection(worktree.path()),
            "last_incremental_at"
        ),
        initial_incremental_at
    );
}

fn open_test_connection(worktree: &Path) -> Connection {
    let conn = Connection::open(graph_db_path(worktree)).expect("open graph database");
    conn.pragma_update(None, "foreign_keys", "ON")
        .expect("enable foreign keys");
    conn
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

fn meta_value(conn: &Connection, key: &str) -> i64 {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
        row.get::<_, String>(0)
    })
    .expect("read meta value")
    .parse()
    .expect("meta value is integer")
}

fn set_meta_value(worktree: &Path, key: &str, value: i64) {
    open_test_connection(worktree)
        .execute(
            "UPDATE meta SET value = ?1 WHERE key = ?2",
            params![value.to_string(), key],
        )
        .expect("update meta value");
}

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let started = std::time::Instant::now();
    while started.elapsed() < timeout {
        if condition() {
            return true;
        }
        thread::sleep(Duration::from_millis(25));
    }
    condition()
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
            "orbit-graph-policy-{name}-{}-{stamp}",
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
