//! Index and history state cannot be redirected or pre-populated by
//! repository content, and is created owner-only, through the packaged
//! `orbit-graph` executable (STD-05 §R6–§R9).
//!
//! Every command runs under `umask 0002`, set on the child process only
//! (STD-04 §R6), so a mode the binary did not ask for would show up.

#![cfg(unix)]
#![allow(clippy::expect_used)]
#![allow(
    clippy::disallowed_methods,
    reason = "fixtures are written with fs::write; clippy.toml bans it only from shipped code"
)]

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

/// Repository content that stands in for orbit-graph state.
#[derive(Debug, Clone, Copy)]
enum Plant {
    /// A committed `.orbit-graph` symlink to a directory outside the repo.
    CommittedScratchLink,
    /// The graph database replaced by a symlink to a file outside the repo.
    GraphDatabaseLink,
    /// The graph database lock replaced by a symlink outside the repo.
    GraphLockLink,
    /// The history index lock replaced by a symlink outside the repo.
    HistoryLockLink,
    /// `.orbit-graph` as a dangling symlink to a path outside the repo.
    DanglingScratchLink,
    /// The graph database as a dangling symlink to a path outside the repo.
    DanglingDatabaseLink,
    /// A committed `.orbit-graph/explorer/snapshots/<sha>/` cache entry.
    TrackedSnapshotEntry,
}

const PLANTS: [Plant; 7] = [
    Plant::CommittedScratchLink,
    Plant::GraphDatabaseLink,
    Plant::GraphLockLink,
    Plant::HistoryLockLink,
    Plant::DanglingScratchLink,
    Plant::DanglingDatabaseLink,
    Plant::TrackedSnapshotEntry,
];

const COMMANDS: [&[&str]; 3] = [
    &["sync"],
    &["history", "status", "--branch", "main"],
    &["clean", "--confirm"],
];

#[test]
fn state_commands_refuse_planted_state_and_touch_nothing_outside_the_repository() {
    for plant in PLANTS {
        for command in COMMANDS {
            let umbrella = TempDir::new().expect("umbrella directory");
            let repo = committed_repository(umbrella.path());
            let outside = umbrella.path().join("outside");
            fs::create_dir(&outside).expect("outside directory");
            for victim in ["victim.db", "victim.lock"] {
                let path = outside.join(victim);
                fs::write(&path, format!("{victim} belongs to someone else\n")).expect("victim");
                fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod");
            }
            fs::set_permissions(&outside, fs::Permissions::from_mode(0o755)).expect("chmod");
            plant_state(&repo, plant);
            let before = outside_state(umbrella.path(), &repo);

            let output = run_under_umask(&repo, command);

            assert!(
                !output.status.success(),
                "{plant:?} `{}` must be refused",
                command.join(" ")
            );
            let error: Value =
                serde_json::from_slice(&output.stderr).expect("JSON error on stderr");
            assert_eq!(
                error["code"],
                "unsafe_state_path",
                "{plant:?} `{}`: {error}",
                command.join(" ")
            );
            let message = error["error"].as_str().expect("error message");
            assert!(
                message.starts_with("refusing orbit-graph state path /"),
                "the refusal names the path: {message}"
            );
            assert_eq!(
                outside_state(umbrella.path(), &repo),
                before,
                "{plant:?} `{}` created, wrote or chmodded something outside the repository",
                command.join(" ")
            );
        }
    }
}

