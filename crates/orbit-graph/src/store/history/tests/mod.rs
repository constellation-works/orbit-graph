use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

use super::*;

#[test]
fn import_is_idempotent_and_preserves_multi_task_provenance() {
    let repo = fixture_repo();
    write(
        repo.path(),
        "src/lib.rs",
        "pub fn value() -> i32 { 1 }\npub fn gone() {}\n",
    );
    commit(repo.path(), "before");
    let before = head(repo.path());
    write(repo.path(), "src/lib.rs", "pub fn value() -> i32 { 2 }\n");
    commit(repo.path(), "after");
    let after = head(repo.path());
    let index = HistoryIndex::open(repo.path(), "main").expect("open history");
    let mut delivery = fixture_delivery(&index, before, after, "verified-1");
    delivery.landing_branch = "refs/heads/main".into();
    delivery.tasks.push(delivery.tasks[0].clone());
    assert!(
        index
            .import(delivery.clone())
            .expect("first import")
            .inserted
    );
    assert!(!index.import(delivery).expect("duplicate import").inserted);
    let status = index.status().expect("status");
    assert_eq!(status.deliveries, 1);
    assert_eq!(status.verified_deliveries, 1);
    assert_eq!(status.task_associations, 2);
    let loaded = index.deliveries().expect("load deliveries");
    assert_eq!(loaded[0].delivery.tasks.len(), 2);
    let historical = loaded[0]
        .files
        .iter()
        .flat_map(|file| &file.symbols)
        .filter_map(|symbol| symbol.before.as_ref())
        .find(|item| item.symbol.name == "gone")
        .expect("deleted symbol");
    let resolution = index
        .resolve_current_symbol(&historical.symbol)
        .expect("resolve deleted symbol");
    assert_eq!(resolution.status, CurrentSymbolStatus::Deleted);
    assert!(resolution.matches.is_empty());
}

#[test]
fn rejects_repository_mismatch_and_conflicting_duplicate() {
    let repo = fixture_repo();
    write(repo.path(), "a.rs", "fn a() {}\n");
    commit(repo.path(), "one");
    let before = head(repo.path());
    write(repo.path(), "a.rs", "fn a() { println!(\"x\"); }\n");
    commit(repo.path(), "two");
    let after = head(repo.path());
    let index = HistoryIndex::open(repo.path(), "main").expect("open");
    let mut mismatch = fixture_delivery(&index, before.clone(), after.clone(), "d");
    mismatch.repository = "different".into();
    assert!(index.import(mismatch).is_err());
    let delivery = fixture_delivery(&index, before, after.clone(), "d");
    index.import(delivery.clone()).expect("import");
    let mut conflict = delivery;
    conflict.after_revision = conflict.before_revision.clone();
    assert!(index.import(conflict).is_err());
    assert_eq!(index.status().expect("status").deliveries, 1);
}

#[test]
fn sync_is_incremental_and_rejects_rebound_history_without_partial_writes() {
    let repo = fixture_repo();
    write(repo.path(), "a.rs", "fn a() -> i32 { 1 }\n");
    commit(repo.path(), "root");
    write(repo.path(), "a.rs", "fn a() -> i32 { 2 }\n");
    commit_with_body(repo.path(), "second\n\nTask-Id: ORB-1, ORB-2");
    let index = HistoryIndex::open(repo.path(), "main").expect("open");
    let first = index.sync(Some(10)).expect("sync");
    assert_eq!(first.deliveries_inserted, 1);
    let git_only = index.deliveries().expect("load Git-only delivery");
    assert_eq!(git_only[0].delivery.evidence, DeliveryEvidence::GitOnly);
    assert_eq!(
        git_only[0].delivery.delivered_at.status,
        TemporalStatus::Uncertain
    );
    assert_eq!(
        git_only[0].delivery.delivered_at.source.system,
        "git_commit"
    );
    assert_eq!(
        git_only[0].delivery.tasks[0].created_at.status,
        TemporalStatus::Unavailable
    );
    assert_eq!(
        git_only[0].delivery.tasks[0].text_availability,
        TaskTextAvailability::Uncertain
    );
    assert_eq!(
        index
            .sync(Some(10))
            .expect("idempotent sync")
            .deliveries_inserted,
        0
    );
    let original_count = index.status().expect("status").deliveries;
    git(repo.path(), &["reset", "--hard", "HEAD~1"]);
    write(repo.path(), "a.rs", "fn a() -> i32 { 3 }\n");
    commit(repo.path(), "replacement");
    let error = index.sync(Some(10)).expect_err("rebound cursor rejected");
    assert!(error.to_string().contains("stored cursor is not on"));
    assert_eq!(
        index.status().expect("unchanged status").deliveries,
        original_count
    );
}

