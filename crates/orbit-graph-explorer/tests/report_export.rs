//! Real-binary tests for `orbit-graph-explorer report`.
//!
//! Every test here runs the packaged `orbit-graph-explorer` executable, the
//! same way `tests/cli_smoke.rs` does, and reads the two files it writes.
//! Coverage follows the task's acceptance criteria: schema fields present,
//! truncation carried through, embedded/reference marking per excerpt mode,
//! byte-identical determinism across two runs, HTML escaping with no
//! `<script>`, and `--force` semantics.

#![allow(clippy::expect_used)]

use std::path::Path;
use std::process::{Command, Output};

mod common;

use common::corpus;

/// Pinned so two runs of the same inputs are byte-identical.
const GENERATED_AT: &str = "2024-01-01T00:00:00Z";

#[test]
fn direct_call_report_matches_the_exported_contract() {
    let case = corpus::build_case("direct-call");
    let out = tempfile::tempdir().expect("create output directory");

    let output = run_report(&case, out.path(), "report", &[]);
    assert!(output.status.success(), "{output:?}");

    let json = read_json(out.path(), "report");
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["generated_at"], GENERATED_AT);
    assert_eq!(json["comparison"]["base"]["commit_sha"], case.base());
    assert_eq!(json["comparison"]["head"]["commit_sha"], case.head());
    assert_eq!(json["comparison"]["mode"], "direct_base_head");
    assert_eq!(
        json["comparison"]["effective_base_sha"],
        case.base(),
        "direct mode never rewrites the stated base"
    );
    // No absolute host path is emitted by default.
    assert!(json["comparison"]["repository"].is_null(), "{json}");

    assert!(
        json["index_identity"]["extractor_version"]
            .as_u64()
            .unwrap_or_default()
            > 0
    );
    assert_eq!(json["index_identity"]["store_schema_version"], 1);
    assert!(
        json["index_identity"]["base_index_identity"]
            .as_str()
            .unwrap_or_default()
            .starts_with("blake3:")
    );
    assert!(
        json["index_identity"]["head_index_identity"]
            .as_str()
            .unwrap_or_default()
            .starts_with("blake3:")
    );

    assert_eq!(json["query_options"]["excerpts"], "controlled");
    assert_eq!(json["query_options"]["min_confidence"], "same_module");

    let symbols = json["changed_symbols"]["symbols"]
        .as_array()
        .expect("symbols array");
    assert_eq!(symbols.len(), 1, "{json}");
    assert_eq!(symbols[0]["status"], "modified");
    assert!(json["changed_symbols"]["out_of_scope"].is_array());

    assert!(json["evidence_paths"].is_array());
    assert!(
        !json["evidence_paths"]
            .as_array()
            .expect("evidence_paths array")
            .is_empty(),
        "{json}"
    );
    assert!(json["entry_points"].is_array());
    assert!(json["candidate_tests"]["candidates"].is_array());
    assert!(json["unresolved"].is_array());
    assert!(json["scope"]["truncated"].is_array());
    assert!(json["scope"]["unsupported"].is_array());
    assert!(json["scope"]["excluded"].is_array());
    assert_eq!(json["source_rendering"], "excerpt");

    // Controlled mode over a same-file evidence edge resolves to an embedded
    // excerpt, since the edge's source line is known and the file is
    // readable in the snapshot.
    let embedded = json["evidence_paths"]
        .as_array()
        .expect("evidence_paths array")
        .iter()
        .flat_map(|path| path["edges"].as_array().expect("edges array").iter())
        .any(|edge| edge["rendering"] == "embedded" && edge["excerpt"]["text"].is_string());
    assert!(embedded, "{json}");

    let html = read_html(out.path(), "report");
    assert!(!html.contains("<script"), "{html}");
    assert!(html.contains("Change report"), "{html}");
    assert!(html.contains("embedded"), "{html}");
}

#[test]
fn removed_symbol_source_is_embedded_from_the_base_snapshot() {
    let case = corpus::build_case("removed-symbol");
    let out = tempfile::tempdir().expect("create output directory");
    let output = run_report(&case, out.path(), "report", &[]);
    assert!(output.status.success(), "{output:?}");

    let json = read_json(out.path(), "report");
    let removed = json["changed_symbols"]["symbols"]
        .as_array()
        .expect("symbols")
        .iter()
        .find(|symbol| symbol["status"] == "removed")
        .unwrap_or_else(|| panic!("no removed symbol in {json}"))
        .clone();
    assert!(removed["head"].is_null(), "{removed}");
    let base = &removed["base"];
    assert_eq!(base["snapshot"], "base");
    assert_eq!(base["commit_sha"], case.base());
    // The removed symbol's own declaration is embedded from the base
    // snapshot: reporting on a removed symbol must not require the head
    // snapshot, which does not have it.
    assert_eq!(base["rendering"], "embedded", "{removed}");
    assert!(
        base["excerpt"]["text"]
            .as_str()
            .unwrap_or_default()
            .contains("old_helper"),
        "{removed}"
    );
}

