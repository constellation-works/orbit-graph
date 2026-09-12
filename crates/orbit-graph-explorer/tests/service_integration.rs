//! The loopback service, exercised through the real `orbit-graph-explorer`
//! executable.
//!
//! Every test here launches the packaged binary with `serve`, reads the
//! per-launch bearer token from its standard error, and drives it over a real
//! TCP socket with hand-written HTTP/1.1 requests. Nothing is asserted against
//! an in-process handler: the contract under test is what the shipped binary
//! serves on the wire.

#![allow(clippy::expect_used)]

use std::net::TcpStream;
use std::time::{Duration, Instant};

use serde_json::Value;

mod common;

use common::corpus;
use common::http_service::{Service, percent_encode};
use orbit_graph_explorer::service::ServeOptions;

#[test]
fn a_request_without_the_token_is_refused_before_any_graph_work() {
    let service = Service::launch("direct-call");
    let response = service.request("GET", "/api/comparison", &[]);
    assert_eq!(response.status, 401, "{response:?}");
    assert_eq!(response.json()["error"]["code"], "unauthorized");
    // The rejection must not leak the credential it is checking against.
    assert!(
        !response.body.contains(service.token.as_str()),
        "a rejection must never echo the token: {}",
        response.body
    );

    let wrong = service.request(
        "GET",
        "/api/comparison",
        &[("Authorization", "Bearer not-the-token")],
    );
    assert_eq!(wrong.status, 401, "{wrong:?}");

    // A token that is a prefix of the real one must not be accepted either.
    let prefix = format!("Bearer {}", &service.token[..16]);
    let truncated = service.request("GET", "/api/comparison", &[("Authorization", &prefix)]);
    assert_eq!(truncated.status, 401, "{truncated:?}");
}

#[test]
fn a_cross_origin_request_is_refused_even_with_a_valid_token() {
    let service = Service::launch("direct-call");
    for (field, value) in [
        ("Origin", "http://evil.example"),
        ("Referer", "http://evil.example/page"),
        // A different loopback port is a different origin.
        ("Origin", "http://127.0.0.1:1"),
        // A hostname that resolves to loopback is still not this origin.
        ("Origin", "http://localhost:80"),
    ] {
        let response = service.authorized("GET", "/api/comparison", &[(field, value)]);
        assert_eq!(
            response.status, 403,
            "{field}: {value} must be refused: {response:?}"
        );
        assert_eq!(response.json()["error"]["code"], "origin_mismatch");
    }

    // The service's own origin is accepted.
    let own = service.authorized("GET", "/api/health", &[("Origin", service.origin.as_str())]);
    assert_eq!(own.status, 200, "{own:?}");
}

#[test]
fn a_request_naming_another_repository_is_refused() {
    let service = Service::launch("direct-call");
    service.wait_until_ready();

    let response = service.authorized("GET", "/api/changed-symbols?repo=/not/this/repo", &[]);
    assert_eq!(response.status, 403, "{response:?}");
    assert_eq!(response.json()["error"]["code"], "repository_out_of_scope");

    // The launch scope itself is accepted.
    let own = format!(
        "/api/changed-symbols?repo={}",
        percent_encode(service.repository.as_str())
    );
    let accepted = service.authorized("GET", own.as_str(), &[]);
    assert_eq!(accepted.status, 200, "{accepted:?}");
}

#[test]
fn health_answers_while_indexing_and_echoes_the_launch_scope() {
    let service = Service::launch("direct-call");

    // Answered before anything is indexed: the scope is resolved synchronously
    // at launch, the snapshots are not.
    let first = service.authorized("GET", "/api/health", &[]);
    assert_eq!(first.status, 200, "{first:?}");
    let body = first.json();
    assert_eq!(body["status"], "ok");
    assert_eq!(body["mode"], "direct_base_head");
    assert_eq!(body["base_sha"], service.base_sha.as_str());
    assert_eq!(body["head_sha"], service.head_sha.as_str());
    assert!(
        matches!(
            body["indexing_status"].as_str(),
            Some("indexing") | Some("ready")
        ),
        "{body}"
    );

    service.wait_until_ready();
    let ready = service.authorized("GET", "/api/health", &[]).json();
    assert_eq!(ready["indexing_status"], "ready");
    assert_eq!(ready["indexing_error"], Value::Null);
}

