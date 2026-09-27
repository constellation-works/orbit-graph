//! Snapshot-module coverage against a deterministic throwaway Git repository.
//!
//! These tests build their own repository with a fixed author and commit time,
//! so the resolved commit SHAs are stable across machines and runs.

#![allow(clippy::expect_used)]
#![allow(
    clippy::disallowed_methods,
    reason = "fixtures are written with fs::write; clippy.toml bans it only from shipped code"
)]

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Mutex;

use orbit_graph::{Confidence, DEFAULT_IMPACT_DEPTH, RefOpts, Selector};
use orbit_graph_changes::snapshot::{
    BuildState, BuildStatus, Comparison, ComparisonMode, ComparisonOptions, ComparisonOutcome,
    ComparisonProgress, ExclusionReason, SnapshotSide, WorkingTreeChange,
};

mod common;

use common::{build_fixture, commit_files, fingerprint_working_tree, head_commit};

#[test]
fn removed_function_is_base_evidence_and_absent_from_head() {
    let fixture = build_fixture();
    let before = fingerprint_working_tree(fixture.path());
    let head_before = head_commit(fixture.path());

    let comparison = Comparison::open(fixture.path(), fixture.base.as_str(), fixture.head.as_str())
        .expect("open comparison");

    assert_eq!(comparison.base().commit_sha(), fixture.base);
    assert_eq!(comparison.head().commit_sha(), fixture.head);
    assert_ne!(
        comparison.base().commit_sha(),
        comparison.head().commit_sha()
    );
    assert_eq!(comparison.base().side(), SnapshotSide::Base);
    assert_eq!(comparison.head().side(), SnapshotSide::Head);
    assert_eq!(comparison.mode().label(), "direct_base_head");
    assert_ne!(comparison.base().root(), comparison.head().root());
    // Canonicalized: the snapshot resolves symlinked temp roots (macOS
    // `/var` -> `/private/var`) and the comparison must be on equal terms.
    let cache_dir = fs::canonicalize(
        fixture
            .path()
            .join(".orbit-graph")
            .join("explorer")
            .join("snapshots"),
    )
    .expect("cache directory exists after a cold build");
    for side in [SnapshotSide::Base, SnapshotSide::Head] {
        let snapshot = comparison.snapshot(side);
        assert_eq!(
            snapshot.cache_outcome().label(),
            "miss",
            "{side} is built on a cold cache"
        );
        assert!(
            snapshot.root().starts_with(cache_dir.as_path()),
            "{side} snapshot tree must live in the cache directory: {}",
            snapshot.root().display()
        );
        assert!(
            snapshot.db_path().starts_with(cache_dir.as_path()),
            "{side} index must live in the cache directory: {}",
            snapshot.db_path().display()
        );
    }
    // The cache is confined to its own subdirectory: the repository's own
    // `.orbit-graph/*.db` index files are neither read nor written.
    let own_databases: Vec<std::path::PathBuf> = fs::read_dir(fixture.path().join(".orbit-graph"))
        .expect("read graph scratch directory")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|extension| extension == "db"))
        .collect();
    assert!(
        own_databases.is_empty(),
        "the repository's own graph databases must never be written: {own_databases:?}"
    );
    assert!(comparison.base().files_indexed() > 0);
    assert!(comparison.head().files_indexed() > 0);

    let selector: Selector = "symbol:src/lib.rs#removed_helper:function"
        .parse()
        .expect("parse selector");

    let base_refs = comparison
        .base()
        .refs(&selector, &RefOpts::default())
        .expect("base refs");
    assert_eq!(
        base_refs.target.qualified.as_deref(),
        Some("removed_helper"),
        "base snapshot resolves the removed symbol: {base_refs:?}"
    );
    assert!(
        base_refs
            .refs
            .iter()
            .any(|entry| entry.file == "src/lib.rs"),
        "base snapshot keeps the call site: {base_refs:?}"
    );
    let base_impact = comparison
        .base()
        .impact(&selector, DEFAULT_IMPACT_DEPTH, Confidence::default())
        .expect("base impact");
    assert!(
        base_impact
            .touched
            .iter()
            .any(|entry| entry.qualified_name == "entry"),
        "base snapshot reaches the caller: {base_impact:?}"
    );

    let head_refs = comparison
        .head()
        .refs(&selector, &RefOpts::default())
        .expect("head refs");
    assert_eq!(
        head_refs.target.qualified, None,
        "head snapshot must not resolve the removed symbol: {head_refs:?}"
    );
    assert!(head_refs.refs.is_empty(), "{head_refs:?}");
    assert!(head_refs.relations.is_empty(), "{head_refs:?}");
    let head_impact = comparison
        .head()
        .impact(&selector, DEFAULT_IMPACT_DEPTH, Confidence::default())
        .expect("head impact");
    assert!(head_impact.touched.is_empty(), "{head_impact:?}");
    assert!(head_impact.fallback.is_none(), "{head_impact:?}");

    let surviving: Selector = "symbol:src/lib.rs#entry:function"
        .parse()
        .expect("parse selector");
    let head_entry = comparison
        .head()
        .refs(&surviving, &RefOpts::default())
        .expect("head refs for surviving symbol");
    assert_eq!(head_entry.target.qualified.as_deref(), Some("entry"));

    assert!(!comparison.working_tree().dirty);
    assert_eq!(comparison.working_tree().notice(), None);

    // A cached tree outlives the comparison on purpose: the next launch reuses
    // it, and only `clean` removes it.
    let base_root = comparison.base().root().to_path_buf();
    let head_root = comparison.head().root().to_path_buf();
    drop(comparison);
    assert!(base_root.exists(), "a cached base tree is kept for reuse");
    assert!(head_root.exists(), "a cached head tree is kept for reuse");

    assert_eq!(
        fingerprint_working_tree(fixture.path()),
        before,
        "the user working tree must be byte-for-byte unchanged"
    );
    assert_eq!(head_commit(fixture.path()), head_before);
}