#[test]
fn bounded_sync_resumes_and_failed_rebuild_leaves_existing_scope_intact() {
    let repo = fixture_repo();
    write(repo.path(), "a.rs", "fn a() {}\n");
    commit(repo.path(), "one");
    write(repo.path(), "a.rs", "fn a() { }\n");
    commit(repo.path(), "two");
    write(repo.path(), "a.rs", "fn a() {  }\n");
    commit(repo.path(), "three");
    let index = HistoryIndex::open(repo.path(), "main").expect("open");
    let partial = index.sync(Some(1)).expect("bounded partial sync");
    assert!(!partial.complete);
    assert!(partial.resume_from.is_some());
    let partial_status = index.status().expect("partial status");
    assert_eq!(partial_status.deliveries, 1);
    assert_eq!(partial_status.cursor, None);
    assert!(!partial_status.complete);
    let completed = index.sync(Some(10)).expect("resumed full sync");
    assert!(completed.complete);
    assert_eq!(index.status().expect("complete status").deliveries, 2);
    let before = index.status().expect("before failed rebuild");
    assert!(index.rebuild(Some(1)).is_err());
    assert_eq!(
        index.status().expect("after failed rebuild").deliveries,
        before.deliveries
    );
    assert_eq!(
        index.status().expect("cursor retained").cursor,
        before.cursor
    );
}

#[test]
fn interrupted_sync_rolls_back_delivery_rows_and_cursor() {
    let repo = fixture_repo();
    write(repo.path(), "a.rs", "fn a() -> i32 { 1 }\n");
    commit(repo.path(), "one");
    write(repo.path(), "a.rs", "fn a() -> i32 { 2 }\n");
    commit(repo.path(), "two");
    write(repo.path(), "b.rs", "fn b() -> i32 { 3 }\n");
    commit(repo.path(), "three");
    let index = HistoryIndex::open(repo.path(), "main").expect("open");
    set_sync_interruption(Some(index.repo_root.clone()));
    let error = index.sync(Some(10)).expect_err("simulated interruption");
    set_sync_interruption(None);
    assert!(error.to_string().contains("simulated interruption"));
    let status = index.status().expect("rolled-back status");
    assert_eq!(status.deliveries, 0);
    assert_eq!(status.cursor, None);
    assert_eq!(index.sync(Some(10)).expect("retry").deliveries_inserted, 2);
}

#[test]
fn merge_sync_uses_first_parent_boundary_and_keeps_multi_file_commit() {
    let repo = fixture_repo();
    write(repo.path(), "root.rs", "fn root() {}\n");
    commit(repo.path(), "root");
    git(repo.path(), &["checkout", "-b", "feature"]);
    write(repo.path(), "feature.rs", "fn feature() {}\n");
    commit(repo.path(), "feature work");
    git(repo.path(), &["checkout", "main"]);
    write(repo.path(), "main.rs", "fn main_line() {}\n");
    write(repo.path(), "second.rs", "fn second() {}\n");
    commit(repo.path(), "main multi-file");
    let first_parent = head(repo.path());
    git(
        repo.path(),
        &["merge", "--no-ff", "feature", "-m", "merge feature"],
    );
    let merge = head(repo.path());

    let index = HistoryIndex::open(repo.path(), "main").expect("open");
    let report = index.sync(Some(10)).expect("sync merge history");
    assert_eq!(report.deliveries_inserted, 2);
    let deliveries = index.deliveries().expect("deliveries");
    let merge_delivery = deliveries
        .iter()
        .find(|delivery| delivery.delivery.after_revision == merge)
        .expect("merge delivery");
    assert_eq!(merge_delivery.delivery.before_revision, first_parent);
    assert!(
        merge_delivery
            .files
            .iter()
            .any(|file| file.new_path.as_deref() == Some("feature.rs"))
    );
    assert!(deliveries.iter().any(|delivery| delivery.files.len() == 2));
}

