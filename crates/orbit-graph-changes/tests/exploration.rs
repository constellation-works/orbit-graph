//! Bounded exploration through the change-analysis library API.

#![allow(clippy::expect_used)]

use orbit_graph::Confidence;
use orbit_graph_changes::changes::ChangedSymbols;
use orbit_graph_changes::evidence::{EvidenceCollector, EvidenceDirection, EvidenceQuery};
use orbit_graph_changes::snapshot::{Comparison, SnapshotSide};

mod common;
use common::corpus;

#[test]
fn inbound_paths_reach_the_caller_and_candidate_test() {
    let case = corpus::build_case("direct-call");
    let comparison =
        Comparison::open(&case.repository, case.base(), case.head()).expect("open comparison");
    let changed = ChangedSymbols::compute(&comparison).expect("changed symbols");
    let mut collector =
        EvidenceCollector::new(&comparison, SnapshotSide::Head).expect("head collector");
    let query = EvidenceQuery {
        changes: Some(&changed),
        ..EvidenceQuery::new(Confidence::SameModule)
    };
    let selector = "symbol:src/lib.rs#helper:function";
    let report = collector.evidence(selector, &query).expect("evidence");
    assert!(report.resolved);
    assert_eq!(report.commit_sha, case.head());
    assert!(report.paths.iter().any(|path| {
        path.distance >= 1
            && path
                .edges
                .iter()
                .any(|edge| edge.source.file == "src/lib.rs")
    }));
    let candidates = collector
        .candidate_tests(selector, &query, vec![])
        .expect("candidate tests");
    assert!(candidates.candidates.iter().any(
        |candidate| candidate.test.selector == "symbol:tests/test_lib.rs#test_helper:function"
    ));
}

#[test]
fn outbound_paths_include_a_callee_and_terminate_at_a_leaf() {
    let case = corpus::build_case("direct-call");
    let comparison =
        Comparison::open(&case.repository, case.base(), case.head()).expect("open comparison");
    let mut collector =
        EvidenceCollector::new(&comparison, SnapshotSide::Head).expect("head collector");
    let query = EvidenceQuery {
        direction: EvidenceDirection::Outbound,
        ..EvidenceQuery::new(Confidence::SameModule)
    };
    let caller = collector
        .evidence("symbol:src/lib.rs#entry:function", &query)
        .expect("caller evidence");
    assert!(
        caller
            .paths
            .iter()
            .any(|path| path.to.selector == "symbol:src/lib.rs#helper:function"),
        "{caller:?}"
    );
    let leaf = collector
        .evidence("symbol:src/lib.rs#helper:function", &query)
        .expect("leaf evidence");
    assert!(leaf.paths.is_empty(), "{leaf:?}");
}

#[test]
fn a_cycle_is_bounded_and_discloses_pruning() {
    let case = corpus::build_case("cycle");
    let comparison =
        Comparison::open(&case.repository, case.base(), case.head()).expect("open comparison");
    let mut collector =
        EvidenceCollector::new(&comparison, SnapshotSide::Head).expect("head collector");
    let mut query = EvidenceQuery::new(Confidence::SameModule);
    query.bounds.depth = 1;
    let report = collector
        .evidence("symbol:src/lib.rs#is_even:function", &query)
        .expect("cycle evidence");
    assert!(report.truncated || report.cycles_pruned > 0, "{report:?}");
    assert!(!report.paths.is_empty(), "{report:?}");
}

#[test]
fn an_exhausted_budget_is_disclosed_as_a_bound() {
    let case = corpus::build_case("direct-call");
    let comparison =
        Comparison::open(&case.repository, case.base(), case.head()).expect("open comparison");
    let mut collector =
        EvidenceCollector::new(&comparison, SnapshotSide::Head).expect("head collector");
    let mut query = EvidenceQuery::new(Confidence::SameModule);
    query.bounds.time_budget_ms = 0;
    let report = collector
        .evidence("symbol:src/lib.rs#helper:function", &query)
        .expect("budgeted evidence");
    assert!(report.truncated, "{report:?}");
    assert_eq!(report.truncated_by.as_deref(), Some("time_budget"));
}

#[test]
fn removed_symbol_evidence_is_only_on_the_base_side() {
    let case = corpus::build_case("removed-symbol");
    let comparison =
        Comparison::open(&case.repository, case.base(), case.head()).expect("open comparison");
    let selector = "symbol:src/lib.rs#old_helper:function";
    let query = EvidenceQuery::new(Confidence::SameModule);
    let mut base = EvidenceCollector::new(&comparison, SnapshotSide::Base).expect("base collector");
    let base = base.evidence(selector, &query).expect("base evidence");
    assert!(base.resolved, "{base:?}");
    assert_eq!(base.commit_sha, case.base());
    let mut head = EvidenceCollector::new(&comparison, SnapshotSide::Head).expect("head collector");
    let head = head.evidence(selector, &query).expect("head evidence");
    assert!(!head.resolved, "{head:?}");
    assert!(head.paths.is_empty(), "{head:?}");
}

