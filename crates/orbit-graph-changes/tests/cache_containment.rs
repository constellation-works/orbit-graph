//! The snapshot cache cannot be redirected or pre-populated by repository
//! content, and is created owner-only (STD-05 §R6–§R9).
//!
//! A refused cache is not fatal: the comparison indexes both sides into
//! task-owned temporary trees and names the refusal in its cache note.

#![cfg(unix)]
#![allow(clippy::expect_used)]
#![allow(
    clippy::disallowed_methods,
    reason = "fixtures are written with fs::write; clippy.toml bans it only from shipped code"
)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;

use git2::Repository;
use orbit_graph_changes::snapshot::{Comparison, SnapshotSide};
use tempfile::TempDir;

mod common;

use common::build_fixture;

/// Repository content that stands in for the snapshot cache.
#[derive(Debug, Clone, Copy)]
enum Plant {
    /// `.orbit-graph` itself, tracked by the Git index, is a symlink out.
    TrackedScratchLink,
    /// `.orbit-graph/explorer` is a symlink out.
    ExplorerLink,
    /// `.orbit-graph/explorer/snapshots` is a dangling symlink out.
    DanglingSnapshotsLink,
    /// A graph database in `.orbit-graph/` is a symlink out.
    DatabaseLink,
    /// A tracked `.orbit-graph/explorer/snapshots/<sha>/` cache entry.
    TrackedSnapshotEntry,
}

#[test]
fn a_planted_cache_is_refused_by_name_and_nothing_outside_the_repository_changes() {
    for plant in [
        Plant::TrackedScratchLink,
        Plant::ExplorerLink,
        Plant::DanglingSnapshotsLink,
        Plant::DatabaseLink,
        Plant::TrackedSnapshotEntry,
    ] {
        let fixture = build_fixture();
        let outside = TempDir::new().expect("outside directory");
        let victim = outside.path().join("victim.db");
        fs::write(&victim, "not orbit-graph state\n").expect("victim");
        fs::set_permissions(&victim, fs::Permissions::from_mode(0o644)).expect("chmod");
        plant_cache(fixture.path(), outside.path(), fixture.base.as_str(), plant);
        let before = tree_state(outside.path());

        let comparison =
            Comparison::open(fixture.path(), fixture.base.as_str(), fixture.head.as_str())
                .unwrap_or_else(|error| panic!("{plant:?}: a refused cache falls back: {error}"));

        let note = comparison.cache_note().unwrap_or_default();
        assert!(
            note.contains("refusing orbit-graph state path"),
            "{plant:?}: the note names the refusal: {note:?}"
        );
        assert_eq!(comparison.cache_dir(), None, "{plant:?}");
        for side in [SnapshotSide::Base, SnapshotSide::Head] {
            assert_eq!(
                comparison.snapshot(side).cache_outcome().label(),
                "disabled",
                "{plant:?} {side}"
            );
        }
        assert_eq!(
            tree_state(outside.path()),
            before,
            "{plant:?} created, wrote or chmodded something outside the repository"
        );
    }
}

/// The umask is set on a re-executed copy of this test only (STD-04 §R6):
/// the parent runs the child under `umask 0002`, and the child builds the
/// cache and checks every mode.
#[test]
fn the_cache_is_owner_only_under_a_permissive_umask() {
    const CHILD: &str = "ORBIT_GRAPH_CHANGES_UMASK_CHILD";
    const NAME: &str = "the_cache_is_owner_only_under_a_permissive_umask";
    if std::env::var_os(CHILD).is_none() {
        let output = Command::new("sh")
            .args(["-c", "umask 0002; exec \"$@\"", "sh"])
            .arg(std::env::current_exe().expect("test binary"))
            .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
            .env(CHILD, "1")
            .output()
            .expect("re-run this test under umask 0002");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "{stdout}{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let fixture = build_fixture();
    let comparison = Comparison::open(fixture.path(), fixture.base.as_str(), fixture.head.as_str())
        .expect("open comparison");
    assert_eq!(comparison.cache_note(), None);
    drop(comparison);

    let scratch = fixture.path().join(".orbit-graph");
    let state = tree_state(&scratch);
    assert!(
        state.keys().any(|path| path.ends_with("entry.json"))
            && state.keys().any(|path| path.ends_with("last_used"))
            && state.keys().any(|path| path.ends_with("in_use.lock"))
            && state.keys().any(|path| path.ends_with("src/lib.rs"))
            && state
                .keys()
                .any(|path| path.extension().is_some_and(|ext| ext == "db")),
        "{:?}",
        state.keys().collect::<Vec<_>>()
    );
    for (path, (kind, mode, _)) in &state {
        // The snapshot tree's `.git` is libgit2's discovery anchor, not
        // materialized state; it sits inside a `0700` tree.
        if path.components().any(|part| part.as_os_str() == ".git") {
            continue;
        }
        let expected = if kind == "dir" { 0o700 } else { 0o600 };
        assert_eq!(mode & 0o777, expected, "{}", path.display());
    }
    assert_eq!(
        fs::metadata(&scratch)
            .expect("scratch")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

fn plant_cache(repo: &Path, outside: &Path, base: &str, plant: Plant) {
    let scratch = repo.join(".orbit-graph");
    match plant {
        Plant::TrackedScratchLink => {
            symlink(outside, &scratch).expect("scratch link");
            track(repo, &[".orbit-graph"]);
        }
        Plant::ExplorerLink => {
            fs::create_dir(&scratch).expect("scratch");
            symlink(outside, scratch.join("explorer")).expect("explorer link");
        }
        Plant::DanglingSnapshotsLink => {
            fs::create_dir_all(scratch.join("explorer")).expect("explorer");
            symlink(
                outside.join("created-by-orbit-graph"),
                scratch.join("explorer/snapshots"),
            )
            .expect("dangling snapshots link");
        }
        Plant::DatabaseLink => {
            fs::create_dir(&scratch).expect("scratch");
            symlink(outside.join("victim.db"), scratch.join("main.20.db")).expect("db link");
        }
        Plant::TrackedSnapshotEntry => {
            let entry = scratch.join("explorer/snapshots").join(base);
            fs::create_dir_all(entry.join("tree")).expect("entry");
            fs::write(entry.join("entry.json"), "{}\n").expect("entry.json");
            let relative = format!(".orbit-graph/explorer/snapshots/{base}/entry.json");
            track(repo, &[relative.as_str()]);
        }
    }
}

/// Stage `paths` in the fixture's Git index without committing.
fn track(repo: &Path, paths: &[&str]) {
    let repository = Repository::open(repo).expect("open fixture");
    let mut index = repository.index().expect("index");
    for path in paths {
        index.add_path(Path::new(path)).expect("stage path");
    }
    index.write().expect("write index");
}

/// Every path under `root` with its type, mode and contents.
fn tree_state(root: &Path) -> BTreeMap<PathBuf, (String, u32, Vec<u8>)> {
    let mut state = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).expect("read directory") {
            let path = entry.expect("directory entry").path();
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
