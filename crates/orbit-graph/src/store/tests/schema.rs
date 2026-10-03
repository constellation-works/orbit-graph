use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use git2::{Oid, Repository, Signature, build::CheckoutBuilder};
use rusqlite::Connection;
use tempfile::TempDir;

use crate::store::schema::SCHEMA_VERSION;
use crate::{EXTRACTOR_VERSION, Graph, SyncPolicy, resolve_db_path, resolve_db_path_for_commit};

#[test]
fn graph_open_with_db_path_indexes_source_root_outside_its_scratch_directory() {
    let worktree = TestWorktree::new("explicit-db-path", "main");
    fs::create_dir_all(worktree.path().join("src")).expect("create source directory");
    fs::write(worktree.path().join("src/lib.rs"), "pub fn indexed() {}\n")
        .expect("write source file");
    let database_home = TempDir::new().expect("create external database home");
    let db_path = database_home.path().join("nested/snapshot.db");

    let graph = Graph::open_with_db_path(worktree.path(), &db_path, SyncPolicy::Manual)
        .expect("open graph at explicit database path");
    graph
        .sync(crate::SyncMode::Full)
        .expect("index source root");

    assert_eq!(graph.worktree_root(), worktree.path());
    assert_eq!(graph.db_path().path(), db_path);
    assert!(db_path.is_file());
    assert!(!worktree.path().join(".orbit-graph").exists());
    let conn = open_test_connection(&db_path);
    assert_eq!(row_count(&conn, "files"), 1);
}

#[test]
fn graph_open_with_revision_names_synthetic_tree_database_for_revision() {
    let worktree = TestWorktree::new("synthetic-revision", "main");
    let revision = "0123456789abcdef0123456789abcdef01234567";

    let graph = Graph::open_with_revision(worktree.path(), revision, SyncPolicy::Manual)
        .expect("open synthetic revision graph");

    assert_eq!(graph.worktree_root(), worktree.path());
    assert_eq!(graph.db_path().branch(), "HEAD");
    assert_eq!(
        graph.db_path().schema_version(),
        crate::STORE_SCHEMA_VERSION
    );
    assert_eq!(
        graph
            .db_path()
            .path()
            .file_name()
            .and_then(|name| name.to_str()),
        Some(format!("detached-{}.{}.db", &revision[..12], EXTRACTOR_VERSION).as_str())
    );
}

#[test]
fn graph_open_with_db_path_rejects_a_directory() {
    let worktree = TestWorktree::new("explicit-db-directory", "main");
    let directory = TempDir::new().expect("create database directory");

    let error = Graph::open_with_db_path(worktree.path(), directory.path(), SyncPolicy::Manual)
        .err()
        .expect("directory path must fail");

    assert!(error.to_string().contains("database path must name a file"));
}

#[test]
fn graph_open_creates_documented_schema_and_initial_meta() {
    let worktree = TestWorktree::new("creates-schema", "feat/schema-open");
    let commit_sha = worktree.init_git_repo();

    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    drop(graph);

    let db_path = resolve_db_path(worktree.path(), "feat/schema-open", EXTRACTOR_VERSION)
        .path()
        .to_path_buf();
    let conn = open_test_connection(&db_path);

    assert_eq!(
        object_names(&conn, "table"),
        BTreeSet::from([
            "commands".to_string(),
            "configs".to_string(),
            "configs_fts".to_string(),
            "configs_fts_config".to_string(),
            "configs_fts_data".to_string(),
            "configs_fts_docsize".to_string(),
            "configs_fts_idx".to_string(),
            "files".to_string(),
            "imports".to_string(),
            "meta".to_string(),
            "refs".to_string(),
            "relations".to_string(),
            "strings".to_string(),
            "strings_fts".to_string(),
            "strings_fts_config".to_string(),
            "strings_fts_data".to_string(),
            "strings_fts_docsize".to_string(),
            "strings_fts_idx".to_string(),
            "symbols".to_string(),
            "symbols_fts".to_string(),
            "symbols_fts_config".to_string(),
            "symbols_fts_data".to_string(),
            "symbols_fts_docsize".to_string(),
            "symbols_fts_idx".to_string(),
        ])
    );
    assert_eq!(
        object_names(&conn, "index"),
        BTreeSet::from([
            "commands_file".to_string(),
            "commands_handler".to_string(),
            "configs_file".to_string(),
            "imports_from_file".to_string(),
            "refs_from_file".to_string(),
            "refs_target_name".to_string(),
            "refs_target_qualified".to_string(),
            "relations_def_file".to_string(),
            "relations_from".to_string(),
            "relations_kind".to_string(),
            "relations_to".to_string(),
            "strings_context_symbol".to_string(),
            "strings_file".to_string(),
            "symbols_file".to_string(),
            "symbols_name".to_string(),
            "symbols_parent".to_string(),
            "symbols_qualified".to_string(),
        ])
    );

    for table in [
        "files",
        "symbols",
        "refs",
        "relations",
        "imports",
        "commands",
        "strings",
        "configs",
        "meta",
    ] {
        assert!(
            table_sql(&conn, table).contains("STRICT"),
            "{table} table should be STRICT"
        );
    }
    assert_eq!(
        table_sql(&conn, "symbols_fts"),
        "CREATE VIRTUAL TABLE symbols_fts USING fts5(name, qualified, signature, content='symbols')"
    );
    assert_eq!(
        table_sql(&conn, "strings_fts"),
        "CREATE VIRTUAL TABLE strings_fts USING fts5(value, content='strings')"
    );
    assert_eq!(
        table_sql(&conn, "configs_fts"),
        "CREATE VIRTUAL TABLE configs_fts USING fts5(key, content='configs')"
    );

    let meta = read_meta(&conn);
    assert_eq!(
        meta.get("extractor_version").map(String::as_str),
        Some(EXTRACTOR_VERSION.to_string().as_str())
    );
    assert_eq!(
        meta.get("schema_version").map(String::as_str),
        Some(SCHEMA_VERSION.to_string().as_str())
    );
    assert_eq!(
        meta.get("branch").map(String::as_str),
        Some("feat/schema-open")
    );
    assert_eq!(
        meta.get("commit_sha").map(String::as_str),
        Some(commit_sha.as_str())
    );
    assert_eq!(
        meta.get("last_full_build_at").map(String::as_str),
        Some("0")
    );
    assert_eq!(
        meta.get("last_incremental_at").map(String::as_str),
        Some("0")
    );
}

