//! JSON report contract through the public change-analysis library.

#![allow(clippy::expect_used)]

use orbit_graph_changes::report::{ExcerptMode, ReportOptions, build_report};
use orbit_graph_changes::snapshot::Comparison;
use serde_json::Value;

mod common;
use common::corpus;

const GENERATED_AT: &str = "2024-01-01T00:00:00Z";

fn report(case_id: &str, configure: impl FnOnce(&mut ReportOptions)) -> Value {
    let case = corpus::build_case(case_id);
    let comparison =
        Comparison::open(&case.repository, case.base(), case.head()).expect("open comparison");
    let mut options = ReportOptions {
        generated_at: Some(GENERATED_AT.to_string()),
        ..ReportOptions::default()
    };
    configure(&mut options);
    serde_json::to_value(build_report(&comparison, &options).expect("build report"))
        .expect("serialize report")
}

#[test]
fn direct_call_report_matches_exported_contract() {
    let json = report("direct-call", |_| {});
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["generated_at"], GENERATED_AT);
    assert_eq!(json["comparison"]["mode"], "direct_base_head");
    assert_eq!(
        json["comparison"]["effective_base_sha"],
        json["comparison"]["base"]["commit_sha"]
    );
    assert!(json["comparison"]["repository"].is_null());
    assert_eq!(json["index_identity"]["store_schema_version"], 1);
    assert!(
        json["index_identity"]["base_index_identity"]
            .as_str()
            .unwrap_or_default()
            .starts_with("blake3:")
    );
    assert_eq!(json["query_options"]["excerpts"], "controlled");
    assert_eq!(json["query_options"]["min_confidence"], "same_module");
    let symbols = json["changed_symbols"]["symbols"]
        .as_array()
        .expect("symbols");
    assert_eq!(symbols.len(), 1, "{json}");
    assert_eq!(symbols[0]["status"], "modified");
    assert!(
        json["evidence_paths"]
            .as_array()
            .is_some_and(|paths| !paths.is_empty())
    );
    assert!(json["outbound_paths"].is_array());
    assert!(json["entry_points"].is_array());
    assert!(json["candidate_tests"]["candidates"].is_array());
    assert!(json["unresolved"].is_array());
    for key in ["truncated", "unsupported", "excluded"] {
        assert!(json["scope"][key].is_array(), "{key}: {json}");
    }
    assert_eq!(json["source_rendering"], "excerpt");
    assert!(
        json["evidence_paths"]
            .as_array()
            .expect("paths")
            .iter()
            .flat_map(|path| path["edges"].as_array().expect("edges").iter())
            .any(|edge| edge["rendering"] == "embedded" && edge["excerpt"]["text"].is_string())
    );
}

#[test]
fn removed_symbol_source_comes_from_base_snapshot() {
    let json = report("removed-symbol", |_| {});
    let removed = json["changed_symbols"]["symbols"]
        .as_array()
        .expect("symbols")
        .iter()
        .find(|symbol| symbol["status"] == "removed")
        .expect("removed symbol");
    assert!(removed["head"].is_null());
    assert_eq!(removed["base"]["snapshot"], "base");
    assert_eq!(removed["base"]["rendering"], "embedded");
    assert!(
        removed["base"]["excerpt"]["text"]
            .as_str()
            .unwrap_or_default()
            .contains("old_helper")
    );
}

#[test]
fn excerpt_modes_keep_reference_and_full_span_semantics() {
    let none = report("direct-call", |options| {
        options.excerpts = ExcerptMode::None
    });
    assert_eq!(none["source_rendering"], "reference");
    assert!(
        none["evidence_paths"]
            .as_array()
            .expect("paths")
            .iter()
            .flat_map(|path| path["edges"].as_array().expect("edges").iter())
            .all(|edge| edge["rendering"] == "reference")
    );
    let full = report("direct-call", |options| {
        options.excerpts = ExcerptMode::FullSpan
    });
    assert_eq!(full["source_rendering"], "excerpt");
    assert!(
        full["evidence_paths"]
            .as_array()
            .expect("paths")
            .iter()
            .flat_map(|path| path["edges"].as_array().expect("edges").iter())
            .any(|edge| edge["rendering"] == "embedded"
                && edge["excerpt"]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("helper"))
    );
}

