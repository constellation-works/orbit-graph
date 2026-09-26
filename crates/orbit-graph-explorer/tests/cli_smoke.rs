//! Smoke tests that exercise the packaged `orbit-graph-explorer` executable.

#![allow(clippy::expect_used)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use orbit_graph_explorer::cache::{CACHE_MARKER_FILE, SnapshotCache};
use orbit_graph_explorer::snapshot::{Comparison, ComparisonOptions};
use tempfile::TempDir;

mod common;

use common::{build_fixture, fingerprint_working_tree};

#[test]
fn help_succeeds_and_describes_the_snapshot_command() {
    let output = run(Path::new("."), &["--help"]);
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("snapshot --base"), "{stdout}");
    assert!(stdout.contains("not a stable machine contract"), "{stdout}");
}

#[test]
fn missing_arguments_fail_with_empty_stdout() {
    let output = run(Path::new("."), &["snapshot", "--base", "HEAD"]);
    assert!(!output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("`--head` is required"), "{stderr}");
}

#[test]
fn snapshot_diagnostic_reports_both_revisions_and_leaves_the_tree_alone() {
    let fixture = build_fixture();
    let before = fingerprint_working_tree(fixture.path());

    let output = run(
        fixture.path(),
        &[
            "snapshot",
            "--base",
            fixture.base.as_str(),
            "--head",
            fixture.head.as_str(),
            "--selector",
            "symbol:src/lib.rs#removed_helper:function",
        ],
    );
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("mode\tdirect_base_head"), "{stdout}");
    assert!(stdout.contains("working_tree\tclean"), "{stdout}");
    assert!(
        stdout.contains(format!("base\tref\t{}\tcommit\t{}", fixture.base, fixture.base).as_str()),
        "{stdout}"
    );
    assert!(
        stdout.contains(format!("head\tref\t{}\tcommit\t{}", fixture.head, fixture.head).as_str()),
        "{stdout}"
    );
    assert!(
        stdout
            .contains("base\tselector\tsymbol:src/lib.rs#removed_helper:function\tresolved\ttrue"),
        "{stdout}"
    );
    assert!(
        stdout
            .contains("head\tselector\tsymbol:src/lib.rs#removed_helper:function\tresolved\tfalse"),
        "{stdout}"
    );

    assert_eq!(
        fingerprint_working_tree(fixture.path()),
        before,
        "the explorer binary must not touch the user working tree"
    );
}

#[test]
fn dirty_working_tree_is_reported_by_the_binary() {
    let fixture = build_fixture();
    std::fs::write(fixture.path().join("scratch.rs"), "pub fn scratch() {}\n")
        .expect("add untracked file");

    let output = run(
        fixture.path(),
        &[
            "snapshot",
            "--base",
            fixture.base.as_str(),
            "--head",
            fixture.head.as_str(),
        ],
    );
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("working_tree\tdirty"), "{stdout}");
    assert!(stdout.contains("uncommitted change(s)"), "{stdout}");
}

/// A cache directory under the canonical temp root. `clean` reports canonical
/// paths, and on macOS the default temp root sits under the `/var` ->
/// `/private/var` symlink, so comparisons need the canonical form.
fn canonical_tempdir() -> TempDir {
    let root = std::env::temp_dir()
        .canonicalize()
        .expect("canonical temp root");
    TempDir::new_in(root).expect("cache directory")
}