#[test]
fn excerpt_none_mode_never_embeds_source_and_full_span_embeds_the_whole_file() {
    let case = corpus::build_case("direct-call");

    let none_out = tempfile::tempdir().expect("create output directory");
    let none_output = run_report(&case, none_out.path(), "report", &["--excerpts", "none"]);
    assert!(none_output.status.success(), "{none_output:?}");
    let none_json = read_json(none_out.path(), "report");
    assert_eq!(none_json["source_rendering"], "reference");
    for path in none_json["evidence_paths"]
        .as_array()
        .expect("evidence_paths array")
    {
        for edge in path["edges"].as_array().expect("edges array") {
            assert_eq!(edge["rendering"], "reference", "{edge}");
            assert!(edge["reference"].as_str().unwrap_or_default().contains('@'));
        }
    }

    let full_out = tempfile::tempdir().expect("create output directory");
    let full_output = run_report(
        &case,
        full_out.path(),
        "report",
        &["--excerpts", "full-span"],
    );
    assert!(full_output.status.success(), "{full_output:?}");
    let full_json = read_json(full_out.path(), "report");
    assert_eq!(full_json["source_rendering"], "excerpt");
    let mut saw_embedded_edge = false;
    for path in full_json["evidence_paths"]
        .as_array()
        .expect("evidence_paths array")
    {
        for edge in path["edges"].as_array().expect("edges array") {
            if edge["rendering"] == "embedded" {
                saw_embedded_edge = true;
                // A full-span excerpt starts at line 1 of the bounded file
                // read, not at a window around the cited line.
                assert_eq!(edge["excerpt"]["start_line"], 1, "{edge}");
            }
        }
    }
    assert!(saw_embedded_edge, "{full_json}");
}

#[test]
fn renamed_file_round_trips_moved_and_renamed_paths() {
    let case = corpus::build_case("renamed-file");
    let out = tempfile::tempdir().expect("create output directory");
    let output = run_report(&case, out.path(), "report", &[]);
    assert!(output.status.success(), "{output:?}");

    let json = read_json(out.path(), "report");
    let symbols = json["changed_symbols"]["symbols"]
        .as_array()
        .expect("symbols");
    assert!(!symbols.is_empty(), "{json}");

    // Every occurrence's selector round-trips its exact path, including the
    // renamed `formatting/formatter.py` and `helpers.py` paths, both in JSON
    // and in the escaped HTML.
    let html = read_html(out.path(), "report");
    for symbol in symbols {
        for side in ["base", "head"] {
            if let Some(selector) = symbol[side]["selector"].as_str() {
                assert!(
                    selector.contains("formatter.py") || selector.contains("helper"),
                    "{selector}"
                );
                assert!(
                    html.contains(selector),
                    "selector {selector} missing from HTML"
                );
            }
        }
    }
    assert!(!html.contains("<script"), "{html}");
}

#[test]
fn cycle_with_a_small_depth_cap_reports_truncation() {
    let case = corpus::build_case("cycle");
    let out = tempfile::tempdir().expect("create output directory");
    // `is_even` and `is_odd` call each other; a depth of 1 stops the
    // traversal before the mutual edge is re-expanded, which is exactly the
    // bound this test exercises.
    let output = run_report(&case, out.path(), "report", &["--depth", "1"]);
    assert!(output.status.success(), "{output:?}");

    let json = read_json(out.path(), "report");
    let truncated = json["scope"]["truncated"]
        .as_array()
        .expect("truncated array");
    assert!(!truncated.is_empty(), "{json}");
    assert!(
        truncated
            .iter()
            .any(|flag| flag["bound"] == "depth" && flag["value"] == 1),
        "{json}"
    );

    let html = read_html(out.path(), "report");
    assert!(html.contains("truncated"), "{html}");
}

#[test]
fn two_runs_with_the_same_inputs_are_byte_identical() {
    let case = corpus::build_case("direct-call");
    let first_out = tempfile::tempdir().expect("create output directory");
    let second_out = tempfile::tempdir().expect("create output directory");

    let first = run_report(&case, first_out.path(), "report", &[]);
    assert!(first.status.success(), "{first:?}");
    let second = run_report(&case, second_out.path(), "report", &[]);
    assert!(second.status.success(), "{second:?}");

    let first_json = std::fs::read(first_out.path().join("report.json")).expect("read first json");
    let second_json =
        std::fs::read(second_out.path().join("report.json")).expect("read second json");
    assert_eq!(
        first_json, second_json,
        "JSON must be byte-identical across runs"
    );

    let first_html = std::fs::read(first_out.path().join("report.html")).expect("read first html");
    let second_html =
        std::fs::read(second_out.path().join("report.html")).expect("read second html");
    assert_eq!(
        first_html, second_html,
        "HTML must be byte-identical across runs"
    );
}

