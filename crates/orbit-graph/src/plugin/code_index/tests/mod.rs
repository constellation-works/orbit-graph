use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use super::*;

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(root)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?}");
}

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("fixture");
    git(dir.path(), &["init", "-q", "-b", "main"]);
    fs::create_dir_all(dir.path().join("src")).expect("src");
    fs::write(
        dir.path().join("src/lib.rs"),
        "pub fn helper() -> i32 { 1 }\npub fn entry() -> i32 { helper() }\n",
    )
    .expect("lib");
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "init"]);
    dir
}

/// Block until no build holds the index directory's lock.
fn wait_for_idle_builder(index_dir: &Path) {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        if acquire_lock(index_dir).is_ok() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "build never released its lock"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn request(full: bool) -> SyncRequest {
    SyncRequest {
        full,
        budget: Duration::from_secs(60),
    }
}

fn published(index_dir: &Path) -> PublishedIndex {
    match IndexState::read(index_dir).expect("read state") {
        IndexState::Ready(published) => published,
        other => panic!("expected a published index, got {other:?}"),
    }
}

#[test]
fn a_build_publishes_a_new_generation_and_removes_superseded_ones() {
    let repo = fixture();
    let state = tempfile::tempdir().expect("state");
    assert_eq!(
        IndexState::read(state.path()).expect("state"),
        IndexState::Missing
    );

    let first = sync(repo.path(), state.path(), request(false)).expect("first build");
    assert!(first.incomplete.is_none());
    assert!(!first.seeded);
    let one = published(state.path());
    assert_eq!(one.database, format!("graph.{EXTRACTOR_VERSION}.1.db"));
    assert_eq!(one.mode, "full");
    assert_eq!(one.files, 1);
    assert!(!one.worktree_dirty);
    let head = Repository::open(repo.path())
        .expect("repo")
        .head()
        .expect("head")
        .peel_to_commit()
        .expect("commit")
        .id()
        .to_string();
    assert_eq!(one.revision.as_deref(), Some(head.as_str()));

    fs::write(repo.path().join("src/extra.rs"), "pub fn extra() {}\n").expect("extra");
    let second = sync(repo.path(), state.path(), request(false)).expect("incremental build");
    assert!(second.incomplete.is_none());
    assert!(second.seeded);
    assert_eq!(second.files_changed, 1);
    let two = published(state.path());
    assert_eq!(two.database, format!("graph.{EXTRACTOR_VERSION}.2.db"));
    assert_eq!(two.mode, "incremental");
    assert_eq!(two.files, 2);
    assert!(two.worktree_dirty);
    // The superseded generation stays until the next build removes it.
    assert!(state.path().join(&one.database).exists());

    let third = sync(repo.path(), state.path(), request(true)).expect("full build");
    assert!(!third.seeded);
    assert_eq!(published(state.path()).mode, "full");
    assert!(!state.path().join(&one.database).exists());
}

#[test]
fn an_exhausted_budget_publishes_nothing_and_keeps_the_previous_index() {
    let repo = fixture();
    let state = tempfile::tempdir().expect("state");
    sync(repo.path(), state.path(), request(false)).expect("first build");
    let before = published(state.path());

    // Pass 1 stops before its first file when its share of the budget is gone.
    fs::write(repo.path().join("src/extra.rs"), "pub fn extra() {}\n").expect("extra");
    let result = sync(
        repo.path(),
        state.path(),
        SyncRequest {
            full: true,
            budget: Duration::ZERO,
        },
    )
    .expect("bounded build");
    assert_eq!(result.incomplete, Some(Incomplete::ExtractionBudget));
    assert_eq!(published(state.path()), before);
    // The abandoned build discards its generation before it releases the lock.
    wait_for_idle_builder(state.path());
    assert_eq!(published(state.path()), before);
    let leftovers = fs::read_dir(state.path())
        .expect("list")
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .filter(|name| graph_database_of(name).is_some_and(|db| db != before.database))
        .collect::<Vec<_>>();
    assert!(
        leftovers.is_empty(),
        "unpublished files left: {leftovers:?}"
    );
}

#[test]
fn only_recorded_generations_are_removed_and_unrecorded_databases_are_reported() {
    let repo = fixture();
    let state = tempfile::tempdir().expect("state");
    let unrecorded = [
        format!("graph.{EXTRACTOR_VERSION}.db"),
        "graph.10.db".to_string(),
        format!("graph.{EXTRACTOR_VERSION}.2.db"),
    ];
    for name in &unrecorded {
        fs::write(state.path().join(name), b"x").expect("seed database");
    }
    fs::write(state.path().join("change-history.3.sqlite3"), b"x").expect("seed history");

    let first = sync(repo.path(), state.path(), request(false)).expect("first build");
    let mut expected = unrecorded.to_vec();
    expected.sort();
    assert_eq!(first.unowned, expected);
    let one = published(state.path()).database;
    assert_eq!(one, format!("graph.{EXTRACTOR_VERSION}.1.db"));
    sync(repo.path(), state.path(), request(true)).expect("second build");
    let third = sync(repo.path(), state.path(), request(true)).expect("third build");
    assert_eq!(third.unowned, expected);
    // The builds skipped the unrecorded generation 2 instead of writing into it.
    assert_eq!(
        fs::read(state.path().join(format!("graph.{EXTRACTOR_VERSION}.2.db"))).expect("read"),
        b"x"
    );
    assert_eq!(
        published(state.path()).database,
        format!("graph.{EXTRACTOR_VERSION}.4.db")
    );
    for name in unrecorded
        .iter()
        .chain(["change-history.3.sqlite3".to_string()].iter())
    {
        assert!(state.path().join(name).exists(), "{name} was deleted");
    }
    // The first generation was recorded and superseded twice, so it is gone.
    assert!(!state.path().join(&one).exists());
}

#[test]
fn an_unreadable_record_deletes_nothing() {
    let repo = fixture();
    let state = tempfile::tempdir().expect("state");
    sync(repo.path(), state.path(), request(false)).expect("first build");
    let one = published(state.path()).database;
    sync(repo.path(), state.path(), request(false)).expect("second build");
    fs::write(state.path().join(OWNED_FILE), b"{not json").expect("corrupt record");
    let third = sync(repo.path(), state.path(), request(false)).expect("third build");
    assert!(
        state.path().join(&one).exists(),
        "unrecorded generation deleted"
    );
    assert!(third.unowned.contains(&one), "{:?}", third.unowned);
}

#[test]
fn a_concurrent_build_is_refused_while_the_lock_is_held() {
    let repo = fixture();
    let state = tempfile::tempdir().expect("state");
    let held = acquire_lock(state.path()).expect("first lock");
    let error = sync(repo.path(), state.path(), request(false)).expect_err("second build");
    let message = error.to_string();
    assert!(message.contains("another graph_sync"), "{message}");
    // The refusal names the holder (STD-03 R7).
    assert!(
        message.contains(&format!("\"pid\":{}", std::process::id())),
        "{message}"
    );
    drop(held);
    // A git child that another test forked while the lock file was open
    // shares its flock until it execs (close-on-exec), so allow a short wait.
    wait_for_idle_builder(state.path());
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        match sync(repo.path(), state.path(), request(false)) {
            Ok(_) => break,
            Err(error)
                if error.to_string().contains("another graph_sync")
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => panic!("build after release: {error}"),
        }
    }
}