#[test]
fn renamed_paths_and_depth_truncation_are_exported() {
    let renamed = report("renamed-file", |_| {});
    let symbols = renamed["changed_symbols"]["symbols"]
        .as_array()
        .expect("symbols");
    assert!(!symbols.is_empty());
    assert!(symbols.iter().any(|symbol| {
        ["base", "head"].iter().any(|side| {
            symbol[*side]["selector"]
                .as_str()
                .is_some_and(|s| s.contains("formatter.py"))
        })
    }));
    let cycle = report("cycle", |options| options.bounds.depth = 1);
    assert!(
        cycle["scope"]["truncated"]
            .as_array()
            .expect("truncation")
            .iter()
            .any(|flag| flag["bound"] == "depth" && flag["value"] == 1),
        "{cycle}"
    );
    assert!(
        cycle["outbound_paths"]
            .as_array()
            .is_some_and(|paths| !paths.is_empty())
    );
}

#[test]
fn pinned_reports_are_byte_identical() {
    let case = corpus::build_case("direct-call");
    let options = ReportOptions {
        generated_at: Some(GENERATED_AT.to_string()),
        ..ReportOptions::default()
    };
    let first =
        Comparison::open(&case.repository, case.base(), case.head()).expect("first comparison");
    let first =
        serde_json::to_string_pretty(&build_report(&first, &options).expect("first report"))
            .expect("serialize first");
    let second =
        Comparison::open(&case.repository, case.base(), case.head()).expect("second comparison");
    let second =
        serde_json::to_string_pretty(&build_report(&second, &options).expect("second report"))
            .expect("serialize second");
    assert_eq!(first, second);
}

#[test]
fn spaces_and_unicode_paths_survive_json_serialization() {
    let repository = tempfile::tempdir().expect("create repository");
    let repo = git2::Repository::init(repository.path()).expect("init repository");
    let path = "src/spa ce \u{fc}nicode.rs";
    let base = common::commit_files(
        &repo,
        repository.path(),
        &[(path, "pub fn helper() -> i32 {\n    1\n}\n")],
        "base",
        0,
    );
    let head = common::commit_files(
        &repo,
        repository.path(),
        &[(path, "pub fn helper() -> i32 {\n    2\n}\n")],
        "head",
        1,
    );
    drop(repo);
    let comparison = Comparison::open(repository.path(), &base, &head).expect("open comparison");
    let json = serde_json::to_string(
        &build_report(&comparison, &ReportOptions::default()).expect("build report"),
    )
    .expect("serialize report");
    assert!(json.contains(path), "path missing from JSON: {json}");
}

#[test]
fn absolute_host_path_is_opt_in() {
    let case = corpus::build_case("direct-call");
    let comparison =
        Comparison::open(&case.repository, case.base(), case.head()).expect("open comparison");
    let default = serde_json::to_string(
        &build_report(&comparison, &ReportOptions::default()).expect("default report"),
    )
    .expect("serialize default");
    assert!(!default.contains(case.repository.to_str().expect("utf8 path")));
    let options = ReportOptions {
        include_absolute_paths: true,
        ..ReportOptions::default()
    };
    let opted = serde_json::to_value(build_report(&comparison, &options).expect("opted-in report"))
        .expect("serialize opted-in");
    assert!(
        opted["comparison"]["repository"]
            .as_str()
            .unwrap_or_default()
            .contains(case.repository.to_str().expect("utf8 path"))
    );
}

#[test]
fn runtime_invocation_candidates_keep_their_disclosure() {
    let fixture = common::build_runtime_invocation_fixture();
    let comparison =
        Comparison::open(fixture.path(), &fixture.base, &fixture.head).expect("open comparison");
    let json = serde_json::to_value(
        build_report(&comparison, &ReportOptions::default()).expect("build report"),
    )
    .expect("serialize report");
    let row = json["candidate_tests"]["candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .find(|candidate| candidate["source"] == "runtime_invocation")
        .unwrap_or_else(|| panic!("missing runtime invocation: {json}"));
    assert_eq!(row["category"], "runtime_invocation");
    assert_eq!(row["label"], "runtime-invocation");
    assert!(
        row["note"]
            .as_str()
            .unwrap_or_default()
            .contains("by program name only")
    );
}
