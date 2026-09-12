//! The loopback HTTP service the change-explorer UI calls.
//!
//! One service instance serves exactly one scope, fixed at launch: one
//! repository, one base revision, one head revision. The scope is never
//! inferred from a request, and a request naming a different repository is
//! refused.
//!
//! Three defences run before any graph work happens, in this order:
//!
//! 1. **Loopback only.** The listener binds `127.0.0.1`, and a request whose
//!    `Host` header is not this service's own origin is refused, so a DNS
//!    rebinding attempt cannot reach it through a hostname.
//! 2. **Same-origin only.** A request carrying an `Origin` or `Referer` that
//!    is not this service's own origin is refused, so a page in another tab
//!    cannot drive the service with the browser's credentials.
//! 3. **Per-launch bearer token.** A fresh token is generated for every launch
//!    and printed once to standard error. It is never written to a file and
//!    never appears in a response body.
//!
//! Indexing runs on a background thread. `GET /api/health` answers from the
//! launch scope alone and therefore stays responsive while both revisions are
//! still being materialized and indexed; every other endpoint reports
//! `indexing` until the comparison is ready.
//!
//! Source text is data, never markup: `GET /api/source` returns a JSON string
//! (or a byte array for non-UTF-8 source), and no endpoint ever renders
//! repository content into HTML.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Cursor, Read};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use orbit_graph::{Confidence, DEFAULT_SHOW_MAX_BYTES, EXTRACTOR_VERSION, RefConfidence, Selector};
use serde::Serialize;
use serde_json::{Value, json};
use thiserror::Error;
use tiny_http::{Header, Request, Response, Server};

use crate::changes::{ChangedSymbols, OutOfScopeEntry};
use crate::evidence::{EvidenceCollector, parse_confidence};
use crate::snapshot::{Comparison, Snapshot, SnapshotSide};

/// Schema version of every payload this service serves.
pub const SERVICE_SCHEMA_VERSION: u32 = 1;

/// Bytes of entropy in a per-launch bearer token.
const TOKEN_BYTES: usize = 32;

/// Request-handling threads. One long request must never starve `/api/health`.
const WORKER_THREADS: usize = 4;

/// Pause after a failed accept, so a persistently failing listener cannot spin
/// a handler thread at full speed.
const RECV_ERROR_BACKOFF: Duration = Duration::from_millis(10);

/// Interval between `unblock` calls while waiting for handlers to stop.
const SHUTDOWN_POLL: Duration = Duration::from_millis(5);

/// Minimal shell served at `/` until the real UI lands.
///
/// Static markup from the binary. No repository content is interpolated into
/// it, and the UI it will be replaced by must insert source text as text
/// content, never as markup.
const PLACEHOLDER_SHELL: &str = r#"<!DOCTYPE html>
<html lang="en">
<head><meta charset="utf-8"><title>orbit-graph change explorer</title></head>
<body>
<h1>orbit-graph change explorer</h1>
<p>This build serves the JSON API only; the user interface is a later milestone.</p>
<p>Every request needs the per-launch bearer token printed to standard error at launch.</p>
<ul>
<li><code>GET /api/health</code></li>
<li><code>GET /api/comparison</code></li>
<li><code>GET /api/changed-symbols</code></li>
<li><code>GET /api/evidence?selector=&amp;side=&amp;depth=&amp;confidence=</code></li>
<li><code>GET /api/candidate-tests?selector=&amp;side=</code></li>
<li><code>GET /api/source?selector=&amp;side=</code></li>
</ul>
</body>
</html>
"#;

/// Failure surface of service startup.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ServiceError {
    /// The launch scope could not be resolved.
    #[error("{0}")]
    Scope(String),
    /// The listener could not be bound to the loopback interface.
    #[error("bind 127.0.0.1:{port}: {reason}")]
    Bind {
        /// Requested port; `0` requests an ephemeral port.
        port: u16,
        /// Failure reason.
        reason: String,
    },
    /// No operating-system randomness source was available for the token.
    #[error("generate per-launch token: {0}")]
    Token(String),
    /// A service thread could not be started.
    #[error("start the {name} thread: {reason}")]
    Thread {
        /// What the thread would have done.
        name: &'static str,
        /// Failure reason.
        reason: String,
    },
}

/// Launch scope of one service instance.
#[derive(Debug, Clone)]
pub struct ServeOptions {
    /// Repository to inspect.
    pub repository: PathBuf,
    /// Base revision, exactly as supplied.
    pub base: String,
    /// Head revision, exactly as supplied.
    pub head: String,
    /// Port to bind; `0` requests an ephemeral port.
    pub port: u16,
}

