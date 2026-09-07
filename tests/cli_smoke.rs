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
