//! An interrupted sync never leaves files current without their refs: the
//! next incremental `orbit-graph sync` restores the full ref set a rebuild
//! stores (`STD-03 §R8`, `§R9`). Each scenario interrupts a sync of the real
//! executable, at a fault point `ORBIT_GRAPH_FAULT_INJECT` names or by
//! cancelling a library sync, then syncs with the executable.

#![allow(clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

use orbit_graph::{Graph, SyncMode, SyncObserver, SyncOutcome, SyncPolicy, SyncProgress};
use serde_json::Value;
use tempfile::TempDir;

const FAULT_INJECT_ENV: &str = "ORBIT_GRAPH_FAULT_INJECT";
/// Source files in the chain fixture, each with one outbound call.
const CHAIN_FILES: usize = 24;

/// The P3 scenario: a first sync killed after pass 1 committed, or halfway
/// through pass 2, used to leave files current with no outbound refs, and no
/// later incremental sync repaired them.
#[test]
fn a_sync_killed_after_pass1_or_mid_pass2_is_repaired_by_the_next_sync() {
    for fault in ["after-pass1", "mid-pass2"] {
        let repo = chain_fixture();
        let interrupted = run_with_env(repo.path(), &["sync"], &[(FAULT_INJECT_ENV, fault)]);
        assert!(
            !interrupted.status.success(),
            "the {fault} fault must stop the sync: {}",
            String::from_utf8_lossy(&interrupted.stdout)
        );

        let report = run_json(repo.path(), &["sync"]);
        let db = db_path(repo.path());
        assert_eq!(
            files_without_refs(&db),
            Vec::<String>::new(),
            "{fault}: files left without outbound refs after {report}"
        );
        assert_eq!(
            report["files_changed"],
            CHAIN_FILES + 1,
            "{fault}: every file the interrupted sync wrote is extracted again: {report}"
        );
        assert_matches_full_rebuild(repo.path(), &db, fault);
    }
}

/// An interruption after pass 1 removed a duplicate definition loses what
/// the removed file defined; the next sync must still promote the fuzzy ref
/// that only the duplicate kept ambiguous.
#[test]
fn a_sync_killed_after_removing_a_definition_still_re_resolves_its_dependents() {
    for change in ["delete", "rewrite"] {
        let repo = duplicate_fixture();
        run_json(repo.path(), &["sync"]);
        assert_eq!(later_target_confidence(repo.path()), "fuzzy_name");

        if change == "delete" {
            fs::remove_file(repo.path().join("src/later_dup.rs")).expect("remove duplicate");
        } else {
            write(repo.path(), "src/later_dup.rs", "pub fn unrelated() {}\n");
        }
        let interrupted =
            run_with_env(repo.path(), &["sync"], &[(FAULT_INJECT_ENV, "after-pass1")]);
        assert!(
            !interrupted.status.success(),
            "{change}: fault must stop it"
        );

        run_json(repo.path(), &["sync"]);
        assert_eq!(
            later_target_confidence(repo.path()),
            "same_module",
            "{change}: the remaining unique definition is found"
        );
        assert_matches_full_rebuild(repo.path(), &db_path(repo.path()), change);
    }
}

/// A cancelled sync skips pass 2, so none of the files it wrote may count as
/// current.
#[test]
fn a_cancelled_sync_is_repaired_by_the_next_sync() {
    let repo = chain_fixture();
    let db = db_path(repo.path());
    {
        let graph = Graph::open(repo.path(), SyncPolicy::Manual).expect("open graph");
        assert_eq!(
            fs::canonicalize(graph.db_path().path()).expect("library database"),
            fs::canonicalize(&db).expect("CLI database"),
            "the library and the executable share one database"
        );
        let observer = CancelAfter::new(CHAIN_FILES / 2);
        let outcome = graph
            .sync_with_observer(SyncMode::Full, &observer)
            .expect("cancelled sync");
        assert!(matches!(outcome, SyncOutcome::Cancelled(_)), "{outcome:?}");
    }

    let report = run_json(repo.path(), &["sync"]);
    assert_eq!(
        files_without_refs(&db),
        Vec::<String>::new(),
        "files left without outbound refs after {report}"
    );
    assert_eq!(report["files_changed"], CHAIN_FILES + 1, "{report}");
    assert_matches_full_rebuild(repo.path(), &db, "cancel");
}

/// Stops pass 1 once `after` files are written.
struct CancelAfter {
    after: usize,
    written: AtomicUsize,
}

impl CancelAfter {
    fn new(after: usize) -> Self {
        Self {
            after,
            written: AtomicUsize::new(0),
        }
    }
}

impl SyncObserver for CancelAfter {
    fn on_progress(&self, progress: &SyncProgress) {
        self.written.store(progress.files_indexed, Ordering::SeqCst);
    }

    fn is_cancelled(&self) -> bool {
        self.written.load(Ordering::SeqCst) >= self.after
    }
}

