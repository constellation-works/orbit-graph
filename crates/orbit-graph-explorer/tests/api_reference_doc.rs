//! Consistency check for the "API reference" appendix in
//! `docs/design/change-explorer.md`.
//!
//! This launches the real built `orbit-graph-explorer serve` binary (the same
//! real-binary harness `service_integration.rs` uses), exercises every
//! documented route, and reads the actual `error.code` the service returns
//! for a representative request per error code. It then asserts that every
//! route path and every error code the running service really produced
//! appears somewhere in the design doc's text. A route or a code the doc
//! forgets to mention fails this test; a route or a code the service no
//! longer produces does not (the check is deliberately one-directional, per
//! the task's "keep the check simple: names present in the doc text").
//!
//! A handful of codes are not triggered live because doing so needs a broken
//! or racy service state (a corrupted index, a build that fails, a request
//! that lands mid-build, a spoofed `Host` header the shared HTTP harness does
//! not support): those are listed in `UNTRIGGERED_CODES` below and are
//! checked for doc presence only, not cross-checked against a live response.

#![allow(clippy::expect_used)]

use std::fs;
use std::path::PathBuf;

mod common;

use common::http_service::{Service, percent_encode};

const HELPER_SELECTOR: &str = "symbol:src/lib.rs#helper:function";
const MISSING_SELECTOR: &str = "symbol:src/lib.rs#zzz_missing_symbol:function";

/// Error codes the service can return but this test does not trigger live,
/// because doing so needs state this harness cannot cheaply or reliably
/// produce. Checked for doc presence only.
const UNTRIGGERED_CODES: &[&str] = &[
    "host_mismatch",
    "index_unavailable",
    "indexing_failed",
    "side_not_ready",
    "changed_symbols_failed",
    "report_failed",
    "source_failed",
];

