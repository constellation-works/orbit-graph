//! Smoke tests that exercise the packaged `orbit-graph` executable.

#![allow(clippy::expect_used)]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

#[test]
fn real_binary_indexes_and_queries_a_fixture() {
    let fixture = fixture_repository();

    let sync = run_json(fixture.path(), ["sync", "--full"]);
    assert!(
        sync["files_indexed"]
            .as_u64()
            .is_some_and(|count| count >= 1)
    );

    let search = run_json(
        fixture.path(),
        ["search", "helper", "--kind", "symbol", "--limit", "5"],
    );
    assert!(
        search["matches"]
            .as_array()
            .is_some_and(|matches| !matches.is_empty())
    );

    let show = run_json(
        fixture.path(),
        [
            "show",
            "symbol:src/lib.rs#entry:function",
            "--max-bytes",
            "256",
        ],
    );
    assert_eq!(show["metadata"]["file"], "src/lib.rs");
    assert!(
        show["source"]
            .as_str()
            .is_some_and(|source| source.contains("pub fn entry"))
    );

    let refs = run_json(
        fixture.path(),
        [
            "refs",
            "symbol:src/lib.rs#helper:function",
            "--confidence",
            "fuzzy",
            "--kind",
            "call",
        ],
    );
    assert!(refs["refs"].as_array().is_some_and(|refs| !refs.is_empty()));

    let callees = run_json(
        fixture.path(),
        ["callees", "symbol:src/lib.rs#entry:function"],
    );
    assert!(
        callees["callees"]
            .as_array()
            .is_some_and(|calls| !calls.is_empty())
    );

    let db_path = run_json(fixture.path(), ["db-path"]);
    assert!(
        db_path["path"]
            .as_str()
            .is_some_and(|path| path.contains("/.orbit-graph/"))
    );
}

#[test]
fn real_binary_rejects_malformed_selectors_with_json_error() {
    let fixture = fixture_repository();
    let output = run(fixture.path(), ["show", "not-a-selector"]);

    assert!(!output.status.success());
    let error: Value = serde_json::from_slice(&output.stderr).expect("JSON error payload");
    assert_eq!(error["error"]["code"], "selector_parse_error");
    assert!(
        error["error"]["message"]
            .as_str()
            .is_some_and(|message| { message.contains("selectors must start with") })
    );
}

#[test]
fn real_binary_help_succeeds() {
    let fixture = fixture_repository();
    let output = run(fixture.path(), ["--help"]);

    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Usage: orbit-graph <COMMAND>"));
    assert!(output.stderr.is_empty());
}

#[test]
fn real_binary_imports_syncs_reports_and_rebuilds_history() {
    let fixture = fixture_repository();
    let before = git_stdout(fixture.path(), ["rev-parse", "HEAD"]);
    fs::write(
        fixture.path().join("src/lib.rs"),
        "pub fn helper() -> i32 { 2 }\n\npub fn entry() -> i32 { helper() }\n",
    )
    .expect("edit fixture source");
    run_git(fixture.path(), ["add", "."]);
    run_git(
        fixture.path(),
        ["commit", "-m", "deliver update\n\nTask-Id: ORB-CLI"],
    );
    let after = git_stdout(fixture.path(), ["rev-parse", "HEAD"]);
    let repository = fixture
        .path()
        .canonicalize()
        .expect("canonical fixture")
        .to_string_lossy()
        .into_owned();
    let envelope = serde_json::json!({
        "schema_version": 1,
        "repository": repository,
        "landing_branch": "main",
        "before_revision": before,
        "after_revision": after,
        "delivery_id": "verified-cli-1",
        "evidence": "verified_delivery",
        "source": {"system": "cli_test", "record_id": "delivery-1"},
        "captured_at": "2026-09-07T00:00:00Z",
        "tasks": [{
            "task_id": "ORB-CLI",
            "title": "Exercise history CLI",
            "description": "Verify the public import contract",
            "acceptance_criteria": ["CLI operations return JSON"],
            "source": {"system": "cli_test", "record_id": "ORB-CLI"},
            "captured_at": "2026-09-07T00:00:00Z"
        }]
    });
    let envelope_path = fixture.path().join("delivery.json");
    fs::write(
        envelope_path.as_path(),
        serde_json::to_vec(&envelope).expect("encode envelope"),
    )
    .expect("write envelope");
    let envelope_arg = envelope_path.to_string_lossy();

    let imported = run_json(
        fixture.path(),
        ["history", "import", "--input", envelope_arg.as_ref()],
    );
    assert_eq!(imported["inserted"], true);
    let duplicate = run_json(
        fixture.path(),
        ["history", "import", "--input", envelope_arg.as_ref()],
    );
    assert_eq!(duplicate["inserted"], false);

    let synced = run_json(
        fixture.path(),
        ["history", "sync", "--branch", "main", "--limit", "10"],
    );
    assert_eq!(synced["complete"], true);
    let status = run_json(fixture.path(), ["history", "status", "--branch", "main"]);
    assert_eq!(status["verified_deliveries"], 1);
    assert_eq!(status["git_only_deliveries"], 1);
    assert_eq!(status["task_associations"], 2);

    let rebuilt = run_json(
        fixture.path(),
        ["history", "rebuild", "--branch", "main", "--limit", "10"],
    );
    assert_eq!(rebuilt["removed_deliveries"], 2);
    assert_eq!(rebuilt["sync"]["deliveries_inserted"], 1);
}