/// Keep the standalone connection defaults pinned against accidental drift.
#[test]
fn configure_connection_applies_shared_pragma_defaults() {
    let worktree = TestWorktree::new("pragma-defaults", "feat/pragma-defaults");
    let conn = Connection::open(worktree.path().join("pragmas.db")).expect("open connection");

    super::super::configure_connection(&conn).expect("configure connection");

    let journal_mode = conn
        .pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))
        .expect("journal_mode");
    assert_eq!(journal_mode.to_lowercase(), "wal");
    let pragma_i64 = |name: &str| -> i64 {
        conn.pragma_query_value(None, name, |row| row.get::<_, i64>(0))
            .expect("query pragma")
    };
    assert_eq!(pragma_i64("busy_timeout"), 5_000);
    assert_eq!(pragma_i64("foreign_keys"), 1);
    // synchronous=NORMAL reports as 1.
    assert_eq!(pragma_i64("synchronous"), 1);
}

#[test]
fn graph_open_removes_nothing_and_clean_removes_only_strictly_older_unlocked_versions() {
    let worktree = TestWorktree::new("cleans-stale-dbs", "main");
    worktree.init_git_repo();

    let [stale_version_a, stale_version_b] = stale_versions();
    let db = |branch: &str, version: u32| {
        resolve_db_path(worktree.path(), branch, version)
            .path()
            .to_path_buf()
    };
    let sidecar = |db: &Path, suffix: &str| PathBuf::from(format!("{}{suffix}", db.display()));
    let stale_main = db("main", stale_version_a);
    let stale_locked = db("feat/old", stale_version_b);
    let newer_main = db("main", EXTRACTOR_VERSION + 1);
    fs::create_dir_all(stale_main.parent().expect("stale db parent")).expect("create graph dir");
    for path in [&stale_main, &stale_locked, &newer_main] {
        fs::write(path, "database").expect("write planted db");
        fs::write(sidecar(path, "-wal"), "wal").expect("write planted wal");
        fs::write(sidecar(path, "-shm"), "shm").expect("write planted shm");
    }
    // Another process syncing the old database holds its lock.
    let held = fs::File::create(sidecar(&stale_locked, ".lock")).expect("create held lock");
    held.lock().expect("hold stale lock");

    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    let active_db = graph.db_path().path().to_path_buf();
    drop(graph);
    for path in [&stale_main, &stale_locked, &newer_main] {
        assert!(
            path.exists(),
            "opening must remove nothing: {}",
            path.display()
        );
    }

    // `clean` reports physical paths (`/private/var/...` for a temp root
    // under `/var/...` on macOS), so compare against the resolved ones.
    let physical = |path: &Path| path.canonicalize().expect("resolve planted db");
    let (stale_main_physical, newer_main_physical) = (physical(&stale_main), physical(&newer_main));
    let report = crate::clean_old_databases(worktree.path()).expect("clean old databases");

    assert!(active_db.exists(), "the active DB is never removed");
    for suffix in ["", "-wal", "-shm"] {
        assert!(
            !sidecar(&stale_main, suffix).exists(),
            "a strictly older, unlocked DB family is removed ({suffix})"
        );
        assert!(
            sidecar(&stale_locked, suffix).exists(),
            "a DB whose lock another process holds is kept ({suffix})"
        );
        assert!(
            sidecar(&newer_main, suffix).exists(),
            "a newer extractor's DB is never removed ({suffix})"
        );
    }
    assert!(report.deleted.contains(&stale_main_physical));
    assert!(
        !report
            .deleted
            .iter()
            .any(|path| path.starts_with(&newer_main_physical))
    );

    // A concurrent test may spawn a child after this descriptor is opened.
    // A forked child can briefly retain the same lock after `drop(held)`,
    // whereas an explicit unlock releases it even with inherited descriptors.
    held.unlock().expect("release stale lock");
    drop(held);
    crate::clean_old_databases(worktree.path()).expect("clean after the lock is released");
    assert!(!stale_locked.exists(), "the released old DB is removed");
    assert!(!sidecar(&stale_locked, ".lock").exists());
    assert!(newer_main.exists(), "a newer extractor's DB is still kept");
}

#[test]
fn clean_does_not_create_the_active_database() {
    let worktree = TestWorktree::new("clean-creates-nothing", "main");
    worktree.init_git_repo();

    let report = crate::clean_old_databases(worktree.path()).expect("clean old databases");

    assert!(report.deleted.is_empty());
    assert!(!worktree.path().join(".orbit-graph").exists());
}

#[test]
fn an_unreadable_head_fails_instead_of_selecting_the_head_family() {
    let worktree = TestWorktree::new("unreadable-head", "main");
    worktree.init_git_repo();
    // HEAD names `main`, whose loose ref no longer holds an object ID.
    fs::write(
        worktree.path().join(".git/refs/heads/main"),
        "not an object id\n",
    )
    .expect("corrupt the branch HEAD names");

    let error =
        crate::resolve_worktree_db_path(worktree.path()).expect_err("an unreadable HEAD must fail");
    assert!(
        error.to_string().contains("HEAD") || error.to_string().contains("repository"),
        "the error names what could not be read: {error}"
    );
    assert!(Graph::open(worktree.path(), SyncPolicy::Manual).is_err());
    assert!(!worktree.path().join(".orbit-graph").exists());
}