#[test]
fn an_index_from_another_extractor_is_incompatible() {
    let state = tempfile::tempdir().expect("state");
    let pointer = PublishedIndex {
        database: "graph.1.1.db".to_string(),
        extractor_version: 1,
        store_schema_version: STORE_SCHEMA_VERSION,
        revision: None,
        worktree_dirty: false,
        synced_at: "2026-01-01T00:00:00Z".to_string(),
        mode: "full".to_string(),
        files: 0,
    };
    write_pointer(state.path(), &pointer).expect("pointer");
    let state_value = IndexState::read(state.path()).expect("read");
    assert_eq!(state_value, IndexState::Incompatible(pointer));
    match state_value.structure_index(state.path()) {
        StructureIndex::Unavailable { kind, reason } => {
            assert_eq!(kind, "structure_index_incompatible");
            assert!(reason.contains("full: true"), "{reason}");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn generation_names_are_parsed_strictly() {
    assert_eq!(
        generation_number(&format!("graph.{EXTRACTOR_VERSION}.12.db")),
        Some(12)
    );
    assert_eq!(
        generation_number(&format!("graph.{EXTRACTOR_VERSION}.db")),
        None
    );
    assert_eq!(
        graph_database_of("graph.11.3.db-shm"),
        Some("graph.11.3.db")
    );
    assert_eq!(graph_database_of(POINTER_FILE), None);
    assert_eq!(graph_database_of(LOCK_FILE), None);
    assert_eq!(graph_database_of(OWNED_FILE), None);
    assert_eq!(generation_of_any_extractor("graph.10.4.db"), Some(4));
    assert_eq!(generation_of_any_extractor("graph.10.db"), None);
    assert_eq!(generation_of_any_extractor("graph.x.4.db"), None);
    assert_eq!(graph_database_of("change-history.3.sqlite3"), None);
}

#[cfg(unix)]
#[test]
fn a_published_index_opens_read_only_in_a_read_only_directory() {
    use std::os::unix::fs::PermissionsExt;

    let repo = fixture();
    let state = tempfile::tempdir().expect("state");
    sync(repo.path(), state.path(), request(false)).expect("build");
    let published = published(state.path());
    let db_path = state.path().join(&published.database);
    let before = fs::read_dir(state.path()).expect("list").count();

    fs::set_permissions(state.path(), fs::Permissions::from_mode(0o555)).expect("freeze");
    let read = (|| {
        let graph = Graph::open_read_only(repo.path(), db_path.as_path())?;
        graph.with_read_connection(|conn| {
            conn.query_row("SELECT COUNT(*) FROM files", [], |row| row.get::<_, i64>(0))
                .map_err(|source| GraphError::sqlite("count files", source))
        })
    })();
    fs::set_permissions(state.path(), fs::Permissions::from_mode(0o700)).expect("thaw");

    assert_eq!(read.expect("read-only open and query"), 1);
    assert_eq!(fs::read_dir(state.path()).expect("list").count(), before);
}
