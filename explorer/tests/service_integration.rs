//! The loopback service, exercised through the real `orbit-graph-explorer`
//! executable.
//!
//! Every test here launches the packaged binary with `serve`, reads the
//! per-launch bearer token from its standard error, and drives it over a real
//! TCP socket with hand-written HTTP/1.1 requests. Nothing is asserted against
//! an in-process handler: the contract under test is what the shipped binary
//! serves on the wire.

#![allow(clippy::expect_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, ChildStderr, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

mod common;

use common::corpus;
use orbit_graph_explorer::service::ServeOptions;

/// How long to wait for indexing to finish before failing a test.
const READY_TIMEOUT: Duration = Duration::from_secs(60);

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
    assert_eq!(evidence["query_options"]["depth"], 1);
    assert_eq!(evidence["query_options"]["min_confidence"], "same_module");
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

    // Depth beyond this milestone is refused rather than silently answered at
    // depth 1.
    let deep = service.authorized(
        "GET",
        "/api/evidence?selector=symbol%3Asrc%2Flib.rs%23helper%3Afunction&depth=3",
        &[],
    );
    assert_eq!(deep.status, 400, "{deep:?}");
    assert_eq!(deep.json()["error"]["code"], "unsupported_depth");

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

/// A launched `orbit-graph-explorer serve` process.
struct Service {
    child: Mutex<Child>,
    pid: u32,
    _stderr: BufReader<ChildStderr>,
    origin: String,
    authority: String,
    token: String,
    repository: String,
    base_sha: String,
    head_sha: String,
    _case: corpus::CorpusCase,
}