/// Whether the background indexing thread has finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexingStatus {
    /// Snapshots are still being materialized and indexed.
    Indexing,
    /// Both snapshots are indexed and queryable.
    Ready,
    /// Indexing failed; the reason is served with the status.
    Failed,
}

impl IndexingStatus {
    /// Stable label used in payloads.
    pub fn label(self) -> &'static str {
        match self {
            Self::Indexing => "indexing",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }
}

enum IndexState {
    Indexing,
    Ready(Box<Comparison>),
    Failed(String),
}

impl IndexState {
    fn status(&self) -> IndexingStatus {
        match self {
            Self::Indexing => IndexingStatus::Indexing,
            Self::Ready(_) => IndexingStatus::Ready,
            Self::Failed(_) => IndexingStatus::Failed,
        }
    }
}

/// Immutable launch scope shared by every request handler.
struct Scope {
    repository: PathBuf,
    base_ref: String,
    head_ref: String,
    base_sha: String,
    head_sha: String,
    origin: String,
    authority: String,
    token: String,
}

impl Scope {
    /// Whether a `repo` request parameter addresses the launch scope.
    ///
    /// Compared after canonicalization so an equivalent spelling of the same
    /// directory is accepted while a different repository is not. The scope is
    /// never taken from the request.
    fn owns(&self, requested: &str) -> bool {
        let requested = Path::new(requested);
        let canonical = fs::canonicalize(requested).unwrap_or_else(|_| requested.to_path_buf());
        canonical == self.repository || requested == self.repository
    }
}

/// A bound, running loopback service.
pub struct Service {
    server: Arc<Server>,
    scope: Arc<Scope>,
    index: Arc<RwLock<IndexState>>,
    address: SocketAddr,
    shutdown: Arc<AtomicBool>,
    /// Handlers that have not yet left their receive loop.
    live_workers: Arc<AtomicUsize>,
    workers: Vec<JoinHandle<()>>,
    indexer: Option<JoinHandle<()>>,
}

