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
//!    never appears in a response body. The one exception is `GET /` and its
//!    two static assets (`/ui/app.css`, `/ui/app.js`): a plain navigation or
//!    `<link>`/`<script>` fetch cannot attach a custom header, and none of
//!    the three carries repository content or a secret of its own. The
//!    embedded page instead receives the token once, through the launch
//!    URL's fragment, which the browser never transmits to the server, and
//!    holds it in memory only for the rest of the session.
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
use std::sync::{Arc, Mutex, RwLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use orbit_graph::{
    Confidence, DEFAULT_SEARCH_LIMIT, DEFAULT_SHOW_MAX_BYTES, EXTRACTOR_VERSION, IMPACT_NODE_CAP,
    Match, RefConfidence, STORE_SCHEMA_VERSION, SearchKind, SearchQuery, Selector,
};
use serde::Serialize;
use serde_json::{Value, json};
use thiserror::Error;
use tiny_http::{Header, Request, Response, Server};

use crate::changes::{ChangedSymbols, OutOfScopeEntry};
use crate::evidence::{
    DEFAULT_TIME_BUDGET_MS, EVIDENCE_DEPTH, EvidenceBounds, EvidenceCollector, EvidenceDirection,
    EvidenceQuery, MAX_EVIDENCE_DEPTH, parse_confidence,
};
use crate::filters::{FilterSet, split_terms};
use crate::report::{ExcerptMode, ReportOptions, build_report};
use crate::snapshot::{
    Comparison, ComparisonOptions, ComparisonOutcome, Snapshot, SnapshotError, SnapshotSide,
};
use crate::status::{ServiceProgress, StatusBoard};

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

/// Embedded three-pane UI shell: one HTML document, one stylesheet, one
/// framework-free JavaScript module. Compiled into the binary at build time —
/// no CDN, no build step, no network access other than this service's own
/// `/api/*` endpoints.
///
/// Static markup from the binary. No repository content is interpolated into
/// it: the JSON API is the only source of repository-derived data, and the
/// script that consumes it inserts source text as text content, never as
/// markup.
const SHELL_HTML: &str = include_str!("../ui/index.html");

/// Embedded stylesheet for [`SHELL_HTML`].
const SHELL_CSS: &str = include_str!("../ui/app.css");

/// Embedded JavaScript module for [`SHELL_HTML`].
const SHELL_JS: &str = include_str!("../ui/app.js");

/// CSP for the shell and its two static assets: same-origin only, no inline
/// script or style execution, no framing.
const SHELL_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; \
                          connect-src 'self'; img-src 'self'; base-uri 'none'; \
                          frame-ancestors 'none'";

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
    /// Snapshot cache directory; `None` selects
    /// `<repository>/.orbit-graph/explorer/snapshots`.
    pub cache_dir: Option<PathBuf>,
    /// Skip the snapshot cache entirely.
    pub no_cache: bool,
    /// Per-request wall-clock budget for traversals, in milliseconds.
    pub time_budget_ms: u64,
    /// Node cap for traversals.
    pub node_cap: usize,
}

impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            repository: PathBuf::from("."),
            base: String::new(),
            head: String::new(),
            port: 0,
            cache_dir: None,
            no_cache: false,
            time_budget_ms: DEFAULT_TIME_BUDGET_MS,
            node_cap: IMPACT_NODE_CAP,
        }
    }
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
    /// `POST /api/cancel` stopped the build. `GET /api/comparison` (or any
    /// other `/api/*` route) restarts it on the next request.
    Cancelled,
}