#[test]
fn every_documented_route_and_error_code_appears_in_the_design_doc() {
    let service = Service::launch("direct-call");
    service.wait_until_ready();

    let selector = percent_encode(HELPER_SELECTOR);
    let missing = percent_encode(MISSING_SELECTOR);

    let mut exercised_paths: Vec<&'static str> = Vec::new();
    let mut exercised_codes: Vec<String> = Vec::new();

    // Routes that must succeed for real, proving each path is live.
    let success_routes: &[(&str, String, u16)] = &[
        ("GET /", "/".to_string(), 200),
        ("GET /api/health", "/api/health".to_string(), 200),
        ("GET /api/comparison", "/api/comparison".to_string(), 200),
        (
            "GET /api/changed-symbols",
            "/api/changed-symbols".to_string(),
            200,
        ),
        (
            "GET /api/evidence",
            format!("/api/evidence?selector={selector}&side=head"),
            200,
        ),
        (
            "GET /api/entry-points",
            format!("/api/entry-points?selector={selector}&side=head"),
            200,
        ),
        (
            "GET /api/candidate-tests",
            format!("/api/candidate-tests?selector={selector}&side=head"),
            200,
        ),
        (
            "GET /api/source",
            format!("/api/source?selector={selector}&side=head"),
            200,
        ),
        ("GET /api/search", "/api/search?q=helper".to_string(), 200),
        ("GET /api/status", "/api/status".to_string(), 200),
    ];
    for (label, path, expected_status) in success_routes {
        let response = service.authorized("GET", path.as_str(), &[]);
        assert_eq!(
            response.status, *expected_status,
            "{label} ({path}): {response:?}"
        );
        exercised_paths.push(*label);
    }

    let report = service.authorized_with_body("POST", "/api/report", &[], b"");
    assert_eq!(report.status, 200, "POST /api/report: {report:?}");
    exercised_paths.push("POST /api/report");

    // `unauthorized`: no bearer token at all.
    let unauthorized = service.request("GET", "/api/comparison", &[]);
    assert_eq!(unauthorized.status, 401, "{unauthorized:?}");
    exercised_codes.push(expect_code(&unauthorized));

    // `origin_mismatch`: a foreign Origin with an otherwise valid token.
    let origin_mismatch = service.authorized(
        "GET",
        "/api/comparison",
        &[("Origin", "http://evil.example")],
    );
    assert_eq!(origin_mismatch.status, 403, "{origin_mismatch:?}");
    exercised_codes.push(expect_code(&origin_mismatch));

    // `repository_out_of_scope`: a `repo` parameter naming another repository.
    let out_of_scope = service.authorized("GET", "/api/changed-symbols?repo=/not/this/repo", &[]);
    assert_eq!(out_of_scope.status, 403, "{out_of_scope:?}");
    exercised_codes.push(expect_code(&out_of_scope));

    // `not_found`: an unrecognized path.
    let not_found = service.authorized("GET", "/api/nope", &[]);
    assert_eq!(not_found.status, 404, "{not_found:?}");
    exercised_codes.push(expect_code(&not_found));

    // `method_not_allowed`: a recognized path with the wrong method.
    let wrong_method = service.authorized("POST", "/api/health", &[]);
    assert_eq!(wrong_method.status, 405, "{wrong_method:?}");
    exercised_codes.push(expect_code(&wrong_method));

    // Evidence-route parameter errors.
    let missing_selector = service.authorized("GET", "/api/evidence", &[]);
    assert_eq!(missing_selector.status, 400, "{missing_selector:?}");
    exercised_codes.push(expect_code(&missing_selector));

    let evidence_failed = service.authorized("GET", "/api/evidence?selector=not-a-selector", &[]);
    assert_eq!(evidence_failed.status, 400, "{evidence_failed:?}");
    exercised_codes.push(expect_code(&evidence_failed));

    let invalid_side = service.authorized(
        "GET",
        format!("/api/evidence?selector={selector}&side=sideways").as_str(),
        &[],
    );
    assert_eq!(invalid_side.status, 400, "{invalid_side:?}");
    exercised_codes.push(expect_code(&invalid_side));

    let invalid_confidence = service.authorized(
        "GET",
        format!("/api/evidence?selector={selector}&confidence=maybe").as_str(),
        &[],
    );
    assert_eq!(invalid_confidence.status, 400, "{invalid_confidence:?}");
    exercised_codes.push(expect_code(&invalid_confidence));

    let invalid_direction = service.authorized(
        "GET",
        format!("/api/evidence?selector={selector}&direction=sideways").as_str(),
        &[],
    );
    assert_eq!(invalid_direction.status, 400, "{invalid_direction:?}");
    exercised_codes.push(expect_code(&invalid_direction));

    let unsupported_depth = service.authorized(
        "GET",
        format!("/api/evidence?selector={selector}&depth=250").as_str(),
        &[],
    );
    assert_eq!(unsupported_depth.status, 400, "{unsupported_depth:?}");
    exercised_codes.push(expect_code(&unsupported_depth));

    let entry_points_failed =
        service.authorized("GET", "/api/entry-points?selector=not-a-selector", &[]);
    assert_eq!(entry_points_failed.status, 400, "{entry_points_failed:?}");
    exercised_codes.push(expect_code(&entry_points_failed));

    let candidate_tests_failed =
        service.authorized("GET", "/api/candidate-tests?selector=not-a-selector", &[]);
    assert_eq!(
        candidate_tests_failed.status, 400,
        "{candidate_tests_failed:?}"
    );
    exercised_codes.push(expect_code(&candidate_tests_failed));

    // Source-route parameter errors.
    let invalid_selector = service.authorized("GET", "/api/source?selector=not-a-selector", &[]);
    assert_eq!(invalid_selector.status, 400, "{invalid_selector:?}");
    exercised_codes.push(expect_code(&invalid_selector));

    let not_in_snapshot = service.authorized(
        "GET",
        format!("/api/source?selector={missing}&side=head").as_str(),
        &[],
    );
    assert_eq!(not_in_snapshot.status, 404, "{not_in_snapshot:?}");
    exercised_codes.push(expect_code(&not_in_snapshot));

    // Search-route parameter errors.
    let invalid_query = service.authorized("GET", "/api/search?q=foo%00bar", &[]);
    assert_eq!(invalid_query.status, 400, "{invalid_query:?}");
    exercised_codes.push(expect_code(&invalid_query));

    let invalid_kind = service.authorized("GET", "/api/search?kind=bogus", &[]);
    assert_eq!(invalid_kind.status, 400, "{invalid_kind:?}");
    exercised_codes.push(expect_code(&invalid_kind));

    let invalid_limit = service.authorized("GET", "/api/search?limit=abc", &[]);
    assert_eq!(invalid_limit.status, 400, "{invalid_limit:?}");
    exercised_codes.push(expect_code(&invalid_limit));

    let unsupported_limit = service.authorized("GET", "/api/search?limit=99999", &[]);
    assert_eq!(unsupported_limit.status, 400, "{unsupported_limit:?}");
    exercised_codes.push(expect_code(&unsupported_limit));

    // Report-route body errors.
    let invalid_excerpts =
        service.authorized_with_body("POST", "/api/report", &[], br#"{"excerpts":"bogus"}"#);
    assert_eq!(invalid_excerpts.status, 400, "{invalid_excerpts:?}");
    exercised_codes.push(expect_code(&invalid_excerpts));

    let invalid_request_body =
        service.authorized_with_body("POST", "/api/report", &[], b"{not json");
    assert_eq!(invalid_request_body.status, 400, "{invalid_request_body:?}");
    exercised_codes.push(expect_code(&invalid_request_body));

    // `not_indexing`: nothing is in progress once the launch scope is ready.
    let not_indexing = service.authorized("POST", "/api/cancel", &[]);
    assert_eq!(not_indexing.status, 409, "{not_indexing:?}");
    exercised_codes.push(expect_code(&not_indexing));
    exercised_paths.push("POST /api/cancel");

    let doc = read_design_doc();

    for path in exercised_paths {
        assert!(
            doc.contains(path),
            "the design doc's API reference must name `{path}`, a route the real \
             service actually serves"
        );
    }
    for code in exercised_codes {
        assert!(
            doc.contains(code.as_str()),
            "the design doc's API reference must name error code `{code}`, which the \
             real service actually returned"
        );
    }
    for code in UNTRIGGERED_CODES {
        assert!(
            doc.contains(code),
            "the design doc's API reference must name error code `{code}`"
        );
    }
}

fn expect_code(response: &common::http_service::HttpResponse) -> String {
    let json = response.json();
    json["error"]["code"]
        .as_str()
        .unwrap_or_else(|| panic!("response carries no error.code: {json}"))
        .to_string()
}

fn read_design_doc() -> String {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "..",
        "..",
        "docs",
        "design",
        "change-explorer.md",
    ]
    .iter()
    .collect();
    fs::read_to_string(path.as_path())
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}