impl Service {
    /// Resolve the launch scope, bind the loopback listener, and start
    /// indexing in the background.
    ///
    /// Both refs are resolved to immutable commit SHAs synchronously, so the
    /// health endpoint can echo them from the first request onwards. The
    /// expensive work — materializing and indexing both revisions — runs on a
    /// background thread.
    pub fn start(options: &ServeOptions) -> Result<Self, ServiceError> {
        let repository = fs::canonicalize(options.repository.as_path())
            .unwrap_or_else(|_| options.repository.clone());
        let (workdir, base_sha, head_sha) = resolve_scope(
            repository.as_path(),
            options.base.as_str(),
            options.head.as_str(),
        )?;

        let address = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, options.port));
        let server = Server::http(address).map_err(|error| ServiceError::Bind {
            port: options.port,
            reason: error.to_string(),
        })?;
        let address = match server.server_addr().to_ip() {
            Some(address) => address,
            None => {
                return Err(ServiceError::Bind {
                    port: options.port,
                    reason: "listener is not bound to an IP address".to_string(),
                });
            }
        };
        let authority = format!("127.0.0.1:{}", address.port());
        let scope = Arc::new(Scope {
            repository: workdir,
            base_ref: options.base.clone(),
            head_ref: options.head.clone(),
            base_sha,
            head_sha,
            origin: format!("http://{authority}"),
            authority,
            token: generate_token()?,
        });

        let server = Arc::new(server);
        let index = Arc::new(RwLock::new(IndexState::Indexing));
        let shutdown = Arc::new(AtomicBool::new(false));

        let indexer = Some({
            let index = Arc::clone(&index);
            let repository = scope.repository.clone();
            let base = scope.base_sha.clone();
            let head = scope.head_sha.clone();
            thread::Builder::new()
                .name("orbit-graph-explorer-index".to_string())
                .spawn(move || {
                    let next = match Comparison::open(repository.as_path(), &base, &head) {
                        Ok(comparison) => IndexState::Ready(Box::new(comparison)),
                        Err(error) => IndexState::Failed(error.to_string()),
                    };
                    if let Ok(mut state) = index.write() {
                        *state = next;
                    }
                })
                .map_err(|error| ServiceError::Thread {
                    name: "indexing",
                    reason: error.to_string(),
                })?
        });

        // A service that could not start every handler would accept
        // connections it never answers, so a spawn failure is fatal rather
        // than silently absorbed.
        let live_workers = Arc::new(AtomicUsize::new(WORKER_THREADS));
        let mut workers = Vec::with_capacity(WORKER_THREADS);
        for worker in 0..WORKER_THREADS {
            let server = Arc::clone(&server);
            let scope = Arc::clone(&scope);
            let index = Arc::clone(&index);
            let shutdown = Arc::clone(&shutdown);
            let worker_live = Arc::clone(&live_workers);
            let handle = thread::Builder::new()
                .name(format!("orbit-graph-explorer-http-{worker}"))
                .spawn(move || {
                    while !shutdown.load(Ordering::Relaxed) {
                        match server.recv() {
                            Ok(request) => {
                                let response = handle(&scope, &index, &request);
                                let _ = request.respond(response);
                            }
                            // One failed accept must not retire a handler for
                            // the life of the process: a worker that exited
                            // here would leave the service accepting
                            // connections it never answers. `unblock` during
                            // shutdown also surfaces as an error, which the
                            // loop condition catches on the next pass.
                            Err(_) => {
                                if shutdown.load(Ordering::Relaxed) {
                                    break;
                                }
                                thread::sleep(RECV_ERROR_BACKOFF);
                            }
                        }
                    }
                    worker_live.fetch_sub(1, Ordering::Release);
                })
                .map_err(|error| {
                    live_workers.fetch_sub(1, Ordering::Release);
                    ServiceError::Thread {
                        name: "request handling",
                        reason: error.to_string(),
                    }
                })?;
            workers.push(handle);
        }

        Ok(Self {
            server,
            scope,
            index,
            address,
            shutdown,
            live_workers,
            workers,
            indexer,
        })
    }

    /// Address the listener is bound to.
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// Base URL of this service.
    pub fn origin(&self) -> &str {
        self.scope.origin.as_str()
    }

    /// Per-launch bearer token.
    ///
    /// Print it once at launch and never write it to a world-readable file.
    pub fn token(&self) -> &str {
        self.scope.token.as_str()
    }

    /// Current indexing status.
    pub fn indexing_status(&self) -> IndexingStatus {
        self.index
            .read()
            .map(|state| state.status())
            .unwrap_or(IndexingStatus::Failed)
    }

    /// Block until the process is asked to stop.
    ///
    /// The service owns both snapshot trees, so returning from this call drops
    /// them.
    pub fn run(self) {
        for worker in self.workers {
            let _ = worker.join();
        }
        if let Some(indexer) = self.indexer {
            let _ = indexer.join();
        }
    }

    /// Stop accepting requests, join every thread, and release both snapshot
    /// trees.
    ///
    /// `Server::unblock` releases exactly one thread stuck in `recv` per call,
    /// so it is called repeatedly until every handler has observed the shutdown
    /// flag and left its loop. Joining without that would deadlock on the
    /// handlers still blocked in `recv`.
    pub fn shutdown(self) {
        self.shutdown.store(true, Ordering::Relaxed);
        while self.live_workers.load(Ordering::Acquire) > 0 {
            self.server.unblock();
            thread::sleep(SHUTDOWN_POLL);
        }
        for worker in self.workers {
            let _ = worker.join();
        }
        if let Some(indexer) = self.indexer {
            let _ = indexer.join();
        }
    }
}

/// Resolve the launch repository and both refs without materializing anything.
fn resolve_scope(
    repository: &Path,
    base: &str,
    head: &str,
) -> Result<(PathBuf, String, String), ServiceError> {
    let repo = git2::Repository::discover(repository).map_err(|error| {
        ServiceError::Scope(format!(
            "{} is not a usable Git working tree: {}",
            repository.display(),
            error.message()
        ))
    })?;
    if repo.is_bare() || repo.workdir().is_none() {
        return Err(ServiceError::Scope(format!(
            "{} is bare and has no working tree",
            repository.display()
        )));
    }
    let workdir = repo
        .workdir()
        .map(|path| fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()))
        .unwrap_or_else(|| repository.to_path_buf());

    let resolve = |reference: &str| -> Result<String, ServiceError> {
        repo.revparse_single(reference.trim())
            .and_then(|object| object.peel_to_commit())
            .map(|commit| commit.id().to_string())
            .map_err(|error| {
                ServiceError::Scope(format!(
                    "reference `{reference}` did not resolve to a commit: {}",
                    error.message()
                ))
            })
    };
    let base_sha = resolve(base)?;
    let head_sha = resolve(head)?;
    Ok((workdir, base_sha, head_sha))
}