#[test]
fn an_unborn_branch_and_a_directory_outside_git_select_the_head_family() {
    // The fixture is a repository without commits.
    let unborn = TestWorktree::new("unborn-head", "main");
    let no_git = TempDir::new().expect("create non-git directory");
    let mut roots = vec![unborn.path()];
    // Discovery from a directory outside Git has no boundary a fixture can
    // set, so that case runs only when the temporary directory is outside
    // every repository, and says so when it cannot (`STD-04 §R8`).
    if Repository::discover(no_git.path()).is_ok() {
        assert!(
            std::env::var_os("CI").is_none(),
            "CI must run the outside-Git case, but {} is inside a Git repository",
            no_git.path().display()
        );
        #[allow(clippy::print_stderr)]
        {
            eprintln!(
                "skipped the outside-Git case: {} is inside a Git repository",
                no_git.path().display()
            );
        }
    } else {
        roots.push(no_git.path());
    }

    for root in roots {
        let db_path = crate::resolve_worktree_db_path(root).expect("resolve HEAD family");
        assert_eq!(db_path.branch(), "HEAD");
        assert_eq!(
            db_path.path(),
            resolve_db_path(root, "HEAD", EXTRACTOR_VERSION).path()
        );
    }
}

#[test]
fn graph_open_records_commit_sha_for_detached_head() {
    let worktree = TestWorktree::new("detached-head-meta", "main");
    let commit_sha = worktree.init_git_repo();
    worktree.detach_head(commit_sha.as_str());

    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open detached graph");
    drop(graph);

    let db_path = resolve_db_path_for_commit(
        worktree.path(),
        "HEAD",
        commit_sha.as_str(),
        EXTRACTOR_VERSION,
    )
    .path()
    .to_path_buf();
    let conn = open_test_connection(&db_path);
    let meta = read_meta(&conn);
    assert_eq!(meta.get("branch").map(String::as_str), Some("HEAD"));
    assert_eq!(
        meta.get("commit_sha").map(String::as_str),
        Some(commit_sha.as_str())
    );
    assert!(!commit_sha.is_empty());
}

#[test]
fn graph_open_uses_distinct_db_files_for_detached_commits() {
    let worktree = TestWorktree::new("detached-distinct-dbs", "main");
    let first_commit = worktree.init_git_repo();
    let second_commit = worktree.commit_file("second.txt", "second\n", "second");

    worktree.detach_head(first_commit.as_str());
    let first_graph =
        Graph::open(worktree.path(), SyncPolicy::Manual).expect("open first detached graph");
    let first_db = first_graph.db_path().path().to_path_buf();
    drop(first_graph);

    worktree.detach_head(second_commit.as_str());
    let second_graph =
        Graph::open(worktree.path(), SyncPolicy::Manual).expect("open second detached graph");
    let second_db = second_graph.db_path().path().to_path_buf();
    drop(second_graph);

    assert_ne!(first_db, second_db);
    assert!(
        first_db.exists(),
        "first detached DB should remain after opening another detached commit"
    );
    assert!(
        second_db.exists(),
        "second detached DB should be created separately"
    );
    let expected_first_file = format!("detached-{}.{}.db", &first_commit[..12], EXTRACTOR_VERSION);
    let expected_second_file =
        format!("detached-{}.{}.db", &second_commit[..12], EXTRACTOR_VERSION);
    assert_eq!(
        first_db.file_name().and_then(|name| name.to_str()),
        Some(expected_first_file.as_str())
    );
    assert_eq!(
        second_db.file_name().and_then(|name| name.to_str()),
        Some(expected_second_file.as_str())
    );
}

#[test]
fn clean_removes_unreachable_detached_databases() {
    let worktree = TestWorktree::new("cleans-unreachable-detached", "main");
    worktree.init_git_repo();
    let reachable_commit = worktree.commit_file("reachable.txt", "reachable\n", "reachable");
    worktree.detach_head(reachable_commit.as_str());
    let reachable_graph =
        Graph::open(worktree.path(), SyncPolicy::Manual).expect("open reachable detached graph");
    let reachable_db = reachable_graph.db_path().path().to_path_buf();
    drop(reachable_graph);

    worktree.checkout_branch();
    let unreachable_commit =
        worktree.create_unreachable_detached_commit("unreachable.txt", "unreachable\n");
    let unreachable_graph =
        Graph::open(worktree.path(), SyncPolicy::Manual).expect("open unreachable detached graph");
    let unreachable_db = unreachable_graph.db_path().path().to_path_buf();
    drop(unreachable_graph);

    worktree.checkout_branch();
    let main_graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open main graph");
    let main_db = main_graph.db_path().path().to_path_buf();
    drop(main_graph);
    assert!(unreachable_db.exists(), "opening a graph removes nothing");

    crate::clean_old_databases(worktree.path()).expect("clean old databases");

    assert!(main_db.exists(), "active branch DB should remain");
    assert!(
        reachable_db.exists(),
        "detached DB reachable from a branch ref should remain"
    );
    assert!(
        !unreachable_db.exists(),
        "detached DB for commit {unreachable_commit} should be cleaned once no local ref reaches it"
    );
}

