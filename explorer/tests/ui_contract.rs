//! Real-binary coverage for the embedded three-pane UI shell.
//!
//! Three things are asserted here, all against the packaged
//! `orbit-graph-explorer` binary rather than an in-process handler:
//!
//! - `GET /` and its two static assets serve the embedded shell with the
//!   correct `Content-Type` and a restrictive CSP, without requiring the
//!   per-launch bearer token (a plain navigation or `<link>`/`<script>` fetch
//!   cannot attach one), and without any repository-derived content.
//! - The JSON field names `explorer/ui/app.js` depends on — its data
//!   contract, restated in the module doc comment there — are present in the
//!   real service's serializers for the `direct-call` and `removed-symbol`
//!   fixtures.
//! - A selector naming a file with a space and a Unicode character round-trips
//!   through the API's percent-encoded query strings.

#![allow(clippy::expect_used)]

use std::fs;
use std::path::Path;

use git2::{Repository, Signature, Time};
use serde_json::Value;
use tempfile::TempDir;

mod common;

use common::http_service::{Service, percent_encode};

const APP_CSS: &str = include_str!("../ui/app.css");
const APP_JS: &str = include_str!("../ui/app.js");

#[test]
fn the_shell_is_served_without_a_token_and_carries_no_repository_content() {
    let service = Service::launch("direct-call");

    // No Authorization header at all: a plain browser navigation cannot send
    // one, so the shell must still be reachable.
    let shell = service.request("GET", "/", &[]);
    assert_eq!(shell.status, 200, "{shell:?}");
    assert!(
        shell.header("content-type").contains("text/html"),
        "{shell:?}"
    );
    let csp = shell.header("content-security-policy");
    assert!(csp.contains("default-src 'self'"), "{csp}");
    assert!(csp.contains("script-src 'self'"), "{csp}");
    assert!(!csp.contains("unsafe-inline"), "{csp}");

    // The shell is static markup from the binary: no fixture-specific symbol
    // name, path, or SHA is baked into it at serve time.
    assert!(
        shell.body.contains("orbit-graph change explorer"),
        "{}",
        shell.body
    );
    assert!(!shell.body.contains("helper"), "{}", shell.body);
    assert!(
        !shell.body.contains(service.repository.as_str()),
        "{}",
        shell.body
    );
    assert!(
        !shell.body.contains(service.base_sha.as_str()),
        "{}",
        shell.body
    );
    assert!(shell.body.contains("/ui/app.css"), "{}", shell.body);
    assert!(shell.body.contains("/ui/app.js"), "{}", shell.body);
}

#[test]
fn static_assets_resolve_to_the_embedded_source_without_a_token() {
    let service = Service::launch("direct-call");

    let css = service.request("GET", "/ui/app.css", &[]);
    assert_eq!(css.status, 200, "{css:?}");
    assert!(css.header("content-type").contains("text/css"), "{css:?}");
    assert_eq!(
        css.body, APP_CSS,
        "served CSS must match the embedded asset"
    );
    assert!(
        css.header("content-security-policy")
            .contains("default-src 'self'"),
        "{css:?}"
    );

    let js = service.request("GET", "/ui/app.js", &[]);
    assert_eq!(js.status, 200, "{js:?}");
    assert!(js.header("content-type").contains("javascript"), "{js:?}");
    assert_eq!(js.body, APP_JS, "served JS must match the embedded asset");

    // Never sent to storage or logs: the module never touches
    // localStorage/sessionStorage, and never assigns to `.innerHTML`
    // (comments discussing the rule are fine; the property access is not).
    assert!(!js.body.contains("localStorage"), "localStorage referenced");
    assert!(
        !js.body.contains("sessionStorage"),
        "sessionStorage referenced"
    );
    assert!(
        !js.body.contains(".innerHTML"),
        "innerHTML property referenced"
    );
}

#[test]
fn api_routes_still_require_the_bearer_token() {
    let service = Service::launch("direct-call");
    let response = service.request("GET", "/api/health", &[]);
    assert_eq!(response.status, 401, "{response:?}");
}

#[test]
fn direct_call_data_contract_matches_the_page() {
    assert_data_contract("direct-call");
}