/// Generate a per-launch bearer token from operating-system randomness.
///
/// `/dev/urandom` is an endless stream, so exactly [`TOKEN_BYTES`] are read and
/// the handle is closed. There is deliberately no fallback to a time- or
/// process-derived value: a predictable token would silently remove the
/// service's only credential.
fn generate_token() -> Result<String, ServiceError> {
    let mut bytes = [0u8; TOKEN_BYTES];
    fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(&mut bytes))
        .map_err(|error| ServiceError::Token(error.to_string()))?;
    let mut token = String::with_capacity(TOKEN_BYTES * 2);
    for byte in bytes {
        token.push_str(format!("{byte:02x}").as_str());
    }
    Ok(token)
}

/// Constant-time comparison of two credentials.
fn credentials_match(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (left, right) in left.as_bytes().iter().zip(right.as_bytes()) {
        difference |= left ^ right;
    }
    difference == 0
}

/// A request's method and route, reduced to what the router needs.
struct RequestInfo {
    path: String,
    query: BTreeMap<String, String>,
    method: String,
}

fn request_info(request: &Request) -> RequestInfo {
    let url = request.url().to_string();
    let (path, raw_query) = match url.split_once('?') {
        Some((path, query)) => (path.to_string(), query.to_string()),
        None => (url, String::new()),
    };
    let mut query = BTreeMap::new();
    for pair in raw_query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        query.insert(percent_decode(key), percent_decode(value));
    }
    RequestInfo {
        path,
        query,
        method: request.method().as_str().to_string(),
    }
}

/// Decode `application/x-www-form-urlencoded` text.
fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        index += 3;
                    }
                    Err(_) => {
                        out.push(bytes[index]);
                        index += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(out.as_slice()).into_owned()
}

fn header_value<'a>(request: &'a Request, name: &'static str) -> Option<&'a str> {
    request
        .headers()
        .iter()
        .find(|header| header.field.equiv(name))
        .map(|header| header.value.as_str())
}

/// Whether an `Origin` or `Referer` value addresses this service.
fn origin_matches(scope: &Scope, value: &str) -> bool {
    let value = value.trim();
    if value == scope.origin {
        return true;
    }
    // A `Referer` carries a full URL; compare only its origin.
    value
        .strip_prefix(scope.origin.as_str())
        .is_some_and(|rest| rest.starts_with('/'))
}

type Body = Response<Cursor<Vec<u8>>>;

fn json_response(status: u16, value: &Value) -> Body {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    let mut response = Response::from_data(body).with_status_code(status);
    for header in [
        ("Content-Type", "application/json; charset=utf-8"),
        ("Cache-Control", "no-store"),
        ("X-Content-Type-Options", "nosniff"),
        ("Referrer-Policy", "no-referrer"),
        // The API serves data, never markup, and nothing may embed it.
        (
            "Content-Security-Policy",
            "default-src 'none'; frame-ancestors 'none'",
        ),
    ] {
        if let Ok(header) = Header::from_bytes(header.0.as_bytes(), header.1.as_bytes()) {
            response.add_header(header);
        }
    }
    response
}

fn error_response(status: u16, code: &str, message: &str) -> Body {
    json_response(
        status,
        &json!({
            "schema_version": SERVICE_SCHEMA_VERSION,
            "error": {"code": code, "message": message},
        }),
    )
}

fn html_response(status: u16, body: &'static str) -> Body {
    let mut response = Response::from_data(body.as_bytes().to_vec()).with_status_code(status);
    for header in [
        ("Content-Type", "text/html; charset=utf-8"),
        ("Cache-Control", "no-store"),
        ("X-Content-Type-Options", "nosniff"),
        (
            "Content-Security-Policy",
            "default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'",
        ),
    ] {
        if let Ok(header) = Header::from_bytes(header.0.as_bytes(), header.1.as_bytes()) {
            response.add_header(header);
        }
    }
    response
}

/// Route one request, after the three launch-scope defences have passed.
fn handle(scope: &Scope, index: &RwLock<IndexState>, request: &Request) -> Body {
    if let Some(rejection) = reject(scope, request) {
        return rejection;
    }

    let info = request_info(request);
    match (info.method.as_str(), info.path.as_str()) {
        ("GET", "/") => html_response(200, PLACEHOLDER_SHELL),
        ("GET", "/api/health") => json_response(200, &health_payload(scope, index)),
        ("POST", "/api/report") => error_response(
            501,
            "not_implemented",
            "Report export is a later milestone. No partial report is emitted, because a report \
             that omits its scope, bounds, and truncation flags would misstate the evidence.",
        ),
        ("GET", path) if path.starts_with("/api/") => with_comparison(scope, index, |comparison| {
            api(scope, comparison, path, &info.query)
        }),
        ("GET", _) => error_response(404, "not_found", "No such route."),
        _ => error_response(
            405,
            "method_not_allowed",
            "Unsupported method for this route.",
        ),
    }
}

