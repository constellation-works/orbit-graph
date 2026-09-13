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

use git2::Repository;
use serde_json::Value;
use tempfile::TempDir;

mod common;

use common::http_service::{Service, percent_encode};
use common::{commit_files, corpus};
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

    // Report export with no request body reports on every changed symbol
    // under the launch scope's own bounds. `tests/report_export.rs` covers
    // the report contract itself in depth; this is just wiring coverage for
    // the live route.
    let report = service.authorized_with_body("POST", "/api/report", &[], b"");
    assert_eq!(report.status, 200, "{report:?}");
    let report_body = report.json();
    assert_eq!(report_body["schema_version"], 1);
    assert_eq!(
        report_body["comparison"]["base"]["commit_sha"],
        service.base_sha.as_str()
    );
    assert_eq!(
        report_body["comparison"]["head"]["commit_sha"],
        service.head_sha.as_str()
    );
    assert!(report_body["comparison"]["repository"].is_null());
    assert!(report_body["changed_symbols"]["symbols"].is_array());

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

#[test]
fn search_finds_a_changed_symbol_on_both_sides_with_its_changed_status() {
    let service = Service::launch("direct-call");
    service.wait_until_ready();

    for side in ["base", "head"] {
        let response = service.authorized(
            "GET",
            format!("/api/search?q=helper&side={side}").as_str(),
            &[],
        );
        assert_eq!(response.status, 200, "{response:?}");
        let body = response.json();
        assert_eq!(body["q"], "helper");
        assert_eq!(body["snapshot"], side);
        assert_eq!(body["limit"], 20);
        assert_eq!(body["truncated"], false);
        assert_eq!(body["truncated_by"], Value::Null);
        assert_scope(&body, &service);
        let matches = body["matches"].as_array().expect("matches");
        // The FTS5 tokenizer splits `test_helper` into `test` and `helper`,
        // so the query also matches the test that calls `helper`; only the
        // exact `helper` entry is asserted on here.
        assert!(!matches.is_empty(), "{body}");
        let found = matches
            .iter()
            .find(|found| found["label"] == "helper")
            .unwrap_or_else(|| panic!("no `helper` match in {body}"));
        assert_eq!(found["kind"], "symbol");
        assert_eq!(found["file"], "src/lib.rs");
        assert_eq!(found["selector"], "symbol:src/lib.rs#helper:function");
        // `helper`'s body changed on both sides of a fixed selector, so both
        // sides report the same `modified` status for it.
        assert_eq!(found["changed"], "modified", "{body}");
    }

    // `entry` never changed and is not in the changed-symbol list at all.
    let unchanged = service
        .authorized("GET", "/api/search?q=entry&side=head", &[])
        .json();
    let matches = unchanged["matches"].as_array().expect("matches");
    assert_eq!(matches.len(), 1, "{unchanged}");
    assert_eq!(matches[0]["changed"], "unchanged", "{unchanged}");
    assert_eq!(
        matches[0]["selector"], "symbol:src/lib.rs#entry:function",
        "{unchanged}"
    );
}

#[test]
fn search_with_no_hits_returns_an_empty_list_with_the_query_echoed() {
    let service = Service::launch("direct-call");
    service.wait_until_ready();

    let response = service.authorized("GET", "/api/search?q=no_such_symbol_anywhere", &[]);
    assert_eq!(response.status, 200, "{response:?}");
    let body = response.json();
    assert_eq!(body["q"], "no_such_symbol_anywhere");
    assert_eq!(body["matches"].as_array(), Some(&Vec::new()));
    assert_eq!(body["truncated"], false);
}

#[test]
fn malformed_search_query_is_a_400_not_a_500() {
    let service = Service::launch("direct-call");
    service.wait_until_ready();

    // An embedded NUL byte defeats SQLite's C-string binding, producing a
    // genuine FTS5 syntax error from the query text alone: the query is
    // always bound as data, never interpolated into SQL.
    let response = service.authorized("GET", "/api/search?q=foo%00bar", &[]);
    assert_eq!(response.status, 400, "{response:?}");
    let body = response.json();
    assert_eq!(body["error"]["code"], "invalid_query", "{body}");
    assert!(body["error"]["details"].is_object(), "{body}");
}