#[test]
fn real_binary_rejects_history_repository_mismatch_with_json_error() {
    let fixture = fixture_repository();
    fs::write(
        fixture.path().join("src/lib.rs"),
        "pub fn helper() -> i32 { 2 }\n",
    )
    .expect("edit fixture source");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "second"]);
    let after = git_stdout(fixture.path(), ["rev-parse", "HEAD"]);
    let before = git_stdout(fixture.path(), ["rev-parse", "HEAD~1"]);
    let envelope_path = fixture.path().join("bad-delivery.json");
    let envelope = serde_json::json!({
        "schema_version": 1, "repository": "wrong", "landing_branch": "main",
        "before_revision": before, "after_revision": after, "delivery_id": "bad",
        "evidence": "verified_delivery", "source": {"system": "test"},
        "captured_at": "2026-09-07T00:00:00Z", "tasks": []
    });
    fs::write(
        &envelope_path,
        serde_json::to_vec(&envelope).expect("encode"),
    )
    .expect("write bad envelope");
    let envelope_arg = envelope_path.to_string_lossy();
    let output = run(
        fixture.path(),
        ["history", "import", "--input", envelope_arg.as_ref()],
    );
    assert!(!output.status.success());
    let error: Value = serde_json::from_slice(&output.stderr).expect("JSON error");
    assert_eq!(error["error"]["code"], "graph_error");
    assert!(
        error["details"]
            .as_str()
            .is_some_and(|details| details.contains("mismatch"))
    );
}

fn run_json<const N: usize>(cwd: &Path, args: [&str; N]) -> Value {
    let output = run(cwd, args);
    assert!(
        output.status.success(),
        "orbit-graph failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON command output")
}

fn run<const N: usize>(cwd: &Path, args: [&str; N]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("run orbit-graph")
}

fn fixture_repository() -> TempDir {
    let fixture = TempDir::new().expect("create fixture repository");
    run_git(fixture.path(), ["init", "-b", "main"]);
    run_git(
        fixture.path(),
        ["config", "user.email", "graph@example.invalid"],
    );
    run_git(fixture.path(), ["config", "user.name", "Graph Test"]);
    fs::create_dir_all(fixture.path().join("src")).expect("create source directory");
    fs::write(
        fixture.path().join("src/lib.rs"),
        "pub fn helper() -> i32 { 1 }\n\npub fn entry() -> i32 { helper() }\n",
    )
    .expect("write fixture source");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "fixture"]);
    fixture
}

fn run_git<const N: usize>(cwd: &Path, args: [&str; N]) {
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

fn git_stdout<const N: usize>(cwd: &Path, args: [&str; N]) -> String {
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
    String::from_utf8(output.stdout)
        .expect("Git output is UTF-8")
        .trim()
        .to_string()
}