#[test]
fn temporal_facts_round_trip_and_invalid_timestamps_are_atomic() {
    let repo = fixture_repo();
    write(repo.path(), "a.rs", "fn a() -> i32 { 1 }\n");
    commit(repo.path(), "one");
    let before = head(repo.path());
    write(repo.path(), "a.rs", "fn a() -> i32 { 2 }\n");
    commit(repo.path(), "two");
    let after = head(repo.path());
    let index = HistoryIndex::open(repo.path(), "main").expect("open");
    let delivery = fixture_delivery(&index, before, after, "temporal");
    let mut legacy = delivery.clone();
    legacy.schema_version = 1;
    assert!(index.import(legacy).is_err());
    assert_eq!(
        index.status().expect("empty after v1 rejection").deliveries,
        0
    );
    index
        .import(delivery.clone())
        .expect("import temporal facts");
    let loaded = index.deliveries().expect("load delivery");
    assert_eq!(loaded[0].delivery, delivery);

    let before_count = index
        .status()
        .expect("status before invalid import")
        .deliveries;
    let mut invalid = delivery;
    invalid.delivery_id = "invalid-time".into();
    invalid.captured_at = "2026-99-99".into();
    assert!(index.import(invalid).is_err());
    assert_eq!(
        index
            .status()
            .expect("status after invalid import")
            .deliveries,
        before_count
    );
}

#[test]
fn post_execution_and_uncertain_snapshots_are_not_pre_execution_evidence() {
    let repo = fixture_repo();
    write(repo.path(), "a.rs", "fn a() -> i32 { 1 }\n");
    commit(repo.path(), "one");
    let before = head(repo.path());
    write(repo.path(), "a.rs", "fn a() -> i32 { 2 }\n");
    commit(repo.path(), "two");
    let after = head(repo.path());
    let index = HistoryIndex::open(repo.path(), "main").expect("open");
    let mut invalid = fixture_delivery(
        &index,
        before.clone(),
        after.clone(),
        "invalid-availability",
    );
    invalid.tasks[0].snapshot_available_at = unavailable_time("missing-snapshot-time");
    assert!(index.import(invalid).is_err());
    assert_eq!(index.status().expect("still empty").deliveries, 0);

    let mut delivery = fixture_delivery(&index, before, after, "availability");
    delivery.tasks[0].text_availability = TaskTextAvailability::PostExecution;
    delivery.tasks[1].text_availability = TaskTextAvailability::Uncertain;
    delivery.tasks[1].snapshot_available_at = unavailable_time("snapshot-unavailable");
    index
        .import(delivery)
        .expect("import unavailable snapshots");
    let loaded = index.deliveries().expect("load delivery");
    assert_eq!(
        loaded[0].delivery.tasks[0].text_availability,
        TaskTextAvailability::PostExecution
    );
    assert_eq!(
        loaded[0].delivery.tasks[1].text_availability,
        TaskTextAvailability::Uncertain
    );
    assert_eq!(
        loaded[0].delivery.tasks[1].snapshot_available_at.status,
        TemporalStatus::Unavailable
    );
}

#[test]
fn path_lineage_records_renames_and_deletions_and_survives_rebuild() {
    let repo = fixture_repo();
    write(repo.path(), "a.rs", "fn alpha() -> i32 { 1 }\n");
    write(repo.path(), "b.rs", "fn beta() -> i32 { 1 }\n");
    write(repo.path(), "c.rs", "fn gamma() -> i32 { 1 }\n");
    commit(repo.path(), "root");
    std::fs::create_dir_all(repo.path().join("moved")).expect("moved dir");
    git(repo.path(), &["mv", "a.rs", "moved/a.rs"]);
    std::fs::remove_file(repo.path().join("b.rs")).expect("delete b");
    write(repo.path(), "c.rs", "fn gamma() -> i32 { 2 }\n");
    commit(repo.path(), "move, delete, modify");
    let index = HistoryIndex::open(repo.path(), "main").expect("open");
    index.sync(Some(10)).expect("sync");
    let summarize = |steps: Vec<PathLineageStep>| {
        steps
            .into_iter()
            .map(|step| (step.old_path, step.new_path))
            .collect::<Vec<_>>()
    };
    let expected = vec![
        ("a.rs".to_string(), Some("moved/a.rs".to_string())),
        ("b.rs".to_string(), None),
    ];
    let mut synced = summarize(index.path_lineage().expect("lineage"));
    synced.sort();
    assert_eq!(synced, expected);
    index.rebuild(Some(10)).expect("rebuild");
    let mut rebuilt = summarize(index.path_lineage().expect("rebuilt lineage"));
    rebuilt.sort();
    assert_eq!(rebuilt, expected);
}