impl Service {
    fn launch(case_id: &str) -> Self {
        let case = corpus::build_case(case_id);
        let repository = case
            .repository
            .canonicalize()
            .unwrap_or_else(|_| case.repository.clone());
        let base_sha = case.base().to_string();
        let head_sha = case.head().to_string();

        let mut child = Command::new(env!("CARGO_BIN_EXE_orbit-graph-explorer"))
            .args([
                "serve",
                "--repo",
                repository.to_str().expect("utf8 repository path"),
                "--base",
                base_sha.as_str(),
                "--head",
                head_sha.as_str(),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("launch orbit-graph-explorer serve");

        let stderr = child.stderr.take().expect("capture stderr");
        let (mut stderr, origin, token) = read_launch_banner(stderr);
        // Keep the pipe open for the service's lifetime: closing the read end
        // would make any later write to the child's standard error fail.
        let _ = stderr.fill_buf();
        let authority = origin
            .strip_prefix("http://")
            .expect("loopback origin")
            .to_string();
        assert!(
            authority.starts_with("127.0.0.1:"),
            "the service must bind loopback only: {origin}"
        );

        Self {
            pid: child.id(),
            child: Mutex::new(child),
            _stderr: stderr,
            origin,
            authority,
            token,
            repository: repository.to_string_lossy().into_owned(),
            base_sha,
            head_sha,
            _case: case,
        }
    }

    fn authorized(&self, method: &str, path: &str, headers: &[(&str, &str)]) -> HttpResponse {
        let authorization = format!("Bearer {}", self.token);
        let mut all = vec![("Authorization", authorization.as_str())];
        all.extend_from_slice(headers);
        self.request(method, path, all.as_slice())
    }

    fn request(&self, method: &str, path: &str, headers: &[(&str, &str)]) -> HttpResponse {
        let mut stream = TcpStream::connect(self.authority.as_str()).expect("connect to service");
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .expect("set read timeout");
        let mut request = format!("{method} {path} HTTP/1.1\r\nHost: {}\r\n", self.authority);
        for (field, value) in headers {
            request.push_str(format!("{field}: {value}\r\n").as_str());
        }
        request.push_str("Content-Length: 0\r\n\r\n");
        stream.write_all(request.as_bytes()).expect("write request");
        stream.flush().expect("flush request");

        // Read exactly `Content-Length` body bytes rather than reading to
        // end-of-stream, so a connection close cannot discard an already
        // delivered response.
        let mut response = read_response(&mut stream)
            .unwrap_or_else(|error| panic!("`{method} {path}`: {error}; {}", self.diagnose()));
        response.request_line = format!("{method} {path}");
        response
    }

    /// Describe the service process, for a failure message that distinguishes
    /// a transport hiccup from a service that died.
    fn diagnose(&self) -> String {
        let mut child = Command::new("kill")
            .arg("-0")
            .arg(self.pid.to_string())
            .status();
        let alive = matches!(&mut child, Ok(status) if status.success());
        format!(
            "service pid {} is {}; origin {}",
            self.pid,
            if alive { "alive" } else { "gone" },
            self.origin
        )
    }

    fn wait_until_ready(&self) {
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            let health = self.authorized("GET", "/api/health", &[]).json();
            match health["indexing_status"].as_str() {
                Some("ready") => return,
                Some("failed") => panic!("indexing failed: {health}"),
                _ => {}
            }
            assert!(
                Instant::now() < deadline,
                "indexing did not finish within {READY_TIMEOUT:?}"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Read the launch banner: the origin and the per-launch token, printed once.
///
/// Returns the reader so the caller can keep the pipe open.
fn read_launch_banner(stderr: ChildStderr) -> (BufReader<ChildStderr>, String, String) {
    let mut reader = BufReader::new(stderr);
    let mut origin = None;
    let mut token = None;
    for _ in 0..8 {
        let mut line = String::new();
        let read = reader.read_line(&mut line).expect("read launch banner");
        assert!(read > 0, "the service exited before printing its banner");
        if let Some(rest) = line
            .trim()
            .strip_prefix("orbit-graph-explorer listening on ")
        {
            origin = Some(rest.to_string());
        }
        if let Some(rest) = line.trim().strip_prefix("Authorization: Bearer ") {
            token = Some(rest.to_string());
        }
        if origin.is_some() && token.is_some() {
            break;
        }
    }
    let origin = origin.expect("launch banner names the service origin");
    let token = token.expect("launch banner prints the per-launch token");
    assert_eq!(token.len(), 64, "the token must carry 32 bytes of entropy");
    assert!(token.chars().all(|ch| ch.is_ascii_hexdigit()), "{token}");
    (reader, origin, token)
}

/// Read one HTTP/1.1 response: the status line, the headers, and exactly the
/// number of body bytes the response declares.
fn read_response(stream: &mut TcpStream) -> Result<HttpResponse, String> {
    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .map_err(|error| format!("read status line: {error}"))?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| format!("malformed status line: {status_line:?}"))?;

    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .map_err(|error| format!("read header line: {error}"))?;
        if read == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((field, value)) = line.split_once(':') {
            headers.push((field.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }

    let length: usize = headers
        .iter()
        .find(|(field, _)| field == "content-length")
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    reader
        .read_exact(body.as_mut_slice())
        .map_err(|error| format!("read {length}-byte body: {error}"))?;

    Ok(HttpResponse {
        status,
        headers,
        body: String::from_utf8_lossy(body.as_slice()).into_owned(),
        request_line: String::new(),
    })
}

#[derive(Debug)]
struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
    request_line: String,
}

impl HttpResponse {
    fn header(&self, field: &str) -> String {
        self.headers
            .iter()
            .find(|(name, _)| name == field)
            .map(|(_, value)| value.clone())
            .unwrap_or_default()
    }

    fn json(&self) -> Value {
        serde_json::from_str(self.body.as_str()).unwrap_or_else(|error| {
            panic!(
                "`{}` -> {} body is not JSON ({error}): {:?}",
                self.request_line, self.status, self.body
            )
        })
    }
}

/// Percent-encode a value for use in a query string.
fn percent_encode(raw: &str) -> String {
    let mut encoded = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(format!("%{byte:02X}").as_str());
        }
    }
    encoded
}