#[test]
fn state_is_owner_only_under_a_permissive_umask_and_a_loose_database_is_repaired() {
    let umbrella = TempDir::new().expect("umbrella directory");
    let repo = committed_repository(umbrella.path());
    for command in [&["sync"][..], &["history", "sync", "--branch", "main"][..]] {
        let output = run_under_umask(&repo, command);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let scratch = repo.join(".orbit-graph");
    assert_eq!(mode(&scratch), 0o700, "{}", scratch.display());
    let files = state_files(&scratch);
    assert!(files.len() >= 4, "database, history and locks: {files:?}");
    for file in &files {
        assert_eq!(mode(file), 0o600, "{}", file.display());
    }

    let database = graph_database(&repo);
    fs::set_permissions(&database, fs::Permissions::from_mode(0o644)).expect("loosen database");
    fs::set_permissions(&scratch, fs::Permissions::from_mode(0o775)).expect("loosen scratch");
    let output = run_under_umask(&repo, &["sync"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        mode(&database),
        0o600,
        "a 0644 database is repaired on open"
    );
    assert_eq!(mode(&scratch), 0o700, "the scratch directory is repaired");
}

#[test]
fn show_refuses_a_selector_that_a_symlinked_directory_carries_outside_the_worktree() {
    let umbrella = TempDir::new().expect("umbrella directory");
    let repo = committed_repository(umbrella.path());
    fs::create_dir_all(repo.join("src/dir")).expect("source directory");
    fs::write(repo.join("src/dir/x.rs"), "pub fn inside() -> i32 { 1 }\n").expect("source");
    let output = run_under_umask(&repo, &["sync"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let outside = umbrella.path().join("outside");
    fs::create_dir(&outside).expect("outside directory");
    fs::write(
        outside.join("x.rs"),
        "pub fn inside() -> i32 { 0x5ec2e7 }\n",
    )
    .expect("secret");
    fs::remove_dir_all(repo.join("src/dir")).expect("remove source directory");
    symlink(&outside, repo.join("src/dir")).expect("symlink the directory outside");

    let output = run_under_umask(&repo, &["show", "symbol:src/dir/x.rs#inside:function"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !output.status.success(),
        "show must refuse the selector: {stdout}"
    );
    assert!(
        !stdout.contains("0x5ec2e7"),
        "bytes outside the worktree: {stdout}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("source path must stay inside the worktree")
            && stderr.contains("symbolic link"),
        "{stderr}"
    );
}

fn plant_state(repo: &Path, plant: Plant) {
    let scratch = repo.join(".orbit-graph");
    let outside = repo.parent().expect("umbrella").join("outside");
    let prepared_state = |repo: &Path| {
        for command in [&["sync"][..], &["history", "sync", "--branch", "main"][..]] {
            let output = run_under_umask(repo, command);
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    };
    let replace_with_link = |path: &Path, target: &Path| {
        fs::remove_file(path).expect("remove state file");
        symlink(target, path).expect("plant symlink");
    };
    match plant {
        Plant::CommittedScratchLink => {
            symlink("../outside", &scratch).expect("plant scratch link");
            git(repo, &["add", ".orbit-graph"]);
            git(repo, &["commit", "-q", "-m", "commit a scratch link"]);
        }
        Plant::GraphDatabaseLink => {
            prepared_state(repo);
            replace_with_link(&graph_database(repo), &outside.join("victim.db"));
        }
        Plant::GraphLockLink => {
            prepared_state(repo);
            let lock = with_suffix(&graph_database(repo), ".lock");
            replace_with_link(&lock, &outside.join("victim.lock"));
        }
        Plant::HistoryLockLink => {
            prepared_state(repo);
            let lock = state_files(&scratch)
                .into_iter()
                .find(|path| {
                    let name = path.file_name().expect("name").to_string_lossy();
                    name.starts_with("change-history.") && name.ends_with(".lock")
                })
                .expect("history lock");
            replace_with_link(&lock, &outside.join("victim.lock"));
        }
        Plant::DanglingScratchLink => {
            symlink("../outside/created-by-orbit-graph", &scratch).expect("plant dangling link");
        }
        Plant::DanglingDatabaseLink => {
            prepared_state(repo);
            replace_with_link(
                &graph_database(repo),
                &outside.join("created-by-orbit-graph.db"),
            );
        }
        Plant::TrackedSnapshotEntry => {
            let entry = scratch
                .join("explorer/snapshots")
                .join("0123456789abcdef0123456789abcdef01234567");
            fs::create_dir_all(entry.join("tree")).expect("entry directory");
            fs::write(entry.join("entry.json"), "{}\n").expect("entry.json");
            git(repo, &["add", "-f", ".orbit-graph"]);
            git(
                repo,
                &["commit", "-q", "-m", "commit a snapshot cache entry"],
            );
        }
    }
}

/// Every path under `umbrella` outside `repo`, with its type, mode and
/// contents, so a created, written or chmodded file shows up as a difference.
fn outside_state(umbrella: &Path, repo: &Path) -> BTreeMap<PathBuf, (String, u32, Vec<u8>)> {
    let mut state = BTreeMap::new();
    let mut pending = vec![umbrella.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).expect("read directory") {
            let path = entry.expect("directory entry").path();
            if path == repo {
                continue;
            }
            let metadata = fs::symlink_metadata(&path).expect("metadata");
            let mode = metadata.permissions().mode() & 0o7777;
            let (kind, contents) = if metadata.file_type().is_symlink() {
                let target = fs::read_link(&path).expect("link target");
                ("link", target.into_os_string().into_encoded_bytes())
            } else if metadata.is_dir() {
                pending.push(path.clone());
                ("dir", Vec::new())
            } else {
                ("file", fs::read(&path).expect("file contents"))
            };
            state.insert(path, (kind.to_string(), mode, contents));
        }
    }
    state
}

fn committed_repository(umbrella: &Path) -> PathBuf {
    let repo = umbrella.join("repo");
    fs::create_dir(&repo).expect("repository directory");
    git(&repo, &["init", "-q", "-b", "main"]);
    fs::create_dir_all(repo.join("src")).expect("source directory");
    fs::write(
        repo.join("src/lib.rs"),
        "pub fn entry() -> i32 {\n    helper()\n}\n\npub fn helper() -> i32 {\n    1\n}\n",
    )
    .expect("source");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "initial"]);
    repo
}

fn graph_database(repo: &Path) -> PathBuf {
    state_files(&repo.join(".orbit-graph"))
        .into_iter()
        .find(|path| {
            let name = path.file_name().expect("name").to_string_lossy();
            name.ends_with(".db") && !name.starts_with("change-history.")
        })
        .expect("graph database")
}

/// Regular files and links directly inside `dir`, in name order.
fn state_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = fs::read_dir(dir)
        .expect("read state directory")
        .map(|entry| entry.expect("state entry").path())
        .filter(|path| !fs::symlink_metadata(path).expect("metadata").is_dir())
        .collect::<Vec<_>>();
    files.sort();
    files
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut spelled = path.as_os_str().to_os_string();
    spelled.push(suffix);
    PathBuf::from(spelled)
}

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777
}

/// Run the binary in JSON mode with `umask 0002` set on the child only.
fn run_under_umask(cwd: &Path, args: &[&str]) -> Output {
    Command::new("sh")
        .current_dir(cwd)
        .args(["-c", "umask 0002; exec \"$@\"", "sh"])
        .arg(env!("CARGO_BIN_EXE_orbit-graph"))
        .arg("--json")
        .args(args)
        .output()
        .expect("run orbit-graph under umask 0002")
}

fn git(cwd: &Path, args: &[&str]) {
    let output = common::git_command(cwd)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
