use std::fs;
use std::path::Path;

use super::super::{IndexDirOwner, create_index_dir, write_new_gitignore};

fn gitignore(dir: &Path) -> Option<String> {
    fs::read_to_string(dir.join(".gitignore")).ok()
}

fn scratch(worktree_root: &Path) -> IndexDirOwner<'_> {
    IndexDirOwner::Scratch { worktree_root }
}

fn entries(dir: &Path) -> Vec<String> {
    let mut names = fs::read_dir(dir)
        .expect("read directory")
        .map(|entry| {
            entry
                .expect("directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[test]
fn scratch_dir_gets_a_gitignore_that_ignores_everything() {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = root.path().join(".orbit-graph");

    create_index_dir(&dir, scratch(root.path()), "create test directory").expect("create");

    let contents = gitignore(&dir).expect("scratch .gitignore");
    assert!(contents.lines().any(|line| line == "*"), "{contents:?}");
    assert_eq!(gitignore(root.path()), None);
    assert_eq!(entries(&dir), [".gitignore"], "no temp file is left behind");
}

#[test]
fn existing_scratch_dir_gets_a_gitignore_and_an_existing_one_is_kept() {
    let bare = tempfile::tempdir().expect("tempdir");
    fs::create_dir(bare.path().join(".orbit-graph")).expect("existing scratch dir");
    let kept = tempfile::tempdir().expect("tempdir");
    fs::create_dir(kept.path().join(".orbit-graph")).expect("existing scratch dir");
    fs::write(kept.path().join(".orbit-graph/.gitignore"), "user rule\n").expect("user file");

    for root in [bare.path(), kept.path()] {
        create_index_dir(
            &root.join(".orbit-graph"),
            scratch(root),
            "create test directory",
        )
        .expect("reuse scratch dir");
    }

    assert!(gitignore(&bare.path().join(".orbit-graph")).is_some_and(|text| text.contains('*')));
    assert_eq!(
        gitignore(&kept.path().join(".orbit-graph")).as_deref(),
        Some("user rule\n")
    );
    assert_eq!(entries(&kept.path().join(".orbit-graph")), [".gitignore"]);
}

#[test]
fn caller_directory_and_its_created_parents_are_never_marked() {
    let root = tempfile::tempdir().expect("tempdir");
    let nested = root.path().join("indexes/graph");

    create_index_dir(&nested, IndexDirOwner::Caller, "create test directory").expect("create");
    create_index_dir(root.path(), IndexDirOwner::Caller, "create test directory")
        .expect("existing dir");

    assert!(nested.is_dir());
    assert_eq!(gitignore(&nested), None);
    assert_eq!(gitignore(&root.path().join("indexes")), None);
    assert_eq!(gitignore(root.path()), None);
}

#[test]
fn plugin_state_dir_is_marked_but_not_its_created_parents() {
    let state = tempfile::tempdir().expect("tempdir");
    let dir = state.path().join("plugins/orbit-graph/0123abcd");

    create_index_dir(&dir, IndexDirOwner::PluginState, "create test directory").expect("create");

    assert!(gitignore(&dir).is_some_and(|text| text.contains('*')));
    assert_eq!(gitignore(&state.path().join("plugins")), None);
}

#[test]
fn scratch_rule_applies_only_directly_under_the_worktree() {
    let root = tempfile::tempdir().expect("tempdir");
    let nested = root.path().join("sub/.orbit-graph");

    create_index_dir(&nested, scratch(root.path()), "create test directory").expect("create");

    assert_eq!(gitignore(&nested), None);
    assert_eq!(gitignore(&root.path().join("sub")), None);
}

#[cfg(unix)]
#[test]
fn symlinked_scratch_dir_is_never_written_through() {
    use std::os::unix::fs::symlink;

    // `root/.orbit-graph -> root/outside`.
    let root = tempfile::tempdir().expect("tempdir");
    let outside = root.path().join("outside");
    fs::create_dir(&outside).expect("outside dir");
    symlink(&outside, root.path().join(".orbit-graph")).expect("symlink to sibling");
    // `repo/.orbit-graph -> ..`: the file would land in the directory that
    // holds the repository.
    let umbrella = tempfile::tempdir().expect("tempdir");
    let repo = umbrella.path().join("repo");
    fs::create_dir(&repo).expect("repo");
    symlink("..", repo.join(".orbit-graph")).expect("symlink to parent");

    for worktree in [root.path(), repo.as_path()] {
        create_index_dir(
            &worktree.join(".orbit-graph"),
            scratch(worktree),
            "create test directory",
        )
        .expect("a symlinked scratch dir still opens");
    }

    assert!(!outside.join(".gitignore").exists());
    assert!(entries(&outside).is_empty());
    assert_eq!(gitignore(umbrella.path()), None);
    assert_eq!(gitignore(&repo), None);
}

#[cfg(unix)]
#[test]
fn gitignore_write_refuses_a_symlinked_directory_and_keeps_any_existing_entry() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().expect("tempdir");
    let target = root.path().join("target");
    fs::create_dir(&target).expect("target");
    let link = root.path().join("link");
    symlink(&target, &link).expect("dir symlink");

    let refused = write_new_gitignore(&link, b"*\n").expect_err("symlinked dir is refused");
    // Linux reports ENOTDIR for `O_DIRECTORY | O_NOFOLLOW` on a symlink;
    // other Unix systems report ELOOP.
    assert!(
        matches!(refused.raw_os_error(), Some(libc::ENOTDIR | libc::ELOOP)),
        "{refused}"
    );
    assert!(entries(&target).is_empty());

    // A dangling `.gitignore` symlink is an existing entry: it is neither
    // followed nor replaced.
    let dir = root.path().join("dir");
    fs::create_dir(&dir).expect("dir");
    symlink(root.path().join("victim"), dir.join(".gitignore")).expect("dangling symlink");
    assert!(!write_new_gitignore(&dir, b"*\n").expect("kept"));
    assert!(!root.path().join("victim").exists());
    assert_eq!(entries(&dir), [".gitignore"]);

    let fresh = root.path().join("fresh");
    fs::create_dir(&fresh).expect("fresh");
    assert!(write_new_gitignore(&fresh, b"*\n").expect("written"));
    assert_eq!(gitignore(&fresh).as_deref(), Some("*\n"));
    assert!(!write_new_gitignore(&fresh, b"other\n").expect("kept"));
    assert_eq!(gitignore(&fresh).as_deref(), Some("*\n"));
    assert_eq!(entries(&fresh), [".gitignore"]);
}

#[cfg(unix)]
#[test]
fn owned_dirs_and_their_marker_are_owner_only_but_caller_dirs_keep_the_default() {
    use std::os::unix::fs::PermissionsExt;
    let mode = |path: &Path| {
        fs::symlink_metadata(path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777
    };
    let root = tempfile::tempdir().expect("tempdir");
    let plugin = root.path().join("state/0123abcd");
    create_index_dir(&plugin, IndexDirOwner::PluginState, "create test directory").expect("create");
    assert_eq!(mode(&root.path().join("state")), 0o700);
    assert_eq!(mode(&plugin), 0o700);
    assert_eq!(mode(&plugin.join(".gitignore")), 0o600);

    let scratch_dir = root.path().join(".orbit-graph");
    create_index_dir(&scratch_dir, scratch(root.path()), "create test directory").expect("create");
    assert_eq!(mode(&scratch_dir), 0o700);

    let caller = root.path().join("caller");
    let umask_default = {
        let probe = root.path().join("probe");
        fs::create_dir(&probe).expect("probe");
        mode(&probe)
    };
    create_index_dir(&caller, IndexDirOwner::Caller, "create test directory").expect("create");
    assert_eq!(mode(&caller), umask_default);
}