/// The three defences, applied before any graph work.
fn reject(scope: &Scope, request: &Request) -> Option<Body> {
    if let Some(host) = header_value(request, "Host")
        && host.trim() != scope.authority
    {
        return Some(error_response(
            403,
            "host_mismatch",
            "This service answers only on its own loopback origin.",
        ));
    }
    for field in ["Origin", "Referer"] {
        if let Some(value) = header_value(request, field)
            && !origin_matches(scope, value)
        {
            return Some(error_response(
                403,
                "origin_mismatch",
                "Origin or Referer does not match this service's origin.",
            ));
        }
    }

    let presented = header_value(request, "Authorization")
        .and_then(|value| value.trim().strip_prefix("Bearer "))
        .map(str::trim)
        .unwrap_or_default();
    if !credentials_match(presented, scope.token.as_str()) {
        return Some(error_response(
            401,
            "unauthorized",
            "A valid `Authorization: Bearer <token>` header is required. The token is printed once \
             at launch.",
        ));
    }
    None
}

/// Run `run` against a ready comparison, or report the indexing status.
fn with_comparison(
    scope: &Scope,
    index: &RwLock<IndexState>,
    run: impl FnOnce(&Comparison) -> Body,
) -> Body {
    let Ok(state) = index.read() else {
        return error_response(
            500,
            "index_unavailable",
            "The comparison state is unavailable.",
        );
    };
    match &*state {
        IndexState::Ready(comparison) => run(comparison),
        IndexState::Indexing => json_response(
            503,
            &json!({
                "schema_version": SERVICE_SCHEMA_VERSION,
                "scope": scope_envelope(scope, None, IndexingStatus::Indexing, None),
                "error": {
                    "code": "indexing",
                    "message": "Both revisions are still being indexed. `GET /api/health` stays \
                                responsive while this runs.",
                },
            }),
        ),
        IndexState::Failed(reason) => json_response(
            500,
            &json!({
                "schema_version": SERVICE_SCHEMA_VERSION,
                "scope": scope_envelope(scope, None, IndexingStatus::Failed, Some(reason)),
                "error": {"code": "indexing_failed", "message": reason},
            }),
        ),
    }
}

fn api(
    scope: &Scope,
    comparison: &Comparison,
    path: &str,
    query: &BTreeMap<String, String>,
) -> Body {
    if let Some(requested) = query.get("repo")
        && !scope.owns(requested.as_str())
    {
        return error_response(
            403,
            "repository_out_of_scope",
            "This service is scoped to one repository, fixed at launch. The scope is never taken \
             from a request.",
        );
    }

    match path {
        "/api/comparison" => json_response(200, &comparison_payload(scope, comparison)),
        "/api/changed-symbols" => changed_symbols_route(scope, comparison),
        "/api/evidence" => evidence_route(scope, comparison, query),
        "/api/candidate-tests" => candidate_tests_route(scope, comparison, query),
        "/api/source" => source_route(scope, comparison, query),
        _ => error_response(404, "not_found", "No such route."),
    }
}

/// The scope every payload echoes: both SHAs, the mode, the dirty notice, and
/// the indexing status.
fn scope_envelope(
    scope: &Scope,
    comparison: Option<&Comparison>,
    status: IndexingStatus,
    failure: Option<&str>,
) -> Value {
    let working_tree = match comparison {
        Some(comparison) => {
            let state = comparison.working_tree();
            json!({
                "dirty": state.dirty,
                "truncated": state.truncated,
                "notice": state.notice(),
                "entries": state
                    .entries
                    .iter()
                    .map(|entry| json!({"path": entry.path, "change": entry.change.label()}))
                    .collect::<Vec<Value>>(),
            })
        }
        // The working tree is inspected when the comparison is opened, so its
        // state is not yet known while indexing runs. It is reported as unknown
        // rather than as clean.
        None => json!({"dirty": null, "truncated": false, "notice": null, "entries": []}),
    };
    let mode = comparison
        .map(|comparison| comparison.mode().label())
        .unwrap_or("direct_base_head");

    json!({
        "repository": scope.repository.display().to_string(),
        "mode": mode,
        "base": {"requested_ref": scope.base_ref, "commit_sha": scope.base_sha},
        "head": {"requested_ref": scope.head_ref, "commit_sha": scope.head_sha},
        "base_sha": scope.base_sha,
        "head_sha": scope.head_sha,
        // `direct_base_head` never rewrites the user's stated base, so the
        // effective base is the base.
        "effective_base_sha": scope.base_sha,
        "working_tree": working_tree,
        "indexing_status": status.label(),
        "indexing_error": failure,
        "extractor_version": EXTRACTOR_VERSION,
    })
}

