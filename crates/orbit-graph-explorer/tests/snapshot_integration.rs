//! Snapshot-module coverage against a deterministic throwaway Git repository.
//!
//! These tests build their own repository with a fixed author and commit time,
//! so the resolved commit SHAs are stable across machines and runs.

#![allow(clippy::expect_used)]

use std::collections::BTreeMap;
use std::fs;

use orbit_graph::{Confidence, DEFAULT_IMPACT_DEPTH, RefOpts, Selector};
use orbit_graph_explorer::snapshot::{Comparison, SnapshotSide, WorkingTreeChange};

mod common;

use common::{build_fixture, fingerprint_working_tree, head_commit};

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