#[test]
fn search_parameters_are_refused_with_a_reason() {
    let service = Service::launch("direct-call");
    service.wait_until_ready();

    for (route, code) in [
        ("/api/search?q=helper&kind=bogus", "invalid_kind"),
        ("/api/search?q=helper&limit=0", "invalid_limit"),
        ("/api/search?q=helper&limit=not-a-number", "invalid_limit"),
        ("/api/search?q=helper&limit=99999", "unsupported_limit"),
        ("/api/search?q=helper&side=sideways", "invalid_side"),
    ] {
        let response = service.authorized("GET", route, &[]);
        assert_eq!(response.status, 400, "{route}: {response:?}");
        assert_eq!(response.json()["error"]["code"], code, "{route}");
    }
}

#[test]
fn status_reports_monotonic_progress_during_a_cold_build_of_a_large_repository() {
    // A synthetic repository rather than this workspace's own history: CI
    // checks the workspace out at depth 1, so `HEAD` has no parent there.
    let (repository, base, head) = build_large_repo(2_000);
    let cache_dir = TempDir::new().expect("create cache directory");
    let cache_dir_str = cache_dir.path().to_string_lossy().into_owned();
    let service = Service::launch_at_with(
        repository.path().to_path_buf(),
        base,
        head,
        Box::new(repository),
        &["--cache-dir", cache_dir_str.as_str()],
    );

    let mut previous = [0u64, 0u64];
    let mut ready = [false, false];
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let status = service.authorized("GET", "/api/status", &[]).json();
        for (index, side) in ["base", "head"].iter().enumerate() {
            let files_indexed = status["indexing"][*side]["files_indexed"]
                .as_u64()
                .unwrap_or_default();
            assert!(
                files_indexed >= previous[index],
                "{side} files_indexed must never regress: {} -> {files_indexed}",
                previous[index]
            );
            previous[index] = files_indexed;
            if status["indexing"][*side]["state"] == "ready" {
                ready[index] = true;
            }
        }
        if ready[0] && ready[1] {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "indexing did not finish within the deadline: {status}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(previous[0] > 0, "base indexed at least one file");
    assert!(previous[1] > 0, "head indexed at least one file");
}

#[test]
fn post_cancel_stops_a_cold_build_discards_its_cache_entry_and_a_later_request_restarts_it() {
    let (repository, base, head) = build_large_repo(4_000);
    let cache_dir = TempDir::new().expect("create cache directory");
    let cache_dir_str = cache_dir.path().to_string_lossy().into_owned();

    let service = Service::launch_at_with(
        repository.path().to_path_buf(),
        base.clone(),
        head.clone(),
        Box::new(repository),
        &["--cache-dir", cache_dir_str.as_str()],
    );

    // A cold build of 4,000 files takes long enough to reliably observe a
    // genuine side-not-ready refusal and a genuine cancellation; a
    // `direct-call`-sized fixture would too often finish before either
    // request lands.
    let first = service.authorized("GET", "/api/comparison", &[]);
    if first.status != 200 {
        assert_eq!(first.status, 409, "{first:?}");
        assert_eq!(first.json()["error"]["code"], "side_not_ready");
    }

    let cancel = service.authorized("POST", "/api/cancel", &[]);
    assert!(matches!(cancel.status, 200 | 409), "{cancel:?}");

    let deadline = Instant::now() + Duration::from_secs(60);
    let mut cancelled = false;
    loop {
        let status = service.authorized("GET", "/api/status", &[]).json();
        let states: Vec<&str> = ["base", "head"]
            .iter()
            .map(|side| {
                status["indexing"][*side]["state"]
                    .as_str()
                    .unwrap_or_default()
            })
            .collect();
        if states.contains(&"cancelled") {
            cancelled = true;
            break;
        }
        if states.iter().all(|state| *state == "ready") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the build neither cancelled nor finished within the deadline: {status}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        cancelled,
        "the build must have been cancelled rather than racing to completion; \
         raise the file count in `build_large_repo` if this is flaky"
    );

    // No half-indexed cache entry survives under either SHA.
    for sha in [base.as_str(), head.as_str()] {
        assert!(
            !cache_dir.path().join(sha).exists(),
            "a cancelled build must not leave a cache entry for {sha}"
        );
    }

    // The next request restarts the build, and it completes normally.
    let restarted = service.authorized("GET", "/api/comparison", &[]);
    assert!(matches!(restarted.status, 200 | 409), "{restarted:?}");
    service.wait_until_ready();
    let ready = service.authorized("GET", "/api/comparison", &[]);
    assert_eq!(ready.status, 200, "{ready:?}");
    for sha in [base.as_str(), head.as_str()] {
        assert!(
            cache_dir.path().join(sha).exists(),
            "the restarted build must publish a cache entry for {sha}"
        );
    }
}

#[test]
fn post_cancel_with_nothing_in_progress_is_refused() {
    let service = Service::launch("direct-call");
    service.wait_until_ready();

    let response = service.authorized("POST", "/api/cancel", &[]);
    assert_eq!(response.status, 409, "{response:?}");
    assert_eq!(response.json()["error"]["code"], "not_indexing");
}

#[test]
fn changed_symbols_reports_why_an_empty_list_is_empty() {
    // Comparing a revision to itself is a real, valid comparison with
    // nothing to report: no changed file, so no changed symbol either.
    let case = corpus::build_case("direct-call");
    let same_revision = case.base().to_string();
    let service = Service::launch_at(
        case.repository.clone(),
        same_revision.clone(),
        same_revision,
        Box::new(case),
    );
    service.wait_until_ready();

    let no_diff = service
        .authorized("GET", "/api/changed-symbols", &[])
        .json();
    assert_eq!(no_diff["symbols"].as_array().map(Vec::len), Some(0));
    assert_eq!(no_diff["reason"], "no_diff", "{no_diff}");

    // The real base/head pair has one changed symbol, which a language
    // filter that admits nothing removes entirely.
    let other = Service::launch("direct-call");
    other.wait_until_ready();
    let all_filtered = other
        .authorized(
            "GET",
            "/api/changed-symbols?language=cobol-does-not-exist",
            &[],
        )
        .json();
    assert_eq!(all_filtered["symbols"].as_array().map(Vec::len), Some(0));
    assert_eq!(all_filtered["reason"], "all_filtered", "{all_filtered}");
    assert!(
        !all_filtered["filtered_out"]
            .as_array()
            .expect("filtered_out")
            .is_empty(),
        "{all_filtered}"
    );

    // A non-empty result carries no reason: there is nothing to explain.
    let present = other.authorized("GET", "/api/changed-symbols", &[]).json();
    assert_eq!(present["symbols"].as_array().map(Vec::len), Some(1));
    assert_eq!(present["reason"], Value::Null, "{present}");
}

/// Build a throwaway repository with `file_count` files on each side, the
/// same content shifted by one, so materializing and indexing it takes long
/// enough to observe an in-progress build instead of racing it.
fn build_large_repo(file_count: usize) -> (TempDir, String, String) {
    let dir = TempDir::new().expect("create large fixture repository");
    let repo = Repository::init(dir.path()).expect("init large fixture repository");

    let base_files: Vec<(String, String)> = (0..file_count)
        .map(|index| {
            (
                format!("src/file_{index}.rs"),
                format!("pub fn marker_{index}() -> i32 {{\n    {index}\n}}\n"),
            )
        })
        .collect();
    let base_refs: Vec<(&str, &str)> = base_files
        .iter()
        .map(|(path, contents)| (path.as_str(), contents.as_str()))
        .collect();
    let base = commit_files(&repo, dir.path(), base_refs.as_slice(), "base", 0);

    let head_files: Vec<(String, String)> = (0..file_count)
        .map(|index| {
            (
                format!("src/file_{index}.rs"),
                format!("pub fn marker_{index}() -> i32 {{\n    {}\n}}\n", index + 1),
            )
        })
        .collect();
    let head_refs: Vec<(&str, &str)> = head_files
        .iter()
        .map(|(path, contents)| (path.as_str(), contents.as_str()))
        .collect();
    let head = commit_files(&repo, dir.path(), head_refs.as_slice(), "head", 1);

    (dir, base, head)
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
