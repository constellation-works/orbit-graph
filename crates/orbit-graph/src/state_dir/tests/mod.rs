use std::fs;
use std::path::Path;

use crate::GraphError;
use crate::tests::support::{git_command, init_fixture_repository};

use super::{
    StateAccess, atomic_write, atomic_write_temp_pid, check_state_file, chosen_state_dir,
    open_state_file, private_state_dir, scratch_state_dir,
};

fn refusal(result: Result<impl std::fmt::Debug, GraphError>) -> (std::path::PathBuf, String) {
    match result {
        Err(GraphError::UnsafeStatePath { path, reason }) => (path, reason),
        other => panic!("expected an unsafe state path refusal, got {other:?}"),
    }
}

fn git(root: &Path, args: &[&str]) {
    let output = git_command(root).args(args).output().expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::symlink_metadata(path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777
}

#[cfg(unix)]
#[test]
fn scratch_dir_is_created_owner_only_and_repaired_by_a_writer() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().expect("tempdir");

    assert_eq!(
        scratch_state_dir(root.path(), Path::new("explorer"), StateAccess::Read).expect("read"),
        None,
        "a reader creates nothing"
    );
    assert!(!root.path().join(".orbit-graph").exists());

    let dir = scratch_state_dir(
        root.path(),
        Path::new("explorer/snapshots"),
        StateAccess::Write,
    )
    .expect("create")
    .expect("created");
    let canonical = root.path().canonicalize().expect("canonical root");
    assert_eq!(dir, canonical.join(".orbit-graph/explorer/snapshots"));
    for created in [
        ".orbit-graph",
        ".orbit-graph/explorer",
        ".orbit-graph/explorer/snapshots",
    ] {
        assert_eq!(mode(&root.path().join(created)), 0o700, "{created}");
    }

    let scratch = root.path().join(".orbit-graph");
    fs::set_permissions(&scratch, fs::Permissions::from_mode(0o775)).expect("loosen");
    scratch_state_dir(root.path(), Path::new(""), StateAccess::Read).expect("read");
    assert_eq!(mode(&scratch), 0o775, "a reader repairs nothing");
    scratch_state_dir(root.path(), Path::new(""), StateAccess::Write).expect("write");
    assert_eq!(mode(&scratch), 0o700, "a writer repairs the directory");
}

#[cfg(unix)]
#[test]
fn symlinked_or_dangling_components_are_refused_and_nothing_is_created_through_them() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().expect("tempdir");
    let repo = root.path().join("repo");
    let outside = root.path().join("outside");
    fs::create_dir(&repo).expect("repo");
    fs::create_dir(&outside).expect("outside");

    symlink(&outside, repo.join(".orbit-graph")).expect("link scratch dir");
    for access in [StateAccess::Read, StateAccess::Write] {
        let (path, reason) = refusal(scratch_state_dir(&repo, Path::new("explorer"), access));
        assert!(path.ends_with(".orbit-graph"), "{path:?}");
        assert!(reason.contains("symbolic link"), "{reason}");
    }
    assert_eq!(fs::read_dir(&outside).expect("outside").count(), 0);

    fs::remove_file(repo.join(".orbit-graph")).expect("unlink");
    symlink(root.path().join("missing"), repo.join(".orbit-graph")).expect("dangling");
    let (_, reason) = refusal(scratch_state_dir(&repo, Path::new(""), StateAccess::Write));
    assert!(reason.contains("symbolic link"), "{reason}");
    assert!(!root.path().join("missing").exists());

    fs::remove_file(repo.join(".orbit-graph")).expect("unlink");
    fs::create_dir(repo.join(".orbit-graph")).expect("real scratch dir");
    symlink(&outside, repo.join(".orbit-graph/explorer")).expect("link nested");
    let (path, _) = refusal(scratch_state_dir(
        &repo,
        Path::new("explorer/snapshots"),
        StateAccess::Write,
    ));
    assert!(path.ends_with("explorer"), "{path:?}");
    assert_eq!(fs::read_dir(&outside).expect("outside").count(), 0);

    fs::write(repo.join("plain"), b"not a directory").expect("file");
    let (_, reason) = refusal(private_state_dir(
        &repo,
        Path::new("plain"),
        StateAccess::Write,
    ));
    assert!(reason.contains("not a directory"), "{reason}");
}