fn health_payload(scope: &Scope, index: &RwLock<IndexState>) -> Value {
    let (status, failure) = match index.read() {
        Ok(state) => match &*state {
            IndexState::Ready(_) => (IndexingStatus::Ready, None),
            IndexState::Indexing => (IndexingStatus::Indexing, None),
            IndexState::Failed(reason) => (IndexingStatus::Failed, Some(reason.clone())),
        },
        Err(_) => (
            IndexingStatus::Failed,
            Some("state lock poisoned".to_string()),
        ),
    };
    json!({
        "schema_version": SERVICE_SCHEMA_VERSION,
        "status": "ok",
        "repository": scope.repository.display().to_string(),
        "base_sha": scope.base_sha,
        "head_sha": scope.head_sha,
        "mode": "direct_base_head",
        "indexing_status": status.label(),
        "indexing_error": failure,
    })
}

fn snapshot_payload(snapshot: &Snapshot) -> Value {
    json!({
        "side": snapshot.side().label(),
        "requested_ref": snapshot.requested_ref(),
        "commit_sha": snapshot.commit_sha(),
        "files_indexed": snapshot.files_indexed(),
        "files_written": snapshot.materialization().files_written,
        "extractor_version": EXTRACTOR_VERSION,
        "excluded": snapshot
            .materialization()
            .excluded
            .iter()
            .map(|entry| json!({"path": entry.path, "reason": entry.reason.label()}))
            .collect::<Vec<Value>>(),
    })
}

fn comparison_payload(scope: &Scope, comparison: &Comparison) -> Value {
    let mut payload = scope_envelope(scope, Some(comparison), IndexingStatus::Ready, None);
    if let Some(object) = payload.as_object_mut() {
        object.insert("schema_version".to_string(), json!(SERVICE_SCHEMA_VERSION));
        object.insert(
            "snapshots".to_string(),
            json!([
                snapshot_payload(comparison.base()),
                snapshot_payload(comparison.head()),
            ]),
        );
    }
    payload
}

fn envelope_for(scope: &Scope, comparison: &Comparison) -> Value {
    scope_envelope(scope, Some(comparison), IndexingStatus::Ready, None)
}

fn serialized<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

fn changed_symbols_route(scope: &Scope, comparison: &Comparison) -> Body {
    match ChangedSymbols::compute(comparison) {
        Ok(changed) => {
            let mut payload = serialized(&changed);
            if let Some(object) = payload.as_object_mut() {
                object.insert("scope".to_string(), envelope_for(scope, comparison));
            }
            json_response(200, &payload)
        }
        Err(error) => error_response(500, "changed_symbols_failed", error.to_string().as_str()),
    }
}

fn side_of(query: &BTreeMap<String, String>) -> Result<SnapshotSide, Body> {
    match query.get("side").map(String::as_str) {
        Some("base") => Ok(SnapshotSide::Base),
        Some("head") | None => Ok(SnapshotSide::Head),
        Some(other) => Err(error_response(
            400,
            "invalid_side",
            format!("`side` must be `base` or `head`, not `{other}`.").as_str(),
        )),
    }
}

fn confidence_of(query: &BTreeMap<String, String>) -> Result<Confidence, Body> {
    match query.get("confidence").map(String::as_str) {
        None => Ok(RefConfidence::default()),
        Some(label) => parse_confidence(label).ok_or_else(|| {
            error_response(
                400,
                "invalid_confidence",
                format!(
                    "`confidence` must be `exact`, `import_resolved`, `same_module`, or \
                     `fuzzy_name`, not `{label}`."
                )
                .as_str(),
            )
        }),
    }
}

fn selector_of(query: &BTreeMap<String, String>) -> Result<String, Body> {
    query
        .get("selector")
        .filter(|selector| !selector.is_empty())
        .cloned()
        .ok_or_else(|| {
            error_response(
                400,
                "missing_selector",
                "`selector` is required and must be a canonical selector.",
            )
        })
}