#[test]
fn reopening_existing_db_preserves_meta_rows() {
    let worktree = TestWorktree::new("preserves-meta", "main");
    worktree.init_git_repo();

    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("first open");
    drop(graph);

    let db_path = resolve_db_path(worktree.path(), "main", EXTRACTOR_VERSION)
        .path()
        .to_path_buf();
    {
        let conn = open_test_connection(&db_path);
        conn.execute(
            "UPDATE meta SET value = 'kept' WHERE key = 'last_full_build_at'",
            [],
        )
        .expect("mutate meta before reopen");
    }

    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("second open");
    drop(graph);

    let conn = open_test_connection(&db_path);
    let meta = read_meta(&conn);
    assert_eq!(
        meta.get("last_full_build_at").map(String::as_str),
        Some("kept")
    );
    assert_eq!(meta.get("branch").map(String::as_str), Some("main"));
}

#[test]
fn default_index_refuses_a_mismatched_stored_branch() {
    let worktree = TestWorktree::new("mismatched-meta-branch", "main");
    worktree.init_git_repo();
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    let path = graph.db_path().path().to_path_buf();
    drop(graph);
    let conn = open_test_connection(&path);
    conn.execute("UPDATE meta SET value = 'other' WHERE key = 'branch'", [])
        .expect("change stored branch");
    drop(conn);

    let read = Graph::open_existing_read_only(worktree.path())
        .err()
        .expect("read must refuse wrong branch");
    assert!(
        matches!(read, crate::GraphError::IndexIncompatible { .. }),
        "{read}"
    );
    let write = Graph::open(worktree.path(), SyncPolicy::Manual)
        .err()
        .expect("writer must refuse wrong branch");
    assert!(
        matches!(write, crate::GraphError::IndexIncompatible { .. }),
        "{write}"
    );
}

#[test]
fn colliding_branches_never_read_each_others_index_and_legacy_is_reused_safely() {
    let worktree = TestWorktree::new("branch-collision", "feat/foo");
    let first_commit = worktree.init_git_repo();
    let first = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open first branch");
    let first_path = first.db_path().path().to_path_buf();
    drop(first);

    // A pre-hash database for feat/foo has the same old filename as feat_foo.
    let legacy = crate::db_path::legacy_db_path(worktree.path(), "feat/foo", EXTRACTOR_VERSION);
    fs::rename(&first_path, &legacy).expect("plant compatible legacy index");
    assert_eq!(
        crate::resolve_worktree_db_path(worktree.path())
            .expect("resolve compatible legacy index")
            .path(),
        legacy
    );
    let compatible = Graph::open_existing_read_only(worktree.path()).expect("read legacy index");
    assert_eq!(compatible.db_path().path(), legacy);
    drop(compatible);

    let repo = Repository::open(worktree.path()).expect("open repo");
    let commit = repo
        .find_commit(Oid::from_str(&first_commit).expect("first oid"))
        .expect("find first commit");
    repo.branch("feat_foo", &commit, false)
        .expect("create colliding branch");
    repo.set_head("refs/heads/feat_foo").expect("switch branch");
    assert_ne!(
        crate::resolve_worktree_db_path(worktree.path())
            .expect("resolve colliding branch")
            .path(),
        legacy
    );
    let error = Graph::open_existing_read_only(worktree.path())
        .err()
        .expect("other branch cannot read legacy index");
    assert!(
        matches!(error, crate::GraphError::IndexMissing { .. }),
        "{error}"
    );
    let other = Graph::open(worktree.path(), SyncPolicy::Manual).expect("create other index");
    assert_ne!(other.db_path().path(), legacy);
    assert_ne!(other.db_path().path(), first_path);
    assert!(legacy.exists());
}

#[test]
fn named_detached_like_branch_cannot_read_detached_head_index() {
    let worktree = TestWorktree::new("named-detached", "main");
    let commit = worktree.init_git_repo();
    worktree.detach_head(&commit);
    let detached = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open detached index");
    let detached_path = detached.db_path().path().to_path_buf();
    drop(detached);

    let repo = Repository::open(worktree.path()).expect("open repo");
    let tip = repo
        .find_commit(Oid::from_str(&commit).expect("oid"))
        .expect("tip");
    let branch = format!("detached-{}", &commit[..12]);
    repo.branch(&branch, &tip, false)
        .expect("create detached-like branch");
    repo.set_head(&format!("refs/heads/{branch}"))
        .expect("attach HEAD");
    let error = Graph::open_existing_read_only(worktree.path())
        .err()
        .expect("named branch must not read detached index");
    assert!(
        matches!(error, crate::GraphError::IndexMissing { .. }),
        "{error}"
    );
    let named = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open named branch");
    assert_ne!(named.db_path().path(), detached_path);
    assert!(detached_path.exists());
    let plan = crate::plan_clean_old_databases(worktree.path()).expect("plan cleanup");
    assert!(
        plan.would_delete.is_empty(),
        "reachable detached index is retained"
    );
    // Cleanup reports physical paths; macOS temp dirs sit behind /var -> /private/var.
    let detached_physical = detached_path
        .canonicalize()
        .expect("resolve detached index");
    assert!(plan.kept.iter().any(|item| item.path == detached_physical));
}

