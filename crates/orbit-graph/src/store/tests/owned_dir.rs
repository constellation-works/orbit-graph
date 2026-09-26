use std::fs;

use super::super::create_owned_dir;

fn gitignore(dir: &std::path::Path) -> Option<String> {
    fs::read_to_string(dir.join(".gitignore")).ok()
}

#[test]
fn scratch_dir_gets_a_gitignore_that_ignores_everything() {
    let root = tempfile::tempdir().expect("tempdir");
    let scratch = root.path().join(".orbit-graph");

    create_owned_dir(&scratch, "create test directory").expect("create scratch dir");

    let contents = gitignore(&scratch).expect("scratch .gitignore");
    assert!(contents.lines().any(|line| line == "*"), "{contents:?}");
    assert_eq!(gitignore(root.path()), None);
}

#[test]
fn existing_scratch_dir_gets_a_gitignore_and_an_existing_one_is_kept() {
    let root = tempfile::tempdir().expect("tempdir");
    let bare = root.path().join("bare/.orbit-graph");
    fs::create_dir_all(&bare).expect("existing scratch dir");
    let kept = root.path().join("kept/.orbit-graph");
    fs::create_dir_all(&kept).expect("existing scratch dir");
    fs::write(kept.join(".gitignore"), "user rule\n").expect("user .gitignore");

    create_owned_dir(&bare, "create test directory").expect("reuse scratch dir");
    create_owned_dir(&kept.join("explorer/snapshots"), "create test directory")
        .expect("create below scratch dir");

    assert!(gitignore(&bare).is_some_and(|contents| contents.contains('*')));
    assert_eq!(gitignore(&kept).as_deref(), Some("user rule\n"));
    // The nested directories were created inside an ignored directory; only
    // the topmost one created and the scratch directory get a file.
    assert!(gitignore(&kept.join("explorer")).is_some());
    assert_eq!(gitignore(&kept.join("explorer/snapshots")), None);
}

#[test]
fn caller_supplied_existing_directory_is_left_alone() {
    let root = tempfile::tempdir().expect("tempdir");

    create_owned_dir(root.path(), "create test directory").expect("existing dir");

    assert_eq!(gitignore(root.path()), None);
}

#[test]
fn created_custom_directory_is_marked_at_its_topmost_new_level() {
    let root = tempfile::tempdir().expect("tempdir");
    let nested = root.path().join("indexes/graph");

    create_owned_dir(&nested, "create test directory").expect("create nested dir");

    assert!(gitignore(&root.path().join("indexes")).is_some());
    assert_eq!(gitignore(&nested), None);
    assert_eq!(gitignore(root.path()), None);
}
