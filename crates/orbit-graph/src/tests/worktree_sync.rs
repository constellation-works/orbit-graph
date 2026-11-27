use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::support::init_fixture_repository;
use crate::sync::fail_next_sync_after_scan;
use crate::worktree_sync::require_indexed_files;
use crate::{
    EXTRACTOR_VERSION, Graph, GraphError, SyncFailure, SyncMode, SyncPolicy, SyncReport,
    sync_worktree,
};

/// Cleanup runs before indexing: an indexing failure does not leave the free
/// obsolete database behind. A locked one is kept.
#[test]
fn obsolete_databases_are_removed_before_indexing_and_locked_ones_are_kept() {
    let worktree = fixture_worktree();
    let active = active_database(worktree.path());
    let free = plant_older_database(&active, "free");
    let locked = plant_older_database(&active, "locked");
    let held = fs::File::create(sidecar(&locked, ".lock")).expect("create held lock");
    held.lock().expect("hold the older database lock");
    fail_next_sync_after_scan(&active);

    let error = sync_worktree(worktree.path(), SyncMode::Auto).expect_err("indexing must fail");

    assert!(
        error.to_string().contains("injected sync failure"),
        "{error}"
    );
    assert!(!free.exists(), "cleanup had not run when indexing failed");
    assert!(locked.exists(), "a locked database was removed");

    // Explicit, so a descriptor a concurrent test's child inherited cannot
    // keep the lock.
    held.unlock().expect("release the older database lock");
    let report = sync_worktree(worktree.path(), SyncMode::Auto).expect("sync");
    assert_eq!(report.files_indexed, 1, "{report:?}");
    assert_eq!(report.database_path, active);
    assert!(!locked.exists(), "the released database was kept");
}

#[test]
fn a_graph_that_cannot_be_opened_removes_no_database() {
    let worktree = fixture_worktree();
    let active = active_database(worktree.path());
    let older = plant_older_database(&active, "older");
    fs::write(&active, b"not a SQLite database").expect("corrupt the active database");

    sync_worktree(worktree.path(), SyncMode::Auto).expect_err("open must fail");

    assert!(older.exists(), "cleanup ran although the open failed");
}

#[test]
fn failures_are_fatal_only_when_no_file_is_indexed() {
    let error = require_indexed_files(report(0, &["a.rs", "b.rs"]))
        .expect_err("nothing indexed with failures must fail");
    assert!(matches!(error, GraphError::InvalidData { .. }), "{error:?}");
    assert_eq!(
        error.to_string(),
        "sync graph: indexed no file: 2 path(s) failed; first: a.rs (read: denied)"
    );

    for successful in [report(1, &["a.rs"]), report(1, &[]), report(0, &[])] {
        let expected = successful.clone();
        assert_eq!(
            require_indexed_files(successful).expect("a successful sync"),
            expected
        );
    }
}

fn fixture_worktree() -> tempfile::TempDir {
    let worktree = tempfile::tempdir().expect("create worktree");
    init_fixture_repository(worktree.path(), "main");
    fs::write(worktree.path().join("lib.rs"), "pub fn indexed() {}\n").expect("write source");
    worktree
}

/// Creates the worktree's active database, empty, and returns its path.
fn active_database(worktree_root: &Path) -> PathBuf {
    Graph::open(worktree_root, SyncPolicy::Manual)
        .expect("open graph")
        .db_path()
        .path()
        .to_path_buf()
}

fn plant_older_database(active: &Path, branch: &str) -> PathBuf {
    let older = active.with_file_name(format!("{branch}.{}.db", EXTRACTOR_VERSION - 1));
    fs::write(&older, b"an older extractor's database").expect("plant older database");
    older
}

fn sidecar(db_path: &Path, suffix: &str) -> PathBuf {
    let mut name = db_path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn report(files_indexed: usize, failed: &[&str]) -> SyncReport {
    SyncReport {
        files_indexed,
        files_changed: files_indexed,
        files_removed: 0,
        duration: Duration::ZERO,
        failed: failed
            .iter()
            .map(|path| SyncFailure {
                path: (*path).to_string(),
                operation: "read".to_string(),
                error_kind: "permission_denied".to_string(),
                message: "denied".to_string(),
            })
            .collect(),
        skipped: Vec::new(),
        database_path: PathBuf::new(),
        branch: "main".to_string(),
    }
}
