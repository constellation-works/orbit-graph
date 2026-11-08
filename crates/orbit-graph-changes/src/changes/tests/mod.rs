// Unit tests for `changes`, including the moved `changed_symbols`
// integration suite.

mod changed_symbols;

use super::*;

#[test]
fn labels_are_stable() {
    assert_eq!(ChangeStatus::Added.label(), "added");
    assert_eq!(ChangeStatus::Removed.label(), "removed");
    assert_eq!(ChangeStatus::Modified.label(), "modified");
    assert_eq!(ChangeStatus::SignatureChanged.label(), "signature_changed");
    assert_eq!(ChangeStatus::Moved.label(), "moved");
    assert_eq!(ChangeStatus::Renamed.label(), "renamed");
    assert_eq!(ChangeStatus::Uncertain.label(), "uncertain");
    assert_eq!(Pairing::SameSelector.label(), "same_selector");
    assert_eq!(Pairing::Uncertain.label(), "uncertain");
    assert_eq!(FileChangeKind::TypeChanged.label(), "type_changed");
    assert_eq!(
        OutOfScopeReason::UnsupportedLanguage.label(),
        "unsupported_language"
    );
}

#[test]
fn signature_line_skips_leading_blank_lines() {
    assert_eq!(signature_line(b"\n\n  fn a() {\n  x\n}"), "fn a() {");
    assert_eq!(
        signature_line(b"def process(x, y=0):\n"),
        "def process(x, y=0):"
    );
    assert_eq!(signature_line(b""), "");
}

#[test]
fn exclusion_reasons_map_to_out_of_scope_reasons() {
    assert_eq!(
        OutOfScopeReason::from_exclusion(ExclusionReason::Symlink),
        OutOfScopeReason::Symlink
    );
    assert_eq!(
        OutOfScopeReason::from_exclusion(ExclusionReason::OversizeBlob { bytes: 9 }),
        OutOfScopeReason::OversizeBlob
    );
    assert_eq!(
        OutOfScopeReason::from_exclusion(ExclusionReason::UnsafeName),
        OutOfScopeReason::UnsafeName
    );
}

#[test]
fn file_change_kinds_map_from_git_deltas() {
    assert_eq!(
        FileChangeKind::from_delta(Delta::Renamed),
        FileChangeKind::Renamed
    );
    assert_eq!(
        FileChangeKind::from_delta(Delta::Typechange),
        FileChangeKind::TypeChanged
    );
    assert_eq!(
        FileChangeKind::from_delta(Delta::Unreadable),
        FileChangeKind::Other
    );
}

#[test]
fn changed_files_index_both_sides_of_a_rename() {
    let mut files = ChangedFiles::default();
    files.push(FileDelta {
        kind: FileChangeKind::Renamed,
        base_path: Some("helper.py".to_string()),
        head_path: Some("helpers.py".to_string()),
    });
    assert_eq!(
        files.for_base("helper.py").map(|delta| delta.kind),
        Some(FileChangeKind::Renamed)
    );
    assert_eq!(
        files.for_head("helpers.py").map(|delta| delta.kind),
        Some(FileChangeKind::Renamed)
    );
    let pairs: Vec<_> = files
        .renamed_pairs()
        .map(|(base, head, _)| (base.clone(), head.clone()))
        .collect();
    assert_eq!(
        pairs,
        vec![("helper.py".to_string(), "helpers.py".to_string())]
    );
}
