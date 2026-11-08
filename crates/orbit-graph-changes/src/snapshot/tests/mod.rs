// Unit tests for `snapshot`, including the moved `snapshot_integration`
// and `cache_containment` integration suites.

#[cfg(unix)]
mod cache_containment;
mod snapshot_integration;

use super::*;

#[test]
fn unsafe_entry_names_are_rejected() {
    assert!(is_safe_entry_name("src"));
    assert!(is_safe_entry_name("lib.rs"));
    assert!(!is_safe_entry_name(""));
    assert!(!is_safe_entry_name("."));
    assert!(!is_safe_entry_name(".."));
    assert!(!is_safe_entry_name("a/b"));
    assert!(!is_safe_entry_name("a\\b"));
    assert!(!is_safe_entry_name(".git"));
    assert!(!is_safe_entry_name(".GIT"));
}

#[test]
fn clean_working_tree_has_no_notice() {
    let state = WorkingTreeState::default();
    assert!(!state.dirty);
    assert_eq!(state.notice(), None);
}

#[test]
fn dirty_working_tree_notice_counts_entries() {
    let state = WorkingTreeState {
        dirty: true,
        entries: vec![WorkingTreeEntry {
            path: "src/lib.rs".to_string(),
            change: WorkingTreeChange::Modified,
        }],
        truncated: false,
    };
    let notice = state.notice().expect("dirty state has a notice");
    assert!(notice.contains("1 uncommitted change(s)"), "{notice}");
}

#[test]
fn graph_scratch_paths_are_not_uncommitted_work() {
    assert!(is_graph_scratch(".orbit-graph"));
    assert!(is_graph_scratch(
        ".orbit-graph/explorer/snapshots/abc/tree/src/lib.rs"
    ));
    assert!(!is_graph_scratch("src/lib.rs"));
    assert!(!is_graph_scratch(".orbit-graphics/lib.rs"));
}

#[test]
fn exclusion_reasons_round_trip_through_their_labels() {
    for reason in [
        ExclusionReason::Symlink,
        ExclusionReason::Submodule,
        ExclusionReason::UnsafeName,
        ExclusionReason::UnsupportedKind,
        ExclusionReason::OversizeBlob { bytes: 9 },
    ] {
        assert_eq!(
            ExclusionReason::from_label(reason.label(), reason.bytes()),
            reason
        );
    }
    assert_eq!(
        ExclusionReason::from_label("something-new", None),
        ExclusionReason::UnsupportedKind
    );
}

#[test]
fn labels_are_stable() {
    assert_eq!(SnapshotSide::Base.label(), "base");
    assert_eq!(SnapshotSide::Head.label(), "head");
    assert_eq!(ComparisonMode::DirectBaseHead.label(), "direct_base_head");
    assert_eq!(ExclusionReason::Symlink.label(), "symlink");
    assert_eq!(
        ExclusionReason::OversizeBlob { bytes: 1 }.label(),
        "oversize_blob"
    );
    assert_eq!(WorkingTreeChange::Untracked.label(), "untracked");
}
