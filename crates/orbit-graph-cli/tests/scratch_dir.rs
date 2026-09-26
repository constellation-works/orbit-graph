//! The `.orbit-graph/` scratch directory stays out of `git status`, through
//! the packaged `orbit-graph` executable.

#![allow(clippy::expect_used)]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

#[test]
fn sync_and_history_leave_no_untracked_orbit_graph_entries() {
    let repo = committed_repository();

    let sync = run_json(repo.path(), ["sync"]);
    assert_eq!(sync["files_indexed"], 1, "{sync}");
    let history = run_json(repo.path(), ["history", "sync", "--branch", "main"]);
    assert!(history.is_object(), "{history}");
    let status = run_json(repo.path(), ["history", "status", "--branch", "main"]);
    assert!(status.is_object(), "{status}");

    let scratch = repo.path().join(".orbit-graph");
    let names = fs::read_dir(&scratch)
        .expect("read scratch directory")
        .map(|entry| entry.expect("scratch entry").file_name())
        .collect::<Vec<_>>();
    assert!(
        names
            .iter()
            .any(|name| name.to_string_lossy().ends_with(".db")),
        "{names:?}"
    );
    assert!(
        names
            .iter()
            .any(|name| name.to_string_lossy().starts_with("change-history.")),
        "{names:?}"
    );
    let gitignore = fs::read_to_string(scratch.join(".gitignore")).expect("scratch .gitignore");
    assert!(gitignore.lines().any(|line| line == "*"), "{gitignore:?}");

    let status = git_stdout(
        repo.path(),
        &["status", "--porcelain", "--untracked-files=all"],
    );
    assert!(
        !status.contains(".orbit-graph"),
        "untracked scratch entries: {status}"
    );
}

#[test]
fn sync_keeps_an_existing_scratch_gitignore() {
    let repo = committed_repository();
    let scratch = repo.path().join(".orbit-graph");
    fs::create_dir_all(&scratch).expect("create scratch directory");
    let user_rules = "# kept by the user\n*.db\n*.db-*\n*.lock\n";
    fs::write(scratch.join(".gitignore"), user_rules).expect("write user .gitignore");

    run_json(repo.path(), ["sync"]);

    assert_eq!(
        fs::read_to_string(scratch.join(".gitignore")).expect("scratch .gitignore"),
        user_rules
    );
}

/// A `.orbit-graph` symlink that points out of the repository is never written
/// through: a `.gitignore` with `*` in the umbrella repository's root would hide
/// every untracked file there. Where the index itself lands is ORB-13162.
#[cfg(unix)]
#[test]
fn sync_never_writes_a_gitignore_through_a_symlinked_scratch_dir() {
    let umbrella = committed_repository();
    fs::write(
        umbrella.path().join("notes.txt"),
        "untracked umbrella file\n",
    )
    .expect("write untracked file");
    let child = umbrella.path().join("child");
    fs::create_dir(&child).expect("create child repository");
    git(&child, &["init", "-q", "-b", "main"]);
    git(&child, &["config", "user.email", "graph@example.invalid"]);
    git(&child, &["config", "user.name", "Graph Test"]);
    fs::write(child.join("lib.rs"), "pub fn child() {}\n").expect("write child source");
    git(&child, &["add", "."]);
    git(&child, &["commit", "-q", "-m", "child"]);
    std::os::unix::fs::symlink("..", child.join(".orbit-graph")).expect("symlink scratch dir");

    let sync = run(&child, ["sync"]);
    assert!(
        sync.status.success(),
        "{}",
        String::from_utf8_lossy(&sync.stderr)
    );

    assert!(
        !umbrella.path().join(".gitignore").exists(),
        "a .gitignore was written outside the child repository"
    );
    let status = git_stdout(
        umbrella.path(),
        &["status", "--porcelain", "--untracked-files=all"],
    );
    assert!(
        status.lines().any(|line| line == "?? notes.txt"),
        "umbrella untracked files must stay visible: {status}"
    );
}

fn committed_repository() -> TempDir {
    let repo = TempDir::new().expect("create repository");
    git(repo.path(), &["init", "-q", "-b", "main"]);
    git(
        repo.path(),
        &["config", "user.email", "graph@example.invalid"],
    );
    git(repo.path(), &["config", "user.name", "Graph Test"]);
    fs::create_dir_all(repo.path().join("src")).expect("create source directory");
    fs::write(
        repo.path().join("src/lib.rs"),
        "pub fn entry() -> i32 {\n    1\n}\n",
    )
    .expect("write source");
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "fixture"]);
    repo
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

fn git(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

fn git_stdout(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("run git");
    assert!(output.status.success(), "git {args:?} failed");
    String::from_utf8(output.stdout).expect("UTF-8 git output")
}
