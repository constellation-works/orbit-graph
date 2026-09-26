//! Incremental sync re-resolves references in unchanged files whose target
//! was defined, removed or renamed in a changed file, through the packaged
//! `orbit-graph` executable.

#![allow(clippy::expect_used)]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

/// Callers in files the scenarios never edit. Their outbound refs are what an
/// incremental sync must bring in line with a full sync.
const CALLERS: [&str; 2] = [
    "symbol:src/importer.rs#import_caller:function",
    "symbol:src/plain.rs#plain_caller:function",
];

#[test]
fn renaming_a_definition_demotes_remote_callers_like_a_full_sync() {
    let repo = fixture_repository();
    sync(repo.path(), false);
    assert_eq!(
        callee(repo.path(), CALLERS[0], "remote_target"),
        (
            "import_resolved".to_string(),
            Some("remote_target".to_string())
        )
    );
    assert_eq!(
        callee(repo.path(), CALLERS[1], "remote_target"),
        ("same_module".to_string(), Some("remote_target".to_string()))
    );

    write(
        repo.path(),
        "src/defs.rs",
        "pub fn remote_target_renamed() -> i32 {\n    1\n}\n",
    );
    let report = sync(repo.path(), false);
    assert_eq!(report["files_changed"], 1, "{report}");

    for caller in CALLERS {
        assert_eq!(
            callee(repo.path(), caller, "remote_target"),
            ("fuzzy_name".to_string(), None),
            "{caller}"
        );
    }
    assert_incremental_matches_full(repo.path());
}

#[test]
fn removing_a_definition_file_demotes_remote_callers_like_a_full_sync() {
    let repo = fixture_repository();
    sync(repo.path(), false);

    fs::remove_file(repo.path().join("src/defs.rs")).expect("remove definition file");
    let report = sync(repo.path(), false);
    assert_eq!(report["files_removed"], 1, "{report}");

    for caller in CALLERS {
        assert_eq!(
            callee(repo.path(), caller, "remote_target"),
            ("fuzzy_name".to_string(), None),
            "{caller}"
        );
    }
    assert_incremental_matches_full(repo.path());
}

#[test]
fn adding_a_unique_definition_promotes_a_fuzzy_remote_call_like_a_full_sync() {
    let repo = fixture_repository();
    sync(repo.path(), false);
    assert_eq!(
        callee(repo.path(), CALLERS[1], "later_target"),
        ("fuzzy_name".to_string(), None)
    );

    write(
        repo.path(),
        "src/later.rs",
        "pub fn later_target() -> i32 {\n    2\n}\n",
    );
    let report = sync(repo.path(), false);
    assert_eq!(report["files_changed"], 1, "{report}");

    assert_eq!(
        callee(repo.path(), CALLERS[1], "later_target"),
        ("same_module".to_string(), Some("later_target".to_string()))
    );
    assert_incremental_matches_full(repo.path());
}

/// Every caller's outbound refs after the incremental sync that just ran
/// equal those after rebuilding the same tree from scratch.
fn assert_incremental_matches_full(repo: &Path) {
    let incremental = CALLERS.map(|caller| run_json(repo, ["callees", caller]));
    let rebuilt = sync(repo, true);
    assert!(
        rebuilt["files_indexed"].as_u64().is_some_and(|n| n >= 3),
        "{rebuilt}"
    );
    let full = CALLERS.map(|caller| run_json(repo, ["callees", caller]));
    assert_eq!(incremental, full);
}

/// The confidence and resolved qualified name of the caller's one callee
/// named `target_name`.
fn callee(repo: &Path, caller: &str, target_name: &str) -> (String, Option<String>) {
    let callees = run_json(repo, ["callees", caller]);
    let matches = callees["callees"]
        .as_array()
        .expect("callees array")
        .iter()
        .filter(|callee| callee["target_name"] == target_name)
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "{callees}");
    (
        matches[0]["confidence"]
            .as_str()
            .expect("confidence")
            .to_string(),
        matches[0]["target_qualified"].as_str().map(str::to_string),
    )
}

fn sync(repo: &Path, full: bool) -> Value {
    if full {
        run_json(repo, ["sync", "--full"])
    } else {
        run_json(repo, ["sync"])
    }
}

fn fixture_repository() -> TempDir {
    let repo = TempDir::new().expect("create fixture repository");
    run_git(repo.path(), &["init", "-q", "-b", "main"]);
    write(
        repo.path(),
        "src/lib.rs",
        "mod defs;\nmod importer;\nmod later;\nmod plain;\n",
    );
    write(
        repo.path(),
        "src/defs.rs",
        "pub fn remote_target() -> i32 {\n    1\n}\n",
    );
    write(
        repo.path(),
        "src/importer.rs",
        "use crate::defs::remote_target;\n\npub fn import_caller() -> i32 {\n    remote_target()\n}\n",
    );
    write(
        repo.path(),
        "src/plain.rs",
        "pub fn plain_caller() -> i32 {\n    remote_target() + later_target()\n}\n",
    );
    repo
}

fn write(repo: &Path, path: &str, contents: &str) {
    let target = repo.join(path);
    fs::create_dir_all(target.parent().expect("parent directory")).expect("create directory");
    fs::write(target, contents).expect("write fixture file");
    // Incremental sync compares mtimes before hashing; make sure a rewrite
    // within the same timestamp tick still reads as a change.
    bump_mtime(repo, path);
}

fn bump_mtime(repo: &Path, path: &str) {
    let file = fs::File::options()
        .write(true)
        .open(repo.join(path))
        .expect("open fixture file");
    let modified = file
        .metadata()
        .and_then(|metadata| metadata.modified())
        .expect("fixture mtime");
    file.set_modified(modified + std::time::Duration::from_secs(2))
        .expect("bump fixture mtime");
}

fn run_json<const N: usize>(cwd: &Path, args: [&str; N]) -> Value {
    let output = run(cwd, args);
    assert!(
        output.status.success(),
        "orbit-graph {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON command output")
}

fn run<const N: usize>(cwd: &Path, args: [&str; N]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
        .current_dir(cwd)
        .args(["--format", "json"])
        .args(args)
        .output()
        .expect("run orbit-graph")
}

fn run_git(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}
