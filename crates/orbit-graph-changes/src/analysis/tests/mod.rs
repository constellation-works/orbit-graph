use std::path::PathBuf;

use orbit_graph::Confidence;

use super::{
    AnalysisError, ChangesBounds, ChangesRange, ChangesRequest, confidence_rank, path_source,
    validate, weakest_confidence,
};
use crate::evidence::{EndpointRef, EvidenceCategory};
use crate::filters::FilterSet;
use crate::report::ReportEvidencePath;
use crate::snapshot::ComparisonOptions;

fn request(bounds: ChangesBounds, selection: Vec<String>) -> ChangesRequest {
    ChangesRequest {
        repository: PathBuf::from("."),
        range: ChangesRange::WorkingTree { base: None },
        selection,
        filters: FilterSet::default(),
        min_confidence: Confidence::default(),
        bounds,
        cache: ComparisonOptions::default(),
    }
}

#[test]
fn two_dot_range_names_two_revisions() {
    assert_eq!(
        ChangesRange::parse("main..feature").unwrap(),
        ChangesRange::Revisions {
            base: "main".to_string(),
            head: "feature".to_string(),
        }
    );
}

#[test]
fn a_bare_ref_compares_the_working_tree() {
    assert_eq!(
        ChangesRange::parse("origin/main").unwrap(),
        ChangesRange::WorkingTree {
            base: Some("origin/main".to_string()),
        }
    );
}

#[test]
fn malformed_ranges_are_rejected_with_the_range_named() {
    for text in ["", "  ", "main...feature", "..feature", "main..", "a..b..c"] {
        match ChangesRange::parse(text) {
            Err(AnalysisError::InvalidRange { range, .. }) => assert_eq!(range, text),
            other => panic!("{text:?} parsed as {other:?}"),
        }
    }
}

#[test]
fn default_bounds_are_valid() {
    ChangesBounds::default().validate().unwrap();
}

#[test]
fn out_of_range_bounds_are_rejected_not_clamped() {
    let cases = [
        (
            ChangesBounds {
                depth: 0,
                ..ChangesBounds::default()
            },
            "depth",
        ),
        (
            ChangesBounds {
                depth: 11,
                ..ChangesBounds::default()
            },
            "depth",
        ),
        (
            ChangesBounds {
                max_tests: 0,
                ..ChangesBounds::default()
            },
            "max_tests",
        ),
        (
            ChangesBounds {
                budget_ms: Some(999),
                ..ChangesBounds::default()
            },
            "budget_ms",
        ),
        (
            ChangesBounds {
                node_cap: 2_001,
                ..ChangesBounds::default()
            },
            "node_cap",
        ),
    ];
    for (bounds, expected) in cases {
        match bounds.validate() {
            Err(AnalysisError::InvalidBound { name, .. }) => assert_eq!(name, expected),
            other => panic!("{bounds:?} validated as {other:?}"),
        }
    }
}

#[test]
fn selection_must_be_symbol_selectors() {
    let valid = request(
        ChangesBounds::default(),
        vec!["symbol:src/lib.rs#run:function".to_string()],
    );
    validate(&valid).unwrap();

    for selector in ["file:src/lib.rs", "not a selector"] {
        let invalid = request(ChangesBounds::default(), vec![selector.to_string()]);
        match validate(&invalid) {
            Err(AnalysisError::InvalidSelection {
                selector: named, ..
            }) => {
                assert_eq!(named, selector);
            }
            other => panic!("{selector:?} validated as {other:?}"),
        }
    }
}

#[test]
fn confidence_ranks_strongest_first_and_unknown_last() {
    let ranked = [
        "exact",
        "import_resolved",
        "same_module",
        "fuzzy_name",
        "other",
    ]
    .map(confidence_rank);
    assert!(ranked.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn a_distance_zero_endpoint_is_the_changed_symbol_itself_not_an_unknown_path() {
    let endpoint = EndpointRef {
        selector: "symbol:tests/cli.rs#runs:test".to_string(),
        snapshot: "head".to_string(),
        label: "tests/cli.rs#runs".to_string(),
        origin: "symbol".to_string(),
    };
    let path = ReportEvidencePath {
        schema_version: 1,
        path_id: "p-0".to_string(),
        from: endpoint.clone(),
        to: endpoint,
        distance: 0,
        category: EvidenceCategory::ResolvedCall,
        truncated: false,
        truncated_by: None,
        edges: Vec::new(),
    };
    assert_eq!(path_source(&path), "changed_symbol");
    assert_eq!(weakest_confidence(&path), "exact");
}
