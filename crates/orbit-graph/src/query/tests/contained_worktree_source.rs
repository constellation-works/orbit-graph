use std::path::Path;

use crate::query::contained_worktree_source;

#[test]
fn relative_database_paths_stay_under_the_worktree() {
    let resolved =
        contained_worktree_source(Path::new("/repo"), "src/lib.rs").expect("relative path");
    assert_eq!(resolved, Path::new("/repo/src/lib.rs"));

    let dotted = contained_worktree_source(Path::new("/repo"), "src/./lib.rs").expect("dot path");
    assert_eq!(dotted, Path::new("/repo/src/./lib.rs"));
}

#[test]
fn absolute_and_parent_database_paths_are_rejected() {
    let root = Path::new("/repo");
    for stored in [
        "/etc/passwd",
        "../outside.rs",
        "src/../../outside.rs",
        "src/../src/lib.rs",
        "./../../outside.rs",
    ] {
        let error = contained_worktree_source(root, stored).expect_err(stored);
        let rendered = error.to_string();
        assert!(rendered.contains("worktree"), "{rendered}");
        assert!(rendered.contains(stored), "{rendered}");
    }
}

#[cfg(unix)]
#[test]
fn a_symlinked_component_leaving_the_worktree_is_rejected() {
    let root = tempfile::tempdir().expect("tempdir");
    let worktree = root.path().join("repo");
    let outside = root.path().join("outside");
    std::fs::create_dir_all(worktree.join("src")).expect("worktree");
    std::fs::create_dir(&outside).expect("outside");
    std::fs::write(outside.join("secret.rs"), b"fn secret() {}\n").expect("secret");
    std::fs::write(worktree.join("src/lib.rs"), b"fn lib() {}\n").expect("lib");
    std::os::unix::fs::symlink(&outside, worktree.join("src/dir")).expect("link out");
    std::os::unix::fs::symlink(worktree.join("src"), worktree.join("alias")).expect("link in");

    let error = contained_worktree_source(&worktree, "src/dir/secret.rs")
        .expect_err("a link out of the worktree is rejected");
    assert!(error.to_string().contains("symbolic link"), "{error}");
    assert_eq!(
        contained_worktree_source(&worktree, "alias/lib.rs").expect("a link inside"),
        worktree.join("alias/lib.rs")
    );
    assert_eq!(
        contained_worktree_source(&worktree, "src/missing.rs").expect("missing"),
        worktree.join("src/missing.rs")
    );
}
