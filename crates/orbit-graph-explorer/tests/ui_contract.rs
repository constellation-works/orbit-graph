//! Real-binary coverage for the embedded three-pane UI shell.
//!
//! Four things are asserted here, all against the packaged
//! `orbit-graph-explorer` binary rather than an in-process handler:
//!
//! - `GET /` and its two static assets serve the embedded shell with the
//!   correct `Content-Type` and a restrictive CSP, without requiring the
//!   per-launch bearer token (a plain navigation or `<link>`/`<script>` fetch
//!   cannot attach one), and without any repository-derived content.
//! - The JSON field names `crates/orbit-graph-explorer/ui/app.js` depends on —
//!   its data contract, restated in the module doc comment there — are present
//!   in the real service's serializers for the `direct-call`, `removed-symbol`,
//!   `cycle`, and `ambiguous-same-name` fixtures. This covers the comparison's
//!   per-side cache/index-identity envelope, the changed-symbol list's
//!   `filtered_out` block, and the evidence and entry-point payloads the
//!   filter bar, path following, and entry-points section read.
//! - The served `app.js` persists filter state through the URL fragment
//!   without the per-launch token: it reads and discards `token` before ever
//!   writing the fragment back, and only the filter keys the service accepts
//!   as query parameters are serialized.
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
    // The hand-built SVG neighbourhood graph is built with DOM APIs
    // (`document.createElementNS`, `setAttribute`, `textContent`) only.
    assert!(
        !js.body.contains(".outerHTML"),
        "outerHTML property referenced"
    );
    assert!(
        !js.body.contains("insertAdjacentHTML"),
        "insertAdjacentHTML referenced"
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

#[test]
fn cycle_data_contract_matches_the_page() {
    assert_data_contract("cycle");
}

#[test]
fn ambiguous_same_name_data_contract_matches_the_page() {
    assert_data_contract("ambiguous-same-name");
}

/// Filter, search, and table/graph-toggle state must round-trip through the
/// URL fragment the page parses, and the per-launch token must never be
/// written back into it.
///
/// The fragment is client-side state: nothing in the real HTTP contract
/// encodes it. This asserts the invariant against the served `app.js` itself
/// (the same asset the CSP and static-asset tests already fetch from the real
/// binary), the way the existing data-contract tests assert field names
/// against the real serializers rather than against a copy of the source.
#[test]
fn filter_state_round_trips_through_the_fragment_without_the_token() {
    let service = Service::launch("direct-call");
    let js = service.request("GET", "/ui/app.js", &[]);
    assert_eq!(js.status, 200, "{js:?}");
    let body = js.body.as_str();

    // Exactly the query parameters the service accepts as filters, plus the
    // search and view-toggle keys, are ever persisted, so a bookmarked or
    // copied URL carries all three kinds of state, never a credential.
    for key in [
        "confidence",
        "language",
        "change_kind",
        "depth",
        "scope",
        "q",
        "search_side",
        "view",
    ] {
        assert!(
            body.contains(format!("\"{key}\"").as_str()),
            "app.js must persist the `{key}` key in FRAGMENT_KEYS: missing from the served asset"
        );
    }

    // The token is read from the fragment once, then discarded before the
    // fragment is ever written back.
    assert!(
        body.contains("initialParams.delete(\"token\")"),
        "app.js must strip the token before persisting the fragment"
    );
    assert!(
        body.contains("function encodeFragment"),
        "app.js must expose the fragment encoder the round trip depends on"
    );
    assert!(
        body.contains("function decodeFragment"),
        "app.js must expose the fragment decoder the round trip depends on"
    );
    assert!(
        body.contains("function writeFragment"),
        "app.js must expose the fragment writer used on every filter change"
    );

    // The encoder's own scan is bounded to `FRAGMENT_KEYS`, so `token` cannot
    // reach the fragment through that path even if a caller mishandled it
    // upstream.
    let encode_start = body
        .find("function encodeFragment")
        .expect("encodeFragment is defined");
    let encode_body = &body[encode_start..encode_start + 400.min(body.len() - encode_start)];
    assert!(
        encode_body.contains("FRAGMENT_KEYS"),
        "encodeFragment must iterate FRAGMENT_KEYS, not an unbounded set of params: {encode_body}"
    );
    assert!(
        !encode_body.contains("token"),
        "encodeFragment must never reference `token`: {encode_body}"
    );
}

/// Assert every field `crates/orbit-graph-explorer/ui/app.js` reads is present
/// in the real service's JSON, for both endpoints and both sides of `case_id`.
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
    // Header cache/index-identity status: per-side hit/miss plus the
    // extractor and store schema versions the page prints next to it.
    let snapshots = comparison["snapshots"].as_array().expect("snapshots array");
    assert_eq!(snapshots.len(), 2, "{comparison}");
    for snapshot in snapshots {
        for field in ["side", "cache", "index_identity"] {
            assert!(
                snapshot.get(field).is_some(),
                "comparison snapshot missing `{field}`: {snapshot}"
            );
        }
        for field in ["extractor_version", "store_schema_version"] {
            assert!(
                snapshot["index_identity"].get(field).is_some(),
                "comparison snapshot index_identity missing `{field}`: {snapshot}"
            );
        }
    }

    let changed = service
        .authorized("GET", "/api/changed-symbols", &[])
        .json();
    assert!(changed.get("schema_version").is_some(), "{changed}");
    assert!(
        changed.get("filtered_out").is_some(),
        "changed-symbols.filtered_out missing, needed for the pane-1 \"hidden by filters\" line: \
         {changed}"
    );
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
        "bounds_hit",
        "filtered_out",
        "no_path_reasons",
    ] {
        assert!(
            evidence.get(field).is_some(),
            "evidence.{field} missing: {evidence}"
        );
    }
    for field in [
        "depth",
        "node_cap",
        "min_confidence",
        "time_budget_ms",
        "source_max_bytes",
    ] {
        assert!(evidence["query_options"].get(field).is_some(), "{evidence}");
    }
    for path in evidence["paths"].as_array().expect("paths array") {
        for field in [
            "from",
            "to",
            "distance",
            "category",
            "truncated",
            "truncated_by",
            "edges",
        ] {
            assert!(
                path.get(field).is_some(),
                "evidence path missing `{field}`: {path}"
            );
        }
        for endpoint in ["from", "to"] {
            for field in ["selector", "snapshot", "label", "origin"] {
                assert!(
                    path[endpoint].get(field).is_some(),
                    "evidence path {endpoint} missing `{field}`: {path}"
                );
            }
        }
        let edge = &path["edges"][0];
        for field in [
            "from",
            "from_selector",
            "from_origin",
            "to",
            "to_selector",
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

    // Pane 2's entry-points section: each entry's classification rule and its
    // shortest path back to the queried symbol, plus the bounds/truncation
    // envelope the section header states.
    let entry_points = service
        .authorized(
            "GET",
            format!("/api/entry-points?selector={encoded_selector}&side={side}").as_str(),
            &[],
        )
        .json();
    for field in [
        "target",
        "commit_sha",
        "query_options",
        "rules",
        "entry_points",
        "truncated",
        "truncated_by",
        "bounds_hit",
        "filtered_out",
        "no_entry_point_reasons",
    ] {
        assert!(
            entry_points.get(field).is_some(),
            "entry-points.{field} missing: {entry_points}"
        );
    }
    for rule in entry_points["rules"].as_array().expect("rules array") {
        for field in ["id", "description"] {
            assert!(
                rule.get(field).is_some(),
                "entry-point rule missing `{field}`: {rule}"
            );
        }
    }
    for entry in entry_points["entry_points"]
        .as_array()
        .expect("entry_points array")
    {
        for field in [
            "node",
            "rule",
            "rule_description",
            "rules",
            "distance",
            "path",
            "note",
        ] {
            assert!(
                entry.get(field).is_some(),
                "entry point missing `{field}`: {entry}"
            );
        }
        for field in ["selector", "snapshot", "label", "origin"] {
            assert!(
                entry["node"].get(field).is_some(),
                "entry point node missing `{field}`: {entry}"
            );
        }
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

    // The search box's data contract: `label`/`selector` name what the page
    // resolves to, `changed` is what a search result's "not changed in this
    // comparison" wording reads.
    let search_term = symbol_name(selector.as_str());
    let search = service
        .authorized(
            "GET",
            format!("/api/search?q={}&side={side}", percent_encode(search_term)).as_str(),
            &[],
        )
        .json();
    for field in [
        "scope",
        "snapshot",
        "q",
        "limit",
        "truncated",
        "truncated_by",
        "matches",
    ] {
        assert!(
            search.get(field).is_some(),
            "search.{field} missing: {search}"
        );
    }
    let matches = search["matches"].as_array().expect("matches array");
    assert!(
        !matches.is_empty(),
        "{case_id} fixture must have at least one search match for `{search_term}`: {search}"
    );
    for one_match in matches {
        for field in ["kind", "selector", "label", "file", "line", "changed"] {
            assert!(
                one_match.get(field).is_some(),
                "search match missing `{field}`: {one_match}"
            );
        }
    }

    // The `invalid_kind` error envelope the search box relies on when a
    // caller (never the UI itself, which only ever sends `symbol`/`string`/
    // `config`) mis-specifies `kind`.
    let bad_kind = service.authorized("GET", "/api/search?kind=not-a-kind", &[]);
    assert_eq!(bad_kind.status, 400, "{bad_kind:?}");
    let bad_kind_body = bad_kind.json();
    assert_eq!(
        bad_kind_body["error"]["code"], "invalid_kind",
        "{bad_kind_body}"
    );
    assert!(
        bad_kind_body["error"].get("message").is_some(),
        "{bad_kind_body}"
    );
    assert!(
        bad_kind_body["error"].get("details").is_some(),
        "{bad_kind_body}"
    );

    // The indexing-progress header's data contract: per-side state, file
    // counts, languages, and elapsed time, plus the top-level status the
    // Cancel/Retry affordance reads.
    let status = service.authorized("GET", "/api/status", &[]).json();
    for field in [
        "schema_version",
        "repository",
        "base_sha",
        "head_sha",
        "indexing_status",
        "indexing",
    ] {
        assert!(
            status.get(field).is_some(),
            "status.{field} missing: {status}"
        );
    }
    assert_eq!(status["indexing_status"], "ready", "{status}");
    for side in ["base", "head"] {
        for field in [
            "state",
            "files_seen",
            "files_indexed",
            "files_ignored",
            "unsupported_constructs",
            "languages",
            "started_at",
            "elapsed_ms",
            "error",
        ] {
            assert!(
                status["indexing"][side].get(field).is_some(),
                "status.indexing.{side}.{field} missing: {status}"
            );
        }
        assert_eq!(status["indexing"][side]["state"], "ready", "{status}");
    }

    // `POST /api/cancel` once indexing has already finished: the `409
    // not_indexing` error envelope the Cancel button's error banner renders.
    let cancel = service.authorized("POST", "/api/cancel", &[]);
    assert_eq!(cancel.status, 409, "{cancel:?}");
    let cancel_body = cancel.json();
    assert_eq!(
        cancel_body["error"]["code"], "not_indexing",
        "{cancel_body}"
    );
    assert!(
        cancel_body["error"]["details"].get("indexing").is_some(),
        "{cancel_body}"
    );
}

/// The bare symbol name out of a canonical `symbol:<path>#<name>:<kind>`
/// selector, the same identity `app.js`'s `parseSelector` extracts — used
/// here as a search term guaranteed to be indexed for the fixture it came
/// from.
fn symbol_name(selector: &str) -> &str {
    let after_hash = selector.split('#').nth(1).unwrap_or(selector);
    after_hash
        .rsplit_once(':')
        .map(|(name, _)| name)
        .unwrap_or(after_hash)
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