#[cfg(unix)]
#[test]
fn writer_refuses_future_delete_journal_schema_without_changing_the_index() {
    use std::os::unix::fs::PermissionsExt;

    let worktree = TestWorktree::new("future-delete-journal", "main");
    worktree.init_git_repo();
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("initialize graph");
    let db_path = graph.db_path().path().to_path_buf();
    drop(graph);

    {
        let conn = open_test_connection(&db_path);
        conn.pragma_update(None, "journal_mode", "DELETE")
            .expect("switch to rollback journal");
        conn.execute(
            "UPDATE meta SET value = '999' WHERE key = 'schema_version'",
            [],
        )
        .expect("stamp future schema");
    }
    let lock_path = PathBuf::from(format!("{}.lock", db_path.display()));
    fs::remove_file(&lock_path).expect("remove the first open's setup lock");
    let dir = db_path.parent().expect("database directory");
    fs::set_permissions(&db_path, fs::Permissions::from_mode(0o644))
        .expect("set observable database mode");
    fs::set_permissions(dir, fs::Permissions::from_mode(0o755))
        .expect("set observable directory mode");
    let before = fs::read(&db_path).expect("read database before refused open");

    let error = Graph::open(worktree.path(), SyncPolicy::Manual)
        .err()
        .expect("future schema must be refused");
    assert!(matches!(error, crate::GraphError::IndexIncompatible { .. }));
    assert!(error.to_string().contains("schema_version 999"), "{error}");
    assert_eq!(
        fs::read(&db_path).expect("read database after refusal"),
        before
    );
    assert_eq!(
        fs::metadata(&db_path)
            .expect("database metadata")
            .permissions()
            .mode()
            & 0o777,
        0o644
    );
    assert_eq!(
        fs::metadata(dir)
            .expect("directory metadata")
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    for suffix in ["-wal", "-shm", "-journal", ".lock"] {
        assert!(
            !PathBuf::from(format!("{}{suffix}", db_path.display())).exists(),
            "refused open created a {suffix} sidecar"
        );
    }
    let conn = Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("inspect journal mode read-only");
    let mode: String = conn
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .expect("read journal mode");
    assert_eq!(mode.to_ascii_lowercase(), "delete");
}

#[test]
fn concurrent_first_writers_initialize_fresh_and_compatible_delete_journal_databases() {
    use std::sync::{Arc, Barrier};

    for existing in [false, true] {
        let worktree = TestWorktree::new("concurrent-first-writers", "main");
        worktree.init_git_repo();
        if existing {
            let graph = Graph::open(worktree.path(), SyncPolicy::Manual)
                .expect("initialize compatible graph");
            let db_path = graph.db_path().path().to_path_buf();
            drop(graph);
            let conn = open_test_connection(&db_path);
            conn.pragma_update(None, "journal_mode", "DELETE")
                .expect("switch compatible graph to rollback journal");
        }

        let barrier = Arc::new(Barrier::new(8));
        let root = worktree.path().to_path_buf();
        std::thread::scope(|scope| {
            let workers = (0..8)
                .map(|_| {
                    let barrier = Arc::clone(&barrier);
                    let root = root.clone();
                    scope.spawn(move || {
                        barrier.wait();
                        Graph::open(&root, SyncPolicy::Manual).map(drop)
                    })
                })
                .collect::<Vec<_>>();
            for worker in workers {
                worker.join().expect("writer thread").expect("open graph");
            }
        });
        let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("reopen graph");
        let conn = open_test_connection(graph.db_path().path());
        assert_eq!(
            read_meta(&conn).get("schema_version").map(String::as_str),
            Some(SCHEMA_VERSION.to_string().as_str())
        );
    }
}

#[test]
fn refs_target_symbol_hint_has_no_symbol_foreign_key() {
    let worktree = TestWorktree::new("refs-no-fk", "main");
    worktree.init_git_repo();

    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    drop(graph);

    let db_path = resolve_db_path(worktree.path(), "main", EXTRACTOR_VERSION)
        .path()
        .to_path_buf();
    let conn = open_test_connection(&db_path);
    let refs_sql = normalized_sql(&table_sql(&conn, "refs"));

    assert!(refs_sql.contains("target_symbol_hint INTEGER"));
    assert!(
        !refs_sql.contains("target_symbol_hint INTEGER REFERENCES"),
        "refs.target_symbol_hint must stay non-authoritative and must not reference symbols(id): {refs_sql}"
    );
}

#[test]
fn deleting_file_cascades_every_file_anchored_row() {
    let worktree = TestWorktree::new("cascade-delete", "main");
    worktree.init_git_repo();

    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    drop(graph);

    let db_path = resolve_db_path(worktree.path(), "main", EXTRACTOR_VERSION)
        .path()
        .to_path_buf();
    let conn = open_test_connection(&db_path);
    insert_file_anchored_rows(&conn);

    conn.execute("DELETE FROM files WHERE path = 'src/lib.rs'", [])
        .expect("delete file row");

    for table in [
        "symbols",
        "refs",
        "relations",
        "imports",
        "commands",
        "strings",
        "configs",
    ] {
        assert_eq!(
            row_count(&conn, table),
            0,
            "{table} should cascade on file delete"
        );
    }
}

fn insert_file_anchored_rows(conn: &Connection) {
    conn.execute(
        "INSERT INTO files (path, content_hash, mtime_ns, lang, byte_len, extracted_at)
         VALUES ('src/lib.rs', x'00', 1, 'rust', 12, 2)",
        [],
    )
    .expect("insert file");
    conn.execute(
        "INSERT INTO symbols (
            id, file_path, name, qualified, kind, span_start, span_end, signature, parent_symbol
         ) VALUES (1, 'src/lib.rs', 'run', 'crate::run', 'function', 0, 3, 'fn run()', NULL)",
        [],
    )
    .expect("insert symbol");
    conn.execute(
        "INSERT INTO refs (
            from_file, from_span_start, from_span_end, target_name, target_qualified,
            target_symbol_hint, kind, confidence
         ) VALUES ('src/lib.rs', 4, 7, 'run', 'crate::run', 1, 'call', 'exact')",
        [],
    )
    .expect("insert ref");
    conn.execute(
        "INSERT INTO relations (
            from_qualified, to_qualified, kind, def_file, def_span_start, def_span_end, confidence
         ) VALUES ('crate::Type', 'crate::Trait', 'impl', 'src/lib.rs', 0, 10, 'exact')",
        [],
    )
    .expect("insert relation");
    conn.execute(
        "INSERT INTO imports (from_file, target_path, target_symbol)
         VALUES ('src/lib.rs', 'crate::other', 'Other')",
        [],
    )
    .expect("insert import");
    conn.execute(
        "INSERT INTO commands (name, file_path, span_start, handler_symbol)
         VALUES ('run', 'src/lib.rs', 0, 1)",
        [],
    )
    .expect("insert command");
    conn.execute(
        "INSERT INTO strings (file_path, line, value, context_symbol)
         VALUES ('src/lib.rs', 1, 'hello world', 1)",
        [],
    )
    .expect("insert string");
    conn.execute(
        "INSERT INTO configs (file_path, line, key, kind)
         VALUES ('src/lib.rs', 1, 'app.name', 'toml')",
        [],
    )
    .expect("insert config");
}

