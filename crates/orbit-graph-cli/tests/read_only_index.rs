//! Read commands and `read_only` plugin tools never create, initialize or
//! delete index state (`STD-01 §R31`), through the packaged `orbit-graph`
//! executable: they answer from a read-only index without changing a byte,
//! report a missing index with the command that builds it, and leave every
//! database of another version alone. Only `sync` and `clean` remove a
//! database, and only a strictly older one whose lock is free (`STD-03 §R6`,
//! `§R10`).

#![cfg(unix)]
#![allow(clippy::expect_used)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use orbit_graph::{EXTRACTOR_VERSION, STORE_SCHEMA_VERSION};
use serde_json::{Value, json};
use tempfile::TempDir;

/// Every CLI read command that consults the worktree's indexes. `evaluate`
/// is absent: it builds throwaway indexes in isolated temporary workspaces
/// and never opens the worktree's own.
const CLI_READS: &[&[&str]] = &[
    &["search", "helper"],
    &["show", "symbol:src/lib.rs#helper:function"],
    &["refs", "symbol:src/lib.rs#helper:function"],
    &["callees", "symbol:src/lib.rs#entry:function"],
    &["impact", "symbol:src/lib.rs#helper:function"],
    &["trace", "command:ship"],
    &["overview"],
    &["implementors", "symbol:src/lib.rs#Renderer:trait"],
    &["deps", "file:src/lib.rs"],
    &["history", "status", "--branch", "main"],
    &["recommend", "--query", "helper"],
    &["db-path"],
];

/// Every `read_only` plugin tool with a valid request for the fixture.
fn plugin_reads() -> Vec<(&'static str, Value)> {
    let helper = "symbol:src/lib.rs#helper:function";
    vec![
        ("orbit.graph.version", json!({})),
        ("orbit.graph.status", json!({})),
        ("orbit.graph.recommend", json!({"query": "helper"})),
        ("orbit.graph.search", json!({"query": "helper"})),
        ("orbit.graph.show", json!({"selector": helper})),
        ("orbit.graph.refs", json!({"selector": helper})),
        (
            "orbit.graph.callees",
            json!({"selector": "symbol:src/lib.rs#entry:function"}),
        ),
        ("orbit.graph.impact", json!({"selector": helper})),
        ("orbit.graph.trace", json!({"command": "command:ship"})),
        ("orbit.graph.deps", json!({"selector": "file:src/lib.rs"})),
        ("orbit.graph.overview", json!({})),
    ]
}