#[cfg(unix)]
#[test]
fn a_scratch_dir_holding_any_symlink_is_refused_by_every_reader_and_writer() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().expect("tempdir");
    let scratch = root.path().join(".orbit-graph");
    fs::create_dir(&scratch).expect("scratch dir");
    fs::write(scratch.join("main.20.db"), b"").expect("database");
    scratch_state_dir(root.path(), Path::new(""), StateAccess::Read).expect("no link yet");

    symlink(
        root.path().join("elsewhere"),
        scratch.join("main.20.db.lock"),
    )
    .expect("link");
    for (sub, access) in [
        ("", StateAccess::Read),
        ("", StateAccess::Write),
        ("explorer/snapshots", StateAccess::Write),
    ] {
        let (path, reason) = refusal(scratch_state_dir(root.path(), Path::new(sub), access));
        assert_eq!(
            path,
            scratch
                .canonicalize()
                .expect("canonical")
                .join("main.20.db.lock")
        );
        assert!(reason.contains("never creates one"), "{reason}");
    }
    assert!(
        !scratch.join("explorer").exists(),
        "nothing is created below"
    );
}

#[test]
fn a_scratch_dir_with_tracked_entries_is_refused() {
    let root = tempfile::tempdir().expect("tempdir");
    init_fixture_repository(root.path(), "main");
    fs::create_dir_all(root.path().join(".orbit-graph/explorer/snapshots/abc")).expect("dirs");
    fs::write(
        root.path()
            .join(".orbit-graph/explorer/snapshots/abc/entry.json"),
        b"{}",
    )
    .expect("entry");
    scratch_state_dir(root.path(), Path::new(""), StateAccess::Read)
        .expect("untracked content is not refused");

    git(root.path(), &["add", "-f", ".orbit-graph"]);
    for access in [StateAccess::Read, StateAccess::Write] {
        let (path, reason) = refusal(scratch_state_dir(root.path(), Path::new(""), access));
        assert!(path.ends_with(".orbit-graph"), "{path:?}");
        assert!(
            reason.contains("Git index tracks `.orbit-graph`"),
            "{reason}"
        );
    }
    let (_, reason) = refusal(chosen_state_dir(
        &root.path().join(".orbit-graph/explorer/snapshots"),
        Some(root.path()),
        StateAccess::Write,
    ));
    assert!(
        reason.contains("`.orbit-graph/explorer/snapshots`"),
        "{reason}"
    );
}

#[cfg(unix)]
#[test]
fn a_chosen_dir_is_created_owner_only_from_its_nearest_existing_ancestor() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().expect("tempdir");
    let real = root.path().join("real");
    fs::create_dir(&real).expect("real");
    // The caller's own spelling may pass through a symlink it chose.
    symlink(&real, root.path().join("alias")).expect("alias");

    let dir = chosen_state_dir(
        &root.path().join("alias/cache/snapshots"),
        None,
        StateAccess::Write,
    )
    .expect("create")
    .expect("created");
    assert_eq!(
        dir,
        real.canonicalize()
            .expect("canonical")
            .join("cache/snapshots")
    );
    assert_eq!(mode(&real.join("cache")), 0o700);
    assert_eq!(mode(&real.join("cache/snapshots")), 0o700);

    symlink(root.path().join("nowhere"), root.path().join("dangling")).expect("dangling");
    refusal(chosen_state_dir(
        &root.path().join("dangling/cache"),
        None,
        StateAccess::Write,
    ));
    assert!(!root.path().join("nowhere").exists());
}