#[test]
fn removed_symbol_data_contract_matches_the_page() {
    assert_data_contract("removed-symbol");
}

/// Assert every field `explorer/ui/app.js` reads is present in the real
/// service's JSON, for both endpoints and both sides of `case_id`.
fn assert_data_contract(case_id: &str) {
    let service = Service::launch(case_id);
    service.wait_until_ready();

    let comparison = service.authorized("GET", "/api/comparison", &[]).json();
    for field in [
        "mode",
        "base",
        "head",
        "base_sha",
        "head_sha",
        "working_tree",
        "indexing_status",
    ] {
        assert!(
            comparison.get(field).is_some(),
            "comparison.{field} missing: {comparison}"
        );
    }
    assert!(
        comparison["base"].get("commit_sha").is_some(),
        "{comparison}"
    );
    assert!(
        comparison["head"].get("commit_sha").is_some(),
        "{comparison}"
    );
    assert!(
        comparison["working_tree"].get("dirty").is_some(),
        "{comparison}"
    );
    assert!(
        comparison["working_tree"].get("notice").is_some(),
        "{comparison}"
    );

    let changed = service
        .authorized("GET", "/api/changed-symbols", &[])
        .json();
    assert!(changed.get("schema_version").is_some(), "{changed}");
    let symbols = changed["symbols"].as_array().expect("symbols array");
    assert!(
        !symbols.is_empty(),
        "{case_id} fixture must have a changed symbol: {changed}"
    );
    for symbol in symbols {
        for field in [
            "status",
            "pairing",
            "base",
            "head",
            "supporting_snapshots",
            "base_path",
            "head_path",
            "note",
            "uncertain_candidates",
        ] {
            assert!(
                symbol.get(field).is_some(),
                "changed symbol missing `{field}`: {symbol}"
            );
        }
        if symbol["base"].is_object() {
            assert!(symbol["base"].get("selector").is_some(), "{symbol}");
        }
        if symbol["head"].is_object() {
            assert!(symbol["head"].get("selector").is_some(), "{symbol}");
        }
        for candidate in symbol["uncertain_candidates"].as_array().expect("array") {
            for field in ["selector", "snapshot", "reason"] {
                assert!(
                    candidate.get(field).is_some(),
                    "uncertain candidate missing `{field}`: {candidate}"
                );
            }
        }
    }
    assert!(changed.get("out_of_scope").is_some(), "{changed}");
    for entry in changed["out_of_scope"].as_array().expect("array") {
        for field in ["path", "reason", "snapshot"] {
            assert!(
                entry.get(field).is_some(),
                "out-of-scope entry missing `{field}`: {entry}"
            );
        }
    }

    // Pick the primary selector the way the page does: head if present,
    // otherwise base.
    let (selector, side) = symbols
        .iter()
        .find_map(|symbol| {
            symbol["head"]["selector"]
                .as_str()
                .map(|selector| (selector.to_string(), "head"))
                .or_else(|| {
                    symbol["base"]["selector"]
                        .as_str()
                        .map(|selector| (selector.to_string(), "base"))
                })
        })
        .expect("at least one changed symbol resolves on one side");
    let encoded_selector = percent_encode(selector.as_str());

    let evidence = service
        .authorized(
            "GET",
            format!("/api/evidence?selector={encoded_selector}&side={side}").as_str(),
            &[],
        )
        .json();
    for field in [
        "target",
        "commit_sha",
        "resolved",
        "query_options",
        "paths",
        "skipped_low_confidence",
        "truncated",
        "truncated_by",
        "no_path_reasons",
    ] {
        assert!(
            evidence.get(field).is_some(),
            "evidence.{field} missing: {evidence}"
        );
    }
    for field in ["depth", "min_confidence", "source_max_bytes"] {
        assert!(evidence["query_options"].get(field).is_some(), "{evidence}");
    }
    for path in evidence["paths"].as_array().expect("paths array") {
        for field in ["truncated", "truncated_by", "edges"] {
            assert!(
                path.get(field).is_some(),
                "evidence path missing `{field}`: {path}"
            );
        }
        let edge = &path["edges"][0];
        for field in [
            "from",
            "from_selector",
            "to",
            "relationship",
            "category",
            "confidence",
            "snapshot",
            "commit_sha",
            "source",
            "note",
        ] {
            assert!(
                edge.get(field).is_some(),
                "evidence edge missing `{field}`: {edge}"
            );
        }
        assert!(edge["source"].get("file").is_some(), "{edge}");
        assert!(edge["source"].get("line").is_some(), "{edge}");
    }

    let candidates = service
        .authorized(
            "GET",
            format!("/api/candidate-tests?selector={encoded_selector}&side={side}").as_str(),
            &[],
        )
        .json();
    for field in ["candidates", "unsupported_scope", "truncated"] {
        assert!(
            candidates.get(field).is_some(),
            "candidate-tests.{field} missing: {candidates}"
        );
    }
    for candidate in candidates["candidates"]
        .as_array()
        .expect("candidates array")
    {
        for field in ["test", "source", "category", "note", "truncated"] {
            assert!(
                candidate.get(field).is_some(),
                "candidate missing `{field}`: {candidate}"
            );
        }
        assert!(candidate["test"].get("selector").is_some(), "{candidate}");
    }

    let source = service
        .authorized(
            "GET",
            format!("/api/source?selector={encoded_selector}&side={side}").as_str(),
            &[],
        )
        .json();
    for field in [
        "selector",
        "snapshot",
        "commit_sha",
        "encoding",
        "bytes_or_text",
        "truncated",
        "truncated_by",
        "source_max_bytes",
    ] {
        assert!(
            source.get(field).is_some(),
            "source.{field} missing: {source}"
        );
    }
    if source["encoding"] == "text" {
        assert!(source["file"].is_string(), "{source}");
        assert!(source["span"].get("start").is_some(), "{source}");
        assert!(source["span"].get("end").is_some(), "{source}");
    }
}