#[test]
fn html_escapes_a_source_line_with_markup_characters_and_never_contains_script() {
    let repository = tempfile::tempdir().expect("create repository directory");
    let repo = git2::Repository::init(repository.path()).expect("init repository");
    let base = common::commit_files(
        &repo,
        repository.path(),
        &[("src/lib.rs", "pub fn entry() -> i32 {\n    7\n}\n")],
        "base",
        0,
    );
    let head = common::commit_files(
        &repo,
        repository.path(),
        &[(
            "src/lib.rs",
            "pub fn entry() -> i32 {\n    // <b>&\"'\n    8\n}\n",
        )],
        "head",
        1,
    );
    drop(repo);

    let out = tempfile::tempdir().expect("create output directory");
    let output = Command::new(env!("CARGO_BIN_EXE_orbit-graph-explorer"))
        .args([
            "report",
            "--repo",
            repository.path().to_str().expect("utf8 path"),
            "--base",
            base.as_str(),
            "--head",
            head.as_str(),
            "--out",
            out.path().to_str().expect("utf8 path"),
            "--name",
            "report",
            "--excerpts",
            "full-span",
            "--generated-at",
            GENERATED_AT,
        ])
        .output()
        .expect("run orbit-graph-explorer report");
    assert!(output.status.success(), "{output:?}");

    let html = read_html(out.path(), "report");
    assert!(!html.contains("<script"), "{html}");
    assert!(
        !html.contains("<b>&\"'"),
        "raw markup leaked into HTML: {html}"
    );
    assert!(
        html.contains("&lt;b&gt;&amp;&quot;&#39;"),
        "escaped source line missing: {html}"
    );
}

#[test]
fn spaces_and_unicode_file_names_round_trip_in_both_formats() {
    let repository = tempfile::tempdir().expect("create repository directory");
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

    let out = tempfile::tempdir().expect("create output directory");
    let output = Command::new(env!("CARGO_BIN_EXE_orbit-graph-explorer"))
        .args([
            "report",
            "--repo",
            repository.path().to_str().expect("utf8 path"),
            "--base",
            base.as_str(),
            "--head",
            head.as_str(),
            "--out",
            out.path().to_str().expect("utf8 path"),
            "--name",
            "report",
            "--generated-at",
            GENERATED_AT,
        ])
        .output()
        .expect("run orbit-graph-explorer report");
    assert!(output.status.success(), "{output:?}");

    let json = read_json(out.path(), "report");
    let text = serde_json::to_string(&json).expect("serialize json");
    assert!(text.contains(path), "path missing from JSON: {text}");

    let html = read_html(out.path(), "report");
    assert!(html.contains(path), "path missing from HTML: {html}");
}

#[test]
fn report_refuses_to_overwrite_without_force() {
    let case = corpus::build_case("direct-call");
    let out = tempfile::tempdir().expect("create output directory");

    let first = run_report(&case, out.path(), "report", &[]);
    assert!(first.status.success(), "{first:?}");

    let second = run_report(&case, out.path(), "report", &[]);
    assert!(!second.status.success(), "{second:?}");
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(stderr.contains("already exists"), "{stderr}");
    assert!(stderr.contains("--force"), "{stderr}");

    let forced = run_report(&case, out.path(), "report", &["--force"]);
    assert!(forced.status.success(), "{forced:?}");
}

#[test]
fn no_absolute_host_path_unless_opted_in() {
    let case = corpus::build_case("direct-call");
    let repository = case
        .repository
        .to_str()
        .expect("utf8 repository path")
        .to_string();

    let default_out = tempfile::tempdir().expect("create output directory");
    let default_output = run_report(&case, default_out.path(), "report", &[]);
    assert!(default_output.status.success(), "{default_output:?}");
    let default_json =
        std::fs::read_to_string(default_out.path().join("report.json")).expect("read report.json");
    assert!(
        !default_json.contains(repository.as_str()),
        "absolute repository path leaked by default: {default_json}"
    );

    let opted_out = tempfile::tempdir().expect("create output directory");
    let opted_output = run_report(
        &case,
        opted_out.path(),
        "report",
        &["--include-absolute-paths"],
    );
    assert!(opted_output.status.success(), "{opted_output:?}");
    let opted_json = read_json(opted_out.path(), "report");
    assert!(
        opted_json["comparison"]["repository"]
            .as_str()
            .unwrap_or_default()
            .contains(repository.as_str()),
        "{opted_json}"
    );
}

fn run_report(case: &corpus::CorpusCase, out: &Path, name: &str, extra: &[&str]) -> Output {
    let mut args: Vec<&str> = vec![
        "report",
        "--repo",
        case.repository.to_str().expect("utf8 repository path"),
        "--base",
        case.base(),
        "--head",
        case.head(),
        "--out",
        out.to_str().expect("utf8 output path"),
        "--name",
        name,
        "--generated-at",
        GENERATED_AT,
    ];
    args.extend_from_slice(extra);
    Command::new(env!("CARGO_BIN_EXE_orbit-graph-explorer"))
        .args(args.as_slice())
        .output()
        .expect("run orbit-graph-explorer report")
}

fn read_json(out: &Path, name: &str) -> serde_json::Value {
    let bytes = std::fs::read(out.join(format!("{name}.json"))).expect("read report json");
    serde_json::from_slice(bytes.as_slice()).expect("parse report json")
}

fn read_html(out: &Path, name: &str) -> String {
    std::fs::read_to_string(out.join(format!("{name}.html"))).expect("read report html")
}