#[test]
fn changed_test_is_an_entry_point_and_filters_disclose_omissions() {
    let case = corpus::build_case("changed-test");
    let comparison =
        Comparison::open(&case.repository, case.base(), case.head()).expect("open comparison");
    let mut collector =
        EvidenceCollector::new(&comparison, SnapshotSide::Head).expect("head collector");
    let selector = "symbol:tests/add_test.rs#test_add:function";
    let query = EvidenceQuery::new(Confidence::SameModule);
    let entry_points = collector
        .entry_points(selector, &query)
        .expect("entry points");
    assert!(
        entry_points
            .entry_points
            .iter()
            .any(|entry| entry.node.selector == selector),
        "{entry_points:?}"
    );

    let mut filtered_query = EvidenceQuery::new(Confidence::SameModule);
    filtered_query.filters.scope = Some("unrelated/".to_string());
    let filtered = collector
        .evidence("symbol:src/lib.rs#add:function", &filtered_query)
        .expect("filtered evidence");
    assert!(filtered.paths.is_empty());
    assert!(
        filtered
            .filtered_out
            .iter()
            .any(|entry| entry.reason == "scope")
    );
}

#[test]
fn ambiguous_same_name_symbols_keep_distinct_paths() {
    let case = corpus::build_case("ambiguous-same-name");
    let comparison =
        Comparison::open(&case.repository, case.base(), case.head()).expect("open comparison");
    let mut collector =
        EvidenceCollector::new(&comparison, SnapshotSide::Head).expect("head collector");
    let query = EvidenceQuery::new(Confidence::SameModule);
    for (selector, own_line, other_line) in [
        ("symbol:src/a.rs#run:function", 5, 9),
        ("symbol:src/b.rs#run:function", 9, 5),
    ] {
        let report = collector.evidence(selector, &query).expect("evidence");
        assert!(
            report
                .paths
                .iter()
                .flat_map(|path| &path.edges)
                .any(|edge| edge.source.file == "src/lib.rs" && edge.source.line == Some(own_line)),
            "{report:?}"
        );
        assert!(
            report
                .paths
                .iter()
                .flat_map(|path| &path.edges)
                .all(|edge| edge.source.line != Some(other_line)),
            "{report:?}"
        );
    }
}

#[test]
fn filters_report_what_they_remove() {
    use orbit_graph_changes::filters::FilterSet;

    let case = corpus::build_case("direct-call");
    let comparison =
        Comparison::open(&case.repository, case.base(), case.head()).expect("open comparison");
    let changed = ChangedSymbols::compute(&comparison).expect("changed symbols");
    let (none, removed) = changed.filtered(&FilterSet {
        language: Some("python".to_string()),
        ..FilterSet::default()
    });
    assert!(none.symbols.is_empty());
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].reason, "language");
    assert_eq!(removed[0].count, 1);
    let (kept, removed) = changed.filtered(&FilterSet {
        change_kind: vec!["modified".to_string()],
        ..FilterSet::default()
    });
    assert_eq!(kept.symbols.len(), 1);
    assert!(removed.is_empty());

    let mut collector =
        EvidenceCollector::new(&comparison, SnapshotSide::Head).expect("head collector");
    let mut query = EvidenceQuery::new(Confidence::SameModule);
    query.filters.scope = Some("src".to_string());
    let scoped = collector
        .evidence("symbol:src/lib.rs#helper:function", &query)
        .expect("scoped evidence");
    assert!(
        scoped
            .paths
            .iter()
            .flat_map(|path| &path.edges)
            .all(|edge| edge.source.file.starts_with("src/"))
    );
    assert!(
        scoped
            .filtered_out
            .iter()
            .any(|entry| entry.reason == "scope" && entry.count > 0),
        "{scoped:?}"
    );
}

#[test]
fn unsupported_changes_remain_out_of_scope() {
    let repository = tempfile::tempdir().expect("create repository");
    let repo = git2::Repository::init(repository.path()).expect("init repository");
    let base = common::commit_files(
        &repo,
        repository.path(),
        &[
            ("src/lib.rs", "pub fn helper() -> i32 {\n    1\n}\n"),
            ("schema.proto", "// helper\nmessage Config {}\n"),
        ],
        "base",
        0,
    );
    let head = common::commit_files(
        &repo,
        repository.path(),
        &[
            ("src/lib.rs", "pub fn helper() -> i32 {\n    2\n}\n"),
            (
                "schema.proto",
                "// helper\nmessage Config { int32 id = 1; }\n",
            ),
        ],
        "head",
        1,
    );
    drop(repo);
    let comparison = Comparison::open(repository.path(), &base, &head).expect("open comparison");
    let changed = ChangedSymbols::compute(&comparison).expect("changed symbols");
    assert!(
        changed
            .out_of_scope
            .iter()
            .any(|entry| entry.reason.label() == "unsupported_language"),
        "{changed:?}"
    );
}