fn open_test_connection(path: &Path) -> Connection {
    let conn = Connection::open(path).expect("open test sqlite connection");
    conn.pragma_update(None, "foreign_keys", "ON")
        .expect("enable foreign keys");
    conn
}

fn object_names(conn: &Connection, object_type: &str) -> BTreeSet<String> {
    let mut stmt = conn
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type = ?1 AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
        )
        .expect("prepare object name query");
    stmt.query_map([object_type], |row| row.get::<_, String>(0))
        .expect("query object names")
        .collect::<Result<BTreeSet<_>, _>>()
        .expect("collect object names")
}

fn table_sql(conn: &Connection, name: &str) -> String {
    conn.query_row(
        "SELECT sql FROM sqlite_master WHERE name = ?1",
        [name],
        |row| row.get(0),
    )
    .expect("read sqlite_master sql")
}

fn normalized_sql(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn read_meta(conn: &Connection) -> BTreeMap<String, String> {
    let mut stmt = conn
        .prepare("SELECT key, value FROM meta ORDER BY key")
        .expect("prepare meta query");
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query meta")
        .collect::<Result<BTreeMap<_, _>, _>>()
        .expect("collect meta")
}

fn row_count(conn: &Connection, table: &str) -> i64 {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    conn.query_row(&sql, [], |row| row.get(0))
        .expect("count table rows")
}

fn stale_versions() -> [u32; 2] {
    [
        if EXTRACTOR_VERSION > 0 {
            EXTRACTOR_VERSION - 1
        } else {
            EXTRACTOR_VERSION + 1
        },
        if EXTRACTOR_VERSION > 1 {
            EXTRACTOR_VERSION - 2
        } else {
            EXTRACTOR_VERSION + 2
        },
    ]
}

struct TestWorktree {
    path: PathBuf,
    branch: String,
}

impl TestWorktree {
    fn new(name: &str, branch: &str) -> Self {
        let mut path = std::env::temp_dir();
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time after epoch")
            .as_nanos();
        path.push(format!("orbit-graph-{name}-{}-{stamp}", std::process::id()));
        fs::create_dir_all(&path).expect("create test worktree");
        crate::tests::support::init_fixture_repository(&path, branch);
        Self {
            path,
            branch: branch.to_string(),
        }
    }

    fn path(&self) -> &Path {
        self.path.as_path()
    }

    fn init_git_repo(&self) -> String {
        let repo = Repository::open(&self.path).expect("open git repo");
        fs::write(self.path.join("README.md"), "test\n").expect("write initial file");

        let mut index = repo.index().expect("open repo index");
        index
            .add_path(Path::new("README.md"))
            .expect("add initial file");
        index.write().expect("write index");
        let tree_id = index.write_tree().expect("write tree");
        let tree = repo.find_tree(tree_id).expect("find tree");
        let sig = Signature::now("Orbit Test", "orbit@example.test").expect("test signature");
        let oid = repo
            .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .expect("initial commit");
        oid.to_string()
    }

    fn commit_file(&self, rel: &str, content: &str, message: &str) -> String {
        let repo = Repository::open(&self.path).expect("open git repo");
        let path = self.path.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create commit file parent");
        }
        fs::write(path, content).expect("write commit file");

        let mut index = repo.index().expect("open repo index");
        index.add_path(Path::new(rel)).expect("add commit file");
        index.write().expect("write index");
        let tree_id = index.write_tree().expect("write tree");
        let tree = repo.find_tree(tree_id).expect("find tree");
        let parent = repo
            .head()
            .expect("read HEAD")
            .peel_to_commit()
            .expect("peel HEAD to commit");
        let sig = Signature::now("Orbit Test", "orbit@example.test").expect("test signature");
        let oid = repo
            .commit(Some("HEAD"), &sig, &sig, message, &tree, &[&parent])
            .expect("create commit");
        oid.to_string()
    }

    fn checkout_branch(&self) {
        let repo = Repository::open(&self.path).expect("open git repo");
        let branch_ref = format!("refs/heads/{}", self.branch);
        repo.set_head(branch_ref.as_str())
            .expect("set HEAD to branch");
        let mut checkout = CheckoutBuilder::new();
        checkout.force();
        repo.checkout_head(Some(&mut checkout))
            .expect("checkout branch");
    }

    fn create_unreachable_detached_commit(&self, rel: &str, content: &str) -> String {
        let repo = Repository::open(&self.path).expect("open git repo");
        let branch_name = format!("refs/heads/{}", self.branch);
        let branch_commit_id = repo
            .find_reference(branch_name.as_str())
            .expect("find branch ref")
            .peel_to_commit()
            .expect("peel branch to commit")
            .id();
        repo.set_head_detached(branch_commit_id)
            .expect("detach from branch commit");
        drop(repo);
        self.commit_file(rel, content, "unreachable detached")
    }

    fn detach_head(&self, commit_sha: &str) {
        let repo = Repository::open(&self.path).expect("open git repo");
        let oid = Oid::from_str(commit_sha).expect("parse commit oid");
        repo.set_head_detached(oid).expect("detach HEAD");
    }
}