#[test]
fn dirty_working_tree_is_detected_and_reported() {
    let fixture = build_fixture();
    fs::write(
        fixture.path().join("src/lib.rs"),
        "pub fn entry() -> i32 {\n    8\n}\n",
    )
    .expect("modify tracked file");
    fs::write(fixture.path().join("scratch.rs"), "pub fn scratch() {}\n")
        .expect("add untracked file");

    let comparison = Comparison::open(fixture.path(), fixture.base.as_str(), fixture.head.as_str())
        .expect("open comparison");
    let state = comparison.working_tree();

    assert!(state.dirty, "{state:?}");
    assert!(!state.truncated, "{state:?}");
    let by_path: BTreeMap<&str, WorkingTreeChange> = state
        .entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry.change))
        .collect();
    assert_eq!(
        by_path.get("src/lib.rs"),
        Some(&WorkingTreeChange::Modified),
        "{state:?}"
    );
    assert_eq!(
        by_path.get("scratch.rs"),
        Some(&WorkingTreeChange::Untracked),
        "{state:?}"
    );

    let notice = state.notice().expect("dirty comparison carries a notice");
    assert!(notice.contains("uncommitted"), "{notice}");

    // Uncommitted work is excluded from snapshots: the base snapshot still
    // resolves the committed symbol even though the file now differs on disk.
    let selector: Selector = "symbol:src/lib.rs#removed_helper:function"
        .parse()
        .expect("parse selector");
    let base_refs = comparison
        .base()
        .refs(&selector, &RefOpts::default())
        .expect("base refs");
    assert_eq!(
        base_refs.target.qualified.as_deref(),
        Some("removed_helper")
    );
}

#[test]
fn unresolvable_reference_is_reported_without_materializing() {
    let fixture = build_fixture();
    let error = Comparison::open(
        fixture.path(),
        "refs/heads/does-not-exist",
        fixture.head.as_str(),
    )
    .err()
    .expect("unknown ref fails");
    let message = error.to_string();
    assert!(message.contains("does-not-exist"), "{message}");
}

/// Records the last [`BuildStatus`] reported for each side, so a test can
/// inspect what a build's terminal report looked like.
#[derive(Default)]
struct RecordingProgress {
    base: Mutex<Option<BuildStatus>>,
    head: Mutex<Option<BuildStatus>>,
}

impl RecordingProgress {
    fn slot(&self, side: SnapshotSide) -> &Mutex<Option<BuildStatus>> {
        match side {
            SnapshotSide::Base => &self.base,
            SnapshotSide::Head => &self.head,
        }
    }

    fn last_status(&self, side: SnapshotSide) -> BuildStatus {
        self.slot(side)
            .lock()
            .expect("recording progress lock")
            .clone()
            .unwrap_or_else(|| panic!("no status recorded for {side}"))
    }
}

