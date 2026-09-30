//! File size participates in the metadata fast path for incremental sync.

#![allow(clippy::expect_used)]
#![allow(
    clippy::disallowed_methods,
    reason = "fixtures use fs::write, which the production lint forbids"
)]

use std::fs::{self, File, FileTimes};

use orbit_graph::{Graph, SearchQuery, SyncMode, SyncPolicy};

#[test]
fn auto_sync_detects_size_changes_when_a_replacement_preserves_the_timestamp() {
    let root = tempfile::tempdir().expect("temporary worktree");
    let mut options = git2::RepositoryInitOptions::new();
    options.initial_head("main");
    let repo = git2::Repository::init_opts(root.path(), &options).expect("discovery boundary");
    repo.config()
        .and_then(|config| config.open_level(git2::ConfigLevel::Local))
        .and_then(|mut config| {
            config.set_str(
                "core.excludesFile",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
        })
        .expect("isolate host excludes");
    let source = root.path().join("lib.rs");
    fs::write(&source, "pub fn old_marker() {}\n").expect("write initial source");
    let timestamp = fs::metadata(&source)
        .expect("source metadata")
        .modified()
        .expect("source timestamp");
    let graph = Graph::open(root.path(), SyncPolicy::Manual).expect("open graph");
    graph.sync(SyncMode::Auto).expect("initial sync");

    fs::write(&source, "pub fn fresh_after_preserved_timestamp() {}\n")
        .expect("replace source with longer contents");
    File::options()
        .write(true)
        .open(&source)
        .expect("open source for timestamp restoration")
        .set_times(FileTimes::new().set_modified(timestamp))
        .expect("preserve original modification time");
    assert_eq!(
        fs::metadata(&source)
            .expect("source metadata")
            .modified()
            .expect("source timestamp"),
        timestamp,
    );

    let report = graph.sync(SyncMode::Auto).expect("incremental sync");
    assert_eq!(
        report.files_changed, 1,
        "changed size must invalidate the metadata fast path"
    );
    assert_eq!(
        graph
            .search(&SearchQuery::new("fresh_after_preserved_timestamp"))
            .expect("search refreshed graph")
            .matches
            .len(),
        1,
    );
    assert!(
        graph
            .search(&SearchQuery::new("old_marker"))
            .expect("search old marker")
            .matches
            .is_empty()
    );
}