#[test]
fn opening_the_current_schema_copies_a_compatible_previous_index_once() {
    let repo = fixture_repo();
    write(repo.path(), "a.rs", "fn alpha() -> i32 { 1 }\n");
    commit(repo.path(), "root");
    let root = head(repo.path());
    write(repo.path(), "a.rs", "fn alpha() -> i32 { 2 }\n");
    commit(repo.path(), "edit");
    let edited = head(repo.path());
    git(repo.path(), &["mv", "a.rs", "b.rs"]);
    commit(repo.path(), "rename");
    let index = HistoryIndex::open(repo.path(), "main").expect("open");
    index.sync(Some(10)).expect("sync");
    index
        .import(fixture_delivery(&index, root, edited, "verified-edit"))
        .expect("import verified");
    let original = index.status().expect("original status");
    let original_lineage = index.path_lineage().expect("original lineage");
    assert_eq!(original_lineage.len(), 1);

    // Turn the current database into a previous-schema file and remove the current one.
    let current = index.database_path().to_path_buf();
    let previous = current.with_file_name(format!(
        "change-history.{PREVIOUS_HISTORY_INDEX_SCHEMA_VERSION}.sqlite3"
    ));
    std::fs::copy(&current, &previous).expect("copy database");
    {
        let conn = Connection::open(&previous).expect("open previous");
        conn.execute_batch(&format!(
            "DROP TABLE history_path_lineage; DELETE FROM history_meta WHERE key='{LEGACY_COPY_META_KEY}'; UPDATE history_meta SET value='{PREVIOUS_HISTORY_INDEX_SCHEMA_VERSION}' WHERE key='schema_version';"
        ))
        .expect("downgrade copy");
    }
    std::fs::remove_file(&current).expect("remove current");

    let reopened = HistoryIndex::open(repo.path(), "main").expect("reopen");
    let copied = reopened.status().expect("copied status");
    assert_eq!(copied.deliveries, original.deliveries);
    assert_eq!(copied.verified_deliveries, 1);
    assert_eq!(copied.task_associations, original.task_associations);
    assert_eq!(copied.cursor, original.cursor);
    assert_eq!(
        reopened.path_lineage().expect("copied lineage"),
        original_lineage
    );
    assert_eq!(
        reopened.deliveries().expect("copied deliveries"),
        index.deliveries().expect("original deliveries")
    );

    // A second open never copies again, even after the scope is cleared.
    reopened.rebuild(Some(10)).expect("rebuild");
    let again = HistoryIndex::open(repo.path(), "main").expect("open again");
    assert_eq!(again.status().expect("status again").verified_deliveries, 0);

    // An incompatible previous index is left alone and recorded as skipped.
    std::fs::remove_file(&current).expect("remove current again");
    {
        let conn = Connection::open(&previous).expect("open previous");
        conn.execute(
            "UPDATE history_meta SET value='0' WHERE key='extractor_version'",
            [],
        )
        .expect("mark incompatible");
    }
    let skipped = HistoryIndex::open(repo.path(), "main").expect("open skipped");
    assert_eq!(skipped.status().expect("skipped status").deliveries, 0);
    let conn = Connection::open(&current).expect("open current");
    let outcome: String = conn
        .query_row(
            "SELECT value FROM history_meta WHERE key=?1",
            [LEGACY_COPY_META_KEY],
            |row| row.get(0),
        )
        .expect("legacy outcome");
    assert!(outcome.contains("incompatible"), "{outcome}");
}