#[test]
fn every_endpoint_answers_the_direct_call_comparison() {
    let service = Service::launch("direct-call");
    service.wait_until_ready();

    // The placeholder shell is static markup from the binary.
    let shell = service.authorized("GET", "/", &[]);
    assert_eq!(shell.status, 200, "{shell:?}");
    assert!(
        shell.header("content-type").contains("text/html"),
        "{shell:?}"
    );
    assert!(shell.body.contains("change explorer"), "{}", shell.body);

    // Report export is explicitly not implemented rather than partially served.
    let report = service.authorized("POST", "/api/report", &[]);
    assert_eq!(report.status, 501, "{report:?}");
    assert_eq!(report.json()["error"]["code"], "not_implemented");

    let comparison = service.authorized("GET", "/api/comparison", &[]).json();
    assert_eq!(comparison["schema_version"], 1);
    assert_eq!(comparison["mode"], "direct_base_head");
    assert_eq!(comparison["base"]["commit_sha"], service.base_sha.as_str());
    assert_eq!(comparison["head"]["commit_sha"], service.head_sha.as_str());
    assert_eq!(
        comparison["effective_base_sha"],
        service.base_sha.as_str(),
        "direct mode never rewrites the stated base"
    );
    assert_eq!(comparison["indexing_status"], "ready");
    assert_eq!(comparison["working_tree"]["dirty"], false);
    assert_eq!(comparison["working_tree"]["notice"], Value::Null);
    let snapshots = comparison["snapshots"]
        .as_array()
        .expect("snapshots array")
        .clone();
    assert_eq!(snapshots.len(), 2);
    assert_eq!(snapshots[0]["side"], "base");
    assert_eq!(snapshots[1]["side"], "head");
    assert!(snapshots[0]["files_indexed"].as_u64().unwrap_or_default() > 0);
    for snapshot in &snapshots {
        // A cold launch builds both sides and says so, and every snapshot
        // reports the index identity its evidence came from.
        assert_eq!(snapshot["cache"], "miss", "{snapshot}");
        assert_eq!(
            snapshot["index_identity"]["store_schema_version"], 1,
            "{snapshot}"
        );
        assert!(
            snapshot["index_identity"]["extractor_version"]
                .as_u64()
                .unwrap_or_default()
                > 0,
            "{snapshot}"
        );
        assert!(snapshot["prepare_ms"].is_number(), "{snapshot}");
    }
    assert!(comparison["cache"]["directory"].is_string(), "{comparison}");

    let changed = service
        .authorized("GET", "/api/changed-symbols", &[])
        .json();
    assert_eq!(changed["schema_version"], 1);
    assert_scope(&changed, &service);
    let symbols = changed["symbols"].as_array().expect("symbols").clone();
    assert_eq!(symbols.len(), 1, "{changed}");
    assert_eq!(symbols[0]["status"], "modified");
    assert_eq!(
        symbols[0]["base"]["selector"],
        "symbol:src/lib.rs#helper:function"
    );
    assert_eq!(symbols[0]["base"]["commit_sha"], service.base_sha.as_str());
    assert_eq!(symbols[0]["head"]["commit_sha"], service.head_sha.as_str());
    assert_eq!(changed["out_of_scope"].as_array().map(Vec::len), Some(0));

    let evidence = service
        .authorized(
            "GET",
            "/api/evidence?selector=symbol%3Asrc%2Flib.rs%23helper%3Afunction&side=head",
            &[],
        )
        .json();
    assert_eq!(evidence["schema_version"], 1);
    assert_scope(&evidence, &service);
    assert_eq!(evidence["resolved"], true);
    assert_eq!(evidence["query_options"]["depth"], 3);
    assert_eq!(evidence["query_options"]["direction"], "inbound");
    assert_eq!(evidence["query_options"]["min_confidence"], "same_module");
    assert_eq!(evidence["impact"]["direction"], "inbound");
    assert_eq!(evidence["truncated"], false);
    assert_eq!(evidence["truncated_by"], Value::Null);
    let categories: Vec<String> = evidence["paths"]
        .as_array()
        .expect("paths")
        .iter()
        .map(|path| {
            path["edges"][0]["category"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert!(
        categories.contains(&"resolved_call".to_string()),
        "{evidence}"
    );
    assert!(
        categories.contains(&"observed_reference".to_string()),
        "{evidence}"
    );
    for path in evidence["paths"].as_array().expect("paths") {
        assert_eq!(path["edges"][0]["snapshot"], "head");
        assert_eq!(path["edges"][0]["commit_sha"], service.head_sha.as_str());
        assert_eq!(path["truncated"], false);
    }

    // A depth beyond the service's own maximum is refused rather than silently
    // answered at a different depth.
    let deep = service.authorized(
        "GET",
        "/api/evidence?selector=symbol%3Asrc%2Flib.rs%23helper%3Afunction&depth=99",
        &[],
    );
    assert_eq!(deep.status, 400, "{deep:?}");
    assert_eq!(deep.json()["error"]["code"], "unsupported_depth");

    // A depth inside the maximum is answered at exactly that depth.
    let shallow = service
        .authorized(
            "GET",
            "/api/evidence?selector=symbol%3Asrc%2Flib.rs%23helper%3Afunction&depth=1",
            &[],
        )
        .json();
    assert_eq!(shallow["query_options"]["depth"], 1, "{shallow}");
    for path in shallow["paths"].as_array().expect("paths") {
        assert_eq!(path["distance"], 1, "{path}");
    }

    let candidates = service
        .authorized(
            "GET",
            "/api/candidate-tests?selector=symbol%3Asrc%2Flib.rs%23helper%3Afunction&side=head",
            &[],
        )
        .json();
    assert_eq!(candidates["schema_version"], 1);
    assert_scope(&candidates, &service);
    assert_eq!(candidates["truncated"], false);
    let sources: Vec<String> = candidates["candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .map(|candidate| candidate["source"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(sources.contains(&"call_path".to_string()), "{candidates}");
    assert!(
        sources.contains(&"naming_heuristic".to_string()),
        "{candidates}"
    );

    let source = service
        .authorized(
            "GET",
            "/api/source?selector=symbol%3Asrc%2Flib.rs%23helper%3Afunction&side=head",
            &[],
        )
        .json();
    assert_scope(&source, &service);
    assert_eq!(source["snapshot"], "head");
    assert_eq!(source["commit_sha"], service.head_sha.as_str());
    assert_eq!(source["encoding"], "text");
    assert_eq!(source["truncated"], false);
    assert_eq!(source["truncated_by"], Value::Null);
    assert_eq!(source["source_max_bytes"], 65536);
    let text = source["bytes_or_text"]
        .as_str()
        .expect("source text is a JSON string, never markup");
    assert!(text.contains("pub fn helper"), "{text}");
    assert!(text.contains('2'), "head returns the head body: {text}");
}

#[test]
fn source_for_a_removed_symbol_is_served_from_the_base_snapshot() {
    let service = Service::launch("removed-symbol");
    service.wait_until_ready();

    let selector = "symbol%3Asrc%2Flib.rs%23old_helper%3Afunction";
    let base = service.authorized(
        "GET",
        format!("/api/source?selector={selector}&side=base").as_str(),
        &[],
    );
    assert_eq!(base.status, 200, "{base:?}");
    let body = base.json();
    assert_eq!(body["snapshot"], "base");
    assert_eq!(body["commit_sha"], service.base_sha.as_str());
    assert_eq!(body["encoding"], "text");
    let text = body["bytes_or_text"]
        .as_str()
        .expect("base-side source is a JSON string");
    assert!(text.contains("pub fn old_helper"), "{text}");
    assert!(text.contains("42"), "{text}");

    // The same selector on head is absent, and says so about that revision
    // only rather than serving base content under a head label.
    let head = service.authorized(
        "GET",
        format!("/api/source?selector={selector}&side=head").as_str(),
        &[],
    );
    assert_eq!(head.status, 404, "{head:?}");
    let head_body = head.json();
    assert_eq!(head_body["snapshot"], "head");
    assert_eq!(head_body["error"]["code"], "not_in_snapshot");
}

#[test]
fn invalid_parameters_are_refused_with_a_reason() {
    let service = Service::launch("direct-call");
    service.wait_until_ready();

    for (route, code) in [
        ("/api/evidence", "missing_selector"),
        ("/api/evidence?selector=not-a-selector", "evidence_failed"),
        (
            "/api/evidence?selector=symbol%3Asrc%2Flib.rs%23helper%3Afunction&side=sideways",
            "invalid_side",
        ),
        (
            "/api/evidence?selector=symbol%3Asrc%2Flib.rs%23helper%3Afunction&confidence=maybe",
            "invalid_confidence",
        ),
        ("/api/nope", "not_found"),
    ] {
        let response = service.authorized("GET", route, &[]);
        assert!(
            response.status >= 400,
            "{route} must be refused: {response:?}"
        );
        assert_eq!(response.json()["error"]["code"], code, "{route}");
    }
}

/// The in-process lifecycle: a service owns its snapshot trees and releases
/// them when it is shut down.
///
/// This is the one test here that uses the library API rather than the packaged
/// binary, because `Service::shutdown` has no command-line surface: the binary
/// exits with the process.
#[test]
fn shutting_a_service_down_stops_it_answering() {
    let case = corpus::build_case("direct-call");
    let service = orbit_graph_explorer::service::Service::start(&ServeOptions {
        repository: case.repository.clone(),
        base: case.base().to_string(),
        head: case.head().to_string(),
        port: 0,
        ..ServeOptions::default()
    })
    .expect("start service");

    let authority = service
        .origin()
        .strip_prefix("http://")
        .expect("loopback origin")
        .to_string();
    assert!(authority.starts_with("127.0.0.1:"), "{authority}");
    assert!(TcpStream::connect(authority.as_str()).is_ok());

    service.shutdown();

    // The listener is closed, so a fresh connection is refused rather than
    // accepted and left unanswered.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if TcpStream::connect(authority.as_str()).is_err() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the service kept accepting connections after shutdown"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn assert_scope(payload: &Value, service: &Service) {
    let scope = &payload["scope"];
    assert_eq!(scope["mode"], "direct_base_head", "{payload}");
    assert_eq!(scope["base_sha"], service.base_sha.as_str(), "{payload}");
    assert_eq!(scope["head_sha"], service.head_sha.as_str(), "{payload}");
    assert_eq!(scope["indexing_status"], "ready", "{payload}");
    assert!(scope["working_tree"].is_object(), "{payload}");
    assert!(
        scope["working_tree"].get("notice").is_some(),
        "every payload carries the dirty notice slot: {payload}"
    );
}
