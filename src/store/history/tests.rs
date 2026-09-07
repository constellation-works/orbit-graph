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
    assert!(error.to_string().contains("diverged or was rebound"));
    assert_eq!(
        index.status().expect("unchanged status").deliveries,
        original_count
    );
}

#[test]
fn bounded_sync_and_failed_rebuild_leave_existing_scope_intact() {
    let repo = fixture_repo();
    write(repo.path(), "a.rs", "fn a() {}\n");
    commit(repo.path(), "one");
    write(repo.path(), "a.rs", "fn a() { }\n");
    commit(repo.path(), "two");
    write(repo.path(), "a.rs", "fn a() {  }\n");
    commit(repo.path(), "three");
    let index = HistoryIndex::open(repo.path(), "main").expect("open");
    assert!(index.sync(Some(1)).is_err());
    assert_eq!(
        index
            .status()
            .expect("empty after bounded failure")
            .deliveries,
        0
    );
    index.sync(Some(10)).expect("full sync");
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
        captured_at: "2026-09-07T00:00:00Z".into(),
        tasks: vec![task("A"), task("B")],
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