#[test]
fn concurrent_opens_copy_a_previous_index_exactly_once() {
    let repo = fixture_repo();
    write(repo.path(), "a.rs", "fn alpha() -> i32 { 1 }\n");
    commit(repo.path(), "root");
    let root = head(repo.path());
    write(repo.path(), "a.rs", "fn alpha() -> i32 { 2 }\n");
    commit(repo.path(), "edit");
    let edited = head(repo.path());
    git(repo.path(), &["mv", "a.rs", "b.rs"]);
    commit(repo.path(), "rename");
    let index = HistoryIndex::open(repo.path(), "main").expect("open");
    index.sync(Some(10)).expect("sync");
    index
        .import(fixture_delivery(&index, root, edited, "verified-edit"))
        .expect("import verified");
    let original = index.status().expect("original status");
    let current = index.database_path().to_path_buf();
    let previous = current.with_file_name(format!(
        "change-history.{PREVIOUS_HISTORY_INDEX_SCHEMA_VERSION}.sqlite3"
    ));
    std::fs::copy(&current, &previous).expect("copy database");
    {
        let conn = Connection::open(&previous).expect("open previous");
        conn.execute_batch(&format!(
            "DROP TABLE history_path_lineage; DELETE FROM history_meta WHERE key='{LEGACY_COPY_META_KEY}'; UPDATE history_meta SET value='{PREVIOUS_HISTORY_INDEX_SCHEMA_VERSION}' WHERE key='schema_version';"
        ))
        .expect("downgrade copy");
    }

    for round in 0..3 {
        std::fs::remove_file(&current).expect("remove current");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let handles = (0..4)
            .map(|_| {
                let barrier = barrier.clone();
                let root = repo.path().to_path_buf();
                std::thread::spawn(move || {
                    barrier.wait();
                    HistoryIndex::open(root.as_path(), "main")
                        .map(|index| index.status().expect("status").deliveries)
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            let deliveries = handle
                .join()
                .expect("open thread")
                .unwrap_or_else(|error| panic!("round {round}: concurrent open failed: {error}"));
            assert_eq!(deliveries, original.deliveries, "round {round}");
        }
        let reopened = HistoryIndex::open(repo.path(), "main").expect("reopen");
        let status = reopened.status().expect("status");
        assert_eq!(status.deliveries, original.deliveries);
        assert_eq!(status.verified_deliveries, 1);
        assert_eq!(status.task_associations, original.task_associations);
        assert_eq!(status.cursor, original.cursor);
        assert_eq!(reopened.path_lineage().expect("lineage").len(), 1);
        let conn = Connection::open(&current).expect("open current");
        let outcome: String = conn
            .query_row(
                "SELECT value FROM history_meta WHERE key=?1",
                [LEGACY_COPY_META_KEY],
                |row| row.get(0),
            )
            .expect("legacy outcome");
        assert!(outcome.starts_with("copied"), "{outcome}");
    }
}

fn fixture_delivery(
    index: &HistoryIndex,
    before: String,
    after: String,
    id: &str,
) -> DeliveryImport {
    let task = |task_id: &str| TaskAssociation {
        task_id: task_id.into(),
        title: format!("Task {task_id}"),
        description: "description".into(),
        acceptance_criteria: vec!["criterion".into()],
        source: Provenance {
            system: "test_tasks".into(),
            record_id: Some(task_id.into()),
        },
        created_at: known_time(format!("{task_id}:created")),
        snapshot_available_at: known_time(format!("{task_id}:snapshot")),
        text_availability: TaskTextAvailability::KnownPreExecution,
        captured_at: "2026-09-07T00:00:00Z".into(),
    };
    DeliveryImport {
        schema_version: DELIVERY_IMPORT_SCHEMA_VERSION,
        repository: index.repository().into(),
        landing_branch: "main".into(),
        before_revision: before,
        after_revision: after,
        delivery_id: id.into(),
        evidence: DeliveryEvidence::VerifiedDelivery,
        source: Provenance {
            system: "test_delivery".into(),
            record_id: Some(id.into()),
        },
        delivered_at: known_time(format!("{id}:delivery")),
        captured_at: "2026-09-07T00:00:00Z".into(),
        tasks: vec![task("A"), task("B")],
    }
}

fn known_time(record_id: String) -> TemporalFact {
    TemporalFact {
        status: TemporalStatus::Known,
        timestamp: Some("2026-09-06T23:59:59Z".into()),
        source: Provenance {
            system: "test_clock".into(),
            record_id: Some(record_id),
        },
    }
}

fn unavailable_time(record_id: &str) -> TemporalFact {
    TemporalFact {
        status: TemporalStatus::Unavailable,
        timestamp: None,
        source: Provenance {
            system: "test_clock".into(),
            record_id: Some(record_id.into()),
        },
    }
}

fn fixture_repo() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    git(dir.path(), &["init", "-b", "main"]);
    git(
        dir.path(),
        &["config", "user.email", "test@example.invalid"],
    );
    git(dir.path(), &["config", "user.name", "Test"]);
    dir
}
fn write(root: &Path, path: &str, content: &str) {
    let path = root.join(path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(path, content).expect("write");
}
fn commit(root: &Path, message: &str) {
    git(root, &["add", "-A"]);
    git(root, &["commit", "-m", message]);
}
fn commit_with_body(root: &Path, message: &str) {
    git(root, &["add", "-A"]);
    git(root, &["commit", "-m", message]);
}
fn head(root: &Path) -> String {
    String::from_utf8(git_output(root, &["rev-parse", "HEAD"]))
        .expect("utf8")
        .trim()
        .into()
}
fn git(root: &Path, args: &[&str]) {
    let _ = git_output(root, args);
}
fn git_output(root: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}