impl Drop for TestWorktree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Pause the real SQLite checkpoint after its first main-file page write.
/// No synthetic corrupt bytes or global VFS overrides: only this connection's
/// method table is wrapped, and every write still goes through SQLite's VFS.
#[repr(C)]
struct CheckpointPause {
    methods: rusqlite::ffi::sqlite3_io_methods,
    original: *const rusqlite::ffi::sqlite3_io_methods,
    written: std::sync::mpsc::SyncSender<()>,
    resume: std::sync::mpsc::Receiver<()>,
}

struct PausedCheckpoint<'a> {
    file: *mut rusqlite::ffi::sqlite3_file,
    pause: Box<CheckpointPause>,
    _connection: &'a Connection,
}

impl<'a> PausedCheckpoint<'a> {
    fn install(
        connection: &'a Connection,
        written: std::sync::mpsc::SyncSender<()>,
        resume: std::sync::mpsc::Receiver<()>,
    ) -> Self {
        use rusqlite::ffi;
        let mut file: *mut ffi::sqlite3_file = std::ptr::null_mut();
        // SAFETY: the connection is live and borrowed for this guard's lifetime;
        // FILE_POINTER writes a sqlite3_file pointer into the supplied slot.
        let rc = unsafe {
            ffi::sqlite3_file_control(
                connection.handle(),
                c"main".as_ptr(),
                ffi::SQLITE_FCNTL_FILE_POINTER,
                std::ptr::from_mut(&mut file).cast(),
            )
        };
        assert_eq!(rc, ffi::SQLITE_OK);
        assert!(!file.is_null());
        // SAFETY: FILE_POINTER returned the live main file's method table.
        let original = unsafe { (*file).pMethods };
        let mut pause = Box::new(CheckpointPause {
            // SAFETY: SQLite's method table is initialized and Copy.
            methods: unsafe { *original },
            original,
            written,
            resume,
        });
        pause.methods.xWrite = Some(checkpoint_write);
        // SAFETY: the boxed table is stable until Drop restores the original,
        // before the borrowed connection can be closed. Only this thread uses it.
        unsafe { (*file).pMethods = &pause.methods };
        Self {
            file,
            pause,
            _connection: connection,
        }
    }
}

impl Drop for PausedCheckpoint<'_> {
    fn drop(&mut self) {
        // SAFETY: the connection outlives this guard, so its file is still live.
        unsafe { (*self.file).pMethods = self.pause.original };
    }
}

unsafe extern "C" fn checkpoint_write(
    file: *mut rusqlite::ffi::sqlite3_file,
    bytes: *const std::ffi::c_void,
    amount: std::ffi::c_int,
    offset: rusqlite::ffi::sqlite3_int64,
) -> std::ffi::c_int {
    use rusqlite::ffi;
    // SAFETY: install places methods first in repr(C) CheckpointPause, and the
    // guard keeps it alive for every callback. Forward the original arguments.
    let pause = unsafe { &*((*file).pMethods.cast::<CheckpointPause>()) };
    // SAFETY: the underlying VFS owns this file and supplied this xWrite method.
    let rc =
        unsafe { ((*pause.original).xWrite.expect("VFS xWrite"))(file, bytes, amount, offset) };
    if rc == ffi::SQLITE_OK
        && offset == 0
        && (pause.written.send(()).is_err()
            || pause
                .resume
                .recv_timeout(std::time::Duration::from_secs(10))
                .is_err())
    {
        return ffi::SQLITE_IOERR_WRITE;
    }
    rc
}

#[test]
fn identity_precheck_is_ordered_before_first_schema_checkpoint() {
    use std::sync::mpsc::sync_channel;
    use std::time::Duration;

    let worktree = TestWorktree::new("identity-checkpoint", "main");
    worktree.init_git_repo();
    let db = resolve_db_path(worktree.path(), "main", EXTRACTOR_VERSION)
        .path()
        .to_path_buf();
    fs::create_dir_all(db.parent().expect("database directory")).expect("create directory");
    // The initial WAL header exists, but no schema has been checkpointed.
    let conn = Connection::open(&db).expect("create empty database");
    conn.pragma_update(None, "journal_mode", "WAL")
        .expect("WAL mode");
    drop(conn);

    let (sampled_tx, sampled_rx) = sync_channel(1);
    let (read_tx, read_rx) = sync_channel(1);
    let root = worktree.path().to_path_buf();
    let reader = std::thread::spawn(move || {
        super::super::IDENTITY_READ_HOOK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                sampled_tx.send(()).expect("signal main-file observation");
                read_rx
                    .recv_timeout(Duration::from_secs(10))
                    .expect("release identity read");
            }));
        });
        Graph::open(&root, SyncPolicy::Manual).map(drop)
    });
    sampled_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("precheck sampled absent sidecars");

    // A cooperating writer must acquire the same directory guard before
    // publishing WAL state. On the unfixed code it can enter at this point.
    let file =
        fs::File::open(db.parent().expect("database directory")).expect("open observation guard");
    match file.try_lock() {
        Err(fs::TryLockError::WouldBlock) => {
            read_tx.send(()).expect("release protected reader");
            reader
                .join()
                .expect("reader thread")
                .expect("protected first open");
        }
        Ok(()) => {
            let (written_tx, written_rx) = sync_channel(1);
            let (resume_tx, resume_rx) = sync_channel(1);
            let writer_db = db.clone();
            let writer = std::thread::spawn(move || {
                let _guard = file;
                let mut conn = Connection::open(&writer_db).expect("open concurrent initializer");
                conn.pragma_update(None, "wal_autocheckpoint", 0)
                    .expect("explicit checkpoint only");
                super::super::schema::initialize_if_empty(
                    &mut conn,
                    &super::super::schema::InitialMeta {
                        extractor_version: EXTRACTOR_VERSION,
                        branch: "main",
                        commit_sha: "checkpoint-fixture",
                    },
                )
                .expect("initialize schema in WAL");
                let _pause = PausedCheckpoint::install(&conn, written_tx, resume_rx);
                conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
                    .expect("finish real checkpoint");
            });
            written_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("checkpoint wrote page one");
            read_tx.send(()).expect("read partial checkpoint");
            let result = reader.join().expect("reader thread");
            resume_tx.send(()).expect("finish checkpoint");
            writer.join().expect("writer thread");
            result.expect("identity precheck must not observe a partial checkpoint");
        }
        Err(error) => panic!("observation guard: {error}"),
    }
    let conn = Connection::open(&db).expect("inspect completed database");
    let integrity: String = conn
        .pragma_query_value(None, "integrity_check", |row| row.get(0))
        .expect("integrity check");
    assert_eq!(integrity, "ok");
}