impl ComparisonProgress for RecordingProgress {
    fn on_status(&self, side: SnapshotSide, status: &BuildStatus) {
        *self.slot(side).lock().expect("recording progress lock") = Some(status.clone());
    }

    fn is_cancelled(&self) -> bool {
        false
    }
}

/// `docs/design/change-explorer.md` (Milestone 4) describes `languages` as
/// "a best-effort, extension-derived list built up as indexing progresses",
/// reported live in the `indexing` object. A cold build must land that list
/// on the side's `ready` status rather than dropping it once indexing
/// finishes, and a cache hit — which never runs the live indexer at all —
/// must still be able to report it, from whatever the original build
/// persisted.
#[test]
fn languages_survive_into_the_ready_status_on_a_cold_build_and_a_cache_hit() {
    let fixture = build_fixture();

    let cold = RecordingProgress::default();
    let outcome = Comparison::open_with_progress(
        fixture.path(),
        fixture.base.as_str(),
        fixture.head.as_str(),
        &ComparisonOptions::default(),
        &cold,
    )
    .expect("cold build succeeds");
    let ComparisonOutcome::Ready(comparison) = outcome else {
        panic!("a build with no cancellation must not report Cancelled");
    };
    for side in [SnapshotSide::Base, SnapshotSide::Head] {
        assert_eq!(
            comparison.snapshot(side).cache_outcome().label(),
            "miss",
            "{side} is built on a cold cache"
        );
        let status = cold.last_status(side);
        assert_eq!(status.state, BuildState::Ready);
        assert_eq!(
            status.languages,
            vec!["rust".to_string()],
            "{side}'s ready status must keep the language(s) seen while indexing, not reset to empty"
        );
    }
    drop(comparison);

    // Reopen the same scope: both sides are now cache hits, which open the
    // published entry as it stands and never run the live indexer that
    // originally derived `languages`.
    let warm = RecordingProgress::default();
    let outcome = Comparison::open_with_progress(
        fixture.path(),
        fixture.base.as_str(),
        fixture.head.as_str(),
        &ComparisonOptions::default(),
        &warm,
    )
    .expect("warm build succeeds");
    let ComparisonOutcome::Ready(comparison) = outcome else {
        panic!("a build with no cancellation must not report Cancelled");
    };
    for side in [SnapshotSide::Base, SnapshotSide::Head] {
        assert_eq!(
            comparison.snapshot(side).cache_outcome().label(),
            "hit",
            "{side} must reuse the entry the cold build published"
        );
        let status = warm.last_status(side);
        assert_eq!(status.state, BuildState::Ready);
        assert_eq!(
            status.languages,
            vec!["rust".to_string()],
            "a cache hit for {side} must still report the languages the original build persisted"
        );
    }
}

/// Walk `root` without following symlinks and report whether any regular file
/// contains `needle`.
#[cfg(unix)]
fn snapshot_contains(root: &Path, needle: &str) -> bool {
    fn walk(dir: &Path, needle: &[u8]) -> bool {
        let entries = fs::read_dir(dir).expect("read snapshot directory");
        for entry in entries {
            let entry = entry.expect("read snapshot entry");
            let path = entry.path();
            let metadata = fs::symlink_metadata(path.as_path()).expect("snapshot metadata");
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                if walk(path.as_path(), needle) {
                    return true;
                }
            } else if metadata.is_file()
                && let Ok(bytes) = fs::read(path.as_path())
                && bytes.windows(needle.len()).any(|window| window == needle)
            {
                return true;
            }
        }
        false
    }
    walk(root, needle.as_bytes())
}

#[cfg(unix)]
fn head_qualified(comparison: &Comparison, selector: &str) -> Option<String> {
    let selector: Selector = selector.parse().expect("parse selector");
    comparison
        .head()
        .refs(&selector, &RefOpts::default())
        .expect("query working-tree snapshot")
        .target
        .qualified
}