#[test]
fn selectors_with_spaces_and_unicode_round_trip_through_the_api() {
    let (repository, commit) = build_unicode_fixture();
    let path = repository.path().to_path_buf();
    let service = Service::launch_at(path, commit.clone(), commit, Box::new(repository));
    service.wait_until_ready();

    let selector = "symbol:src/héllo world.rs#greet:function";
    let encoded_selector = percent_encode(selector);
    let response = service.authorized(
        "GET",
        format!("/api/source?selector={encoded_selector}&side=head").as_str(),
        &[],
    );
    assert_eq!(response.status, 200, "{response:?}");
    let body: Value = response.json();
    assert_eq!(body["file"], "src/héllo world.rs", "{body}");
    assert_eq!(body["encoding"], "text", "{body}");
    assert!(
        body["bytes_or_text"]
            .as_str()
            .unwrap_or_default()
            .contains("greet"),
        "{body}"
    );

    // The changed-symbols and evidence routes must resolve the same selector
    // too, since the page threads it through both.
    let evidence = service.authorized(
        "GET",
        format!("/api/evidence?selector={encoded_selector}&side=head").as_str(),
        &[],
    );
    assert_eq!(evidence.status, 200, "{evidence:?}");
}

/// A one-commit repository whose only file has a space and a Unicode
/// character in its name, so `base` and `head` are the same commit.
fn build_unicode_fixture() -> (TempDir, String) {
    let dir = TempDir::new().expect("create fixture repository");
    let repo = Repository::init(dir.path()).expect("init fixture repository");

    fs::create_dir_all(dir.path().join("src")).expect("create src directory");
    let relative = Path::new("src").join("héllo world.rs");
    fs::write(
        dir.path().join(relative.as_path()),
        "pub fn greet() -> i32 {\n    42\n}\n",
    )
    .expect("write fixture source");

    let mut index = repo.index().expect("open fixture index");
    index
        .add_path(relative.as_path())
        .expect("stage fixture file");
    index.write().expect("write fixture index");
    let tree_id = index.write_tree().expect("write fixture tree");
    let tree = repo.find_tree(tree_id).expect("find fixture tree");

    let when = Time::new(1_700_000_000, 0);
    let author =
        Signature::new("Fixture Author", "fixture@example.invalid", &when).expect("signature");
    let commit = repo
        .commit(
            Some("HEAD"),
            &author,
            &author,
            "add a unicode file",
            &tree,
            &[],
        )
        .expect("create fixture commit")
        .to_string();

    (dir, commit)
}
