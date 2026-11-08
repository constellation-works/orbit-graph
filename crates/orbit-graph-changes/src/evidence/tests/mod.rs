// Unit tests for `evidence`, including the moved `exploration`
// integration suite.

mod exploration;

use super::*;

#[test]
fn fuzzy_name_is_always_a_heuristic_match() {
    assert_eq!(
        categorize(RefKind::Call, RefConfidence::FuzzyName),
        EvidenceCategory::HeuristicMatch
    );
    assert_eq!(
        categorize(RefKind::Use, RefConfidence::FuzzyName),
        EvidenceCategory::HeuristicMatch
    );
}

#[test]
fn resolved_calls_require_exact_or_import_resolution() {
    assert_eq!(
        categorize(RefKind::Call, RefConfidence::Exact),
        EvidenceCategory::ResolvedCall
    );
    assert_eq!(
        categorize(RefKind::Call, RefConfidence::ImportResolved),
        EvidenceCategory::ResolvedCall
    );
    assert_eq!(
        categorize(RefKind::Call, RefConfidence::SameModule),
        EvidenceCategory::ObservedReference
    );
}

#[test]
fn imports_and_structural_relations_have_their_own_categories() {
    assert_eq!(
        categorize(RefKind::Use, RefConfidence::SameModule),
        EvidenceCategory::ImportRelationship
    );
    assert_eq!(
        categorize(RefKind::Impl, RefConfidence::Exact),
        EvidenceCategory::ObservedReference
    );
}

#[test]
fn category_labels_match_the_contract() {
    assert_eq!(
        EvidenceCategory::ObservedReference.label(),
        "observed_reference"
    );
    assert_eq!(EvidenceCategory::ResolvedCall.label(), "resolved_call");
    assert_eq!(
        EvidenceCategory::ImportRelationship.label(),
        "import_relationship"
    );
    assert_eq!(EvidenceCategory::HeuristicMatch.label(), "heuristic_match");
    assert_eq!(
        EvidenceCategory::RuntimeInvocation.label(),
        "runtime_invocation"
    );
}

#[test]
fn candidate_sources_carry_both_spellings() {
    assert_eq!(CandidateSource::CallPath.label(), "call_path");
    assert_eq!(CandidateSource::CallPath.corpus_label(), "call-path");
    assert_eq!(
        CandidateSource::ImportRelationship.label(),
        "import_relationship"
    );
    assert_eq!(CandidateSource::ImportRelationship.corpus_label(), "import");
    assert_eq!(CandidateSource::NamingHeuristic.label(), "naming_heuristic");
    assert_eq!(
        CandidateSource::NamingHeuristic.corpus_label(),
        "naming-heuristic"
    );
    assert_eq!(
        CandidateSource::RuntimeInvocation.label(),
        "runtime_invocation"
    );
    assert_eq!(
        CandidateSource::RuntimeInvocation.corpus_label(),
        "runtime-invocation"
    );
}

#[test]
fn confidence_labels_round_trip() {
    for confidence in [
        RefConfidence::Exact,
        RefConfidence::ImportResolved,
        RefConfidence::SameModule,
        RefConfidence::FuzzyName,
    ] {
        assert_eq!(
            parse_confidence(confidence_label(confidence)),
            Some(confidence)
        );
    }
    assert_eq!(
        parse_confidence("import"),
        Some(RefConfidence::ImportResolved)
    );
    assert_eq!(parse_confidence("fuzzy"), Some(RefConfidence::FuzzyName));
    assert_eq!(parse_confidence("nonsense"), None);
}

#[test]
fn test_paths_are_recognized_across_conventions() {
    assert!(is_test_path("tests/test_lib.rs"));
    assert!(is_test_path("tests/add_test.rs"));
    assert!(is_test_path("test_process.py"));
    assert!(is_test_path("src/__tests__/widget.ts"));
    assert!(is_test_path("spec/widget_spec.rb"));
    assert!(!is_test_path("src/lib.rs"));
    assert!(!is_test_path("mod.py"));
    assert!(!is_test_path("formatting/formatter.py"));
}

