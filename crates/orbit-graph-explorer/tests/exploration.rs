//! Bounded directed exploration, through the real `orbit-graph-explorer`
//! binary.
//!
//! Every test here launches the packaged executable with `serve`, drives it
//! over a real TCP socket, and asserts on what the shipped service puts on the
//! wire: multi-hop inbound evidence paths, entry-point classification, filter
//! explanations, and the snapshot cache.
//!
//! Two kinds of fixture appear. Cases from the shared corpus under
//! `crates/orbit-graph-cli/tests/fixtures/change-explorer/` cover the
//! behaviours the corpus was built for. Two situations the corpus does not
//! contain — a three-hop call chain with a `main` and a CLI command handler,
//! and a changed file the extractor has no grammar for — are built here
//! directly, with the same fixed identity and timestamps.

#![allow(clippy::expect_used)]

use std::fs;

use git2::Repository;
use serde_json::Value;
use tempfile::TempDir;

mod common;

use common::commit_files;
use common::http_service::{Service, percent_encode};

/// `symbol:src/lib.rs#helper:function`, percent-encoded.
const HELPER: &str = "symbol%3Asrc%2Flib.rs%23helper%3Afunction";

#[test]
fn evidence_paths_chain_multiple_hops_back_to_the_changed_symbol() {
    let (repository, base, head) = build_chain_fixture();
    let service = Service::launch_at(repository.path().to_path_buf(), base, head, Box::new(()));
    service.wait_until_ready();

    let evidence = service
        .authorized(
            "GET",
            format!("/api/evidence?selector={HELPER}&side=head&depth=3").as_str(),
            &[],
        )
        .json();
    assert_eq!(evidence["resolved"], true, "{evidence}");
    assert_eq!(evidence["query_options"]["depth"], 3, "{evidence}");
    assert_eq!(evidence["query_options"]["direction"], "inbound");
    assert_eq!(evidence["impact"]["direction"], "inbound");
    assert!(
        evidence["impact"]["visited_nodes"]
            .as_u64()
            .unwrap_or_default()
            > 1,
        "the core inbound impact query must reach past the direct caller: {evidence}"
    );

    let paths = evidence["paths"].as_array().expect("paths").clone();
    let reached: Vec<(String, u64)> = paths
        .iter()
        .map(|path| {
            (
                path["from"]["selector"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                path["distance"].as_u64().unwrap_or_default(),
            )
        })
        .collect();
    for (selector, distance) in [
        ("symbol:src/lib.rs#entry:function", 1),
        ("symbol:src/lib.rs#top:function", 2),
        ("symbol:src/main.rs#main:function", 3),
    ] {
        assert!(
            reached.contains(&(selector.to_string(), distance)),
            "`{selector}` must be reached at distance {distance}: {reached:?}"
        );
    }

    // Every edge of every chain carries the full contract, and the chain is
    // ordered from the affected symbol to the changed symbol.
    for path in &paths {
        let edges = path["edges"].as_array().expect("edges");
        assert_eq!(
            edges.len() as u64,
            path["distance"].as_u64().unwrap_or_default(),
            "{path}"
        );
        assert_eq!(
            edges[0]["from_selector"], path["from"]["selector"],
            "the first edge starts at the affected symbol: {path}"
        );
        assert_eq!(
            edges[edges.len() - 1]["to_selector"],
            "symbol:src/lib.rs#helper:function",
            "the last edge points at the changed symbol: {path}"
        );
        for edge in edges {
            for field in [
                "relationship",
                "category",
                "confidence",
                "snapshot",
                "commit_sha",
                "from_origin",
            ] {
                assert!(edge.get(field).is_some(), "edge missing `{field}`: {edge}");
            }
            assert_eq!(edge["snapshot"], "head", "{edge}");
            assert_eq!(edge["commit_sha"], service.head_sha.as_str(), "{edge}");
            assert!(edge["source"]["file"].is_string(), "{edge}");
        }
        // A path is only as strong as its weakest edge.
        assert!(path["category"].is_string(), "{path}");
    }

    // Depth is a bound, and reaching it is reported rather than presented as a
    // complete set.
    let shallow = service
        .authorized(
            "GET",
            format!("/api/evidence?selector={HELPER}&side=head&depth=1").as_str(),
            &[],
        )
        .json();
    assert_eq!(shallow["truncated"], true, "{shallow}");
    assert_eq!(shallow["truncated_by"], "depth", "{shallow}");
    let bound = &shallow["bounds_hit"][0];
    assert_eq!(bound["bound"], "depth", "{shallow}");
    assert_eq!(bound["value"], 1, "{shallow}");
}

#[test]
fn entry_points_report_the_rule_that_fired_and_the_shortest_path() {
    let (repository, base, head) = build_chain_fixture();
    let service = Service::launch_at(repository.path().to_path_buf(), base, head, Box::new(()));
    service.wait_until_ready();

    let report = service
        .authorized(
            "GET",
            format!("/api/entry-points?selector={HELPER}&side=head&depth=3").as_str(),
            &[],
        )
        .json();

    // The whole rule set is disclosed, so a reader can see what was applied.
    let rules: Vec<String> = report["rules"]
        .as_array()
        .expect("rules")
        .iter()
        .map(|rule| rule["id"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        rules,
        vec![
            "main_function",
            "cli_command_handler",
            "crate_root_public_item",
            "test_function"
        ],
        "{report}"
    );
    for rule in report["rules"].as_array().expect("rules") {
        assert!(
            !rule["description"].as_str().unwrap_or_default().is_empty(),
            "{rule}"
        );
    }

    let found: Vec<(String, String, u64)> = report["entry_points"]
        .as_array()
        .expect("entry points")
        .iter()
        .map(|entry| {
            (
                entry["node"]["selector"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                entry["rule"].as_str().unwrap_or_default().to_string(),
                entry["distance"].as_u64().unwrap_or_default(),
            )
        })
        .collect();

    assert!(
        found.contains(&(
            "symbol:src/main.rs#main:function".to_string(),
            "main_function".to_string(),
            3
        )),
        "{found:?}"
    );
    assert!(
        found.contains(&(
            "symbol:src/task.rs#add:function".to_string(),
            "cli_command_handler".to_string(),
            3
        )),
        "a command handler the core `command:` machinery resolves is an entry point: {found:?}"
    );
    assert!(
        found.contains(&(
            "symbol:tests/test_lib.rs#test_top:function".to_string(),
            "test_function".to_string(),
            3
        )),
        "{found:?}"
    );
    assert!(
        found.iter().any(
            |(selector, rule, _)| selector == "symbol:src/lib.rs#entry:function"
                && rule == "crate_root_public_item"
        ),
        "{found:?}"
    );

    for entry in report["entry_points"].as_array().expect("entry points") {
        assert!(
            !entry["rule_description"]
                .as_str()
                .unwrap_or_default()
                .is_empty(),
            "{entry}"
        );
        let edges = entry["path"]["edges"].as_array().expect("edges");
        assert_eq!(
            edges.len() as u64,
            entry["distance"].as_u64().unwrap_or_default(),
            "the reported path is the shortest one: {entry}"
        );
        if entry["rule"] == "cli_command_handler" {
            let note = entry["note"].as_str().unwrap_or_default();
            assert!(note.contains("command:task add"), "{entry}");
        }
    }

    // Truncation is reported here too, from the same traversal.
    let bounded = service
        .authorized(
            "GET",
            format!("/api/entry-points?selector={HELPER}&side=head&depth=1").as_str(),
            &[],
        )
        .json();
    assert_eq!(bounded["truncated"], true, "{bounded}");
    assert_eq!(bounded["truncated_by"], "depth", "{bounded}");
}

#[test]
fn a_changed_test_is_classified_as_a_test_entry_point() {
    let service = Service::launch("changed-test");
    service.wait_until_ready();

    let selector = percent_encode("symbol:tests/add_test.rs#test_add:function");
    let report = service
        .authorized(
            "GET",
            format!("/api/entry-points?selector={selector}&side=head").as_str(),
            &[],
        )
        .json();
    let entry = report["entry_points"]
        .as_array()
        .expect("entry points")
        .iter()
        .find(|entry| entry["node"]["selector"] == "symbol:tests/add_test.rs#test_add:function")
        .unwrap_or_else(|| panic!("the changed test must classify as an entry point: {report}"));
    assert_eq!(entry["rule"], "test_function", "{entry}");
    assert_eq!(entry["distance"], 0, "{entry}");
    assert!(
        entry["path"]["edges"].as_array().expect("edges").is_empty(),
        "the queried symbol is its own entry point with no intermediate edge: {entry}"
    );
}

#[test]
fn removed_symbol_evidence_is_base_only_and_head_says_so() {
    let service = Service::launch("removed-symbol");
    service.wait_until_ready();

    let selector = percent_encode("symbol:src/lib.rs#old_helper:function");
    let base = service
        .authorized(
            "GET",
            format!("/api/evidence?selector={selector}&side=base").as_str(),
            &[],
        )
        .json();
    assert_eq!(base["resolved"], true, "{base}");
    assert_eq!(base["change_status"], "removed", "{base}");
    assert_eq!(
        base["evidence_sides"],
        serde_json::json!(["base"]),
        "{base}"
    );
    assert!(
        !base["paths"].as_array().expect("paths").is_empty(),
        "the base snapshot still carries the caller: {base}"
    );
    for path in base["paths"].as_array().expect("paths") {
        assert_eq!(path["edges"][0]["snapshot"], "base", "{path}");
        assert_eq!(
            path["edges"][0]["commit_sha"],
            service.base_sha.as_str(),
            "{path}"
        );
    }

    // The head side is absent, and says so about that revision only rather than
    // returning an empty caller set that reads as "nothing calls it".
    let head = service.authorized(
        "GET",
        format!("/api/evidence?selector={selector}&side=head").as_str(),
        &[],
    );
    assert_eq!(head.status, 404, "{head:?}");
    let body = head.json();
    assert_eq!(body["error"]["code"], "not_in_snapshot", "{body}");
    assert_eq!(
        body["evidence_sides"],
        serde_json::json!(["base"]),
        "{body}"
    );

    let entry_points = service.authorized(
        "GET",
        format!("/api/entry-points?selector={selector}&side=head").as_str(),
        &[],
    );
    assert_eq!(entry_points.status, 404, "{entry_points:?}");
}

#[test]
fn a_cycle_terminates_and_a_small_node_cap_is_reported() {
    let service = Service::launch("cycle");
    service.wait_until_ready();

    let selector = percent_encode("symbol:src/lib.rs#is_even:function");
    let evidence = service
        .authorized(
            "GET",
            format!("/api/evidence?selector={selector}&side=head&depth=5").as_str(),
            &[],
        )
        .json();

    let paths = evidence["paths"].as_array().expect("paths");
    assert!(!paths.is_empty(), "{evidence}");
    for path in paths {
        // No node may repeat inside one path: that is what makes a mutually
        // recursive call graph terminate.
        let mut seen = vec![
            path["to"]["selector"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        ];
        for edge in path["edges"].as_array().expect("edges") {
            let node = edge["from_selector"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert!(!seen.contains(&node), "repeated node in {path}");
            seen.push(node);
        }
    }
    assert!(
        evidence["cycles_pruned"].as_u64().unwrap_or_default() > 0,
        "the mutual recursion must be pruned rather than walked: {evidence}"
    );

    // A deliberately tiny cap stops the traversal and names the bound.
    let capped = Service::launch_case_with("cycle", &["--node-cap", "1"]);
    capped.wait_until_ready();
    let bounded = capped
        .authorized(
            "GET",
            format!("/api/evidence?selector={selector}&side=head&depth=5").as_str(),
            &[],
        )
        .json();
    assert_eq!(bounded["query_options"]["node_cap"], 1, "{bounded}");
    assert_eq!(bounded["truncated"], true, "{bounded}");
    assert_eq!(bounded["truncated_by"], "impact_node_cap", "{bounded}");
    assert_eq!(bounded["bounds_hit"][0]["value"], 1, "{bounded}");
    assert_eq!(
        bounded["paths"].as_array().map(Vec::len),
        Some(1),
        "{bounded}"
    );
    assert!(
        bounded["no_path_reasons"]
            .as_array()
            .is_none_or(|reasons| reasons.is_empty()),
        "a truncated result still has paths: {bounded}"
    );
}

#[test]
fn an_exhausted_time_budget_is_reported_as_the_bound_it_is() {
    let service = Service::launch_case_with("direct-call", &["--time-budget-ms", "0"]);
    service.wait_until_ready();

    let evidence = service
        .authorized(
            "GET",
            format!("/api/evidence?selector={HELPER}&side=head&depth=3").as_str(),
            &[],
        )
        .json();
    assert_eq!(evidence["query_options"]["time_budget_ms"], 0, "{evidence}");
    assert_eq!(evidence["truncated"], true, "{evidence}");
    assert_eq!(evidence["truncated_by"], "time_budget", "{evidence}");
    assert_eq!(evidence["bounds_hit"][0]["value"], 0, "{evidence}");
    assert!(
        evidence["paths"].as_array().expect("paths").is_empty(),
        "{evidence}"
    );
    // An empty result under a bound is never presented as "no callers": the
    // reasons name the bound.
    let reasons = evidence["no_path_reasons"]
        .as_array()
        .expect("no path reasons")
        .iter()
        .map(|reason| reason.as_str().unwrap_or_default().to_string())
        .collect::<Vec<String>>()
        .join(" ");
    assert!(reasons.contains("time_budget"), "{reasons}");
}

#[test]
fn same_name_symbols_in_different_modules_get_distinct_paths() {
    let service = Service::launch("ambiguous-same-name");
    service.wait_until_ready();

    for (module, caller, line) in [("a", "call_a", 5), ("b", "call_b", 9)] {
        let selector = percent_encode(format!("symbol:src/{module}.rs#run:function").as_str());
        let evidence = service
            .authorized(
                "GET",
                format!("/api/evidence?selector={selector}&side=head&depth=3").as_str(),
                &[],
            )
            .json();
        let froms: Vec<String> = evidence["paths"]
            .as_array()
            .expect("paths")
            .iter()
            .map(|path| {
                path["from"]["selector"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        assert_eq!(
            froms,
            vec![format!("symbol:src/lib.rs#{caller}:function")],
            "`{module}::run` must attract only its own caller: {evidence}"
        );
        assert_eq!(evidence["paths"][0]["edges"][0]["source"]["line"], line);
    }
}

#[test]
fn an_unsupported_changed_file_is_out_of_scope_alongside_no_path() {
    let (repository, base, head) = build_unsupported_fixture();
    let service = Service::launch_at(repository.path().to_path_buf(), base, head, Box::new(()));
    service.wait_until_ready();

    let changed = service
        .authorized("GET", "/api/changed-symbols", &[])
        .json();
    let out_of_scope = changed["out_of_scope"].as_array().expect("out of scope");
    assert!(
        out_of_scope
            .iter()
            .any(|entry| entry["path"] == "schema.proto"
                && entry["reason"] == "unsupported_language"),
        "a changed file with no grammar is out of scope, never removed: {changed}"
    );

    // The symbol the unsupported file textually mentions has no path, and the
    // reasons say why rather than implying nothing references it.
    let evidence = service
        .authorized(
            "GET",
            format!("/api/evidence?selector={HELPER}&side=head&depth=3").as_str(),
            &[],
        )
        .json();
    assert!(
        evidence["paths"].as_array().expect("paths").is_empty(),
        "{evidence}"
    );
    let reasons = evidence["no_path_reasons"]
        .as_array()
        .expect("no path reasons")
        .iter()
        .map(|reason| reason.as_str().unwrap_or_default().to_string())
        .collect::<Vec<String>>()
        .join(" ");
    assert!(reasons.contains("syntax-driven"), "{reasons}");
}

#[test]
fn filters_explain_every_item_they_remove() {
    let service = Service::launch("direct-call");
    service.wait_until_ready();

    // A language filter that matches nothing removes the changed symbol and
    // says so, rather than rendering an empty list.
    let changed = service
        .authorized("GET", "/api/changed-symbols?language=python", &[])
        .json();
    assert!(
        changed["symbols"].as_array().expect("symbols").is_empty(),
        "{changed}"
    );
    let filtered = changed["filtered_out"].as_array().expect("filtered out");
    assert_eq!(filtered.len(), 1, "{changed}");
    assert_eq!(filtered[0]["reason"], "language", "{changed}");
    assert_eq!(filtered[0]["count"], 1, "{changed}");
    assert_eq!(
        filtered[0]["examples"][0], "symbol:src/lib.rs#helper:function",
        "{changed}"
    );
    assert!(
        !filtered[0]["explanation"]
            .as_str()
            .unwrap_or_default()
            .is_empty(),
        "{changed}"
    );
    // Filters that cannot apply to a change list are declared, not ignored.
    let inapplicable: Vec<String> = changed["inapplicable_filters"]
        .as_array()
        .expect("inapplicable filters")
        .iter()
        .map(|entry| entry["filter"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(inapplicable, vec!["confidence", "depth"], "{changed}");

    // A change-kind filter that does match keeps the entry.
    let kept = service
        .authorized("GET", "/api/changed-symbols?change_kind=modified", &[])
        .json();
    assert_eq!(kept["symbols"].as_array().map(Vec::len), Some(1), "{kept}");
    assert!(
        kept["filtered_out"]
            .as_array()
            .expect("filtered out")
            .is_empty(),
        "{kept}"
    );

    // The same explanations apply to evidence: scoping to `src` removes the
    // test caller and names it.
    let scoped = service
        .authorized(
            "GET",
            format!("/api/evidence?selector={HELPER}&side=head&scope=src").as_str(),
            &[],
        )
        .json();
    assert_eq!(scoped["query_options"]["scope"], "src", "{scoped}");
    for path in scoped["paths"].as_array().expect("paths") {
        assert!(
            path["edges"][0]["source"]["file"]
                .as_str()
                .unwrap_or_default()
                .starts_with("src/"),
            "{path}"
        );
    }
    let removed = scoped["filtered_out"].as_array().expect("filtered out");
    assert!(
        removed
            .iter()
            .any(|entry| entry["reason"] == "scope" && entry["count"].as_u64() == Some(1)),
        "{scoped}"
    );
    assert!(
        removed.iter().all(|entry| entry["examples"]
            .as_array()
            .is_none_or(|examples| examples.len() <= 5)),
        "{scoped}"
    );

    // A change-kind filter on evidence keeps only callers that themselves
    // changed; in this fixture no caller did.
    let by_kind = service
        .authorized(
            "GET",
            format!("/api/evidence?selector={HELPER}&side=head&change_kind=added").as_str(),
            &[],
        )
        .json();
    assert!(
        by_kind["paths"].as_array().expect("paths").is_empty(),
        "{by_kind}"
    );
    assert!(
        by_kind["filtered_out"]
            .as_array()
            .expect("filtered out")
            .iter()
            .any(|entry| entry["reason"] == "change_kind"),
        "{by_kind}"
    );
}

#[test]
fn a_second_launch_hits_the_cache_and_a_stale_key_rebuilds_it() {
    let cache = TempDir::new().expect("create cache directory");
    let cache_dir = cache.path().to_string_lossy().into_owned();
    let (repository, base, head) = build_chain_fixture();
    let repository_path = repository.path().to_path_buf();

    let cold = Service::launch_at_with(
        repository_path.clone(),
        base.clone(),
        head.clone(),
        Box::new(()),
        &["--cache-dir", cache_dir.as_str()],
    );
    cold.wait_until_ready();
    let first = cold.authorized("GET", "/api/comparison", &[]).json();
    assert_eq!(cache_outcomes(&first), vec!["miss", "miss"], "{first}");
    assert_eq!(
        first["cache"]["directory"].as_str(),
        Some(
            fs::canonicalize(cache.path())
                .unwrap_or_else(|_| cache.path().to_path_buf())
                .to_string_lossy()
                .into_owned()
                .as_str()
        ),
        "{first}"
    );
    // The user repository's own graph databases are never written.
    assert!(
        !repository_path.join(".orbit-graph").exists(),
        "an explicit cache directory keeps everything out of the repository"
    );
    drop(cold);

    let warm = Service::launch_at_with(
        repository_path.clone(),
        base.clone(),
        head.clone(),
        Box::new(()),
        &["--cache-dir", cache_dir.as_str()],
    );
    warm.wait_until_ready();
    let second = warm.authorized("GET", "/api/comparison", &[]).json();
    assert_eq!(cache_outcomes(&second), vec!["hit", "hit"], "{second}");
    // A warm launch serves the same evidence, from the same index identity.
    assert_eq!(
        second["snapshots"][1]["files_indexed"], first["snapshots"][1]["files_indexed"],
        "{second}"
    );
    assert_eq!(
        second["snapshots"][1]["index_identity"], first["snapshots"][1]["index_identity"],
        "{second}"
    );
    let warm_evidence = warm
        .authorized(
            "GET",
            format!("/api/evidence?selector={HELPER}&side=head&depth=3").as_str(),
            &[],
        )
        .json();
    assert!(
        !warm_evidence["paths"].as_array().expect("paths").is_empty(),
        "{warm_evidence}"
    );
    drop(warm);

    // Forge a stale key on the head entry: a mismatched extractor version must
    // rebuild, never be reused with a warning.
    let entry = cache.path().join(head.as_str()).join("entry.json");
    let mut metadata: Value =
        serde_json::from_slice(fs::read(entry.as_path()).expect("read entry").as_slice())
            .expect("parse entry");
    let forged = metadata["extractor_version"].as_u64().unwrap_or_default() + 1;
    metadata["extractor_version"] = Value::from(forged);
    fs::write(
        entry.as_path(),
        serde_json::to_vec(&metadata).expect("serialize entry"),
    )
    .expect("write forged entry");

    let rebuilt = Service::launch_at_with(
        repository_path,
        base,
        head.clone(),
        Box::new(()),
        &["--cache-dir", cache_dir.as_str()],
    );
    rebuilt.wait_until_ready();
    let third = rebuilt.authorized("GET", "/api/comparison", &[]).json();
    assert_eq!(
        cache_outcomes(&third),
        vec!["hit", "miss"],
        "a stale key rebuilds its own entry and leaves the valid one alone: {third}"
    );
    let restored: Value =
        serde_json::from_slice(fs::read(entry.as_path()).expect("read entry").as_slice())
            .expect("parse entry");
    assert_eq!(
        restored["extractor_version"], first["snapshots"][1]["index_identity"]["extractor_version"],
        "the rebuilt entry carries this binary's key: {restored}"
    );
}

#[test]
fn clean_removes_only_stale_and_unreferenced_cache_entries() {
    let cache = TempDir::new().expect("create cache directory");
    let cache_dir = cache.path().to_string_lossy().into_owned();
    let (repository, base, head) = build_chain_fixture();

    let service = Service::launch_at_with(
        repository.path().to_path_buf(),
        base.clone(),
        head.clone(),
        Box::new(()),
        &["--cache-dir", cache_dir.as_str()],
    );
    service.wait_until_ready();
    let _ = service.authorized("GET", "/api/comparison", &[]);
    drop(service);

    // An entry for a commit this repository does not have (with the current
    // key, so it is unreferenced rather than stale), and a file that is not a
    // cache entry at all.
    let unreferenced = "f".repeat(40);
    let entry_root = cache.path().join(unreferenced.as_str());
    fs::create_dir_all(entry_root.join("tree")).expect("create entry");
    fs::write(
        entry_root.join("entry.json"),
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "commit_sha": unreferenced,
            "extractor_version": orbit_graph::EXTRACTOR_VERSION,
            "store_schema_version": orbit_graph::STORE_SCHEMA_VERSION,
            "build": {"files_written": 0, "bytes_written": 0, "excluded": [], "files_indexed": 0},
            "published_at": 0,
        }))
        .expect("serialize entry"),
    )
    .expect("write entry");
    let foreign = cache.path().join("README.txt");
    fs::write(foreign.as_path(), b"not a cache entry\n").expect("write foreign file");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_orbit-graph-explorer"))
        .args([
            "clean",
            "--repo",
            repository.path().to_str().expect("utf8 path"),
            "--cache-dir",
            cache_dir.as_str(),
        ])
        .output()
        .expect("run clean");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(output.stdout.as_slice()).into_owned();

    assert!(
        stdout.contains("removed\tunreferenced_commit\t"),
        "{stdout}"
    );
    assert!(!entry_root.exists(), "an unreferenced entry is removed");
    assert!(
        cache.path().join(head.as_str()).exists(),
        "a current entry is kept: {stdout}"
    );
    assert!(
        foreign.exists(),
        "nothing that is not a cache entry is removed: {stdout}"
    );
    assert!(stdout.contains("kept\tcurrent\t"), "{stdout}");
}

fn cache_outcomes(comparison: &Value) -> Vec<String> {
    comparison["snapshots"]
        .as_array()
        .expect("snapshots")
        .iter()
        .map(|snapshot| snapshot["cache"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// A repository whose head changes `helper`, three call hops below `main`.
///
/// `src/task.rs` carries a clap subcommand whose handler calls `top`, so the
/// command-handler rule has something real to resolve.
fn build_chain_fixture() -> (TempDir, String, String) {
    let lib = |value: &str| {
        format!(
            "pub fn helper() -> i32 {{\n    {value}\n}}\n\npub fn entry() -> i32 {{\n    \
             helper() + 1\n}}\n\npub fn top() -> i32 {{\n    entry() + 1\n}}\n"
        )
    };
    let task = "use clap::Subcommand;\n\n#[derive(Subcommand)]\nenum TaskSubcommand {\n    \
                Add(AddArgs),\n}\n\npub struct AddArgs;\n\nfn dispatch(command: TaskSubcommand) \
                -> i32 {\n    match command {\n        TaskSubcommand::Add(args) => \
                add(args),\n    }\n}\n\npub fn add(_args: AddArgs) -> i32 {\n    top()\n}\n";
    let main = "mod lib;\nmod task;\n\nfn main() {\n    let _ = top();\n}\n";
    let test = "fn test_top() {\n    let _ = top();\n}\n";

    let dir = TempDir::new().expect("create fixture repository");
    let repo = Repository::init(dir.path()).expect("init fixture repository");
    let base = commit_files(
        &repo,
        dir.path(),
        &[
            ("src/lib.rs", lib("1").as_str()),
            ("src/task.rs", task),
            ("src/main.rs", main),
            ("tests/test_lib.rs", test),
        ],
        "base: a three-hop chain into helper",
        0,
    );
    let head = commit_files(
        &repo,
        dir.path(),
        &[("src/lib.rs", lib("2").as_str())],
        "head: change the helper body",
        1,
    );
    drop(repo);
    (dir, base, head)
}

/// A repository whose head edits a file the extractor has no grammar for.
fn build_unsupported_fixture() -> (TempDir, String, String) {
    let dir = TempDir::new().expect("create fixture repository");
    let repo = Repository::init(dir.path()).expect("init fixture repository");
    let lib = |value: &str| format!("pub fn helper() -> i32 {{\n    {value}\n}}\n");
    let base = commit_files(
        &repo,
        dir.path(),
        &[
            ("src/lib.rs", lib("1").as_str()),
            (
                "schema.proto",
                "// generated reference to helper\nmessage Config {}\n",
            ),
        ],
        "base: an unsupported file mentioning helper",
        0,
    );
    let head = commit_files(
        &repo,
        dir.path(),
        &[
            ("src/lib.rs", lib("2").as_str()),
            (
                "schema.proto",
                "// generated reference to helper\nmessage Config { int32 id = 1; }\n",
            ),
        ],
        "head: edit both files",
        1,
    );
    drop(repo);
    (dir, base, head)
}
