//! Short selectors keep the public graph resolver's identity and precedence.

#![allow(clippy::expect_used)]
#![allow(
    clippy::disallowed_methods,
    reason = "the shared Git fixture writes source with fs::write"
)]

use git2::Repository;
use orbit_graph::Confidence;
use orbit_graph_changes::analysis::{ChangesRange, ChangesRequest, analyse};
use orbit_graph_changes::filters::FilterSet;
use orbit_graph_changes::report::{ReportLimits, ReportOptions, build_report_bounded};
use orbit_graph_changes::snapshot::{Comparison, ComparisonOptions};
use tempfile::TempDir;

mod common;

fn check(base: &str, head: &str, requested: &str, expected: &[&str]) {
    let dir = TempDir::new().expect("fixture repository");
    let repo = Repository::init(dir.path()).expect("initialize fixture");
    let base = common::commit_files(&repo, dir.path(), &[("src/example.py", base)], "base", 0);
    let head = common::commit_files(&repo, dir.path(), &[("src/example.py", head)], "head", 1);
    let cache = TempDir::new().expect("snapshot cache");
    let request = ChangesRequest {
        repository: dir.path().to_path_buf(),
        range: ChangesRange::Revisions {
            base: base.clone(),
            head: head.clone(),
        },
        selection: vec![requested.to_owned()],
        cache: ComparisonOptions {
            cache_dir: Some(cache.path().join("snapshots")),
            ..ComparisonOptions::default()
        },
        filters: FilterSet::default(),
        min_confidence: Confidence::SameModule,
        bounds: Default::default(),
    };
    let document = analyse(&request).expect("analyse selected symbols");
    let actual: Vec<&str> = document
        .symbols
        .iter()
        .map(|symbol| symbol.selector.as_str())
        .collect();
    assert_eq!(actual, expected, "selection {requested}");
    assert_eq!(document.query.selection, request.selection);
    if expected.is_empty() {
        assert_eq!(document.unmatched_selection, request.selection);
    } else {
        assert!(document.unmatched_selection.is_empty());
    }

    // Both public report and agent-facing analysis must select the same rows.
    // A zero symbol budget records the selection without spending it on paths.
    let comparison = Comparison::open(dir.path(), &base, &head).expect("comparison");
    let built = build_report_bounded(
        &comparison,
        &ReportOptions {
            selection: request.selection.clone(),
            ..ReportOptions::default()
        },
        &ReportLimits {
            max_symbols: Some(0),
            ..ReportLimits::default()
        },
    )
    .expect("bounded selected report");
    let actual: Vec<&str> = built
        .unanalysed
        .iter()
        .map(|(selector, _)| selector.as_str())
        .collect();
    assert_eq!(actual, expected, "report selection {requested}");
    assert_eq!(built.report.query_options.selection, request.selection);
}

#[test]
fn unique_short_selector_resolves_to_its_qualified_identity() {
    check(
        "def outer():\n    def run():\n        return 1\n    return run()\n",
        "def outer():\n    def run():\n        return 2\n    return run()\n",
        "symbol:src/example.py#run:function",
        &["symbol:src/example.py#outer.run:function"],
    );
    check(
        "class Outer:\n    def run(self):\n        return 1\n",
        "class Outer:\n    def run(self):\n        return 2\n",
        "symbol:src/example.py#run:method",
        &["symbol:src/example.py#Outer.run:method"],
    );
}

#[test]
fn top_level_identity_wins_over_a_same_named_changed_nested_function() {
    let base = "def run():\n    return 0\ndef outer():\n    def run():\n        return 1\n    return run()\n";
    let head = "def run():\n    return 0\ndef outer():\n    def run():\n        return 2\n    return run()\n";
    check(base, head, "symbol:src/example.py#run:function", &[]);
    check(
        base,
        head,
        "symbol:src/example.py#outer.run:function",
        &["symbol:src/example.py#outer.run:function"],
    );
}

#[test]
fn a_base_side_short_alias_can_select_a_renamed_nested_function() {
    check(
        "def outer():\n    def previous():\n        return 1\n    return previous()\n",
        "def outer():\n    def renamed():\n        return 1\n    return renamed()\n",
        "symbol:src/example.py#previous:function",
        &["symbol:src/example.py#outer.renamed:function"],
    );
}

#[test]
fn an_unmatched_alias_never_becomes_an_empty_selection_of_everything() {
    check(
        "def outer():\n    def run():\n        return 1\n    return run()\n",
        "def outer():\n    def run():\n        return 2\n    return run()\n",
        "symbol:src/example.py#absent:function",
        &[],
    );
}
