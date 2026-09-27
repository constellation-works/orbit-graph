use std::path::Path;

use crate::{resolve_db_path, resolve_db_path_for_commit};

#[test]
fn db_path_hashes_branches_and_preserves_raw_branch() {
    let worktree_root = Path::new("/tmp/orbit-worktree");

    let feat = resolve_db_path(worktree_root, "feat/foo", 1);
    let feat_name = format!(
        "branch~feat_foo-{}.1.db",
        blake3::hash(b"feat/foo").to_hex()
    );
    assert_eq!(
        feat.path().file_name().and_then(|name| name.to_str()),
        Some(feat_name.as_str())
    );
    assert_ne!(
        feat.path(),
        resolve_db_path(worktree_root, "feat_foo", 1).path()
    );
    assert_eq!(feat.branch(), "feat/foo");
    assert_eq!(feat.extractor_version(), 1);

    let main = resolve_db_path(worktree_root, "main", 42);
    let main_name = format!("branch~main-{}.42.db", blake3::hash(b"main").to_hex());
    assert_eq!(
        main.path().file_name().and_then(|name| name.to_str()),
        Some(main_name.as_str())
    );
    assert_eq!(main.branch(), "main");
    assert_eq!(main.extractor_version(), 42);
}

#[test]
fn db_path_sanitizes_filesystem_hostile_branch_names() {
    let worktree_root = Path::new("/tmp/orbit-worktree");

    for (branch, expected_prefix) in [
        ("feat:foo", "branch~feat_foo-"),
        ("feat\\foo", "branch~feat_foo-"),
        ("feat..foo", "branch~feat__foo-"),
        (".hidden", "branch~_hidden-"),
        ("", "branch~_-"),
        ("feat\0foo", "branch~feat_foo-"),
    ] {
        let path = resolve_db_path(worktree_root, branch, 1);
        let name = path
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap();
        assert!(name.starts_with(expected_prefix), "{name} for {branch:?}");
        assert!(name.ends_with(".1.db"));
        assert_eq!(path.branch(), branch);
    }
}

#[test]
fn named_detached_like_branch_cannot_use_detached_commit_family() {
    let root = Path::new("/tmp/orbit-worktree");
    let branch = resolve_db_path(root, "detached-1234567890ab", 7);
    let detached = resolve_db_path_for_commit(root, "HEAD", "1234567890abcdef", 7);
    assert_ne!(branch.path(), detached.path());
}

#[test]
fn db_path_uses_detached_commit_filename_for_head() {
    let worktree_root = Path::new("/tmp/orbit-worktree");
    let commit_sha = "1234567890abcdef1234567890abcdef12345678";

    let detached = resolve_db_path_for_commit(worktree_root, "HEAD", commit_sha, 7);

    assert_eq!(
        detached.path(),
        Path::new("/tmp/orbit-worktree/.orbit-graph/detached-1234567890ab.7.db")
    );
    assert_eq!(detached.branch(), "HEAD");
    assert_eq!(detached.extractor_version(), 7);
}
