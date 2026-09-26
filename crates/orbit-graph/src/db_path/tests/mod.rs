use std::path::Path;

use crate::{resolve_db_path, resolve_db_path_for_commit};

#[test]
fn db_path_sanitizes_branch_slashes_and_preserves_raw_branch() {
    let worktree_root = Path::new("/tmp/orbit-worktree");

    let feat = resolve_db_path(worktree_root, "feat/foo", 1);
    assert_eq!(
        feat.path(),
        Path::new("/tmp/orbit-worktree/.orbit-graph/feat_foo.1.db")
    );
    assert_eq!(
        feat.path().file_name().and_then(|name| name.to_str()),
        Some("feat_foo.1.db")
    );
    assert_eq!(feat.branch(), "feat/foo");
    assert_eq!(feat.extractor_version(), 1);

    let main = resolve_db_path(worktree_root, "main", 42);
    assert_eq!(
        main.path(),
        Path::new("/tmp/orbit-worktree/.orbit-graph/main.42.db")
    );
    assert_eq!(
        main.path().file_name().and_then(|name| name.to_str()),
        Some("main.42.db")
    );
    assert_eq!(main.branch(), "main");
    assert_eq!(main.extractor_version(), 42);
}

#[test]
fn db_path_sanitizes_filesystem_hostile_branch_names() {
    let worktree_root = Path::new("/tmp/orbit-worktree");

    for (branch, expected_file_name) in [
        ("feat:foo", "feat_foo.1.db"),
        ("feat\\foo", "feat_foo.1.db"),
        ("feat..foo", "feat__foo.1.db"),
        (".hidden", "_hidden.1.db"),
        ("", "_.1.db"),
        ("feat\0foo", "feat_foo.1.db"),
    ] {
        let path = resolve_db_path(worktree_root, branch, 1);
        assert_eq!(
            path.path().file_name().and_then(|name| name.to_str()),
            Some(expected_file_name),
            "sanitized filename for branch {branch:?}"
        );
        assert_eq!(path.branch(), branch);
    }
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