#[cfg(unix)]
#[test]
fn identity_refusals_preserve_all_state_with_and_without_lock_sidecars() {
    use std::os::unix::fs::PermissionsExt;

    for mode in ["DELETE", "WAL"] {
        for refusal in ["future", "older", "foreign", "corrupt"] {
            for keep_locks in [false, true] {
                let worktree = TestWorktree::new("identity-refusal", "main");
                worktree.init_git_repo();
                let graph =
                    Graph::open(worktree.path(), SyncPolicy::Manual).expect("initialize graph");
                let db = graph.db_path().path().to_path_buf();
                drop(graph);
                let conn = Connection::open(&db).expect("open fixture");
                conn.pragma_update(None, "journal_mode", mode)
                    .expect("journal mode");
                match refusal {
                    "future" => {
                        conn.execute("UPDATE meta SET value='999' WHERE key='schema_version'", [])
                            .expect("future schema");
                    }
                    "older" => {
                        conn.execute("UPDATE meta SET value='0' WHERE key='schema_version'", [])
                            .expect("older schema");
                    }
                    "foreign" => {
                        conn.execute("UPDATE meta SET value='other' WHERE key='branch'", [])
                            .expect("foreign identity");
                    }
                    "corrupt" => {}
                    _ => unreachable!(),
                }
                drop(conn);
                if refusal == "corrupt" {
                    let mut bytes = fs::read(&db).expect("read fixture");
                    bytes[100] = 0; // Invalid page-one btree type, retaining the SQLite/WAL header.
                    fs::write(&db, bytes).expect("corrupt fixture");
                }
                for suffix in [".lock", ".close.lock"] {
                    let path = PathBuf::from(format!("{}{suffix}", db.display()));
                    if keep_locks {
                        fs::write(&path, b"unchanged holder record").expect("set lock sentinel");
                        fs::set_permissions(&path, fs::Permissions::from_mode(0o644))
                            .expect("lock mode");
                    } else {
                        fs::remove_file(path).expect("remove fixture lock");
                    }
                }
                let dir = db.parent().expect("directory");
                fs::set_permissions(&db, fs::Permissions::from_mode(0o644)).expect("database mode");
                fs::set_permissions(dir, fs::Permissions::from_mode(0o755))
                    .expect("directory mode");
                let snapshot = || {
                    fs::read_dir(dir)
                        .expect("entries")
                        .map(|entry| {
                            let path = entry.expect("entry").path();
                            let mode = fs::metadata(&path).expect("metadata").permissions().mode();
                            (path.clone(), (mode, fs::read(path).expect("contents")))
                        })
                        .collect::<BTreeMap<_, _>>()
                };
                let before = snapshot();
                let error = Graph::open(worktree.path(), SyncPolicy::Manual)
                    .err()
                    .expect("refuse identity");
                if refusal == "corrupt" {
                    assert!(matches!(error, crate::GraphError::Sqlite { .. }), "{error}");
                } else {
                    assert!(
                        matches!(error, crate::GraphError::IndexIncompatible { .. }),
                        "{error}"
                    );
                }
                assert_eq!(snapshot(), before, "{mode} {refusal} locks={keep_locks}");
                assert_eq!(
                    fs::metadata(dir)
                        .expect("directory metadata")
                        .permissions()
                        .mode()
                        & 0o777,
                    0o755
                );
            }
        }
    }
}

#[test]
fn observation_lock_is_bounded_and_does_not_create_a_holder_record() {
    use std::time::{Duration, Instant};
    let dir = TempDir::new().expect("fixture directory");
    let db = dir.path().join("graph.db");
    let held = super::super::acquire_observation_lock(&db, Duration::ZERO).expect("take guard");
    let started = Instant::now();
    let error = super::super::acquire_observation_lock(&db, Duration::from_millis(25))
        .expect_err("held observation guard times out");
    assert!(
        matches!(error, crate::GraphError::Timeout { .. }),
        "{error}"
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(error.to_string().contains("holder unknown"));
    assert_eq!(fs::read_dir(dir.path()).expect("entries").count(), 0);
    drop(held);
    // The hermetic suite also runs in one process: another fixture's fork can
    // briefly inherit this descriptor until exec closes it. Honor the same
    // bounded acquisition as production rather than requiring an instant lock.
    let _next = super::super::acquire_observation_lock(&db, Duration::from_secs(10))
        .expect("guard released");
}