/// Depth this milestone supports, echoed back and refused if exceeded.
fn depth_of(query: &BTreeMap<String, String>) -> Result<u8, Body> {
    match query.get("depth").map(String::as_str) {
        None | Some("") | Some("0") | Some("1") => Ok(crate::evidence::EVIDENCE_DEPTH),
        Some(other) => Err(error_response(
            400,
            "unsupported_depth",
            format!(
                "This milestone answers at depth {} only; `depth={other}` was requested. \
                 Multi-hop traversal is a later milestone.",
                crate::evidence::EVIDENCE_DEPTH
            )
            .as_str(),
        )),
    }
}

fn out_of_scope_for(comparison: &Comparison) -> Vec<OutOfScopeEntry> {
    ChangedSymbols::compute(comparison)
        .map(|changed| changed.out_of_scope)
        .unwrap_or_default()
}

fn evidence_route(
    scope: &Scope,
    comparison: &Comparison,
    query: &BTreeMap<String, String>,
) -> Body {
    let selector = match selector_of(query) {
        Ok(selector) => selector,
        Err(response) => return response,
    };
    let side = match side_of(query) {
        Ok(side) => side,
        Err(response) => return response,
    };
    let confidence = match confidence_of(query) {
        Ok(confidence) => confidence,
        Err(response) => return response,
    };
    if let Err(response) = depth_of(query) {
        return response;
    }

    let mut collector = match EvidenceCollector::new(comparison, side) {
        Ok(collector) => collector,
        Err(error) => return error_response(500, "evidence_failed", error.to_string().as_str()),
    };
    match collector.evidence(selector.as_str(), confidence) {
        Ok(report) => {
            let mut payload = serialized(&report);
            if let Some(object) = payload.as_object_mut() {
                object.insert("scope".to_string(), envelope_for(scope, comparison));
            }
            json_response(200, &payload)
        }
        Err(error) => error_response(400, "evidence_failed", error.to_string().as_str()),
    }
}

fn candidate_tests_route(
    scope: &Scope,
    comparison: &Comparison,
    query: &BTreeMap<String, String>,
) -> Body {
    let selector = match selector_of(query) {
        Ok(selector) => selector,
        Err(response) => return response,
    };
    let side = match side_of(query) {
        Ok(side) => side,
        Err(response) => return response,
    };
    let confidence = match confidence_of(query) {
        Ok(confidence) => confidence,
        Err(response) => return response,
    };

    let unsupported = out_of_scope_for(comparison);
    let mut collector = match EvidenceCollector::new(comparison, side) {
        Ok(collector) => collector,
        Err(error) => {
            return error_response(500, "candidate_tests_failed", error.to_string().as_str());
        }
    };
    match collector.candidate_tests(selector.as_str(), confidence, unsupported) {
        Ok(candidates) => {
            let mut payload = serialized(&candidates);
            if let Some(object) = payload.as_object_mut() {
                object.insert("scope".to_string(), envelope_for(scope, comparison));
            }
            json_response(200, &payload)
        }
        Err(error) => error_response(400, "candidate_tests_failed", error.to_string().as_str()),
    }
}

/// Bounded source excerpt for one selector in one snapshot.
///
/// Text is a JSON string; non-UTF-8 source is a byte array. Neither is ever
/// HTML, and the caller must insert it into the DOM as text content.
fn source_route(scope: &Scope, comparison: &Comparison, query: &BTreeMap<String, String>) -> Body {
    let selector = match selector_of(query) {
        Ok(selector) => selector,
        Err(response) => return response,
    };
    let side = match side_of(query) {
        Ok(side) => side,
        Err(response) => return response,
    };
    let parsed = match selector.parse::<Selector>() {
        Ok(parsed) => parsed,
        Err(error) => {
            return error_response(400, "invalid_selector", error.to_string().as_str());
        }
    };

    let snapshot = comparison.snapshot(side);
    let view = match snapshot.graph().show(&parsed, DEFAULT_SHOW_MAX_BYTES) {
        Ok(view) => view,
        Err(error) => return error_response(500, "source_failed", error.to_string().as_str()),
    };
    let Some(view) = view else {
        return json_response(
            404,
            &json!({
                "schema_version": SERVICE_SCHEMA_VERSION,
                "scope": envelope_for(scope, comparison),
                "selector": selector,
                "snapshot": side.label(),
                "commit_sha": snapshot.commit_sha(),
                "error": {
                    "code": "not_in_snapshot",
                    "message": format!(
                        "`{selector}` does not resolve in the {} snapshot. Absence from one \
                         revision is evidence about that revision only.",
                        side.label()
                    ),
                },
            }),
        );
    };

    let (encoding, bytes_or_text) = match String::from_utf8(view.bytes.clone()) {
        Ok(text) => ("text", Value::String(text)),
        Err(_) => ("bytes", json!(view.bytes)),
    };
    json_response(
        200,
        &json!({
            "schema_version": SERVICE_SCHEMA_VERSION,
            "scope": envelope_for(scope, comparison),
            "selector": selector,
            "snapshot": side.label(),
            "commit_sha": snapshot.commit_sha(),
            "file": view.metadata.file,
            "span": {"start": view.metadata.span.start, "end": view.metadata.span.end},
            "kind": view.metadata.kind,
            "name": view.metadata.name,
            "qualified": view.metadata.qualified,
            "encoding": encoding,
            "bytes_or_text": bytes_or_text,
            "truncated": view.metadata.truncated,
            "truncated_by": if view.metadata.truncated {
                Some("source_max_bytes")
            } else {
                None
            },
            "source_max_bytes": DEFAULT_SHOW_MAX_BYTES,
        }),
    )
}