#[test]
fn clean_reports_by_default_and_removes_exactly_the_report_with_confirm() {
    let fixture = build_fixture();
    let cache = canonical_tempdir();
    let cache_dir = cache.path().to_str().expect("utf8 cache path").to_string();
    // A real comparison publishes both entries and marks the directory.
    drop(open_comparison(
        &fixture,
        cache.path(),
        fixture.base.as_str(),
    ));
    let gone = "f".repeat(40);
    write_entry(cache.path(), gone.as_str(), EntryShape::Current);
    let abandoned = cache.path().join(format!(".staging-{gone}-1-2-3"));
    fs::create_dir_all(abandoned.join("tree")).expect("abandoned staging directory");

    let repo = fixture.path().to_str().expect("utf8 repo path");
    let args = ["clean", "--repo", repo, "--cache-dir", cache_dir.as_str()];
    let before = digest_tree(cache.path());
    let repo_before = digest_tree(fixture.path());
    let plan = run(Path::new("."), &args);
    assert!(plan.status.success(), "{plan:?}");
    assert_eq!(digest_tree(cache.path()), before, "a plan changes nothing");
    assert_eq!(digest_tree(fixture.path()), repo_before);
    let plan_out = String::from_utf8_lossy(&plan.stdout).into_owned();
    assert!(plan_out.contains("applied\tfalse\n"), "{plan_out}");
    let planned = listed(plan_out.as_str(), "would_delete");
    assert_eq!(
        planned,
        vec![
            ("abandoned_staging".to_string(), abandoned.clone()),
            (
                "unreferenced_commit".to_string(),
                cache.path().join(gone.as_str())
            ),
        ],
        "{plan_out}"
    );
    let stderr = String::from_utf8_lossy(&plan.stderr);
    assert!(
        stderr.contains("dry run") && stderr.contains("--confirm"),
        "{stderr}"
    );

    let mut confirm = args.to_vec();
    confirm.push("--confirm");
    let applied = run(Path::new("."), &confirm);
    assert!(applied.status.success(), "{applied:?}");
    let applied_out = String::from_utf8_lossy(&applied.stdout).into_owned();
    assert!(applied_out.contains("applied\ttrue\n"), "{applied_out}");
    assert_eq!(listed(applied_out.as_str(), "removed"), planned);
    let after = digest_tree(cache.path());
    let vanished: Vec<&PathBuf> = before
        .keys()
        .filter(|path| !after.contains_key(*path))
        .collect();
    assert!(
        vanished
            .iter()
            .all(|path| planned.iter().any(|(_, root)| path.starts_with(root))),
        "only the listed entries are removed: {vanished:?}"
    );
    for (_, root) in &planned {
        assert!(!root.exists(), "{} was listed and removed", root.display());
    }
    assert_eq!(
        before
            .iter()
            .filter(|(path, _)| !planned.iter().any(|(_, root)| path.starts_with(root)))
            .collect::<BTreeMap<_, _>>(),
        after.iter().collect::<BTreeMap<_, _>>(),
        "everything not listed is byte-for-byte unchanged"
    );
    assert_eq!(digest_tree(fixture.path()), repo_before);
}

#[test]
fn clean_confirm_keeps_what_it_cannot_prove_is_its_own_stale_and_unused() {
    let fixture = build_fixture();
    let cache = canonical_tempdir();
    let cache_dir = cache.path().to_str().expect("utf8 cache path").to_string();
    // A running comparison of head against itself holds the head entry open.
    let comparison = open_comparison(&fixture, cache.path(), fixture.head.as_str());
    let in_use = cache.path().join(fixture.head.as_str());
    fs::write(in_use.join("last_used"), b"100").expect("age the in-use entry");
    // A build in progress holds its staging directory's builder lock.
    let building = SnapshotCache::open(cache.path())
        .expect("open cache")
        .stage("e".repeat(40).as_str())
        .expect("stage a build");
    let unreadable = write_entry(
        cache.path(),
        "1".repeat(40).as_str(),
        EntryShape::Unreadable,
    );
    let newer = write_entry(cache.path(), "2".repeat(40).as_str(), EntryShape::Newer);
    // A live commit whose entry records no age at all.
    let unknown_age = write_entry(cache.path(), fixture.base.as_str(), EntryShape::NoAge);

    let repo = fixture.path().to_str().expect("utf8 repo path");
    let before = digest_tree(cache.path());
    let output = run(
        Path::new("."),
        &[
            "clean",
            "--repo",
            repo,
            "--cache-dir",
            cache_dir.as_str(),
            "--older-than",
            "1d",
            "--confirm",
        ],
    );
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(listed(stdout.as_str(), "removed").is_empty(), "{stdout}");
    let kept = listed(stdout.as_str(), "kept");
    let staging_root = fs::read_dir(cache.path())
        .expect("read cache")
        .map(|entry| entry.expect("entry").path())
        .find(|path| path.to_string_lossy().contains(".staging-"))
        .expect("the staging directory survives");
    for (reason, path) in [
        ("building", staging_root),
        ("in_use", in_use),
        ("unreadable", unreadable),
        ("newer_identity", newer),
        ("unknown_age", unknown_age),
    ] {
        assert!(
            kept.contains(&(reason.to_string(), path.clone())),
            "{reason} {}: {stdout}",
            path.display()
        );
        assert!(path.exists(), "{} is kept", path.display());
    }
    let after = digest_tree(cache.path());
    assert!(
        before.keys().all(|path| after.contains_key(path)),
        "nothing was removed: {stdout}"
    );
    drop(building);
    drop(comparison);

    // A directory without the cache-root marker is never judged, even when
    // it holds a 40-hex directory that looks exactly like a stale entry.
    let unmarked = TempDir::new().expect("unmarked directory");
    let lookalike = write_entry(
        unmarked.path(),
        "f".repeat(40).as_str(),
        EntryShape::Current,
    );
    fs::remove_file(unmarked.path().join(CACHE_MARKER_FILE)).expect("remove marker");
    let unmarked_before = digest_tree(unmarked.path());
    let output = run(
        Path::new("."),
        &[
            "clean",
            "--repo",
            repo,
            "--cache-dir",
            unmarked.path().to_str().expect("utf8 path"),
            "--confirm",
        ],
    );
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let unmarked_root = unmarked.path().canonicalize().expect("canonical path");
    assert_eq!(
        listed(stdout.as_str(), "kept"),
        vec![("no_cache_marker".to_string(), unmarked_root)],
        "{stdout}"
    );
    assert!(lookalike.exists());
    assert_eq!(digest_tree(unmarked.path()), unmarked_before);
}