/// A tracked directory replaced by a symlink to a directory outside the
/// repository must not contribute that directory's bytes to a working-tree
/// snapshot. Ordinary tracked and untracked files are still copied, a deleted
/// tracked file stays absent, and a tracked file replaced by a symlink is
/// excluded without reading its target (STD-05 §R6, §R7).
#[cfg(unix)]
#[test]
fn working_tree_comparison_excludes_descendants_of_an_outside_symlink() {
    use std::os::unix::fs::symlink;

    use git2::Repository;
    use tempfile::TempDir;

    const COMMITTED_LIB: &str = "pub fn kept() -> i32 {\n    1\n}\n";
    const WORKTREE_LIB: &str =
        "pub fn kept() -> i32 {\n    // WORKTREE_KEPT_MARKER_ORB13394\n    3\n}\n";
    const COMMITTED_LEAF: &str = "pub fn inside() -> i32 {\n    2\n}\n";
    const EXTERNAL_LEAF: &str =
        "pub fn leaked() -> i32 {\n    // EXTERNAL_DIRECTORY_MARKER_ORB13394\n    9\n}\n";
    const EXTERNAL_SECRET: &str =
        "pub fn secret() -> i32 {\n    // EXTERNAL_SECRET_MARKER_ORB13394\n    8\n}\n";
    const COMMITTED_LINK: &str = "pub fn link_file() -> i32 {\n    6\n}\n";
    const EXTERNAL_LINK: &str =
        "pub fn stolen() -> i32 {\n    // EXTERNAL_LINK_MARKER_ORB13394\n    7\n}\n";
    const COMMITTED_GONE: &str = "pub fn gone() -> i32 {\n    5\n}\n";
    const EXTRA: &str =
        "pub fn extra() -> i32 {\n    // WORKTREE_EXTRA_MARKER_ORB13394\n    4\n}\n";

    let parent = TempDir::new().expect("parent directory");
    let repo_path = parent.path().join("repo");
    let outside = parent.path().join("outside");
    fs::create_dir(repo_path.as_path()).expect("repository directory");
    fs::create_dir(outside.as_path()).expect("outside directory");
    let outside_leaf = outside.join("leaf.rs");
    let outside_secret = outside.join("secret.rs");
    let outside_link = outside.join("link_target.rs");
    fs::write(outside_leaf.as_path(), EXTERNAL_LEAF).expect("outside leaf");
    fs::write(outside_secret.as_path(), EXTERNAL_SECRET).expect("outside secret");
    fs::write(outside_link.as_path(), EXTERNAL_LINK).expect("outside link target");

    let base = {
        let repo = Repository::init(repo_path.as_path()).expect("init repository");
        let base = commit_files(
            &repo,
            repo_path.as_path(),
            &[
                ("src/lib.rs", COMMITTED_LIB),
                ("src/nested/leaf.rs", COMMITTED_LEAF),
                ("link.rs", COMMITTED_LINK),
                ("gone.rs", COMMITTED_GONE),
            ],
            "base: tracked files a working-tree comparison will mutate",
            0,
        );
        drop(repo);
        base
    };

    fs::write(repo_path.join("src/lib.rs"), WORKTREE_LIB).expect("edit tracked file");
    fs::write(repo_path.join("src/extra.rs"), EXTRA).expect("untracked file");
    fs::remove_file(repo_path.join("gone.rs")).expect("delete tracked file");
    fs::remove_file(repo_path.join("link.rs")).expect("remove file that becomes a symlink");
    symlink(outside_link.as_path(), repo_path.join("link.rs")).expect("leaf symlink");
    fs::remove_dir_all(repo_path.join("src/nested")).expect("remove tracked directory");
    symlink(outside.as_path(), repo_path.join("src/nested")).expect("ancestor symlink");

    let index_before = fs::read(repo_path.join(".git/index")).expect("read index");
    let outside_leaf_before = fs::read(outside_leaf.as_path()).expect("read outside leaf");
    let outside_link_before = fs::read(outside_link.as_path()).expect("read outside link");
    let outside_secret_before = fs::read(outside_secret.as_path()).expect("read outside secret");

    let outcome = Comparison::open_working_tree(
        repo_path.as_path(),
        base.as_str(),
        &ComparisonOptions {
            cache_dir: None,
            no_cache: true,
            scratch_dir: Some(parent.path().join("scratch")),
        },
        &RecordingProgress::default(),
    )
    .expect("open working-tree comparison");
    let ComparisonOutcome::Ready(comparison) = outcome else {
        panic!("an uncancelled build must be ready");
    };

    assert_eq!(comparison.mode(), ComparisonMode::WorkingTree);
    assert_eq!(comparison.head().commit_sha(), "worktree");

    let head = comparison.head();
    let excluded = |path: &str| {
        head.materialization()
            .excluded
            .iter()
            .find(|entry| entry.path == path)
            .map(|entry| entry.reason)
    };
    assert_eq!(
        excluded("src/nested/leaf.rs"),
        Some(ExclusionReason::Symlink),
        "a descendant of an outside directory symlink is excluded: {:?}",
        head.materialization()
    );
    assert_eq!(
        excluded("link.rs"),
        Some(ExclusionReason::Symlink),
        "a tracked file replaced by a symlink is excluded: {:?}",
        head.materialization()
    );
    assert!(
        head.materialization()
            .excluded
            .iter()
            .all(|entry| entry.path != "gone.rs"),
        "a deleted tracked file is absent, not an exclusion: {:?}",
        head.materialization()
    );
    for path in head
        .materialization()
        .excluded
        .iter()
        .filter(|entry| entry.path.starts_with("src/nested/"))
    {
        assert_eq!(
            path.reason,
            ExclusionReason::Symlink,
            "every descendant reached through the outside symlink is excluded: {path:?}"
        );
    }

    let head_root = head.root();
    let read_snapshot = |path: &str| {
        fs::read_to_string(head_root.join(path)).unwrap_or_else(|_| panic!("read snapshot {path}"))
    };
    assert!(
        read_snapshot("src/lib.rs").contains("WORKTREE_KEPT_MARKER_ORB13394"),
        "a tracked regular file is copied from the working tree"
    );
    assert!(
        read_snapshot("src/extra.rs").contains("WORKTREE_EXTRA_MARKER_ORB13394"),
        "an untracked regular file is copied"
    );
    assert!(
        !head_root.join("gone.rs").exists(),
        "a deleted tracked file is not materialized"
    );
    assert!(
        !head_root.join("link.rs").exists(),
        "a symlink is not materialized"
    );
    assert!(
        !head_root.join("src/nested/leaf.rs").exists(),
        "an escaped descendant is not materialized"
    );
    for marker in [
        "EXTERNAL_DIRECTORY_MARKER_ORB13394",
        "EXTERNAL_LINK_MARKER_ORB13394",
        "EXTERNAL_SECRET_MARKER_ORB13394",
    ] {
        assert!(
            !snapshot_contains(head_root, marker),
            "external bytes must not be copied into the snapshot ({marker})"
        );
    }

    assert_eq!(
        head_qualified(&comparison, "symbol:src/lib.rs#kept:function").as_deref(),
        Some("kept")
    );
    assert_eq!(
        head_qualified(&comparison, "symbol:src/extra.rs#extra:function").as_deref(),
        Some("extra")
    );
    assert_eq!(
        head_qualified(&comparison, "symbol:gone.rs#gone:function"),
        None,
        "a deleted file is not indexed"
    );
    for selector in [
        "symbol:src/nested/leaf.rs#leaked:function",
        "symbol:src/nested/leaf.rs#inside:function",
        "symbol:src/nested/secret.rs#secret:function",
        "symbol:link.rs#stolen:function",
        "symbol:link.rs#link_file:function",
    ] {
        assert_eq!(
            head_qualified(&comparison, selector),
            None,
            "{selector} must not be indexed from outside the repository"
        );
    }

    let base_snapshot = comparison.base();
    let base_leaf = fs::read_to_string(base_snapshot.root().join("src/nested/leaf.rs"))
        .expect("base still has the committed leaf");
    assert!(base_leaf.contains("fn inside"), "{base_leaf}");
    assert!(!base_leaf.contains("EXTERNAL_DIRECTORY_MARKER_ORB13394"));
    assert!(base_snapshot.root().join("gone.rs").is_file());
    assert!(base_snapshot.root().join("link.rs").is_file());

    drop(comparison);
    assert_eq!(
        fs::read(repo_path.join(".git/index")).expect("read index"),
        index_before,
        "the Git index is read, never written"
    );
    assert_eq!(
        fs::read(outside_leaf.as_path()).expect("outside leaf"),
        outside_leaf_before
    );
    assert_eq!(
        fs::read(outside_link.as_path()).expect("outside link"),
        outside_link_before
    );
    assert_eq!(
        fs::read(outside_secret.as_path()).expect("outside secret"),
        outside_secret_before
    );
    assert!(
        fs::symlink_metadata(repo_path.join("src/nested"))
            .expect("ancestor symlink")
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::read_to_string(repo_path.join("src/lib.rs")).expect("worktree file"),
        WORKTREE_LIB
    );
}