/// The resolved refs now equal those a full rebuild of the same tree stores.
fn assert_matches_full_rebuild(repo: &Path, db: &Path, scenario: &str) {
    let recovered = all_refs(db);
    assert!(!recovered.is_empty(), "{scenario}: no refs at all");
    run_json(repo, &["sync", "--full"]);
    assert_eq!(
        recovered,
        all_refs(db),
        "{scenario}: refs differ from a full rebuild"
    );
}

/// `src/m00.rs` … each calling the next, so every file has an outbound ref.
fn chain_fixture() -> TempDir {
    let repo = TempDir::new().expect("create fixture repository");
    run_git(repo.path(), &["init", "-q", "-b", "main"]);
    let mut lib = String::new();
    for index in 0..CHAIN_FILES {
        lib.push_str(&format!("pub mod m{index:02};\n"));
        write(
            repo.path(),
            &format!("src/m{index:02}.rs"),
            &format!(
                "pub fn f{index:02}() -> usize {{\n    crate::m{next:02}::f{next:02}() + 1\n}}\n",
                next = (index + 1) % CHAIN_FILES
            ),
        );
    }
    lib.push_str("pub fn root() -> usize {\n    m00::f00()\n}\n");
    write(repo.path(), "src/lib.rs", &lib);
    repo
}

/// `plain_caller` calls `later_target`, which two files define, so the call
/// stays fuzzy until one of them goes.
fn duplicate_fixture() -> TempDir {
    let repo = TempDir::new().expect("create fixture repository");
    run_git(repo.path(), &["init", "-q", "-b", "main"]);
    write(
        repo.path(),
        "src/lib.rs",
        "mod later;\nmod later_dup;\nmod plain;\n",
    );
    write(
        repo.path(),
        "src/later.rs",
        "pub fn later_target() -> i32 {\n    2\n}\n",
    );
    write(
        repo.path(),
        "src/later_dup.rs",
        "pub fn later_target() -> i32 {\n    3\n}\n",
    );
    write(
        repo.path(),
        "src/plain.rs",
        "pub fn plain_caller() -> i32 {\n    later_target()\n}\n",
    );
    repo
}

fn later_target_confidence(repo: &Path) -> String {
    let callees = run_json(
        repo,
        &[
            "callees",
            "--include-unresolved",
            "symbol:src/plain.rs#plain_caller:function",
        ],
    );
    let matches = callees["callees"]
        .as_array()
        .expect("callees array")
        .iter()
        .filter(|callee| callee["target_name"] == "later_target")
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "{callees}");
    matches[0]["confidence"]
        .as_str()
        .expect("confidence")
        .to_string()
}

fn db_path(repo: &Path) -> PathBuf {
    PathBuf::from(
        run_json(repo, &["db-path"])["path"]
            .as_str()
            .expect("database path"),
    )
}

/// Indexed files with no outbound ref.
fn files_without_refs(db: &Path) -> Vec<String> {
    query(
        db,
        "SELECT path FROM files WHERE path NOT IN (SELECT from_file FROM refs) ORDER BY path",
    )
}

/// Every stored ref's location and resolution, without the rowid-based hint.
fn all_refs(db: &Path) -> Vec<String> {
    query(
        db,
        "SELECT from_file || ':' || from_span_start || '-' || from_span_end || ' ' || \
         target_name || ' -> ' || coalesce(target_qualified, '-') || ' ' || confidence \
         FROM refs ORDER BY 1",
    )
}

/// Rows of a one-column query, read with Python's sqlite3 as other tests of
/// the executable do.
fn query(db: &Path, sql: &str) -> Vec<String> {
    let script = r#"
import sqlite3, sys
conn = sqlite3.connect(sys.argv[1])
conn.execute("PRAGMA busy_timeout=5000")
for (value,) in conn.execute(sys.argv[2]):
    print(value)
"#;
    let output = Command::new("python3")
        .arg("-c")
        .arg(script)
        .arg(db)
        .arg(sql)
        .output()
        .expect("run python3 sqlite query");
    assert!(
        output.status.success(),
        "sqlite query failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("UTF-8 query output")
        .lines()
        .map(str::to_string)
        .collect()
}

fn write(repo: &Path, path: &str, contents: &str) {
    let target = repo.join(path);
    fs::create_dir_all(target.parent().expect("parent directory")).expect("create directory");
    fs::write(&target, contents).expect("write fixture file");
    // Incremental sync compares mtimes before hashing; make a rewrite within
    // the same timestamp tick still read as a change.
    let file = fs::File::options()
        .write(true)
        .open(&target)
        .expect("open fixture file");
    let modified = file
        .metadata()
        .and_then(|metadata| metadata.modified())
        .expect("fixture mtime");
    file.set_modified(modified + std::time::Duration::from_secs(2))
        .expect("bump fixture mtime");
}

fn run_json(cwd: &Path, args: &[&str]) -> Value {
    let output = run_with_env(cwd, args, &[]);
    assert!(
        output.status.success(),
        "orbit-graph {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON command output")
}

fn run_with_env(cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orbit-graph"));
    command
        .current_dir(cwd)
        .env_remove(FAULT_INJECT_ENV)
        .args(["--format", "json"])
        .args(args);
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().expect("run orbit-graph")
}

fn run_git(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}