#[cfg(unix)]
#[test]
fn state_files_refuse_symlinks_and_a_writer_repairs_loose_modes() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let root = tempfile::tempdir().expect("tempdir");
    let victim = root.path().join("victim.sh");
    fs::write(&victim, b"#!/bin/sh\n").expect("victim");
    fs::set_permissions(&victim, fs::Permissions::from_mode(0o755)).expect("chmod");

    let link = root.path().join("state.lock");
    symlink(&victim, &link).expect("link");
    for access in [StateAccess::Read, StateAccess::Write] {
        let (path, reason) = refusal(open_state_file(&link, access));
        assert_eq!(path, link);
        assert!(reason.contains("symbolic link"), "{reason}");
    }
    refusal(check_state_file(&link));
    assert_eq!(mode(&victim), 0o755, "the link target is never chmodded");

    let dangling = root.path().join("dangling.db");
    symlink(root.path().join("created-outside.db"), &dangling).expect("dangling");
    refusal(open_state_file(&dangling, StateAccess::Write));
    refusal(check_state_file(&dangling));
    assert!(!root.path().join("created-outside.db").exists());

    let missing = root.path().join("missing.db");
    assert!(
        open_state_file(&missing, StateAccess::Read)
            .expect("read")
            .is_none()
    );
    assert!(!check_state_file(&missing).expect("check"));
    assert!(!missing.exists());
    drop(open_state_file(&missing, StateAccess::Write).expect("create"));
    assert_eq!(mode(&missing), 0o600);

    let loose = root.path().join("loose.db");
    fs::write(&loose, b"").expect("loose");
    fs::set_permissions(&loose, fs::Permissions::from_mode(0o644)).expect("chmod");
    drop(open_state_file(&loose, StateAccess::Read).expect("read"));
    assert_eq!(mode(&loose), 0o644, "a reader repairs nothing");
    drop(open_state_file(&loose, StateAccess::Write).expect("write"));
    assert_eq!(mode(&loose), 0o600, "a writer repairs the file");

    fs::create_dir(root.path().join("dir.db")).expect("dir");
    let (_, reason) = refusal(check_state_file(&root.path().join("dir.db")));
    assert!(reason.contains("not a regular file"), "{reason}");
}

#[cfg(unix)]
#[test]
fn atomic_write_is_owner_only_and_replaces_a_symlink_without_following_it() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().expect("tempdir");
    let target = root.path().join("entry.json");
    atomic_write(&target, b"first").expect("write");
    assert_eq!(fs::read(&target).expect("read"), b"first");
    assert_eq!(mode(&target), 0o600);
    atomic_write(&target, b"second").expect("replace");
    assert_eq!(fs::read(&target).expect("read"), b"second");

    let victim = root.path().join("victim");
    fs::write(&victim, b"keep").expect("victim");
    let link = root.path().join("last_used");
    symlink(&victim, &link).expect("link");
    atomic_write(&link, b"42").expect("replace link");
    assert_eq!(fs::read(&victim).expect("victim"), b"keep");
    assert!(
        !fs::symlink_metadata(&link)
            .expect("metadata")
            .file_type()
            .is_symlink()
    );

    let mut names = fs::read_dir(root.path())
        .expect("list")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .into_string()
                .expect("utf-8")
        })
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, ["entry.json", "last_used", "victim"], "no temp file");
}

#[test]
fn atomic_write_temp_names_parse_back_to_their_writer() {
    assert_eq!(atomic_write_temp_pid("a.json.tmp-12-0", "a.json"), Some(12));
    for other in [
        "a.json",
        "a.json.tmp-12",
        "a.json.tmp--0",
        "a.json.tmp-12-",
        "a.json.tmp-x-0",
        "b.json.tmp-12-0",
    ] {
        assert_eq!(atomic_write_temp_pid(other, "a.json"), None, "{other}");
    }
}