#[test]
fn test_affixes_are_stripped_once() {
    assert_eq!(normalize_test_name("test_add"), "add");
    assert_eq!(normalize_test_name("add_test"), "add");
    assert_eq!(
        normalize_test_name("test_process_dynamic"),
        "process_dynamic"
    );
    assert_eq!(normalize_test_name("lib"), "lib");
}

#[test]
fn from_import_of_a_module_identifies_it_by_specifier_and_name() {
    let edge = |target_path: &str, target_symbol: Option<&str>| DepEdge {
        from_file: "tests/test_run.py".to_string(),
        target_path: target_path.to_string(),
        target_symbol: target_symbol.map(str::to_string),
    };
    // `from pkg_a import run` imports the module `pkg_a/run.py`.
    assert_eq!(
        classify_import_edge("pkg_a/run.py", &edge("pkg_a", Some("run"))),
        Some(ModuleMatch::Identified)
    );
    assert_eq!(
        classify_import_edge("pkg_a/run.py", &edge("pkg_b", Some("run"))),
        None
    );
    assert_eq!(
        classify_import_edge("pkg_a/run.py", &edge("pkg_a", Some("other"))),
        None
    );
    // `from run import main` still shares only the basename.
    assert_eq!(
        classify_import_edge("pkg_a/run.py", &edge("run", Some("main"))),
        Some(ModuleMatch::BasenameOnly)
    );
    // `use crate::sync::Thing` names the module by its specifier alone.
    assert_eq!(
        classify_import_edge("src/sync.rs", &edge("crate::sync", Some("Thing"))),
        Some(ModuleMatch::Identified)
    );
}

#[test]
fn module_import_keeps_path_identity_and_downgrades_a_bare_basename() {
    assert_eq!(
        classify_module_import("pkg_a/run.py", "pkg_a.run"),
        Some(ModuleMatch::Identified)
    );
    assert_eq!(classify_module_import("pkg_a/run.py", "pkg_b.run"), None);
    assert_eq!(
        classify_module_import("pkg_a/run.py", "run"),
        Some(ModuleMatch::BasenameOnly)
    );
    assert_eq!(
        classify_module_import("mod.py", "mod"),
        Some(ModuleMatch::Identified)
    );
    assert_eq!(
        classify_module_import("formatting/formatter.py", "formatting.formatter"),
        Some(ModuleMatch::Identified)
    );
    assert_eq!(
        classify_module_import("formatting/formatter.py", "formatting"),
        None
    );
    assert_eq!(
        classify_module_import("src/sync.rs", "crate::sync"),
        Some(ModuleMatch::Identified)
    );
    assert_eq!(
        classify_module_import("src/foo/sync.rs", "crate::foo::sync"),
        Some(ModuleMatch::Identified)
    );
    assert_eq!(
        classify_module_import("src/foo/sync.rs", "crate::sync"),
        Some(ModuleMatch::BasenameOnly)
    );
    assert_eq!(
        classify_module_import("src/sync/mod.rs", "crate::sync"),
        Some(ModuleMatch::Identified)
    );
    assert_eq!(
        classify_module_import("pkg_a/__init__.py", "pkg_a"),
        Some(ModuleMatch::Identified)
    );
    assert_eq!(
        classify_module_import("utils/foo.ts", "./utils/foo"),
        Some(ModuleMatch::Identified)
    );
    assert_eq!(
        classify_module_import("scheduler.rs", "orbit_core::scheduler"),
        None
    );
    assert_eq!(classify_module_import("", "mod"), None);
    assert_eq!(classify_module_import("mod.py", ""), None);
}

#[test]
fn line_starts_index_every_line() {
    assert_eq!(line_starts(b"a\nbb\nc"), vec![0, 2, 5]);
    assert_eq!(line_starts(b""), vec![0]);
}