impl IndexingStatus {
    /// Stable label used in payloads.
    pub fn label(self) -> &'static str {
        match self {
            Self::Indexing => "indexing",
            Self::Ready => "ready",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

enum IndexState {
    Indexing,
    Ready(Box<Comparison>),
    Failed(String),
    Cancelled,
}

impl IndexState {
    fn status(&self) -> IndexingStatus {
        match self {
            Self::Indexing => IndexingStatus::Indexing,
            Self::Ready(_) => IndexingStatus::Ready,
            Self::Failed(_) => IndexingStatus::Failed,
            Self::Cancelled => IndexingStatus::Cancelled,
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
    /// Traversal bounds fixed at launch. A request may lower `depth`; it can
    /// never raise a bound, because a bound a caller controls is not a bound.
    bounds: EvidenceBounds,
    /// Cache policy fixed at launch, reused by every index build attempt,
    /// including a restart after cancellation.
    comparison_options: ComparisonOptions,
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
    /// The currently running (or most recently finished) index build. `POST
    /// /api/cancel` followed by a later `/api/*` request replaces this with a
    /// fresh handle, so it is shared rather than owned outright.
    indexer: Arc<Mutex<Option<JoinHandle<()>>>>,
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
            bounds: EvidenceBounds {
                depth: EVIDENCE_DEPTH,
                node_cap: options.node_cap.max(1),
                time_budget_ms: options.time_budget_ms,
            },
            comparison_options: ComparisonOptions {
                cache_dir: options.cache_dir.clone(),
                no_cache: options.no_cache,
            },
        });

        let server = Arc::new(server);
        let index = Arc::new(RwLock::new(IndexState::Indexing));
        let board = Arc::new(StatusBoard::new());
        let cancel = Arc::new(AtomicBool::new(false));
        let shutdown = Arc::new(AtomicBool::new(false));

        let indexer = Arc::new(Mutex::new(Some(
            spawn_indexer(
                Arc::clone(&scope),
                Arc::clone(&index),
                Arc::clone(&board),
                Arc::clone(&cancel),
            )
            .map_err(|error| ServiceError::Thread {
                name: "indexing",
                reason: error.to_string(),
            })?,
        )));

        // A service that could not start every handler would accept
        // connections it never answers, so a spawn failure is fatal rather
        // than silently absorbed.
        let live_workers = Arc::new(AtomicUsize::new(WORKER_THREADS));
        let mut workers = Vec::with_capacity(WORKER_THREADS);
        for worker in 0..WORKER_THREADS {
            let server = Arc::clone(&server);
            let scope = Arc::clone(&scope);
            let index = Arc::clone(&index);
            let board = Arc::clone(&board);
            let cancel = Arc::clone(&cancel);
            let indexer = Arc::clone(&indexer);
            let shutdown = Arc::clone(&shutdown);
            let worker_live = Arc::clone(&live_workers);
            let handle = thread::Builder::new()
                .name(format!("orbit-graph-explorer-http-{worker}"))
                .spawn(move || {
                    while !shutdown.load(Ordering::Relaxed) {
                        match server.recv() {
                            Ok(mut request) => {
                                let response =
                                    handle(&scope, &index, &board, &cancel, &indexer, &mut request);
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
        if let Some(indexer) = self
            .indexer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
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
        // Every worker has left its receive loop, so no request still in
        // flight can restart the indexer after this point.
        if let Some(indexer) = self
            .indexer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = indexer.join();
        }
    }
}

/// Start a fresh index build for `scope`, resetting `board` and `cancel`
/// first.
///
/// Used both for the initial build at [`Service::start`] and for a restart
/// after `POST /api/cancel`.
fn spawn_indexer(
    scope: Arc<Scope>,
    index: Arc<RwLock<IndexState>>,
    board: Arc<StatusBoard>,
    cancel: Arc<AtomicBool>,
) -> io::Result<JoinHandle<()>> {
    board.reset();
    cancel.store(false, Ordering::Relaxed);
    thread::Builder::new()
        .name("orbit-graph-explorer-index".to_string())
        .spawn(move || {
            let progress = ServiceProgress::new(Arc::clone(&board), Arc::clone(&cancel));
            let next = match Comparison::open_with_progress(
                scope.repository.as_path(),
                scope.base_sha.as_str(),
                scope.head_sha.as_str(),
                &scope.comparison_options,
                &progress,
            ) {
                Ok(ComparisonOutcome::Ready(comparison)) => IndexState::Ready(comparison),
                Ok(ComparisonOutcome::Cancelled) => {
                    board.mark_cancelled();
                    IndexState::Cancelled
                }
                Err(error) => {
                    board.mark_failed(error_side(&error), error.to_string().as_str());
                    IndexState::Failed(error.to_string())
                }
            };
            if let Ok(mut state) = index.write() {
                *state = next;
            }
        })
}

/// Which side a [`SnapshotError`] is attributable to, when it is known.
///
/// Most variants — an unreadable repository, an unresolvable ref, a cache
/// directory that cannot be created — are not about either side in
/// particular, so `None` marks both sides failed rather than guessing one.
fn error_side(error: &SnapshotError) -> Option<SnapshotSide> {
    match error {
        SnapshotError::Graph { side, .. } => Some(*side),
        _ => None,
    }
}

/// Restart indexing if the current attempt was cancelled.
///
/// Called before every `/api/*` request that needs a ready comparison, so a
/// cancelled build restarts on the next such request rather than requiring a
/// dedicated "resume" call. A restart that is already under way, started by a
/// concurrent request, is not duplicated: the state is re-checked once the
/// indexer lock is held.
fn maybe_restart_index(
    scope: &Arc<Scope>,
    index: &Arc<RwLock<IndexState>>,
    board: &Arc<StatusBoard>,
    cancel: &Arc<AtomicBool>,
    indexer: &Arc<Mutex<Option<JoinHandle<()>>>>,
) {
    let cancelled = matches!(
        &*index
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        IndexState::Cancelled
    );
    if !cancelled {
        return;
    }
    let mut indexer_guard = indexer
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut state = index
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !matches!(&*state, IndexState::Cancelled) {
        // A concurrent request already restarted this build.
        return;
    }
    *state = IndexState::Indexing;
    drop(state);
    match spawn_indexer(
        Arc::clone(scope),
        Arc::clone(index),
        Arc::clone(board),
        Arc::clone(cancel),
    ) {
        Ok(handle) => *indexer_guard = Some(handle),
        Err(error) => {
            if let Ok(mut state) = index.write() {
                *state = IndexState::Failed(format!("restart the indexing thread: {error}"));
            }
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

/// Every `/api/*` non-2xx response uses this envelope: `error.code` is a
/// stable snake_case identifier, `error.message` is for a human, and
/// `error.details` is a JSON object with whatever structured context the
/// caller found useful — empty when there is none.
fn error_response(status: u16, code: &str, message: &str) -> Body {
    error_response_with_details(status, code, message, json!({}))
}

fn error_response_with_details(status: u16, code: &str, message: &str, details: Value) -> Body {
    json_response(
        status,
        &json!({
            "schema_version": SERVICE_SCHEMA_VERSION,
            "error": {"code": code, "message": message, "details": details},
        }),
    )
}

/// Serve a static asset embedded in the binary, with the shell's CSP.
fn asset_response(content_type: &'static str, body: &'static str) -> Body {
    let mut response = Response::from_data(body.as_bytes().to_vec()).with_status_code(200);
    for header in [
        ("Content-Type", content_type),
        ("Cache-Control", "no-store"),
        ("X-Content-Type-Options", "nosniff"),
        ("Content-Security-Policy", SHELL_CSP),
    ] {
        if let Ok(header) = Header::from_bytes(header.0.as_bytes(), header.1.as_bytes()) {
            response.add_header(header);
        }
    }
    response
}

fn html_response(body: &'static str) -> Body {
    asset_response("text/html; charset=utf-8", body)
}

fn css_response(body: &'static str) -> Body {
    asset_response("text/css; charset=utf-8", body)
}

fn js_response(body: &'static str) -> Body {
    asset_response("text/javascript; charset=utf-8", body)
}

/// Route one request, after the three launch-scope defences have passed.
fn handle(
    scope: &Arc<Scope>,
    index: &Arc<RwLock<IndexState>>,
    board: &Arc<StatusBoard>,
    cancel: &Arc<AtomicBool>,
    indexer: &Arc<Mutex<Option<JoinHandle<()>>>>,
    request: &mut Request,
) -> Body {
    if let Some(rejection) = reject(scope, request) {
        return rejection;
    }

    let info = request_info(request);
    match (info.method.as_str(), info.path.as_str()) {
        ("GET", "/") => html_response(SHELL_HTML),
        ("GET", "/ui/app.css") => css_response(SHELL_CSS),
        ("GET", "/ui/app.js") => js_response(SHELL_JS),
        ("GET", "/api/health") => json_response(200, &health_payload(scope, index)),
        ("POST", "/api/report") => report_request(scope, index, board, request),
        ("GET", "/api/status") => json_response(200, &status_payload(scope, index, board)),
        ("POST", "/api/cancel") => cancel_route(index, board, cancel),
        ("GET", path) if path.starts_with("/api/") => {
            maybe_restart_index(scope, index, board, cancel, indexer);
            with_comparison(scope, index, board, |comparison| {
                api(scope, comparison, path, &info.query, board)
            })
        }
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

    if is_public_asset(request) {
        // The shell and its two static assets carry no repository content and
        // no secret of their own, and a plain navigation or a `<link>`/
        // `<script>` fetch cannot attach a custom header. The per-launch
        // token instead reaches the page through the launch URL fragment,
        // which the browser never sends to the server, so these three routes
        // are the one exception to the bearer-token check below.
        return None;
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

/// Whether a request addresses the embedded shell or one of its two static
/// assets: the only routes served without the per-launch bearer token.
fn is_public_asset(request: &Request) -> bool {
    if request.method().as_str() != "GET" {
        return false;
    }
    let url = request.url();
    let path = url.split('?').next().unwrap_or_default();
    matches!(path, "/" | "/ui/app.css" | "/ui/app.js")
}

/// Run `run` against a ready comparison, or report that at least one side is
/// not ready yet.
///
/// A side that is still `indexing` and a side left `cancelled` by
/// `POST /api/cancel` are reported the same way: `maybe_restart_index` runs
/// before this on every `/api/*` request, so a cancelled build is already
/// restarting again by the time this responds.
fn with_comparison(
    scope: &Scope,
    index: &RwLock<IndexState>,
    board: &StatusBoard,
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
        IndexState::Indexing | IndexState::Cancelled => json_response(
            409,
            &json!({
                "schema_version": SERVICE_SCHEMA_VERSION,
                "scope": scope_envelope(scope, None, state.status(), None),
                "error": {
                    "code": "side_not_ready",
                    "message": "At least one side is not ready yet. `GET /api/health` stays \
                                responsive while this runs; `GET /api/status` reports per-side \
                                progress. Retry once both sides report `ready`.",
                    "details": {"indexing": board.to_json()},
                },
            }),
        ),
        IndexState::Failed(reason) => json_response(
            500,
            &json!({
                "schema_version": SERVICE_SCHEMA_VERSION,
                "scope": scope_envelope(scope, None, IndexingStatus::Failed, Some(reason)),
                "error": {
                    "code": "indexing_failed",
                    "message": reason,
                    "details": {"indexing": board.to_json()},
                },
            }),
        ),
    }
}

/// Live per-side build status, for `GET /api/status`.
fn status_payload(scope: &Scope, index: &RwLock<IndexState>, board: &StatusBoard) -> Value {
    let status = index
        .read()
        .map(|state| state.status())
        .unwrap_or(IndexingStatus::Failed);
    json!({
        "schema_version": SERVICE_SCHEMA_VERSION,
        "repository": scope.repository.display().to_string(),
        "base_sha": scope.base_sha,
        "head_sha": scope.head_sha,
        "indexing_status": status.label(),
        "indexing": board.to_json(),
    })
}

/// Abort an in-progress index build.
///
/// Non-blocking: this only raises the cancellation flag the build checks
/// between files. `GET /api/status` reports the eventual transition to
/// `cancelled`, and the next `/api/*` request restarts the build.
fn cancel_route(index: &RwLock<IndexState>, board: &StatusBoard, cancel: &AtomicBool) -> Body {
    let in_progress = matches!(
        index
            .read()
            .map(|state| state.status())
            .unwrap_or(IndexingStatus::Failed),
        IndexingStatus::Indexing
    );
    if !in_progress {
        return error_response_with_details(
            409,
            "not_indexing",
            "No index build is in progress for this scope; there is nothing to cancel.",
            json!({"indexing": board.to_json()}),
        );
    }
    cancel.store(true, Ordering::Relaxed);
    json_response(
        200,
        &json!({
            "schema_version": SERVICE_SCHEMA_VERSION,
            "cancelling": true,
            "indexing": board.to_json(),
        }),
    )
}

fn api(
    scope: &Scope,
    comparison: &Comparison,
    path: &str,
    query: &BTreeMap<String, String>,
    board: &StatusBoard,
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
        "/api/comparison" => json_response(200, &comparison_payload(scope, comparison, board)),
        "/api/changed-symbols" => changed_symbols_route(scope, comparison, query),
        "/api/evidence" => evidence_route(scope, comparison, query),
        "/api/entry-points" => entry_points_route(scope, comparison, query),
        "/api/candidate-tests" => candidate_tests_route(scope, comparison, query),
        "/api/source" => source_route(scope, comparison, query),
        "/api/search" => search_route(scope, comparison, query),
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
            IndexState::Cancelled => (IndexingStatus::Cancelled, None),
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
    let identity = snapshot.index_identity();
    json!({
        "side": snapshot.side().label(),
        "requested_ref": snapshot.requested_ref(),
        "commit_sha": snapshot.commit_sha(),
        "files_indexed": snapshot.files_indexed(),
        "files_written": snapshot.materialization().files_written,
        "extractor_version": EXTRACTOR_VERSION,
        // Cache reporting: `hit` means this launch reused an entry with a
        // matching key and indexed nothing; `miss` means it built one.
        "cache": snapshot.cache_outcome().label(),
        "cache_note": snapshot.cache_note(),
        "tree_is_cached": snapshot.tree_is_cached(),
        "index_identity": {
            "extractor_version": identity.extractor_version,
            "store_schema_version": identity.store_schema_version,
            "db_path": snapshot.db_path().display().to_string(),
        },
        "prepare_ms": snapshot.prepared_in().as_millis() as u64,
        "excluded": snapshot
            .materialization()
            .excluded
            .iter()
            .map(|entry| json!({"path": entry.path, "reason": entry.reason.label()}))
            .collect::<Vec<Value>>(),
    })
}

fn comparison_payload(scope: &Scope, comparison: &Comparison, board: &StatusBoard) -> Value {
    let mut payload = scope_envelope(scope, Some(comparison), IndexingStatus::Ready, None);
    if let Some(object) = payload.as_object_mut() {
        object.insert("schema_version".to_string(), json!(SERVICE_SCHEMA_VERSION));
        object.insert("indexing".to_string(), board.to_json());
        object.insert(
            "snapshots".to_string(),
            json!([
                snapshot_payload(comparison.base()),
                snapshot_payload(comparison.head()),
            ]),
        );
        object.insert(
            "cache".to_string(),
            json!({
                "directory": comparison.cache_dir().map(|dir| dir.display().to_string()),
                "note": comparison.cache_note(),
                "key": ["commit_sha", "extractor_version", "store_schema_version"],
                "index_identity": {
                    "extractor_version": EXTRACTOR_VERSION,
                    "store_schema_version": STORE_SCHEMA_VERSION,
                },
            }),
        );
        object.insert(
            "prepare_ms".to_string(),
            json!(comparison.prepared_in().as_millis() as u64),
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

fn changed_symbols_route(
    scope: &Scope,
    comparison: &Comparison,
    query: &BTreeMap<String, String>,
) -> Body {
    let filters = filters_of(query);
    let bounds = match bounds_of(scope, query) {
        Ok(bounds) => bounds,
        Err(response) => return response,
    };
    let evidence_query = EvidenceQuery {
        min_confidence: match confidence_of(query) {
            Ok(confidence) => confidence,
            Err(response) => return response,
        },
        bounds,
        filters: filters.clone(),
        changes: None,
        direction: EvidenceDirection::Inbound,
    };

    match ChangedSymbols::compute(comparison) {
        Ok(unfiltered) => {
            // An empty list is explicit about why it is empty, rather than
            // reading the same as "no filters were applied and nothing
            // changed": no changed file produced symbol evidence at all
            // (`no_diff`), every changed file was out of the extractor's
            // scope (`all_out_of_scope`), or every changed symbol was
            // present but removed by `filters` (`all_filtered`).
            let had_symbols_before_filtering = !unfiltered.symbols.is_empty();
            let had_out_of_scope_files = !unfiltered.out_of_scope.is_empty();
            let (changed, filtered_out) = unfiltered.filtered(&filters);
            let reason = if !changed.symbols.is_empty() {
                None
            } else if had_symbols_before_filtering {
                Some("all_filtered")
            } else if had_out_of_scope_files {
                Some("all_out_of_scope")
            } else {
                Some("no_diff")
            };
            let mut payload = serialized(&changed);
            if let Some(object) = payload.as_object_mut() {
                object.insert("scope".to_string(), envelope_for(scope, comparison));
                object.insert("reason".to_string(), json!(reason));
                object.insert("filtered_out".to_string(), serialized(&filtered_out));
                object.insert(
                    "query_options".to_string(),
                    serialized(&crate::evidence::QueryOptions::new(&evidence_query)),
                );
                // `confidence` and `depth` bound evidence traversal, not the
                // change list: the pairing ladder has no confidence of its own.
                // They are accepted, echoed, and declared inapplicable here
                // rather than silently ignored.
                object.insert(
                    "inapplicable_filters".to_string(),
                    json!([
                        {
                            "filter": "confidence",
                            "reason": "A changed-symbol entry is a Git and index pairing, not a \
                                       reference, so it carries no confidence to filter on. The \
                                       floor applies to `/api/evidence` and `/api/entry-points`.",
                        },
                        {
                            "filter": "depth",
                            "reason": "Depth bounds evidence traversal. The change list is not a \
                                       traversal, so depth neither adds nor removes entries here.",
                        },
                    ]),
                );
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

/// Traversal depth for one request, bounded by [`MAX_EVIDENCE_DEPTH`].
fn depth_of(query: &BTreeMap<String, String>) -> Result<u8, Body> {
    match query.get("depth").map(String::as_str) {
        None | Some("") => Ok(EVIDENCE_DEPTH),
        Some(raw) => {
            let depth = raw.parse::<u8>().map_err(|_| {
                error_response(
                    400,
                    "unsupported_depth",
                    format!("`depth` must be a whole number of hops, not `{raw}`.").as_str(),
                )
            })?;
            if depth > MAX_EVIDENCE_DEPTH {
                return Err(error_response(
                    400,
                    "unsupported_depth",
                    format!(
                        "`depth={depth}` exceeds the maximum of {MAX_EVIDENCE_DEPTH}. A bound a \
                         request can raise without limit is not a bound."
                    )
                    .as_str(),
                ));
            }
            // `0` selects the default depth, matching `orbit_graph`.
            Ok(if depth == 0 { EVIDENCE_DEPTH } else { depth })
        }
    }
}

/// Bounds for one request: the launch node cap and time budget, with the
/// request's depth.
fn bounds_of(scope: &Scope, query: &BTreeMap<String, String>) -> Result<EvidenceBounds, Body> {
    Ok(EvidenceBounds {
        depth: depth_of(query)?,
        ..scope.bounds
    })
}

/// Presentation filters for one request.
fn filters_of(query: &BTreeMap<String, String>) -> FilterSet {
    FilterSet {
        language: query
            .get("language")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        change_kind: query
            .get("change_kind")
            .map(|value| split_terms(value.as_str()))
            .unwrap_or_default(),
        scope: query
            .get("scope")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
    }
}

fn out_of_scope_for(comparison: &Comparison) -> Vec<OutOfScopeEntry> {
    ChangedSymbols::compute(comparison)
        .map(|changed| changed.out_of_scope)
        .unwrap_or_default()
}

/// Whether `selector` resolves to an indexed symbol in `side`.
fn resolves_in(comparison: &Comparison, side: SnapshotSide, selector: &Selector) -> bool {
    comparison
        .snapshot(side)
        .graph()
        .show(selector, 1)
        .map(|view| view.is_some())
        .unwrap_or(false)
}

/// Refuse a side that cannot carry evidence for this symbol.
///
/// A removed symbol has base-side evidence only and an added symbol head-side
/// only; answering the other side with an empty result would read as "no
/// callers" instead of "not in this revision". A selector that resolves in
/// neither snapshot is answered normally, because "no path in the indexed
/// evidence" is the honest answer there.
fn reject_wrong_side(
    scope: &Scope,
    comparison: &Comparison,
    side: SnapshotSide,
    selector: &str,
) -> Option<Body> {
    let parsed = selector.parse::<Selector>().ok()?;
    if resolves_in(comparison, side, &parsed) {
        return None;
    }
    let other = match side {
        SnapshotSide::Base => SnapshotSide::Head,
        SnapshotSide::Head => SnapshotSide::Base,
    };
    if !resolves_in(comparison, other, &parsed) {
        return None;
    }
    Some(json_response(
        404,
        &json!({
            "schema_version": SERVICE_SCHEMA_VERSION,
            "scope": envelope_for(scope, comparison),
            "selector": selector,
            "snapshot": side.label(),
            "commit_sha": comparison.snapshot(side).commit_sha(),
            "evidence_sides": [other.label()],
            "error": {
                "code": "not_in_snapshot",
                "message": format!(
                    "`{selector}` does not resolve in the {} snapshot; its evidence is {} \
                     evidence. Evidence from the two revisions is reported separately and never \
                     merged.",
                    side.label(),
                    other.label()
                ),
                "details": {"selector": selector, "snapshot": side.label(), "evidence_sides": [other.label()]},
            },
        }),
    ))
}

/// Traversal direction for one `/api/evidence` request: `inbound` (the
/// default) or `outbound`. The other evidence-backed routes (entry points,
/// candidate tests) are inbound by definition and never parse this.
fn direction_of(query: &BTreeMap<String, String>) -> Result<EvidenceDirection, Body> {
    match query.get("direction").map(String::as_str) {
        None | Some("") | Some("inbound") => Ok(EvidenceDirection::Inbound),
        Some("outbound") => Ok(EvidenceDirection::Outbound),
        Some(other) => Err(error_response(
            400,
            "invalid_direction",
            format!("`direction` must be `inbound` or `outbound`, not `{other}`.").as_str(),
        )),
    }
}

/// Build the evidence query for one request, including the changed-symbol
/// slice the `change_kind` filter and the side report need.
fn evidence_query<'a>(
    scope: &Scope,
    query: &BTreeMap<String, String>,
    changes: Option<&'a ChangedSymbols>,
    direction: EvidenceDirection,
) -> Result<EvidenceQuery<'a>, Body> {
    Ok(EvidenceQuery {
        min_confidence: confidence_of(query)?,
        bounds: bounds_of(scope, query)?,
        filters: filters_of(query),
        changes,
        direction,
    })
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
    let direction = match direction_of(query) {
        Ok(direction) => direction,
        Err(response) => return response,
    };
    let changes = ChangedSymbols::compute(comparison).ok();
    let request = match evidence_query(scope, query, changes.as_ref(), direction) {
        Ok(request) => request,
        Err(response) => return response,
    };
    if let Some(response) = reject_wrong_side(scope, comparison, side, selector.as_str()) {
        return response;
    }

    let mut collector = match EvidenceCollector::new(comparison, side) {
        Ok(collector) => collector,
        Err(error) => return error_response(500, "evidence_failed", error.to_string().as_str()),
    };
    match collector.evidence(selector.as_str(), &request) {
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

fn entry_points_route(
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
    let changes = ChangedSymbols::compute(comparison).ok();
    let request = match evidence_query(scope, query, changes.as_ref(), EvidenceDirection::Inbound) {
        Ok(request) => request,
        Err(response) => return response,
    };
    if let Some(response) = reject_wrong_side(scope, comparison, side, selector.as_str()) {
        return response;
    }

    let mut collector = match EvidenceCollector::new(comparison, side) {
        Ok(collector) => collector,
        Err(error) => {
            return error_response(500, "entry_points_failed", error.to_string().as_str());
        }
    };
    match collector.entry_points(selector.as_str(), &request) {
        Ok(report) => {
            let mut payload = serialized(&report);
            if let Some(object) = payload.as_object_mut() {
                object.insert("scope".to_string(), envelope_for(scope, comparison));
                object.insert("snapshot".to_string(), json!(side.label()));
            }
            json_response(200, &payload)
        }
        Err(error) => error_response(400, "entry_points_failed", error.to_string().as_str()),
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
    let changes = ChangedSymbols::compute(comparison).ok();
    let request = match evidence_query(scope, query, changes.as_ref(), EvidenceDirection::Inbound) {
        Ok(request) => request,
        Err(response) => return response,
    };

    let unsupported = out_of_scope_for(comparison);
    let mut collector = match EvidenceCollector::new(comparison, side) {
        Ok(collector) => collector,
        Err(error) => {
            return error_response(500, "candidate_tests_failed", error.to_string().as_str());
        }
    };
    match collector.candidate_tests(selector.as_str(), &request, unsupported) {
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
                    "details": {"selector": selector, "snapshot": side.label()},
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

/// Read a request body as JSON, treating an empty body as `Value::Null`.
///
/// The report route's body is entirely optional: every field defaults, so a
/// caller that sends no body at all gets a report of every changed symbol
/// under the launch scope's bounds.
fn read_json_body(request: &mut Request) -> Result<Value, String> {
    let mut buffer = String::new();
    request
        .as_reader()
        .read_to_string(&mut buffer)
        .map_err(|error| error.to_string())?;
    if buffer.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(buffer.as_str()).map_err(|error| error.to_string())
}

/// Build [`ReportOptions`] from an optional JSON request body, bounded by the
/// launch scope's own bounds: a request may lower a bound, never raise one.
fn report_options_from_body(scope: &Scope, body: &Value) -> Result<ReportOptions, Body> {
    let mut options = ReportOptions {
        bounds: scope.bounds,
        ..ReportOptions::default()
    };
    let Some(object) = body.as_object() else {
        return Ok(options);
    };

    if let Some(selection) = object.get("selection").and_then(Value::as_array) {
        options.selection = selection
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
    }
    if let Some(excerpts) = object.get("excerpts").and_then(Value::as_str) {
        options.excerpts = ExcerptMode::parse(excerpts).ok_or_else(|| {
            error_response(
                400,
                "invalid_excerpts",
                format!(
                    "`excerpts` must be `none`, `controlled`, or `full-span`, not `{excerpts}`."
                )
                .as_str(),
            )
        })?;
    }
    if let Some(confidence) = object.get("confidence").and_then(Value::as_str) {
        options.min_confidence = parse_confidence(confidence).ok_or_else(|| {
            error_response(
                400,
                "invalid_confidence",
                format!(
                    "`confidence` must be `exact`, `import_resolved`, `same_module`, or \
                     `fuzzy_name`, not `{confidence}`."
                )
                .as_str(),
            )
        })?;
    }
    if let Some(depth) = object.get("depth").and_then(Value::as_u64) {
        let depth = u8::try_from(depth).unwrap_or(u8::MAX);
        if depth > MAX_EVIDENCE_DEPTH {
            return Err(error_response(
                400,
                "unsupported_depth",
                format!(
                    "`depth={depth}` exceeds the maximum of {MAX_EVIDENCE_DEPTH}. A bound a \
                     request can raise without limit is not a bound."
                )
                .as_str(),
            ));
        }
        options.bounds.depth = depth;
    }
    if let Some(include) = object
        .get("include_absolute_paths")
        .and_then(Value::as_bool)
    {
        options.include_absolute_paths = include;
    }
    options.filters = FilterSet {
        language: object
            .get("language")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|value| !value.is_empty()),
        change_kind: object
            .get("change_kind")
            .and_then(Value::as_str)
            .map(split_terms)
            .unwrap_or_default(),
        scope: object
            .get("scope")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|value| !value.is_empty()),
    };
    Ok(options)
}

/// `POST /api/report`: export the current comparison as a complete,
/// self-describing report. Unlike every other `/api/*` route, its options
/// come from an optional JSON body rather than the query string, because the
/// selection list and filters are more naturally structured than one query
/// string would make them.
fn report_request(
    scope: &Scope,
    index: &RwLock<IndexState>,
    board: &StatusBoard,
    request: &mut Request,
) -> Body {
    let body = match read_json_body(request) {
        Ok(body) => body,
        Err(message) => {
            return error_response(400, "invalid_request_body", message.as_str());
        }
    };
    with_comparison(scope, index, board, |comparison| {
        let options = match report_options_from_body(scope, &body) {
            Ok(options) => options,
            Err(response) => return response,
        };
        match build_report(comparison, &options) {
            Ok(report) => match serde_json::to_value(&report) {
                Ok(value) => json_response(200, &value),
                Err(_) => error_response(500, "report_failed", "failed to encode the report"),
            },
            Err(error) => error_response(500, "report_failed", error.to_string().as_str()),
        }
    })
}

/// Requests beyond this many search results are refused, not silently
/// clamped: a bound a request can raise without limit is not a bound, the
/// same rule [`depth_of`] applies to evidence traversal depth.
const MAX_SEARCH_LIMIT: usize = 200;

/// Search indexed symbols, strings, and config keys in one side's snapshot.
///
/// Every match carries the canonical selector `/api/evidence`,
/// `/api/entry-points`, `/api/candidate-tests`, and `/api/source` accept, plus
/// its status in the current comparison's changed-symbol list (`unchanged`
/// when the selector is not in that list). Malformed query text — an
/// embedded NUL byte defeats SQLite's C-string binding, for example — is a
/// `400`, never a `500`: the query text is always passed to FTS5 as bound
/// data, never interpolated into SQL.
fn search_route(scope: &Scope, comparison: &Comparison, query: &BTreeMap<String, String>) -> Body {
    let side = match side_of(query) {
        Ok(side) => side,
        Err(response) => return response,
    };
    let limit = match search_limit_of(query) {
        Ok(limit) => limit,
        Err(response) => return response,
    };
    let kind = match query.get("kind").map(String::as_str) {
        None | Some("") => None,
        Some(label) => match SearchKind::parse(label) {
            Some(kind) => Some(kind),
            None => {
                return error_response_with_details(
                    400,
                    "invalid_kind",
                    format!("`kind` must be `symbol`, `string`, or `config`, not `{label}`.")
                        .as_str(),
                    json!({"kind": label}),
                );
            }
        },
    };
    let lang = query
        .get("lang")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let raw_query = query
        .get("q")
        .map(|value| value.trim().to_string())
        .unwrap_or_default();

    let snapshot = comparison.snapshot(side);
    let result = match snapshot.graph().search(&SearchQuery {
        query: raw_query.clone(),
        kind,
        lang,
        limit: Some(limit),
    }) {
        Ok(result) => result,
        Err(error) => {
            return error_response_with_details(
                400,
                "invalid_query",
                format!("`q` is not a valid search query: {error}").as_str(),
                json!({"q": raw_query}),
            );
        }
    };

    let changed = ChangedSymbols::compute(comparison).ok();
    let truncated = result.matches.len() >= limit;
    let matches: Vec<Value> = result
        .matches
        .iter()
        .map(|matched| search_match_payload(snapshot, matched, changed.as_ref()))
        .collect();

    json_response(
        200,
        &json!({
            "schema_version": SERVICE_SCHEMA_VERSION,
            "scope": envelope_for(scope, comparison),
            "snapshot": side.label(),
            "q": raw_query,
            "limit": limit,
            "truncated": truncated,
            "truncated_by": if truncated { Some("limit") } else { None },
            "matches": matches,
        }),
    )
}

/// Search result limit for one request: [`DEFAULT_SEARCH_LIMIT`] when
/// omitted, refused above [`MAX_SEARCH_LIMIT`] rather than clamped.
fn search_limit_of(query: &BTreeMap<String, String>) -> Result<usize, Body> {
    match query.get("limit").map(String::as_str) {
        None | Some("") => Ok(DEFAULT_SEARCH_LIMIT),
        Some(raw) => {
            let limit = raw.parse::<usize>().map_err(|_| {
                error_response_with_details(
                    400,
                    "invalid_limit",
                    format!("`limit` must be a whole number, not `{raw}`.").as_str(),
                    json!({"limit": raw}),
                )
            })?;
            if limit == 0 {
                return Err(error_response_with_details(
                    400,
                    "invalid_limit",
                    "`limit` must be at least 1, not `0`.",
                    json!({"limit": raw}),
                ));
            }
            if limit > MAX_SEARCH_LIMIT {
                return Err(error_response_with_details(
                    400,
                    "unsupported_limit",
                    format!(
                        "`limit={limit}` exceeds the maximum of {MAX_SEARCH_LIMIT}. A bound a \
                         request can raise without limit is not a bound."
                    )
                    .as_str(),
                    json!({"limit": limit, "max_limit": MAX_SEARCH_LIMIT}),
                ));
            }
            Ok(limit)
        }
    }
}

/// One search match, addressed by its canonical selector and labelled with
/// its status in the current comparison's changed-symbol list.
///
/// A symbol match resolves its real kind by scanning its file's overview,
/// since `orbit_graph::Match` does not carry one and `Graph::show` requires
/// an exact kind to resolve a symbol selector at all; a string or config
/// match has no symbol identity of its own, so it is addressed by the file
/// that contains it instead. Either way the selector is exactly what the
/// other selector-addressed routes accept.
fn search_match_payload(
    snapshot: &Snapshot,
    matched: &Match,
    changed: Option<&ChangedSymbols>,
) -> Value {
    let (kind_label, path, line, label, selector) = match matched {
        Match::Symbol { name, path, line } => {
            let kind = symbol_kind_at(snapshot, path.as_str(), name.as_str());
            let selector = Selector::Symbol {
                path: path.clone(),
                symbol: name.clone(),
                kind,
            };
            ("symbol", path.clone(), Some(*line), name.clone(), selector)
        }
        Match::StringLiteral { value, path, line } => (
            "string",
            path.clone(),
            Some(*line),
            value.clone(),
            Selector::File { path: path.clone() },
        ),
        Match::Config { value, path, line } => (
            "config",
            path.clone(),
            Some(*line),
            value.clone(),
            Selector::File { path: path.clone() },
        ),
    };
    let selector_text = selector.to_string();
    let changed_status = changed
        .and_then(|changed| {
            changed
                .entries_for(selector_text.as_str())
                .into_iter()
                .next()
        })
        .map(|entry| entry.status.label())
        .unwrap_or("unchanged");
    json!({
        "kind": kind_label,
        "selector": selector_text,
        "label": label,
        "file": path,
        "line": line,
        "changed": changed_status,
    })
}

/// A symbol's kind, by scanning the overview of the file that contains it.
///
/// `"unknown"` when the file's overview no longer lists a symbol with this
/// name — the index changed underneath this request, for example — rather
/// than failing the whole search over one match.
fn symbol_kind_at(snapshot: &Snapshot, path: &str, name: &str) -> String {
    let scope = Selector::File {
        path: path.to_string(),
    };
    snapshot
        .graph()
        .overview(Some(&scope), orbit_graph::OverviewFormat::Full)
        .ok()
        .and_then(|overview| {
            overview
                .files
                .into_iter()
                .flat_map(|file| file.symbols)
                .find(|symbol| symbol.name == name)
                .map(|symbol| symbol.kind)
        })
        .unwrap_or_else(|| "unknown".to_string())
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
    writeln!(
        stderr,
        "Open: {}/#token={}",
        service.origin(),
        service.token()
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
            bounds: EvidenceBounds::default(),
            comparison_options: ComparisonOptions::default(),
        }
    }
}