/// The R31 gate: after `sync`, with `.orbit-graph/` and the plugin state made
/// read-only, every read succeeds and neither tree changes. Reads used to
/// create, initialize or delete files there.
#[test]
fn every_read_succeeds_on_a_read_only_index_and_changes_nothing() {
    if skip_as_root("every_read_succeeds_on_a_read_only_index_and_changes_nothing") {
        return;
    }
    let repo = fixture_repository();
    run_json(repo.path(), &["sync"]);
    run_json(repo.path(), &["history", "sync", "--branch", "main"]);
    let state = TempDir::new().expect("plugin state");
    for operation in ["graph_sync", "history_sync"] {
        plugin_ok(
            repo.path(),
            Some(state.path()),
            "orbit.graph.maintain",
            json!({"operation": operation}),
        );
    }

    let index = repo.path().join(".orbit-graph");
    let _index_guard = ReadOnlyTree::apply(&index);
    let _state_guard = ReadOnlyTree::apply(state.path());
    let index_before = snapshot(&index);
    let state_before = snapshot(state.path());

    for args in CLI_READS {
        let output = run_cli(repo.path(), args);
        assert!(
            output.status.success(),
            "{args:?} failed on a read-only index: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(snapshot(&index), index_before, "{args:?} changed the index");
    }
    let db_path = run_json(repo.path(), &["db-path"]);
    assert_eq!(db_path["exists"], true, "{db_path}");

    for (tool, input) in plugin_reads() {
        plugin_ok(repo.path(), Some(state.path()), tool, input.clone());
        assert_eq!(
            snapshot(state.path()),
            state_before,
            "{tool} changed the plugin state"
        );
    }
    // Without plugin state, status and recommend read the repository-local
    // history index.
    for (tool, input) in [
        ("orbit.graph.status", json!({})),
        ("orbit.graph.recommend", json!({"query": "helper"})),
    ] {
        plugin_ok(repo.path(), None, tool, input);
    }
    assert_eq!(snapshot(&index), index_before, "a plugin read changed it");
}

/// On a repository that was never synced every read fails with
/// `index_missing` naming the command that builds the index, and creates
/// nothing; `db-path` reports the would-be path.
#[test]
fn reads_on_a_never_synced_repository_report_index_missing_and_create_nothing() {
    let repo = fixture_repository();
    let index = repo.path().join(".orbit-graph");
    for args in CLI_READS {
        if args[0] == "db-path" {
            let report = run_json(repo.path(), args);
            assert_eq!(report["exists"], false, "{report}");
            assert!(
                report["path"]
                    .as_str()
                    .is_some_and(|path| Path::new(path).starts_with(&index)),
                "{report}"
            );
            continue;
        }
        let output = run_cli(repo.path(), args);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        let error: Value = serde_json::from_slice(&output.stderr).expect("JSON error");
        assert_eq!(error["error"]["code"], "index_missing", "{args:?}: {error}");
        let message = error["error"]["message"].as_str().expect("message");
        let builds = if matches!(args[0], "history" | "recommend") {
            "`orbit-graph history sync --branch main`"
        } else {
            "`orbit-graph sync`"
        };
        assert!(message.contains(builds), "{args:?}: {message}");
        assert!(!index.exists(), "{args:?} created {}", index.display());
    }

    for (tool, maintain) in [
        ("orbit.graph.status", "orbit.graph.maintain"),
        ("graph.recommend", "graph.maintain"),
    ] {
        let input = if tool.ends_with("status") {
            json!({})
        } else {
            json!({"query": "helper"})
        };
        let response = plugin_response(repo.path(), None, tool, input);
        assert_eq!(response["ok"], false, "{response}");
        assert_eq!(response["error"]["code"], "index_missing", "{response}");
        let message = response["error"]["message"].as_str().expect("message");
        assert!(
            message.contains(maintain) && message.contains("history_sync"),
            "{tool}: {message}"
        );
        assert!(!index.exists(), "{tool} created {}", index.display());
    }
}

/// Sixteen first syncs racing on a fresh repository all succeed: none sees
/// the schema half-created by another ("table files already exists").
#[test]
fn sixteen_concurrent_first_syncs_all_succeed() {
    let repo = fixture_repository();
    let children = (0..16)
        .map(|_| {
            Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
                .current_dir(repo.path())
                .args(["--format", "json", "sync"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn orbit-graph sync")
        })
        .collect::<Vec<_>>();
    for child in children {
        let output = child.wait_with_output().expect("wait for orbit-graph sync");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "a concurrent sync failed: {stderr}"
        );
        assert!(!stderr.contains("already exists"), "{stderr}");
    }
    let search = run_json(repo.path(), &["search", "helper"]);
    assert!(search.is_object(), "{search}");
}

/// A newer database is never removed; a strictly older one is removed only
/// by `sync` or `clean`, and only once its lock is free. Reads remove
/// nothing.
#[test]
fn only_sync_and_clean_remove_strictly_older_databases_whose_lock_is_free() {
    let repo = fixture_repository();
    run_json(repo.path(), &["sync"]);
    run_json(repo.path(), &["history", "sync", "--branch", "main"]);
    let active = PathBuf::from(
        run_json(repo.path(), &["db-path"])["path"]
            .as_str()
            .expect("database path"),
    );
    let dir = active.parent().expect("graph directory").to_path_buf();
    let newer = plant(&dir, &format!("main.{}.db", EXTRACTOR_VERSION + 1));
    let older_locked = plant(&dir, &format!("main.{}.db", EXTRACTOR_VERSION - 1));
    let older_free = plant(&dir, &format!("feature.{}.db", EXTRACTOR_VERSION - 1));
    let newer_lock = hold_lock(&newer);
    let older_lock = hold_lock(&older_locked);

    for args in CLI_READS {
        let output = run_cli(repo.path(), args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    for path in [&newer, &older_locked, &older_free] {
        assert!(path.exists(), "a read removed {}", path.display());
    }

    let sync = run_json(repo.path(), &["sync"]);
    assert!(sync.is_object(), "{sync}");
    assert!(newer.exists(), "sync removed a newer database");
    assert!(older_locked.exists(), "sync removed a locked database");
    assert!(!older_free.exists(), "sync kept an obsolete free database");
    assert!(
        !sidecar(&older_free, "-wal").exists(),
        "sync left the obsolete database's WAL"
    );

    let clean = run_json(repo.path(), &["clean"]);
    assert_eq!(clean["deleted"], json!([]), "{clean}");
    assert!(newer.exists() && older_locked.exists());

    drop(older_lock);
    let clean = run_json(repo.path(), &["clean"]);
    assert!(
        !older_locked.exists(),
        "clean kept a free obsolete database"
    );
    assert!(
        clean["deleted"]
            .as_array()
            .is_some_and(|deleted| deleted.contains(&json!(older_locked.display().to_string()))),
        "{clean}"
    );

    drop(newer_lock);
    run_json(repo.path(), &["clean"]);
    run_json(repo.path(), &["sync"]);
    assert!(newer.exists(), "a newer database must never be removed");
    assert!(active.exists());
}

/// A database whose `meta.schema_version` differs from the store's is
/// refused with an error naming it, by reads and `sync` alike, and left as
/// it was.
#[test]
fn a_schema_version_mismatch_is_refused_naming_the_database() {
    let repo = fixture_repository();
    run_json(repo.path(), &["sync"]);
    let db = run_json(repo.path(), &["db-path"])["path"]
        .as_str()
        .expect("database path")
        .to_string();

    for (stored, advice) in [
        (
            STORE_SCHEMA_VERSION + 1,
            "use the orbit-graph that wrote it",
        ),
        (
            STORE_SCHEMA_VERSION - 1,
            "run `orbit-graph sync` to rebuild it",
        ),
    ] {
        set_schema_version(&db, stored);
        for args in [&["search", "helper"][..], &["overview"], &["sync"]] {
            let output = run_cli(repo.path(), args);
            assert_eq!(output.status.code(), Some(1), "{args:?} used {stored}");
            let error: Value = serde_json::from_slice(&output.stderr).expect("JSON error");
            assert_eq!(error["error"]["code"], "index_incompatible", "{error}");
            let message = error["error"]["message"].as_str().expect("message");
            assert!(message.contains(db.as_str()), "{message}");
            assert!(
                message.contains(&format!("schema_version {stored}")),
                "{message}"
            );
            assert!(message.contains(advice), "{message}");
        }
        assert_eq!(
            schema_version(&db),
            stored.to_string(),
            "the database changed"
        );
    }
}

fn fixture_repository() -> TempDir {
    let repo = TempDir::new().expect("create fixture repository");
    run_git(repo.path(), &["init", "-b", "main"]);
    run_git(
        repo.path(),
        &["config", "user.email", "graph@example.invalid"],
    );
    run_git(repo.path(), &["config", "user.name", "Graph Test"]);
    fs::create_dir_all(repo.path().join("src")).expect("create source directory");
    fs::write(
        repo.path().join("src/lib.rs"),
        "pub trait Renderer {}\npub struct Human;\nimpl Renderer for Human {}\n\n\
         pub fn helper() -> i32 { 1 }\n\npub fn entry() -> i32 { helper() }\n",
    )
    .expect("write Rust source");
    fs::write(
        repo.path().join("src/cli.py"),
        "import click\n\n@click.command()\ndef ship():\n    helper()\n\ndef helper():\n    return 'ok'\n",
    )
    .expect("write Python source");
    run_git(repo.path(), &["add", "."]);
    run_git(repo.path(), &["commit", "-m", "fixture"]);
    repo
}

/// Every entry below `root`, keyed by relative path, with its mode and
/// contents (empty for a directory).
fn snapshot(root: &Path) -> BTreeMap<PathBuf, (u32, Vec<u8>)> {
    let mut entries = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).expect("read index directory") {
            let path = entry.expect("index entry").path();
            let metadata = fs::symlink_metadata(&path).expect("index entry metadata");
            let contents = if metadata.is_dir() {
                pending.push(path.clone());
                Vec::new()
            } else {
                fs::read(&path).expect("read index file")
            };
            let relative = path
                .strip_prefix(root)
                .expect("relative path")
                .to_path_buf();
            entries.insert(relative, (metadata.permissions().mode(), contents));
        }
    }
    entries
}

/// Makes every directory and file below `root` read-only, restoring the
/// original modes on drop so the temporary directory can be removed.
struct ReadOnlyTree(Vec<(PathBuf, u32)>);

impl ReadOnlyTree {
    fn apply(root: &Path) -> Self {
        let mut original = Vec::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(path) = pending.pop() {
            let metadata = fs::symlink_metadata(&path).expect("entry metadata");
            if metadata.is_dir() {
                for entry in fs::read_dir(&path).expect("read directory") {
                    pending.push(entry.expect("directory entry").path());
                }
            }
            original.push((path, metadata.permissions().mode()));
        }
        // Children first, so no directory is closed before its entries.
        for (path, mode) in original.iter().rev() {
            let read_only = if fs::symlink_metadata(path).expect("metadata").is_dir() {
                0o555
            } else {
                0o444
            };
            fs::set_permissions(path, fs::Permissions::from_mode(mode & !0o777 | read_only))
                .expect("make read-only");
        }
        Self(original)
    }
}

impl Drop for ReadOnlyTree {
    fn drop(&mut self) {
        for (path, mode) in &self.0 {
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(*mode));
        }
    }
}

/// Plants a database family (`.db` and `-wal`) another version would own.
fn plant(dir: &Path, name: &str) -> PathBuf {
    let db = dir.join(name);
    fs::write(&db, b"planted by another orbit-graph version").expect("plant database");
    fs::write(sidecar(&db, "-wal"), b"planted").expect("plant WAL");
    db
}

fn sidecar(db: &Path, suffix: &str) -> PathBuf {
    let mut name = db.file_name().expect("database name").to_os_string();
    name.push(suffix);
    db.with_file_name(name)
}

/// Holds `<db>.lock` as another process using the database would.
fn hold_lock(db: &Path) -> fs::File {
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(sidecar(db, ".lock"))
        .expect("open database lock");
    lock.lock().expect("hold database lock");
    lock
}

fn set_schema_version(db: &str, version: u32) {
    python_sqlite(
        db,
        "conn.execute(\"UPDATE meta SET value=? WHERE key='schema_version'\", (arg,))\nconn.commit()",
        &version.to_string(),
    );
}

fn schema_version(db: &str) -> String {
    python_sqlite(
        db,
        "print(conn.execute(\"SELECT value FROM meta WHERE key='schema_version'\").fetchone()[0])",
        "",
    )
}

fn python_sqlite(db: &str, body: &str, arg: &str) -> String {
    let script = format!(
        "import sqlite3, sys\ndb, arg = sys.argv[1:3]\nconn = sqlite3.connect(db)\n{body}\nconn.close()\n"
    );
    let output = Command::new("python3")
        .args(["-c", script.as_str(), db, arg])
        .output()
        .expect("run python3 sqlite3");
    assert!(
        output.status.success(),
        "python3 sqlite3 failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("UTF-8 output")
        .trim()
        .to_string()
}

fn run_cli(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
        .current_dir(cwd)
        .env_remove("ORBIT_PLUGIN_STATE")
        .env_remove("ORBIT_TOOL_NAME")
        .args(["--format", "json"])
        .args(args)
        .output()
        .expect("run orbit-graph")
}

fn run_json(cwd: &Path, args: &[&str]) -> Value {
    let output = run_cli(cwd, args);
    assert!(
        output.status.success(),
        "orbit-graph {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON command output")
}

/// Runs one plugin tool through the external-tool envelope.
fn plugin_response(repo: &Path, state: Option<&Path>, tool: &str, input: Value) -> Value {
    use std::io::Write as _;
    let request = json!({
        "schema_version": 1,
        "tool": tool,
        "input": input,
        "context": {"workspace_root": repo, "agent": "read-only-test", "model": "test"}
    });
    let mut command = Command::new(env!("CARGO_BIN_EXE_orbit-graph"));
    command
        .current_dir(repo)
        .env("ORBIT_TOOL_NAME", tool)
        .env_remove("ORBIT_PLUGIN_STATE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(state) = state {
        command.env("ORBIT_PLUGIN_STATE", state);
    }
    let mut child = command.spawn().expect("spawn plugin");
    child
        .stdin
        .take()
        .expect("plugin stdin")
        .write_all(request.to_string().as_bytes())
        .expect("write plugin request");
    let output = child.wait_with_output().expect("wait for plugin");
    assert!(
        output.status.success(),
        "{tool}: plugin process failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("plugin response JSON")
}

fn plugin_ok(repo: &Path, state: Option<&Path>, tool: &str, input: Value) -> Value {
    let response = plugin_response(repo, state, tool, input);
    assert_eq!(response["ok"], true, "{tool}: {response}");
    response
}

fn run_git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Whether the test process runs as root, for whom a read-only mode
/// restricts nothing. The test then skips, naming the missing capability on
/// stderr (STD-04 §R8); CI runs it as a regular user.
fn skip_as_root(test: &str) -> bool {
    use std::io::Write as _;
    // SAFETY: geteuid has no preconditions and cannot fail.
    if unsafe { libc::geteuid() } != 0 {
        return false;
    }
    let _ = writeln!(
        std::io::stderr(),
        "skipping {test}: read-only modes do not restrict root"
    );
    true
}