#[test]
fn clean_without_a_cache_directory_creates_nothing() {
    let fixture = build_fixture();
    let parent = TempDir::new().expect("parent directory");
    let missing = parent.path().join("absent");
    let output = run(
        Path::new("."),
        &[
            "clean",
            "--repo",
            fixture.path().to_str().expect("utf8 path"),
            "--cache-dir",
            missing.to_str().expect("utf8 path"),
        ],
    );
    assert!(output.status.success(), "{output:?}");
    assert!(
        !missing.exists(),
        "a plan never creates the cache directory"
    );
    assert!(!fixture.path().join(".orbit-graph").exists());
}

#[test]
fn confirm_applies_only_to_report_and_clean() {
    let output = run(
        Path::new("."),
        &["snapshot", "--base", "HEAD", "--head", "HEAD", "--confirm"],
    );
    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("`--confirm` applies to `report` and `clean` only"),
        "{stderr}"
    );
}

fn open_comparison(fixture: &common::Fixture, cache: &Path, base: &str) -> Comparison {
    Comparison::open_with_options(
        fixture.path(),
        base,
        fixture.head.as_str(),
        &ComparisonOptions {
            cache_dir: Some(cache.to_path_buf()),
            no_cache: false,
        },
    )
    .expect("open comparison")
}

enum EntryShape {
    /// The running binary's key, a recorded publish time and last use.
    Current,
    /// An `entry.json` that does not parse.
    Unreadable,
    /// A newer binary's extractor version.
    Newer,
    /// The running binary's key with neither `published_at` nor `last_used`.
    NoAge,
}

/// Write a cache entry for `commit` under `cache`, marking `cache` as a
/// snapshot cache, and return the entry directory.
fn write_entry(cache: &Path, commit: &str, shape: EntryShape) -> PathBuf {
    SnapshotCache::open(cache).expect("mark the cache");
    let root = cache.canonicalize().expect("canonical cache").join(commit);
    fs::create_dir_all(root.join("tree")).expect("entry tree");
    let extractor_version = match shape {
        EntryShape::Newer => orbit_graph::EXTRACTOR_VERSION + 1,
        _ => orbit_graph::EXTRACTOR_VERSION,
    };
    let mut metadata = serde_json::json!({
        "schema_version": 1,
        "commit_sha": commit,
        "extractor_version": extractor_version,
        "store_schema_version": orbit_graph::STORE_SCHEMA_VERSION,
        "build": {"files_written": 0, "bytes_written": 0, "excluded": [], "files_indexed": 0},
    });
    if !matches!(shape, EntryShape::NoAge) {
        metadata["published_at"] = serde_json::json!(100);
        fs::write(root.join("last_used"), b"100").expect("last used");
    }
    let bytes = match shape {
        EntryShape::Unreadable => b"{not json".to_vec(),
        _ => serde_json::to_vec(&metadata).expect("serialize entry"),
    };
    fs::write(root.join("entry.json"), bytes).expect("write entry");
    root
}

/// `(reason, path)` for every `<action>\t<reason>\t<path>` line.
fn listed(stdout: &str, action: &str) -> Vec<(String, PathBuf)> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            (fields.next() == Some(action)).then(|| {
                let reason = fields.next().unwrap_or_default().to_string();
                (reason, PathBuf::from(fields.next().unwrap_or_default()))
            })
        })
        .collect()
}

/// Every path under `root`, with a file's bytes (`None` for a directory).
fn digest_tree(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let root = root.canonicalize().expect("canonical root");
    let mut digest = BTreeMap::new();
    let mut pending = vec![root];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(dir.as_path()).expect("read directory") {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                digest.insert(path.clone(), None);
                pending.push(path);
            } else {
                let bytes = fs::read(path.as_path()).expect("read file");
                digest.insert(path, Some(bytes));
            }
        }
    }
    digest
}

fn run(current_dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orbit-graph-explorer"))
        .args(args)
        .current_dir(current_dir)
        .output()
        .expect("run orbit-graph-explorer")
}