/// Print the launch banner to standard error, exactly once.
///
/// The token is a credential: it goes to the launching terminal and nowhere
/// else.
pub fn print_launch_banner(service: &Service) -> io::Result<()> {
    use std::io::Write;
    let mut stderr = io::stderr().lock();
    writeln!(
        stderr,
        "orbit-graph-explorer listening on {}",
        service.origin()
    )?;
    writeln!(stderr, "Authorization: Bearer {}", service.token())?;
    writeln!(
        stderr,
        "Loopback only; the token is per-launch and is not written to disk."
    )?;
    stderr.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn assert_shareable<T: Send + Sync>() {}

    #[test]
    fn comparison_state_is_shareable_across_request_threads() {
        assert_shareable::<Comparison>();
        assert_shareable::<Scope>();
    }

    #[test]
    fn tokens_are_unpredictable_and_hex_encoded() {
        let first = generate_token().expect("token");
        let second = generate_token().expect("token");
        assert_eq!(first.len(), TOKEN_BYTES * 2);
        assert_ne!(first, second);
        assert!(first.chars().all(|ch| ch.is_ascii_hexdigit()), "{first}");
    }

    #[test]
    fn credential_comparison_rejects_prefixes_and_lengths() {
        assert!(credentials_match("abc", "abc"));
        assert!(!credentials_match("abc", "abd"));
        assert!(!credentials_match("abc", "ab"));
        assert!(!credentials_match("", "abc"));
    }

    #[test]
    fn origin_check_accepts_only_the_service_origin() {
        let scope = test_scope();
        assert!(origin_matches(&scope, "http://127.0.0.1:9999"));
        assert!(origin_matches(&scope, "http://127.0.0.1:9999/index.html"));
        assert!(!origin_matches(&scope, "http://127.0.0.1:9998"));
        assert!(!origin_matches(&scope, "http://localhost:9999"));
        assert!(!origin_matches(&scope, "https://evil.example"));
        // A prefix that is not an origin boundary must not pass.
        assert!(!origin_matches(&scope, "http://127.0.0.1:99990"));
    }

    #[test]
    fn repository_scope_rejects_other_paths() {
        let scope = test_scope();
        assert!(scope.owns("/work/widgets"));
        assert!(!scope.owns("/work/other"));
        assert!(!scope.owns("/"));
    }

    #[test]
    fn query_strings_are_percent_decoded() {
        assert_eq!(percent_decode("symbol%3Asrc%2Flib.rs"), "symbol:src/lib.rs");
        assert_eq!(percent_decode("a+b"), "a b");
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode("%zz"), "%zz");
    }

    #[test]
    fn indexing_status_labels_are_stable() {
        assert_eq!(IndexingStatus::Indexing.label(), "indexing");
        assert_eq!(IndexingStatus::Ready.label(), "ready");
        assert_eq!(IndexingStatus::Failed.label(), "failed");
    }

    fn test_scope() -> Scope {
        Scope {
            repository: PathBuf::from("/work/widgets"),
            base_ref: "main".to_string(),
            head_ref: "feature/x".to_string(),
            base_sha: "1".repeat(40),
            head_sha: "2".repeat(40),
            origin: "http://127.0.0.1:9999".to_string(),
            authority: "127.0.0.1:9999".to_string(),
            token: "t".repeat(64),
        }
    }
}
