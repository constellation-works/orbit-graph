// orbit-graph change explorer — UI.
//
// Framework-free. No build step, no CDN, no network access beyond this
// service's own `/api/*` loopback endpoints. Source text from the API is
// always inserted as text nodes (`textContent` / `createTextNode`), never
// `innerHTML`, so repository content can never become markup.
//
// Filter state (`confidence`, `language`, `change_kind`, `depth`, `scope`),
// search state (`q`, `search_side`), and the pane-2 table/graph toggle
// (`view`) live in the URL fragment as ordinary `URLSearchParams` pairs, so
// they survive a reload. The per-launch bearer `token` is read from the same
// fragment once at startup and is never written back into it: `writeFragment`
// only ever serializes `FRAGMENT_KEYS`, so a copied or bookmarked URL after
// the first render carries filters, search, and view state, but never the
// credential.
//
// Data-contract fields this module reads from the service JSON (kept here so
// a Rust test can assert the same names against the real serializers):
//
//   comparison:  mode, base.commit_sha, head.commit_sha, working_tree.dirty,
//                working_tree.notice, indexing_status, indexing_error,
//                snapshots[].side, snapshots[].cache,
//                snapshots[].index_identity.extractor_version,
//                snapshots[].index_identity.store_schema_version
//   changed-symbols: schema_version, symbols[].status, symbols[].pairing,
//                symbols[].base.selector, symbols[].head.selector,
//                symbols[].supporting_snapshots, symbols[].base_path,
//                symbols[].head_path, symbols[].note,
//                symbols[].uncertain_candidates[].selector,
//                symbols[].uncertain_candidates[].snapshot,
//                symbols[].uncertain_candidates[].reason,
//                out_of_scope[].path, out_of_scope[].reason,
//                out_of_scope[].snapshot, filtered_out[].reason,
//                filtered_out[].explanation, filtered_out[].count,
//                filtered_out[].examples
//   evidence:    target.selector, commit_sha, query_options.depth,
//                query_options.node_cap, query_options.time_budget_ms,
//                query_options.min_confidence, query_options.source_max_bytes,
//                query_options.direction, truncated, truncated_by,
//                bounds_hit[].bound, bounds_hit[].value, filtered_out,
//                paths[].from.label, paths[].to.label, paths[].distance,
//                paths[].category, paths[].truncated, paths[].truncated_by,
//                paths[].edges[].from, paths[].edges[].from_selector,
//                paths[].edges[].from_origin, paths[].edges[].to,
//                paths[].edges[].relationship, paths[].edges[].category,
//                paths[].edges[].confidence, paths[].edges[].snapshot,
//                paths[].edges[].source.file, paths[].edges[].source.line,
//                paths[].edges[].note, no_path_reasons,
//                unresolved_callees[].name, unresolved_callees[].line,
//                unresolved_callees[].reason (outbound only; `direction=outbound`
//                on the same `/api/evidence` request fetches callee evidence,
//                ordered from the queried symbol outward)
//   entry-points: target, commit_sha, query_options, rules[].id,
//                rules[].description, entry_points[].node,
//                entry_points[].rule, entry_points[].rule_description,
//                entry_points[].distance, entry_points[].path,
//                entry_points[].note, truncated, truncated_by, bounds_hit,
//                filtered_out, no_entry_point_reasons
//   candidate-tests: candidates[].test.selector, candidates[].source,
//                candidates[].category, candidates[].note,
//                candidates[].truncated
//   source:      selector, snapshot, commit_sha, span.start, span.end,
//                encoding, bytes_or_text, truncated, truncated_by,
//                source_max_bytes, error.code, error.message
//   search:      scope, snapshot, q, limit, truncated, truncated_by,
//                matches[].kind, matches[].selector, matches[].label,
//                matches[].file, matches[].line, matches[].changed
//   status:      indexing_status, indexing.base.state,
//                indexing.base.files_seen, indexing.base.files_indexed,
//                indexing.base.languages, indexing.base.elapsed_ms,
//                indexing.head.state, indexing.head.files_seen,
//                indexing.head.files_indexed, indexing.head.languages,
//                indexing.head.elapsed_ms
//   cancel:      cancelling, indexing
//   error envelope (every non-2xx /api/* response): error.code,
//                error.message, error.details

const STATUS_ORDER = [
  "removed",
  "modified",
  "signature_changed",
  "moved",
  "renamed",
  "added",
  "uncertain",
];

// Statuses whose pane-3 default is the base-side source, with an explicit
// toggle to head when head content exists. These are exactly the statuses
// whose two sides may carry different selectors (or no head at all), so
// showing "the" source without naming a side would be ambiguous.
const BASE_DEFAULT_STATUSES = new Set(["removed", "moved", "renamed", "uncertain"]);

// Filter keys persisted in the URL fragment and sent as query parameters.
// Exactly these keys are ever written back into the fragment; `token` is
// deliberately excluded.
const FILTER_KEYS = ["confidence", "language", "change_kind", "depth", "scope"];

// Search and view-toggle state persisted in the URL fragment alongside
// filters. Same rule as `FILTER_KEYS`: exactly these keys round-trip, and
// `token` is never among them.
const SEARCH_KEYS = ["q", "search_side"];
const VIEW_KEYS = ["view"];
const FRAGMENT_KEYS = [...FILTER_KEYS, ...SEARCH_KEYS, ...VIEW_KEYS];

const POLL_INTERVAL_MS = 500;

/** Backoff schedule for `/api/status` polling while indexing is in progress. */
const STATUS_POLL_SCHEDULE_MS = [300, 500, 1000, 2000, 3000, 5000];

/** Debounce delay before a search-box keystroke triggers `/api/search`. */
const SEARCH_DEBOUNCE_MS = 250;

/** Node count above which the graph view declines to render and points at
 * the table instead: a hand-laid-out SVG readable at this size is the
 * documented limit, not a network or traversal bound. */
const GRAPH_RENDER_BUDGET = 60;

/** The per-launch bearer token, held in memory only for the life of this page. */
let authToken = null;

/** Current filter selection, as fragment/query string values (all strings). */
let currentFilters = {};

/** Current search query text and side, as fragment/query string values. */
let currentSearch = {};

/** Current pane-2 view: `"table"` or `"graph"`. */
let currentView = "table";

/**
 * Breadcrumb trail of focused symbols in pane 2, root first. The last entry
 * is the current focus. `changedSymbol` is set only for the trail's root,
 * which came from a pane-1 row and carries both sides for pane-3 defaults.
 */
let focusStack = [];

/** Most recent evidence/entry-points/candidate-tests reports for the current
 * focus, kept so the graph view can be (re)built without a network call when
 * the table/graph toggle flips. */
let lastReports = { evidence: null, outboundEvidence: null, entryPoints: null, candidateTests: null };

/** Error thrown by `apiGet`/`apiFetch` for a non-2xx or error-shaped `/api/*`
 * response, carrying the stable machine code and structured details the
 * error banner and 401/403/409 special cases need. */
class ApiError extends Error {
  constructor(status, code, message, details) {
    super(message || code || "request failed");
    this.status = status;
    this.code = code || "unknown_error";
    this.details = details || {};
  }
}

function main() {
  const rawHash = window.location.hash.replace(/^#/, "");
  const initialParams = new URLSearchParams(rawHash);
  authToken = initialParams.get("token");
  if (!authToken) {
    document.getElementById("token-gate").hidden = false;
    return;
  }
  initialParams.delete("token");
  const decoded = decodeFragment(initialParams.toString());
  currentFilters = filterSubset(decoded, FILTER_KEYS);
  currentSearch = filterSubset(decoded, SEARCH_KEYS);
  currentView = decoded.view === "graph" ? "graph" : decoded.view === "table" ? "table" : defaultView();
  // Strip the token from the visible URL and history right away, and persist
  // whatever filters/search/view (if any) were already present — never the
  // credential.
  writeFragment();

  document.getElementById("app").hidden = false;
  populateFilterForm(currentFilters);
  if (currentSearch.q) {
    document.getElementById("search-input").value = currentSearch.q;
  }
  document.getElementById("search-side").value = currentSearch.search_side || "head";

  setupFilterBar();
  setupSearch();
  setupViewToggle();
  setupKeyboardHelp();
  setupPaneSwitcher();
  setupIndexingProgress();
  enableArrowNavigation(document.getElementById("change-groups"));
  enableArrowNavigation(document.getElementById("evidence-primary"));
  enableArrowNavigation(document.getElementById("evidence-heuristic"));
  enableArrowNavigation(document.getElementById("callees-primary"));
  enableArrowNavigation(document.getElementById("callees-heuristic"));
  enableArrowNavigation(document.getElementById("entry-points-list"));
  enableArrowNavigation(document.getElementById("candidate-tests"));

  run().catch((error) => {
    setText(document.getElementById("indexing-notice"), `Failed to load: ${error.message}`);
    showErrorBanner(error);
  });
}

/** The table view is the accessible, always-available fallback: it is the
 * default on a narrow viewport, when the user has asked for reduced motion,
 * or when this browser has no SVG support. */
function defaultView() {
  const narrow = window.matchMedia && window.matchMedia("(max-width: 720px)").matches;
  const reducedMotion =
    window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  const hasSvg = typeof document.createElementNS === "function";
  return narrow || reducedMotion || !hasSvg ? "table" : "graph";
}

function filterSubset(source, keys) {
  const out = {};
  for (const key of keys) {
    if (source[key] !== undefined) out[key] = source[key];
  }
  return out;
}

/** Whether the first successful `/api/changed-symbols` load has happened.
 * Gates the "waiting for index" placeholders in panes 1–3: once real content
 * has loaded once, a later non-ready status (after a user-triggered cancel,
 * for example) must not clobber it. */
let initialLoadComplete = false;

const DEFAULT_RELATIONSHIP_EMPTY_TEXT =
  "Select a changed symbol from pane 1, or use search, to see its evidence.";
const DEFAULT_SOURCE_EMPTY_TEXT = "Select a changed symbol or an evidence row to see source.";

async function run() {
  const comparison = await pollComparisonUntilReady();
  renderHeader(comparison);
  const changed = await apiGet("/api/changed-symbols", activeFilterParams(currentFilters));
  initialLoadComplete = true;
  renderChangeList(changed);
}

/** Poll `/api/comparison` until indexing finishes, rendering the header each time. */
async function pollComparisonUntilReady() {
  for (;;) {
    const body = await apiGetRaw("/api/comparison");
    const scope = scopeOf(body);
    renderHeader(body);
    if (scope.indexing_status === "ready") {
      return body;
    }
    if (scope.indexing_status === "failed") {
      throw new Error(scope.indexing_error || "indexing failed");
    }
    await sleep(POLL_INTERVAL_MS);
  }
}

/** Either payload shape carries the scope fields directly, or nested under `scope`. */
function scopeOf(body) {
  return body && typeof body.scope === "object" && body.scope !== null ? body.scope : body;
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

// ---------------------------------------------------------------------------
// Filters and URL fragment persistence
// ---------------------------------------------------------------------------

/** Parse known fragment keys — filters, search, and the table/graph toggle —
 * out of a fragment (or query) string. Unknown keys, including a stray
 * `token`, are ignored. */
function decodeFragment(raw) {
  const params = new URLSearchParams(raw);
  const values = {};
  for (const key of FRAGMENT_KEYS) {
    const value = params.get(key);
    if (value !== null && value !== "") {
      values[key] = value;
    }
  }
  return values;
}

/** Serialize only `FRAGMENT_KEYS`. The token never round-trips through here. */
function encodeFragment(values) {
  const params = new URLSearchParams();
  for (const key of FRAGMENT_KEYS) {
    const value = values[key];
    if (value !== undefined && value !== null && String(value).length > 0) {
      params.set(key, value);
    }
  }
  return params.toString();
}

/** The fragment's full current state: filters, search, and the active view. */
function fragmentState() {
  return { ...currentFilters, ...currentSearch, view: currentView };
}

function writeFragment() {
  const query = encodeFragment(fragmentState());
  const url = window.location.pathname + window.location.search + (query.length > 0 ? `#${query}` : "");
  history.replaceState(null, "", url);
}

/** Non-empty filter values as request query parameters. */
function activeFilterParams(filters) {
  const params = {};
  for (const key of FILTER_KEYS) {
    const value = filters[key];
    if (value !== undefined && value !== null && String(value).length > 0) {
      params[key] = value;
    }
  }
  return params;
}

function populateFilterForm(filters) {
  document.getElementById("filter-confidence").value = filters.confidence || "";
  document.getElementById("filter-language").value = filters.language || "";
  document.getElementById("filter-depth").value = filters.depth || "";
  document.getElementById("filter-scope").value = filters.scope || "";
  renderChangeKindOptions(filters);
}

function renderChangeKindOptions(filters) {
  const container = document.getElementById("filter-change-kind-options");
  clear(container);
  const selected = new Set(
    (filters.change_kind || "")
      .split(",")
      .map((value) => value.trim())
      .filter((value) => value.length > 0),
  );
  for (const status of STATUS_ORDER) {
    const checkbox = el("input", {
      attrs: { type: "checkbox", id: `filter-change-kind-${status}`, name: "change_kind", value: status },
    });
    checkbox.checked = selected.has(status);
    const label = el("label", { className: "filter-checkbox" }, [
      checkbox,
      el("span", { text: statusLabel(status) }),
    ]);
    container.appendChild(label);
  }
}

function readFiltersFromForm() {
  const confidence = document.getElementById("filter-confidence").value.trim();
  const language = document.getElementById("filter-language").value.trim();
  const depth = document.getElementById("filter-depth").value.trim();
  const scope = document.getElementById("filter-scope").value.trim();
  const changeKind = Array.from(
    document.querySelectorAll('#filter-change-kind-options input[type="checkbox"]:checked'),
  )
    .map((box) => box.value)
    .join(",");

  const filters = {};
  if (confidence) filters.confidence = confidence;
  if (language) filters.language = language;
  if (changeKind) filters.change_kind = changeKind;
  if (depth) filters.depth = depth;
  if (scope) filters.scope = scope;
  return filters;
}

function setupFilterBar() {
  const form = document.getElementById("filter-bar");
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    currentFilters = readFiltersFromForm();
    writeFragment();
    refreshAfterFilterChange();
  });
  document.getElementById("filter-clear").addEventListener("click", () => {
    clearFilters();
  });
}

function clearFilters() {
  currentFilters = {};
  populateFilterForm(currentFilters);
  writeFragment();
  refreshAfterFilterChange();
}

/** Re-query pane 1, and pane 2 if a symbol is currently focused, under the
 * current filters. Clearing filters restores everything the same way. */
function refreshAfterFilterChange() {
  apiGet("/api/changed-symbols", activeFilterParams(currentFilters))
    .then(renderChangeList)
    .catch((error) => {
      setText(document.getElementById("indexing-notice"), `Failed to reload changed symbols: ${error.message}`);
      showErrorBanner(error);
    });

  if (focusStack.length > 0) {
    const top = focusStack[focusStack.length - 1];
    if (top.selector) {
      loadFocusEvidence(top).catch((error) => {
        setText(document.getElementById("evidence-bounds"), `Failed to load evidence: ${error.message}`);
        showErrorBanner(error);
      });
    }
  }
}

// ---------------------------------------------------------------------------
// Fetching
// ---------------------------------------------------------------------------

function buildUrl(path, params) {
  const search = new URLSearchParams();
  for (const [key, value] of Object.entries(params || {})) {
    if (value !== undefined && value !== null) {
      search.set(key, value);
    }
  }
  const query = search.toString();
  return query.length > 0 ? `${path}?${query}` : path;
}

/** Fetch JSON over `method`, returning both the HTTP status and the parsed
 * body whatever that status was. */
async function apiFetch(method, path, params, body) {
  const response = await fetch(buildUrl(path, params), {
    method,
    headers: { Authorization: `Bearer ${authToken}` },
    cache: "no-store",
    credentials: "same-origin",
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });
  const parsed = await response.json().catch(() => ({}));
  return { status: response.status, body: parsed };
}

/** Fetch JSON, returning the parsed body whatever the HTTP status was. Used
 * where a non-ready or error-shaped envelope is itself the expected shape to
 * inspect (the comparison-readiness poll, source panels with their own
 * inline error rendering). */
async function apiGetRaw(path, params) {
  const { body } = await apiFetch("GET", path, params);
  return body;
}

/** Fetch JSON and throw an [`ApiError`] on a non-2xx status or an
 * error-shaped body, carrying the stable code and structured details every
 * `/api/*` error envelope defines. */
async function apiGet(path, params) {
  const { status, body } = await apiFetch("GET", path, params);
  if (status >= 400 || (body && body.error)) {
    const error = (body && body.error) || {};
    throw new ApiError(status, error.code, error.message, error.details);
  }
  return body;
}

/** `POST` with a JSON body, throwing an [`ApiError`] the same way `apiGet`
 * does. */
async function apiPost(path, params, requestBody) {
  const { status, body } = await apiFetch("POST", path, params, requestBody);
  if (status >= 400 || (body && body.error)) {
    const error = (body && body.error) || {};
    throw new ApiError(status, error.code, error.message, error.details);
  }
  return body;
}

// ---------------------------------------------------------------------------
// Error banners — every `/api/*` error envelope renders here, dismissibly,
// with its stable code.
// ---------------------------------------------------------------------------

let bannerSequence = 0;

/** Render `error` as a dismissible banner in `#error-banner-region`. A
 * `401`/`403` explains the token/origin situation and how to relaunch; a
 * `409` (indexing not finished) states the current per-side indexing state
 * from the error envelope's own `details.indexing`, rather than a second
 * request. */
function showErrorBanner(error) {
  if (!(error instanceof ApiError)) {
    // A plain network/parse failure still gets a banner, with a synthetic
    // code so the UI never renders an error silently.
    error = new ApiError(0, "request_failed", error && error.message, {});
  }
  const region = document.getElementById("error-banner-region");
  const id = `error-banner-${(bannerSequence += 1)}`;

  let message = error.message || error.code;
  if (error.status === 401) {
    message =
      `${message} Reopen the URL printed to standard error when \`orbit-graph-explorer serve\` ` +
      "started — the token is per-launch and this page's copy may be stale.";
  } else if (error.status === 403) {
    message =
      `${message} This browser tab's origin does not match the service's loopback origin. ` +
      "Open the launch URL directly, without a proxy or a different host/port.";
  } else if (error.status === 409) {
    const indexing = error.details && error.details.indexing;
    if (indexing) {
      message = `${message} base: ${indexing.base.state}, head: ${indexing.head.state}.`;
    }
  }

  const banner = el("div", { className: "error-banner", attrs: { role: "alert", id } }, [
    el("div", { className: "error-banner-text" }, [
      el("span", { className: "error-banner-code", text: error.code }),
      el("span", { text: message }),
    ]),
  ]);
  const dismiss = el("button", {
    className: "error-banner-dismiss",
    attrs: { type: "button", "aria-label": "Dismiss this error" },
    text: "Dismiss",
  });
  dismiss.addEventListener("click", () => banner.remove());
  banner.appendChild(dismiss);
  region.appendChild(banner);
}

// ---------------------------------------------------------------------------
// Selector parsing (client-side mirror of the canonical selector grammar)
// ---------------------------------------------------------------------------

function parseSelector(selector) {
  if (typeof selector !== "string" || selector.length === 0) {
    return { kind: "unknown", path: "", name: "", symbolKind: "" };
  }
  if (selector.startsWith("symbol:")) {
    const rest = selector.slice("symbol:".length);
    const hashIndex = rest.lastIndexOf("#");
    if (hashIndex === -1) {
      return { kind: "symbol", path: rest, name: "", symbolKind: "" };
    }
    const path = rest.slice(0, hashIndex);
    const tail = rest.slice(hashIndex + 1);
    const colonIndex = tail.lastIndexOf(":");
    const name = colonIndex === -1 ? tail : tail.slice(0, colonIndex);
    const symbolKind = colonIndex === -1 ? "" : tail.slice(colonIndex + 1);
    return { kind: "symbol", path, name, symbolKind };
  }
  if (selector.startsWith("file:")) {
    return { kind: "file", path: selector.slice("file:".length), name: "", symbolKind: "" };
  }
  if (selector.startsWith("module:")) {
    return { kind: "module", path: "", name: selector.slice("module:".length), symbolKind: "" };
  }
  return { kind: "unknown", path: selector, name: "", symbolKind: "" };
}

// ---------------------------------------------------------------------------
// DOM helpers — text nodes only, never innerHTML.
// ---------------------------------------------------------------------------

function setText(node, text) {
  node.textContent = text;
}

function clear(node) {
  while (node.firstChild) {
    node.removeChild(node.firstChild);
  }
}

function el(tag, options, children) {
  const node = document.createElement(tag);
  if (options) {
    if (options.className) node.className = options.className;
    if (options.text !== undefined) node.textContent = options.text;
    if (options.attrs) {
      for (const [name, value] of Object.entries(options.attrs)) {
        node.setAttribute(name, value);
      }
    }
  }
  for (const child of children || []) {
    if (child) node.appendChild(child);
  }
  return node;
}

function badge(text) {
  return el("span", { className: "badge", text });
}

/** Roving-focus arrow-key navigation across every `.row-button` in `container`,
 * including nested hop, focus, and entry-point buttons. */
function enableArrowNavigation(container) {
  container.addEventListener("keydown", (event) => {
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
    const buttons = Array.from(container.querySelectorAll(".row-button"));
    const index = buttons.indexOf(document.activeElement);
    if (index === -1) return;
    event.preventDefault();
    const delta = event.key === "ArrowDown" ? 1 : -1;
    const next = buttons[(index + delta + buttons.length) % buttons.length];
    next.focus();
  });
}

/** A read-only, focus-to-select text field: the simplest keyboard-reachable
 * "copyable text" that needs no clipboard permission. */
function copyableSelectorRow(sideLabel, selector) {
  const input = el("input", {
    className: "copyable-selector",
    attrs: {
      type: "text",
      readonly: "readonly",
      "aria-label": `${sideLabel} selector`,
    },
  });
  input.value = selector;
  input.addEventListener("focus", () => input.select());
  return el("div", { className: "selector-row" }, [
    el("span", { className: "row-name", text: `${sideLabel}:` }),
    input,
  ]);
}

// ---------------------------------------------------------------------------
// Header: scope, dirty notice, indexing status, cache/index status
// ---------------------------------------------------------------------------

function renderHeader(body) {
  const scope = scopeOf(body);
  setText(document.getElementById("scope-base-sha"), shortSha(scope.base_sha));
  document.getElementById("scope-base-sha").title = scope.base_sha || "";
  setText(document.getElementById("scope-head-sha"), shortSha(scope.head_sha));
  document.getElementById("scope-head-sha").title = scope.head_sha || "";
  setText(document.getElementById("scope-mode"), scope.mode || "unknown");

  const dirtyNotice = document.getElementById("dirty-notice");
  const notice = scope.working_tree && scope.working_tree.notice;
  if (notice) {
    setText(dirtyNotice, `⚠ ${notice}`);
    dirtyNotice.hidden = false;
  } else {
    dirtyNotice.hidden = true;
  }

  const indexingNotice = document.getElementById("indexing-notice");
  if (scope.indexing_status === "ready") {
    setText(indexingNotice, "");
  } else if (scope.indexing_status === "failed") {
    setText(indexingNotice, `Indexing failed: ${scope.indexing_error || "unknown reason"}`);
  } else {
    setText(indexingNotice, "Cold-launch indexing both revisions…");
  }

  renderCacheStatus(body);
}

/** Per-side cache hit/miss and index identity, shown once both snapshots are
 * ready. `body.snapshots` is present only on the flat, ready comparison
 * payload — never on the nested indexing/failed envelope. */
function renderCacheStatus(body) {
  const container = document.getElementById("cache-status");
  clear(container);
  if (!Array.isArray(body.snapshots) || body.snapshots.length === 0) {
    container.hidden = true;
    return;
  }
  container.hidden = false;
  for (const snapshot of body.snapshots) {
    const identity = snapshot.index_identity || {};
    container.appendChild(el("dt", { text: snapshot.side || "?" }));
    container.appendChild(
      el("dd", {
        text:
          `cache ${snapshot.cache || "unknown"}, ` +
          `extractor v${identity.extractor_version ?? "?"}, ` +
          `schema v${identity.store_schema_version ?? "?"}`,
      }),
    );
  }
}

function shortSha(sha) {
  return typeof sha === "string" && sha.length > 0 ? sha.slice(0, 7) : "unknown";
}

// ---------------------------------------------------------------------------
// Cold-launch indexing progress, cancel, and retry
// ---------------------------------------------------------------------------

/** Whether `/api/status` polling should keep running: while a build is
 * indexing, and for one more poll after cancellation so the "cancelled"
 * state itself is rendered before the loop stops. */
let indexingPollActive = true;

function setupIndexingProgress() {
  document.getElementById("cancel-index-button").addEventListener("click", () => {
    apiPost("/api/cancel").catch((error) => showErrorBanner(error));
  });
  document.getElementById("retry-index-button").addEventListener("click", () => {
    document.getElementById("retry-index-button").hidden = true;
    // Any `/api/*` route other than `/api/status` and `/api/cancel` restarts
    // a cancelled build; `/api/comparison` is the cheapest such request.
    apiGetRaw("/api/comparison").catch(() => {});
    indexingPollActive = true;
    pollStatusWithBackoff();
  });
  pollStatusWithBackoff();
}

async function pollStatusWithBackoff() {
  let attempt = 0;
  while (indexingPollActive) {
    let status;
    try {
      status = await apiGetRaw("/api/status");
    } catch (error) {
      showErrorBanner(error);
      return;
    }
    renderIndexingProgress(status);
    if (status.indexing_status === "ready" || status.indexing_status === "failed") {
      return;
    }
    if (status.indexing_status === "cancelled") {
      indexingPollActive = false;
      return;
    }
    const delay = STATUS_POLL_SCHEDULE_MS[Math.min(attempt, STATUS_POLL_SCHEDULE_MS.length - 1)];
    attempt += 1;
    await sleep(delay);
  }
}

function renderIndexingProgress(status) {
  const panel = document.getElementById("indexing-progress");
  const indexing = status.indexing || {};
  const cancelButton = document.getElementById("cancel-index-button");
  const retryButton = document.getElementById("retry-index-button");

  updateWaitingForIndexPlaceholders(status);

  if (status.indexing_status === "ready") {
    panel.hidden = true;
    return;
  }
  panel.hidden = false;

  for (const side of ["base", "head"]) {
    const report = indexing[side] || {};
    const row = panel.querySelector(`.side-progress[data-side="${side}"]`);
    if (!row) continue;
    setText(row.querySelector(".side-progress-state"), `state: ${report.state || "pending"}`);
    setText(
      row.querySelector(".side-progress-files"),
      `files ${report.files_indexed ?? 0}/${report.files_seen ?? 0}`,
    );
    setText(
      row.querySelector(".side-progress-languages"),
      `languages: ${(report.languages || []).join(", ") || "none yet"}`,
    );
    setText(row.querySelector(".side-progress-elapsed"), `${report.elapsed_ms ?? 0}ms elapsed`);
  }

  const cancelled = status.indexing_status === "cancelled";
  cancelButton.hidden = cancelled;
  retryButton.hidden = !cancelled;
}

/** Panes 1–3 show an explicit "waiting for index" placeholder — never a bare
 * spinner — while cold-launch indexing runs, and recover automatically once
 * `run()`'s own poll loop marks the first load complete. Only touches the
 * placeholders before that first load, so a later cancel cannot clobber
 * content already on screen. */
function updateWaitingForIndexPlaceholders(status) {
  if (initialLoadComplete) return;
  const changeEmpty = document.getElementById("change-empty");
  if (status.indexing_status === "ready") {
    changeEmpty.hidden = true;
    setText(document.getElementById("relationship-empty"), DEFAULT_RELATIONSHIP_EMPTY_TEXT);
    setText(document.getElementById("source-empty"), DEFAULT_SOURCE_EMPTY_TEXT);
    return;
  }
  const indexing = status.indexing || {};
  const sideText = (label) => `${label}: ${(indexing[label] && indexing[label].state) || "pending"}`;
  const waitingText = `Waiting for index — ${sideText("base")}, ${sideText("head")}.`;
  clear(changeEmpty);
  changeEmpty.appendChild(el("p", { text: waitingText }));
  changeEmpty.hidden = false;
  setText(document.getElementById("relationship-empty"), waitingText);
  setText(document.getElementById("source-empty"), waitingText);
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

/** Rough client-side language guess from a file extension: `/api/search`
 * matches do not carry a language field, only `file`, so this is a display
 * hint only — the authoritative per-file language lives in the index the
 * search itself already ran against. */
const LANGUAGE_BY_EXTENSION = {
  rs: "rust",
  py: "python",
  js: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  ts: "typescript",
  tsx: "typescript",
  jsx: "javascript",
  go: "go",
  java: "java",
  rb: "ruby",
  c: "c",
  h: "c",
  cpp: "c++",
  hpp: "c++",
  cc: "c++",
  cs: "c#",
  toml: "toml",
  json: "json",
  yaml: "yaml",
  yml: "yaml",
};

function languageOf(file) {
  const match = /\.([^./]+)$/.exec(file || "");
  const extension = match ? match[1].toLowerCase() : "";
  return LANGUAGE_BY_EXTENSION[extension] || (extension ? extension : "unknown");
}

let searchDebounceHandle = null;

function setupSearch() {
  const input = document.getElementById("search-input");
  const side = document.getElementById("search-side");
  const results = document.getElementById("search-results");
  const form = document.getElementById("search-bar");

  form.addEventListener("submit", (event) => event.preventDefault());

  const trigger = () => {
    currentSearch = {};
    const q = input.value.trim();
    if (q) currentSearch.q = q;
    if (side.value && side.value !== "head") currentSearch.search_side = side.value;
    writeFragment();
    if (searchDebounceHandle) clearTimeout(searchDebounceHandle);
    searchDebounceHandle = setTimeout(() => runSearch(q, side.value), SEARCH_DEBOUNCE_MS);
  };

  input.addEventListener("input", trigger);
  side.addEventListener("change", trigger);

  document.addEventListener("click", (event) => {
    if (!form.contains(event.target)) {
      results.hidden = true;
    }
  });

  if (currentSearch.q) {
    runSearch(currentSearch.q, side.value);
  }
}

async function runSearch(query, side) {
  const results = document.getElementById("search-results");
  if (!query) {
    clear(results);
    results.hidden = true;
    return;
  }
  let response;
  try {
    response = await apiGet("/api/search", { q: query, side });
  } catch (error) {
    showErrorBanner(error);
    return;
  }
  renderSearchResults(response.matches || []);
}

function renderSearchResults(matches) {
  const container = document.getElementById("search-results");
  clear(container);
  if (matches.length === 0) {
    container.appendChild(el("p", { className: "search-result-empty", text: "No matches." }));
    container.hidden = false;
    return;
  }
  for (const match of matches) {
    const changedLabel =
      match.changed && match.changed !== "unchanged"
        ? statusLabel(match.changed)
        : "not changed in this comparison";
    const button = el(
      "button",
      { className: "search-result", attrs: { type: "button", role: "option" } },
      [
        el("span", { className: "row-line" }, [
          el("span", { className: "row-name", text: match.label || match.selector }),
          badge(match.kind),
          badge(languageOf(match.file)),
          badge(changedLabel),
        ]),
        el("span", {
          className: "row-file",
          text: `${match.file || "?"}${match.line ? `:${match.line}` : ""}`,
        }),
      ],
    );
    button.addEventListener("click", () => focusFromSearchResult(match));
    container.appendChild(button);
  }
  container.hidden = false;
}

/** A chosen search result focuses pane 2 the same way a pane-1 row or a hop
 * does, whether or not it names a changed symbol: an unchanged selector is
 * still addressable evidence, just labelled as such. */
function focusFromSearchResult(match) {
  document.getElementById("search-results").hidden = true;
  const side = document.getElementById("search-side").value;
  focusStack = [
    {
      selector: match.selector,
      side,
      label: match.label || match.selector,
      changedSymbol: null,
    },
  ];
  renderBreadcrumb();
  loadFocus(focusStack[0]).catch((error) => {
    setText(document.getElementById("evidence-bounds"), `Failed to load evidence: ${error.message}`);
    showErrorBanner(error);
  });
}

// ---------------------------------------------------------------------------
// "Hidden by filters" — shared by the change list, evidence, and entry points
// ---------------------------------------------------------------------------

function renderFilteredOut(container, filteredOut) {
  clear(container);
  const entries = filteredOut || [];
  if (entries.length === 0) {
    return;
  }
  const total = entries.reduce((sum, entry) => sum + entry.count, 0);
  const summary = el("summary", {
    text: `Hidden by filters: ${total} item(s) across ${entries.length} reason(s)`,
  });
  const list = el("ul", { className: "filtered-out-list" });
  for (const entry of entries) {
    const rowChildren = [
      el("div", { className: "row-line" }, [
        el("span", { className: "row-name", text: `${entry.reason.replace(/_/g, " ")}: ${entry.count}` }),
      ]),
      el("p", { className: "filtered-out-explanation", text: entry.explanation }),
    ];
    if (entry.examples && entry.examples.length > 0) {
      const exampleList = el("ul", { className: "candidate-list" });
      for (const example of entry.examples) {
        exampleList.appendChild(el("li", null, [el("span", { className: "row-name", text: example })]));
      }
      rowChildren.push(
        el("details", null, [el("summary", { text: `${entry.examples.length} example(s)` }), exampleList]),
      );
    }
    list.appendChild(el("li", null, rowChildren));
  }
  container.appendChild(el("details", { className: "filtered-out" }, [summary, list]));
}

/** Bounds and filters currently in force, with every value stated inline. */
function boundsLine(bounds, skippedLowConfidence) {
  let text =
    `Bounds in force: depth ${bounds.depth}, node cap ${bounds.node_cap}, ` +
    `confidence ≥ ${bounds.min_confidence}, time budget ${bounds.time_budget_ms}ms, ` +
    `source excerpts ≤ ${bounds.source_max_bytes} bytes`;
  if (skippedLowConfidence) {
    text += `; ${skippedLowConfidence} candidate(s) excluded by the confidence floor`;
  }
  return `${text}.`;
}

/** "N found" or, when a bound cut the result, "at least N found; truncated by
 * <bound>" with every bound's value stated. */
function truncationSummary(report, count, singular, plural) {
  const noun = count === 1 ? singular : plural;
  if (!report.truncated) {
    return `${count} ${noun} found.`;
  }
  const hits = (report.bounds_hit || []).map((hit) => `${hit.bound}=${hit.value}`).join(", ");
  return (
    `At least ${count} ${noun} found; truncated by ${report.truncated_by}` +
    (hits ? ` (bounds hit: ${hits})` : "") +
    "."
  );
}

// ---------------------------------------------------------------------------
// Pane 1 — change list
// ---------------------------------------------------------------------------

/** Human copy for each empty-pane-1 reason the service reports, keyed by
 * `changed.reason`. `all_filtered` additionally gets a "clear filters" link,
 * since that reason is the one an in-page action can resolve. */
function changeEmptyStateMessage(reason) {
  switch (reason) {
    case "all_filtered":
      return "Every changed symbol was hidden by the current filters.";
    case "all_out_of_scope":
      return "Every changed file was out of the extractor's scope; see “Out of scope” below.";
    case "no_diff":
      return "No changed file produced symbol evidence between base and head.";
    default:
      return null;
  }
}

function renderChangeList(changed) {
  renderFilteredOut(document.getElementById("change-hidden-by-filters"), changed.filtered_out);

  const container = document.getElementById("change-groups");
  clear(container);

  const emptyState = document.getElementById("change-empty");
  clear(emptyState);
  const message = (changed.symbols || []).length === 0 ? changeEmptyStateMessage(changed.reason) : null;
  if (message) {
    const children = [el("p", { text: message })];
    if (changed.reason === "all_filtered") {
      const clearLink = el("button", {
        className: "empty-state-clear-filters",
        attrs: { type: "button" },
        text: "Clear filters",
      });
      clearLink.addEventListener("click", () => clearFilters());
      children.push(clearLink);
    }
    for (const child of children) emptyState.appendChild(child);
    emptyState.hidden = false;
  } else {
    emptyState.hidden = true;
  }

  const byStatus = new Map(STATUS_ORDER.map((status) => [status, []]));
  for (const symbol of changed.symbols || []) {
    if (!byStatus.has(symbol.status)) {
      byStatus.set(symbol.status, []);
    }
    byStatus.get(symbol.status).push(symbol);
  }

  for (const status of STATUS_ORDER) {
    const rows = byStatus.get(status) || [];
    if (rows.length === 0) continue;
    container.appendChild(renderStatusGroup(status, rows));
  }

  renderOutOfScopeFooter(changed.out_of_scope || []);
}

function renderStatusGroup(status, rows) {
  const heading = el("h3", { text: `${statusLabel(status)} (${rows.length})` });
  const list = el("ul", { className: "row-list" });
  for (const symbol of rows) {
    list.appendChild(renderChangedSymbolRow(symbol));
  }
  return el("div", { className: "status-group" }, [heading, list]);
}

function statusLabel(status) {
  return status.replace(/_/g, " ");
}

function symbolIdentity(symbol) {
  const primary = symbol.head || symbol.base;
  if (!primary) {
    const first = (symbol.uncertain_candidates || [])[0];
    return primary ? parseSelector(primary.selector) : parseSelector(first ? first.selector : "");
  }
  return parseSelector(primary.selector);
}

function renderChangedSymbolRow(symbol) {
  const identity = symbolIdentity(symbol);
  const file = symbol.head_path || symbol.base_path || identity.path;
  const snapshots = symbol.supporting_snapshots && symbol.supporting_snapshots.length > 0
    ? symbol.supporting_snapshots.join("+")
    : "none";

  const button = el(
    "button",
    { className: "row-button", attrs: { type: "button", "aria-pressed": "false" } },
    [
      el("span", { className: "row-line" }, [
        el("span", { className: "row-name", text: identity.name || "(unnamed)" }),
        identity.symbolKind ? badge(identity.symbolKind) : null,
        badge(statusLabel(symbol.status)),
        badge(snapshots),
      ]),
      el("span", { className: "row-file", text: file || "(unknown file)" }),
    ],
  );
  button.addEventListener("click", () => selectSymbolRow(symbol, button));

  const children = [button];
  if (symbol.status === "uncertain" && (symbol.uncertain_candidates || []).length > 0) {
    children.push(renderUncertainCandidates(symbol.uncertain_candidates));
  }
  return el("li", null, children);
}

function renderUncertainCandidates(candidates) {
  const summary = el("summary", { text: `${candidates.length} candidate(s) — none chosen` });
  const list = el("ul", { className: "candidate-list" });
  for (const candidate of candidates) {
    const identity = parseSelector(candidate.selector);
    const button = el(
      "button",
      { className: "row-button", attrs: { type: "button", "aria-pressed": "false" } },
      [
        el("span", { className: "row-line" }, [
          el("span", { className: "row-name", text: identity.name || candidate.selector }),
          badge(candidate.snapshot),
        ]),
        el("span", { className: "row-file", text: candidate.reason || "" }),
      ],
    );
    button.addEventListener("click", () =>
      selectSymbolRow(
        {
          status: "uncertain",
          base: candidate.snapshot === "base" ? candidate : null,
          head: candidate.snapshot === "head" ? candidate : null,
          uncertain_candidates: [],
        },
        button,
      ),
    );
    list.appendChild(el("li", null, [button, copyableSelectorRow(candidate.snapshot, candidate.selector)]));
  }
  return el("details", { className: "uncertain-candidates" }, [summary, list]);
}

function renderOutOfScopeFooter(entries) {
  const footer = document.getElementById("out-of-scope-footer");
  clear(footer);
  if (entries.length === 0) {
    return;
  }
  const summary = el("summary", { text: `Out of scope: ${entries.length} file(s)` });
  const list = el("ul", { className: "candidate-list" });
  for (const entry of entries) {
    list.appendChild(
      el("li", null, [
        el("span", { className: "row-file", text: `${entry.path} — ${entry.reason} (${entry.snapshot})` }),
      ]),
    );
  }
  footer.appendChild(el("details", { className: "out-of-scope-footer" }, [summary, list]));
}

let selectedRowButton = null;

function selectSymbolRow(symbol, button) {
  if (selectedRowButton) {
    selectedRowButton.setAttribute("aria-pressed", "false");
  }
  selectedRowButton = button;
  button.setAttribute("aria-pressed", "true");

  const reference = primaryReference(symbol);
  const identity = reference ? parseSelector(reference.selector) : { name: "(no resolvable side)" };
  focusStack = [
    {
      selector: reference ? reference.selector : null,
      side: reference ? reference.side : null,
      label: identity.name || "(unnamed)",
      changedSymbol: symbol,
    },
  ];
  renderBreadcrumb();
  loadFocus(focusStack[0]).catch((error) => {
    setText(document.getElementById("evidence-bounds"), `Failed to load evidence: ${error.message}`);
  });
}

// ---------------------------------------------------------------------------
// Pane 2 — focused relationship view, path following, entry points
// ---------------------------------------------------------------------------

function primaryReference(symbol) {
  if (symbol.head) return { selector: symbol.head.selector, side: "head" };
  if (symbol.base) return { selector: symbol.base.selector, side: "base" };
  return null;
}

/** Push a new focus onto the breadcrumb trail and load it. Used when a hop's
 * symbol, or an entry point, is focused from within pane 2. */
function focusSymbol(entry) {
  focusStack.push({ selector: entry.selector, side: entry.side, label: entry.label, changedSymbol: null });
  renderBreadcrumb();
  loadFocus(focusStack[focusStack.length - 1]).catch((error) => {
    setText(document.getElementById("evidence-bounds"), `Failed to load evidence: ${error.message}`);
  });
}

function renderBreadcrumb() {
  const nav = document.getElementById("focus-breadcrumb");
  clear(nav);
  if (focusStack.length <= 1) {
    nav.hidden = true;
    return;
  }
  nav.hidden = false;

  const back = el("button", { className: "breadcrumb-back", attrs: { type: "button" }, text: "← Back" });
  back.addEventListener("click", () => jumpToBreadcrumb(focusStack.length - 2));
  nav.appendChild(back);

  const list = el("ol", { className: "breadcrumb-list" });
  focusStack.forEach((entry, index) => {
    const isCurrent = index === focusStack.length - 1;
    if (isCurrent) {
      list.appendChild(
        el("li", null, [el("span", { className: "row-name", attrs: { "aria-current": "true" }, text: entry.label })]),
      );
      return;
    }
    const button = el("button", { className: "breadcrumb-button", attrs: { type: "button" }, text: entry.label });
    button.addEventListener("click", () => jumpToBreadcrumb(index));
    list.appendChild(el("li", null, [button]));
  });
  nav.appendChild(list);
}

function jumpToBreadcrumb(index) {
  if (index < 0 || index >= focusStack.length) return;
  focusStack = focusStack.slice(0, index + 1);
  renderBreadcrumb();
  loadFocus(focusStack[focusStack.length - 1]).catch((error) => {
    setText(document.getElementById("evidence-bounds"), `Failed to load evidence: ${error.message}`);
  });
}

async function loadFocus(entry) {
  document.getElementById("relationship-empty").hidden = true;
  document.getElementById("relationship-content").hidden = false;

  setText(document.getElementById("selected-symbol-heading"), entry.label || "(unnamed)");
  setText(document.getElementById("selected-symbol-selector"), entry.selector || "");

  if (entry.changedSymbol) {
    await renderSymbolSource(entry.changedSymbol, entry.selector ? { selector: entry.selector, side: entry.side } : null);
  } else {
    await renderFocusedSource(entry);
  }

  if (!entry.selector) {
    setText(document.getElementById("evidence-bounds"), "No resolvable side for this entry.");
    setText(document.getElementById("evidence-summary"), "");
    clear(document.getElementById("evidence-primary"));
    clear(document.getElementById("evidence-heuristic"));
    setText(document.getElementById("callees-bounds"), "");
    setText(document.getElementById("callees-summary"), "");
    clear(document.getElementById("callees-primary"));
    clear(document.getElementById("callees-heuristic"));
    clear(document.getElementById("callees-unresolved"));
    clear(document.getElementById("entry-points-list"));
    setText(document.getElementById("entry-points-bounds"), "");
    lastReports = { evidence: null, outboundEvidence: null, entryPoints: null, candidateTests: null, focus: null };
    renderRelationshipView();
    return;
  }

  await loadFocusEvidence(entry);
}

async function loadFocusEvidence(entry) {
  const params = { selector: entry.selector, side: entry.side, ...activeFilterParams(currentFilters) };
  const [evidence, outboundEvidence, entryPoints, candidates] = await Promise.all([
    apiGet("/api/evidence", params),
    apiGet("/api/evidence", { ...params, direction: "outbound" }),
    apiGet("/api/entry-points", params),
    apiGet("/api/candidate-tests", params),
  ]);
  lastReports = { evidence, outboundEvidence, entryPoints, candidateTests: candidates, focus: entry };
  renderEvidence(evidence);
  renderCallees(outboundEvidence);
  renderEntryPoints(entryPoints);
  renderCandidateTests(candidates);
  renderRelationshipView();
}

function renderEvidence(evidence) {
  renderFilteredOut(document.getElementById("evidence-hidden-by-filters"), evidence.filtered_out);

  setText(document.getElementById("evidence-bounds"), boundsLine(evidence.query_options || {}, evidence.skipped_low_confidence));
  setText(
    document.getElementById("evidence-summary"),
    truncationSummary(evidence, (evidence.paths || []).length, "path", "paths"),
  );

  const primary = document.getElementById("evidence-primary");
  const heuristic = document.getElementById("evidence-heuristic");
  clear(primary);
  clear(heuristic);

  const primaryList = el("ul", { className: "evidence-list" });
  const heuristicList = el("ul", { className: "evidence-list" });
  let primaryCount = 0;
  let heuristicCount = 0;

  for (const path of evidence.paths || []) {
    const row = el("li", null, [renderEvidencePathRow(path)]);
    if (path.category === "heuristic_match") {
      heuristicList.appendChild(row);
      heuristicCount += 1;
    } else {
      primaryList.appendChild(row);
      primaryCount += 1;
    }
  }

  if (primaryCount > 0) primary.appendChild(primaryList);

  if (heuristicCount > 0) {
    heuristic.appendChild(
      el("div", { className: "evidence-heuristic-group" }, [
        el("h5", { text: "Heuristic / fallback matches" }),
        heuristicList,
      ]),
    );
  }

  const noPath = document.getElementById("evidence-no-path");
  if ((evidence.paths || []).length === 0 && (evidence.no_path_reasons || []).length > 0) {
    setText(noPath, `No callers found — reasons: ${evidence.no_path_reasons.join(" ")}`);
    noPath.hidden = false;
  } else {
    noPath.hidden = true;
  }
}

/** The outbound (callee) mirror of `renderEvidence`: same bounds/summary/
 * hidden-by-filters/primary/heuristic/no-path structure, plus the small
 * unresolved-callees list. Path and hop rendering (`renderEvidencePathRow`/
 * `renderHop`) are reused unchanged: an outbound path's `from`/`to`/`edges`
 * shape is the inbound shape with the arrow reversed. */
function renderCallees(report) {
  renderFilteredOut(document.getElementById("callees-hidden-by-filters"), report.filtered_out);

  setText(document.getElementById("callees-bounds"), boundsLine(report.query_options || {}, report.skipped_low_confidence));
  setText(
    document.getElementById("callees-summary"),
    truncationSummary(report, (report.paths || []).length, "path", "paths"),
  );

  const primary = document.getElementById("callees-primary");
  const heuristic = document.getElementById("callees-heuristic");
  clear(primary);
  clear(heuristic);

  const primaryList = el("ul", { className: "evidence-list" });
  const heuristicList = el("ul", { className: "evidence-list" });
  let primaryCount = 0;
  let heuristicCount = 0;

  for (const path of report.paths || []) {
    const row = el("li", null, [renderEvidencePathRow(path)]);
    if (path.category === "heuristic_match") {
      heuristicList.appendChild(row);
      heuristicCount += 1;
    } else {
      primaryList.appendChild(row);
      primaryCount += 1;
    }
  }

  if (primaryCount > 0) primary.appendChild(primaryList);

  if (heuristicCount > 0) {
    heuristic.appendChild(
      el("div", { className: "evidence-heuristic-group" }, [
        el("h5", { text: "Heuristic / fallback matches" }),
        heuristicList,
      ]),
    );
  }

  const noPath = document.getElementById("callees-no-path");
  if ((report.paths || []).length === 0 && (report.no_path_reasons || []).length > 0) {
    setText(noPath, `No callees found — reasons: ${report.no_path_reasons.join(" ")}`);
    noPath.hidden = false;
  } else {
    noPath.hidden = true;
  }

  renderUnresolvedCalleesList(document.getElementById("callees-unresolved"), report.unresolved_callees || []);
}

/** A small disclosure list of calls the resolver could not bind to an
 * indexed symbol: never rendered as nodes/rows, always disclosed with the
 * reason. Shared by the table view's Callees section and the graph view. */
function renderUnresolvedCalleesList(container, entries) {
  clear(container);
  if (entries.length === 0) return;
  const summary = el("summary", { text: `Unresolved callees: ${entries.length}` });
  const list = el("ul", { className: "candidate-list" });
  for (const entry of entries) {
    list.appendChild(
      el("li", null, [
        el("span", { className: "row-line" }, [
          el("span", { className: "row-name", text: entry.name }),
          badge(`line ${entry.line}`),
        ]),
        el("p", { className: "row-file", text: entry.reason }),
      ]),
    );
  }
  container.appendChild(el("details", { className: "unresolved-callees" }, [summary, list]));
}

function categoryLabel(category) {
  return String(category || "").replace(/_/g, " ");
}

/** One evidence path as an ordered hop chain: `<details>` summarizing the
 * affected symbol, the queried symbol, distance, and category, with an
 * `<ol>` of every hop in between. */
function renderEvidencePathRow(path) {
  const summaryLine = [
    el("span", {
      className: "row-name",
      text: `${path.from.label} → ${path.to.label}`,
    }),
    badge(`${path.distance} hop${path.distance === 1 ? "" : "s"}`),
    badge(categoryLabel(path.category)),
  ];
  if (path.truncated) {
    summaryLine.push(el("span", { className: "truncation-marker", text: `[truncated by ${path.truncated_by}]` }));
  }
  const summary = el("summary", null, [el("span", { className: "row-line" }, summaryLine)]);

  const hops = el("ol", { className: "hop-chain" });
  for (const edge of path.edges) {
    hops.appendChild(renderHop(edge));
  }
  hops.appendChild(
    el("li", { className: "hop hop-target" }, [
      el("span", { className: "row-line" }, [
        el("span", { className: "row-name", text: path.to.label }),
        path.to.origin === "file" ? badge("file") : null,
      ]),
    ]),
  );

  return el("details", { className: "evidence-path", attrs: { open: "open" } }, [summary, hops]);
}

/** One hop of a path: the referencing endpoint, its relationship/category/
 * confidence/snapshot, a button that opens its source in pane 3, and — when
 * the endpoint is a symbol, never a file — a button that re-focuses pane 2 on
 * it, extending the breadcrumb trail. */
function renderHop(edge) {
  const line = [
    el("span", { className: "row-name", text: edge.from }),
    edge.from_origin === "file" ? badge("file") : null,
    badge(edge.relationship),
    badge(categoryLabel(edge.category)),
    badge(edge.confidence),
    badge(edge.snapshot),
  ];
  const fileLine = el("span", {
    className: "row-file",
    text: `${edge.source.file}${edge.source.line ? `:${edge.source.line}` : ""}${edge.note ? ` — ${edge.note}` : ""}`,
  });

  const sourceButton = el(
    "button",
    { className: "row-button", attrs: { type: "button", "aria-pressed": "false" } },
    [el("span", { className: "row-line" }, line), fileLine],
  );
  sourceButton.addEventListener("click", () => selectEvidenceRow(edge, sourceButton));

  const children = [sourceButton];
  if (edge.from_origin === "symbol") {
    const focusButton = el("button", {
      className: "row-button hop-focus-button",
      attrs: { type: "button" },
      text: `Focus pane 2 on ${edge.from}`,
    });
    focusButton.addEventListener("click", () =>
      focusSymbol({ selector: edge.from_selector, side: edge.snapshot, label: edge.from }),
    );
    children.push(focusButton);
  }
  return el("li", { className: "hop" }, children);
}

let selectedEvidenceButton = null;

function selectEvidenceRow(edge, button) {
  if (selectedEvidenceButton) {
    selectedEvidenceButton.setAttribute("aria-pressed", "false");
  }
  selectedEvidenceButton = button;
  button.setAttribute("aria-pressed", "true");
  renderReferenceSource(edge).catch((error) => {
    setText(document.getElementById("reference-source-panel"), "");
    document.getElementById("reference-source-panel").appendChild(
      el("p", { text: `Failed to load reference source: ${error.message}` }),
    );
  });
}

function renderEntryPoints(report) {
  renderFilteredOut(document.getElementById("entry-points-hidden-by-filters"), report.filtered_out);

  const rules = (report.rules || []).map((rule) => rule.id).join(", ");
  const entries = report.entry_points || [];
  setText(
    document.getElementById("entry-points-bounds"),
    `${boundsLine(report.query_options || {}, null)} Rules applied: ${rules || "none"}. ` +
      truncationSummary(report, entries.length, "entry point", "entry points"),
  );

  const container = document.getElementById("entry-points-list");
  clear(container);
  const none = document.getElementById("entry-points-none");
  if (entries.length === 0) {
    setText(none, (report.no_entry_point_reasons || []).join(" "));
    none.hidden = false;
    return;
  }
  none.hidden = true;

  const list = el("ul", { className: "entry-point-list" });
  for (const entry of entries) {
    list.appendChild(renderEntryPointRow(entry));
  }
  container.appendChild(list);
}

function renderEntryPointRow(entry) {
  const line = [
    el("span", { className: "row-name", text: entry.node.label }),
    entry.node.origin === "file" ? badge("file") : null,
    badge(entry.rule),
  ];
  const button = el(
    "button",
    { className: "row-button", attrs: { type: "button" } },
    [
      el("span", { className: "row-line" }, line),
      el("span", { className: "row-file", text: `distance ${entry.distance} — ${entry.rule_description}` }),
    ],
  );
  if (entry.node.origin === "symbol") {
    button.addEventListener("click", () =>
      focusSymbol({ selector: entry.node.selector, side: entry.node.snapshot, label: entry.node.label }),
    );
  }

  const children = [button];
  if (entry.note) {
    children.push(el("p", { className: "row-file", text: entry.note }));
  }
  if (entry.path && entry.path.edges && entry.path.edges.length > 0) {
    children.push(renderEvidencePathRow(entry.path));
  }
  return el("li", null, children);
}

function renderCandidateTests(candidates) {
  const container = document.getElementById("candidate-tests");
  clear(container);
  const entries = candidates.candidates || [];
  if (entries.length === 0) {
    container.appendChild(el("p", { text: "No candidate tests were found from the disclosed sources." }));
    return;
  }
  const list = el("ul", { className: "candidate-test-list" });
  for (const candidate of entries) {
    const identity = parseSelector(candidate.test.selector);
    const line = [
      el("span", { className: "row-name", text: identity.name || candidate.test.selector }),
      badge(candidate.source.replace(/_/g, " ")),
      badge(candidate.category.replace(/_/g, " ")),
    ];
    if (candidate.truncated) {
      line.push(el("span", { className: "truncation-marker", text: "[truncated]" }));
    }
    list.appendChild(
      el("li", null, [
        el("span", { className: "row-line" }, line),
        el("span", { className: "row-file", text: candidate.note || candidate.test.selector }),
      ]),
    );
  }
  container.appendChild(list);
}

// ---------------------------------------------------------------------------
// Pane 3 — source / diff evidence
// ---------------------------------------------------------------------------

async function renderSymbolSource(symbol, reference) {
  const emptyNode = document.getElementById("source-empty");
  const symbolSourceNode = document.getElementById("symbol-source");
  const panels = document.getElementById("symbol-source-panels");
  const selectors = document.getElementById("symbol-selectors");
  clear(panels);
  clear(selectors);

  const available = [];
  if (symbol.base) available.push({ selector: symbol.base.selector, side: "base" });
  if (symbol.head) available.push({ selector: symbol.head.selector, side: "head" });
  if (available.length === 0 && reference) available.push(reference);

  if (available.length === 0) {
    symbolSourceNode.hidden = true;
    emptyNode.hidden = false;
    return;
  }
  emptyNode.hidden = true;
  symbolSourceNode.hidden = false;

  // Both selectors are always shown as copyable text, whatever the status:
  // this is the row's addressable identity even when only one side resolves.
  for (const entry of available) {
    selectors.appendChild(copyableSelectorRow(entry.side, entry.selector));
  }

  if (BASE_DEFAULT_STATUSES.has(symbol.status)) {
    const base = available.find((entry) => entry.side === "base") || available[0];
    const other = available.find((entry) => entry.side !== base.side);
    panels.className = "";
    await renderSideToggle(panels, base, other);
    return;
  }

  panels.className = available.length > 1 ? "source-side-by-side" : "";
  const views = await Promise.all(
    available.map((entry) => apiGetRaw("/api/source", { selector: entry.selector, side: entry.side })),
  );
  for (const view of views) {
    panels.appendChild(renderSourcePanel(view, null));
  }
}

/** A single panel defaulting to `activeEntry`, with a toggle button to
 * `otherEntry` when one exists. Used for `removed`/`moved`/`renamed`/
 * `uncertain` rows, whose two sides may carry different selectors (or no
 * head at all), so the default side is always named and switchable rather
 * than shown as an unlabelled pair. */
async function renderSideToggle(panels, activeEntry, otherEntry) {
  const panelHost = el("div", { className: "source-panel-host" });

  const load = async (entry) => {
    clear(panelHost);
    const view = await apiGetRaw("/api/source", { selector: entry.selector, side: entry.side });
    panelHost.appendChild(renderSourcePanel(view, null));
  };

  if (otherEntry) {
    let showingOther = false;
    const toggle = el("button", {
      className: "side-toggle",
      attrs: { type: "button" },
      text: `Show ${otherEntry.side}-side source`,
    });
    toggle.addEventListener("click", () => {
      showingOther = !showingOther;
      const next = showingOther ? otherEntry : activeEntry;
      const label = showingOther ? activeEntry.side : otherEntry.side;
      setText(toggle, `Show ${label}-side source`);
      load(next).catch((error) => {
        clear(panelHost);
        panelHost.appendChild(el("p", { text: `Failed to load source: ${error.message}` }));
      });
    });
    panels.appendChild(toggle);
  }
  panels.appendChild(panelHost);
  await load(activeEntry);
}

/** Source for a hop's or entry point's symbol, focused from pane 2. This is
 * a single-side panel: the focused selector already names its snapshot. */
async function renderFocusedSource(entry) {
  const emptyNode = document.getElementById("source-empty");
  const symbolSourceNode = document.getElementById("symbol-source");
  const panels = document.getElementById("symbol-source-panels");
  const selectors = document.getElementById("symbol-selectors");
  clear(panels);
  clear(selectors);
  emptyNode.hidden = true;
  symbolSourceNode.hidden = false;
  panels.className = "";
  selectors.appendChild(copyableSelectorRow(entry.side, entry.selector));
  const view = await apiGetRaw("/api/source", { selector: entry.selector, side: entry.side });
  panels.appendChild(renderSourcePanel(view, null));
}

async function renderReferenceSource(edge) {
  const container = document.getElementById("reference-source-panel");
  clear(container);
  document.getElementById("reference-source").hidden = false;
  const view = await apiGetRaw("/api/source", { selector: edge.from_selector, side: edge.snapshot });
  container.appendChild(renderSourcePanel(view, edge.source.line));
}

function renderSourcePanel(view, highlightLine) {
  const header = el("div", {
    className: "source-panel-header",
    text: `${view.snapshot || "?"} [${shortSha(view.commit_sha)}] ${view.file || view.selector || ""}`,
  });
  const panel = el("div", { className: "source-panel" }, [header]);

  if (view.error) {
    panel.appendChild(el("p", { text: view.error.message || view.error.code }));
    return panel;
  }

  if (view.encoding === "bytes") {
    const byteCount = Array.isArray(view.bytes_or_text) ? view.bytes_or_text.length : 0;
    panel.appendChild(el("p", { text: `Non-UTF-8 content, ${byteCount} byte(s). Not rendered as text.` }));
  } else {
    const text = typeof view.bytes_or_text === "string" ? view.bytes_or_text : "";
    const startLine = lineNumberOf(view);
    panel.appendChild(renderSourceLines(text, startLine, highlightLine));
  }

  const boundLine = el("p", {
    className: "byte-bound",
    text:
      `Excerpt bounded to ${view.source_max_bytes ?? "?"} bytes` +
      (view.truncated ? ` — truncated by ${view.truncated_by || "a bound"}.` : "."),
  });
  panel.appendChild(boundLine);
  return panel;
}

/** The source view's span starts mid-file; the first rendered line's number. */
function lineNumberOf(view) {
  return 1;
}

/** Render source text as one text node per line — never `innerHTML`. */
function renderSourceLines(text, startLine, highlightLine) {
  const container = el("div", { className: "source-lines" });
  const lines = text.split("\n");
  lines.forEach((lineText, index) => {
    const lineNumber = startLine + index;
    const isHighlighted = highlightLine !== null && highlightLine !== undefined && lineNumber === highlightLine;
    const row = el(
      "div",
      { className: isHighlighted ? "source-line line-highlight" : "source-line" },
      [
        el("span", { className: "line-marker" }),
        el("span", { className: "line-no", text: String(lineNumber) }),
        el("span", { className: "line-text", text: lineText }),
      ],
    );
    if (isHighlighted) {
      row.setAttribute("aria-current", "true");
    }
    container.appendChild(row);
  });
  return container;
}

// ---------------------------------------------------------------------------
// Pane 2 graph view — hand-built inline SVG, built with DOM APIs only.
//
// Built entirely from data already fetched for the table view
// (`/api/evidence` in both directions, `/api/entry-points`,
// `/api/candidate-tests`): no extra network call, and expand-on-demand is
// purely a client-side reveal over that already-fetched neighbourhood, never
// a fresh request. Inbound evidence (callers/importers, plus entry points)
// lays out to the left of the focus; outbound evidence (callees, from the
// `direction=outbound` fetch) lays out to the right, in depth layers the
// same way. Unresolved callees are never rendered as nodes — see
// `renderUnresolvedCalleesList`.
// ---------------------------------------------------------------------------

const SVG_NS = "http://www.w3.org/2000/svg";

const CONFIDENCE_BADGE = { exact: "EX", import_resolved: "IR", same_module: "SM", fuzzy_name: "FN" };
const CONFIDENCE_RANK = { exact: 4, import_resolved: 3, same_module: 2, fuzzy_name: 1 };
const CONFIDENCE_STROKE_WIDTH = { exact: 3, import_resolved: 2.25, same_module: 1.5, fuzzy_name: 0.75 };

const GRAPH_NODE_WIDTH = 160;
const GRAPH_NODE_HEIGHT = 32;
const GRAPH_LAYER_GAP = 190;
const GRAPH_ROW_GAP = 52;
const GRAPH_MARGIN = 24;
const GRAPH_EXPAND_OFFSET = 26;

/** Selectors revealed in the current graph. Reset whenever the focus
 * changes; preserved across a table/graph toggle so reopening the graph does
 * not collapse an expansion the user just made. */
let graphRevealed = new Set();
let graphRevealedFor = null;

function setupViewToggle() {
  document.getElementById("view-toggle-table").addEventListener("click", () => setView("table"));
  document.getElementById("view-toggle-graph").addEventListener("click", () => setView("graph"));
  applyViewToggleButtons();
}

function applyViewToggleButtons() {
  document.getElementById("view-toggle-table").setAttribute("aria-pressed", String(currentView === "table"));
  document.getElementById("view-toggle-graph").setAttribute("aria-pressed", String(currentView === "graph"));
}

function setView(view) {
  currentView = view;
  writeFragment();
  renderRelationshipView();
}

/** Show the table or the graph for the current focus, whichever
 * `currentView` names. Called whenever the focus or the toggle changes. */
function renderRelationshipView() {
  applyViewToggleButtons();
  const tableView = document.getElementById("relationship-table-view");
  const graphView = document.getElementById("relationship-graph-view");
  if (currentView === "graph" && lastReports.evidence && lastReports.focus) {
    tableView.hidden = true;
    graphView.hidden = false;
    renderGraph();
  } else {
    tableView.hidden = false;
    graphView.hidden = true;
  }
}

/** Build the neighbourhood graph model from the already-fetched evidence
 * (both directions), entry-points, and candidate-tests reports: nodes keyed
 * by selector, tagged `"focus"`, `"inbound"`, or `"outbound"`, with their
 * minimum hop distance from the focus, and the deduplicated inbound/outbound
 * edges that connect them. Nodes with no known chain back to the focus are
 * dropped: there is no honest layer to place them in. */
function buildGraphModel() {
  const { evidence, outboundEvidence, entryPoints, candidateTests } = lastReports;
  const nodes = new Map();
  const inboundEdges = [];
  const outboundEdges = [];
  const edgeKeys = new Set();

  const ensureNode = (selector, snapshot, label, origin, direction) => {
    let node = nodes.get(selector);
    if (!node) {
      node = {
        selector,
        snapshot,
        label,
        origin,
        direction,
        depth: Infinity,
        isFocus: false,
        isEntryPoint: false,
        entryRule: null,
        isCandidateTest: false,
        bestConfidence: null,
      };
      nodes.set(selector, node);
    }
    return node;
  };

  const focus = evidence.target;
  const focusNode = ensureNode(focus.selector, focus.snapshot, focus.label, focus.origin, "focus");
  focusNode.isFocus = true;
  focusNode.depth = 0;

  const ingestInboundPath = (path) => {
    let depthCursor = path.distance;
    for (const edge of path.edges) {
      const fromNode = ensureNode(edge.from_selector, edge.snapshot, edge.from, edge.from_origin, "inbound");
      if (depthCursor < fromNode.depth) fromNode.depth = depthCursor;
      const rank = CONFIDENCE_RANK[edge.confidence] || 0;
      if (!fromNode.bestConfidence || rank > CONFIDENCE_RANK[fromNode.bestConfidence]) {
        fromNode.bestConfidence = edge.confidence;
      }
      depthCursor -= 1;
      const key = `in:${edge.from_selector}=>${edge.to_selector}`;
      if (!edgeKeys.has(key)) {
        edgeKeys.add(key);
        inboundEdges.push(edge);
      }
    }
  };

  for (const path of evidence.paths || []) ingestInboundPath(path);
  for (const entry of (entryPoints && entryPoints.entry_points) || []) {
    if (entry.path && entry.path.edges && entry.path.edges.length > 0) ingestInboundPath(entry.path);
    const node = ensureNode(entry.node.selector, entry.node.snapshot, entry.node.label, entry.node.origin, "inbound");
    node.isEntryPoint = true;
    node.entryRule = entry.rule;
  }
  for (const candidate of (candidateTests && candidateTests.candidates) || []) {
    const node = nodes.get(candidate.test.selector);
    if (node) node.isCandidateTest = true;
  }

  // Outbound paths are ordered from the focus outward: `edges[0].from` is the
  // focus and the last edge's `to` is the reached callee. The node reached by
  // edge `i` is that edge's `to`; its origin comes from the next edge's
  // `from_origin` (the same node, one hop closer), or from `path.to` for the
  // final edge, which the API sets directly since no later edge exists.
  const ingestOutboundPath = (path) => {
    let depthCursor = 1;
    path.edges.forEach((edge, index) => {
      const isLast = index === path.edges.length - 1;
      const origin = isLast ? path.to.origin : path.edges[index + 1].from_origin;
      const label = isLast ? path.to.label : path.edges[index + 1].from;
      const toNode = ensureNode(edge.to_selector, edge.snapshot, label, origin, "outbound");
      if (depthCursor < toNode.depth) toNode.depth = depthCursor;
      const rank = CONFIDENCE_RANK[edge.confidence] || 0;
      if (!toNode.bestConfidence || rank > CONFIDENCE_RANK[toNode.bestConfidence]) {
        toNode.bestConfidence = edge.confidence;
      }
      depthCursor += 1;
      const key = `out:${edge.from_selector}=>${edge.to_selector}`;
      if (!edgeKeys.has(key)) {
        edgeKeys.add(key);
        outboundEdges.push(edge);
      }
    });
  };
  for (const path of (outboundEvidence && outboundEvidence.paths) || []) ingestOutboundPath(path);

  for (const selector of Array.from(nodes.keys())) {
    if (nodes.get(selector).depth === Infinity) nodes.delete(selector);
  }
  const filteredInboundEdges = inboundEdges.filter(
    (edge) => nodes.has(edge.from_selector) && nodes.has(edge.to_selector),
  );
  const filteredOutboundEdges = outboundEdges.filter(
    (edge) => nodes.has(edge.from_selector) && nodes.has(edge.to_selector),
  );

  if (graphRevealedFor !== focus.selector) {
    graphRevealed = new Set();
    for (const node of nodes.values()) {
      if (node.depth <= 1) graphRevealed.add(node.selector);
    }
    graphRevealedFor = focus.selector;
  } else {
    graphRevealed.add(focus.selector);
  }

  return {
    nodes,
    edges: [...filteredInboundEdges, ...filteredOutboundEdges],
    inboundEdges: filteredInboundEdges,
    outboundEdges: filteredOutboundEdges,
    focusSelector: focus.selector,
    unresolvedCallees: (outboundEvidence && outboundEvidence.unresolved_callees) || [],
  };
}

/** Inbound nodes one hop farther from the focus than `node`, not yet
 * revealed: the count a "+N" affordance on `node`'s inbound (caller) side
 * promises to reveal. */
function unrevealedInboundChildren(model, node) {
  const children = [];
  for (const edge of model.inboundEdges) {
    if (edge.to_selector !== node.selector) continue;
    const child = model.nodes.get(edge.from_selector);
    if (child && child.depth === node.depth + 1 && !graphRevealed.has(child.selector)) {
      children.push(child);
    }
  }
  return children;
}

/** Outbound nodes one hop farther from the focus than `node`, not yet
 * revealed: the count a "+N" affordance on `node`'s outbound (callee) side
 * promises to reveal. */
function unrevealedOutboundChildren(model, node) {
  const children = [];
  for (const edge of model.outboundEdges) {
    if (edge.from_selector !== node.selector) continue;
    const child = model.nodes.get(edge.to_selector);
    if (child && child.depth === node.depth + 1 && !graphRevealed.has(child.selector)) {
      children.push(child);
    }
  }
  return children;
}

function renderGraph() {
  const svg = document.getElementById("relationship-graph");
  const notice = document.getElementById("graph-budget-notice");
  clear(svg);

  const model = buildGraphModel();
  if (model.nodes.size > GRAPH_RENDER_BUDGET) {
    clear(notice);
    notice.appendChild(
      el("span", {
        text:
          `This neighbourhood has ${model.nodes.size} nodes, over the graph view's budget of ` +
          `${GRAPH_RENDER_BUDGET}. `,
      }),
    );
    const tableLink = el("button", {
      className: "empty-state-clear-filters",
      attrs: { type: "button" },
      text: "Use the table view instead",
    });
    tableLink.addEventListener("click", () => setView("table"));
    notice.appendChild(tableLink);
    notice.hidden = false;
    return;
  }
  notice.hidden = true;
  drawGraph(svg, model);
  renderUnresolvedCalleesList(document.getElementById("graph-unresolved-callees"), model.unresolvedCallees);
}

/** Inbound nodes (callers, entry points, and the focus) lay out to the left
 * in depth layers exactly as before; outbound nodes (callees) lay out to the
 * right in depth layers the same way, so the focus sits at the seam between
 * the two neighbourhoods. */
function drawGraph(svg, model) {
  const revealedNodes = Array.from(model.nodes.values()).filter((node) => graphRevealed.has(node.selector));
  const inboundNodes = revealedNodes.filter((node) => node.direction !== "outbound");
  const outboundNodes = revealedNodes.filter((node) => node.direction === "outbound");

  const maxInboundDepth = inboundNodes.reduce((max, node) => Math.max(max, node.depth), 0);
  const maxOutboundDepth = outboundNodes.reduce((max, node) => Math.max(max, node.depth), 0);

  const inboundLayers = new Map();
  for (const node of inboundNodes) {
    if (!inboundLayers.has(node.depth)) inboundLayers.set(node.depth, []);
    inboundLayers.get(node.depth).push(node);
  }
  const outboundLayers = new Map();
  for (const node of outboundNodes) {
    if (!outboundLayers.has(node.depth)) outboundLayers.set(node.depth, []);
    outboundLayers.get(node.depth).push(node);
  }
  for (const list of [...inboundLayers.values(), ...outboundLayers.values()]) {
    list.sort((left, right) => left.label.localeCompare(right.label));
  }

  const focusX = GRAPH_MARGIN + maxInboundDepth * GRAPH_LAYER_GAP + GRAPH_NODE_WIDTH / 2;
  const maxRows = Math.max(
    1,
    ...Array.from(inboundLayers.values()).map((list) => list.length),
    ...Array.from(outboundLayers.values()).map((list) => list.length),
  );
  const height = GRAPH_MARGIN * 2 + maxRows * GRAPH_ROW_GAP;
  const width = focusX + GRAPH_NODE_WIDTH / 2 + maxOutboundDepth * GRAPH_LAYER_GAP + GRAPH_MARGIN;
  svg.setAttribute("viewBox", `0 0 ${width} ${height}`);

  const positions = new Map();
  for (const [depth, list] of inboundLayers.entries()) {
    const x = GRAPH_MARGIN + (maxInboundDepth - depth) * GRAPH_LAYER_GAP + GRAPH_NODE_WIDTH / 2;
    const rowHeight = height / (list.length + 1);
    list.forEach((node, index) => {
      positions.set(node.selector, { x, y: rowHeight * (index + 1) });
    });
  }
  for (const [depth, list] of outboundLayers.entries()) {
    const x = focusX + depth * GRAPH_LAYER_GAP;
    const rowHeight = height / (list.length + 1);
    list.forEach((node, index) => {
      positions.set(node.selector, { x, y: rowHeight * (index + 1) });
    });
  }

  // Edges first, so nodes paint on top of the lines that reach them.
  for (const edge of model.edges) {
    const from = positions.get(edge.from_selector);
    const to = positions.get(edge.to_selector);
    if (!from || !to) continue;
    svg.appendChild(drawEdge(edge, from, to));
  }
  for (const node of revealedNodes) {
    svg.appendChild(drawNode(model, node, positions.get(node.selector)));
  }
}

function drawEdge(edge, from, to) {
  const strokeWidth = CONFIDENCE_STROKE_WIDTH[edge.confidence] || 1;
  const midX = (from.x + to.x) / 2;
  const midY = (from.y + to.y) / 2;
  const line = svgEl("line", {
    class: "graph-edge-line",
    x1: from.x,
    y1: from.y,
    x2: to.x,
    y2: to.y,
    "stroke-width": strokeWidth,
  });
  const badgeLabel = svgText(
    `${edge.relationship} · ${categoryLabel(edge.category)} · ${CONFIDENCE_BADGE[edge.confidence] || edge.confidence}`,
    { x: midX, y: midY - 4, class: "graph-edge-badge", "text-anchor": "middle" },
  );
  const group = svgEl(
    "g",
    {
      class: "graph-edge",
      tabindex: "0",
      role: "button",
      "aria-pressed": "false",
      "aria-label":
        `${edge.from} to ${edge.to}: ${edge.relationship}, ${categoryLabel(edge.category)} evidence, ` +
        `${edge.confidence} confidence, ${edge.snapshot} snapshot`,
    },
    [line, badgeLabel],
  );
  const activate = () => selectEvidenceRow(edge, group);
  group.addEventListener("click", activate);
  group.addEventListener("keydown", (event) => {
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      activate();
    }
  });
  return group;
}

function drawNode(model, node, position) {
  const shape = node.isFocus
    ? svgEl("circle", { cx: position.x, cy: position.y, r: GRAPH_NODE_HEIGHT / 2 })
    : svgEl("rect", {
        x: position.x - GRAPH_NODE_WIDTH / 2,
        y: position.y - GRAPH_NODE_HEIGHT / 2,
        width: GRAPH_NODE_WIDTH,
        height: GRAPH_NODE_HEIGHT,
        rx: 4,
      });

  const badges = [];
  if (node.origin === "file") badges.push("file");
  if (node.isEntryPoint) badges.push(`entry point (${node.entryRule})`);
  if (node.isCandidateTest) badges.push("candidate test");
  if (node.bestConfidence) badges.push(CONFIDENCE_BADGE[node.bestConfidence] || node.bestConfidence);

  const label = svgText(truncateLabel(node.label), {
    x: position.x,
    y: position.y + (node.isFocus ? 4 : -2),
    class: "graph-node-label",
    "text-anchor": "middle",
  });
  const children = [shape, label];
  if (badges.length > 0) {
    children.push(
      svgText(badges.join(" · "), {
        x: position.x,
        y: position.y + (node.isFocus ? 18 : 12),
        class: "graph-node-badge",
        "text-anchor": "middle",
      }),
    );
  }

  const group = svgEl(
    "g",
    {
      class: node.isFocus ? "graph-node graph-node-focus" : "graph-node",
      tabindex: "0",
      role: "button",
      "aria-label": `${node.label}${node.isFocus ? " (focused symbol)" : ""}, ${node.origin}${
        badges.length > 0 ? `, ${badges.join(", ")}` : ""
      }`,
    },
    children,
  );
  const activate = () => openNodeSource(node);
  group.addEventListener("click", activate);
  group.addEventListener("keydown", (event) => {
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      activate();
    }
  });

  const expandBubble = (unrevealed, x, verb) => {
    if (unrevealed.length === 0) return null;
    const bubble = svgEl(
      "g",
      {
        class: "graph-expand",
        tabindex: "0",
        role: "button",
        "aria-label": `Show ${unrevealed.length} more ${verb} of ${node.label}`,
      },
      [
        svgEl("circle", { cx: x, cy: position.y, r: 11 }),
        svgText(`+${unrevealed.length}`, {
          x,
          y: position.y + 3,
          class: "graph-expand-label",
          "text-anchor": "middle",
        }),
      ],
    );
    const expand = () => {
      for (const child of unrevealed) graphRevealed.add(child.selector);
      renderGraph();
    };
    bubble.addEventListener("click", expand);
    bubble.addEventListener("keydown", (event) => {
      if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        expand();
      }
    });
    return bubble;
  };

  // A caller's deeper callers lie farther left; a callee's deeper callees
  // lie farther right. The focus can have both.
  const inboundBubble = expandBubble(
    unrevealedInboundChildren(model, node),
    position.x - GRAPH_NODE_WIDTH / 2 - GRAPH_EXPAND_OFFSET,
    "caller(s)",
  );
  const outboundBubble = expandBubble(
    unrevealedOutboundChildren(model, node),
    position.x + GRAPH_NODE_WIDTH / 2 + GRAPH_EXPAND_OFFSET,
    "callee(s)",
  );
  if (!inboundBubble && !outboundBubble) {
    return group;
  }
  return svgEl("g", null, [group, inboundBubble, outboundBubble]);
}

function truncateLabel(label) {
  const text = String(label || "");
  return text.length > 22 ? `${text.slice(0, 21)}…` : text;
}

function svgEl(tag, attrs, children) {
  const node = document.createElementNS(SVG_NS, tag);
  if (attrs) {
    for (const [key, value] of Object.entries(attrs)) {
      node.setAttribute(key, String(value));
    }
  }
  for (const child of children || []) {
    if (child) node.appendChild(child);
  }
  return node;
}

function svgText(text, attrs) {
  const node = svgEl("text", attrs, []);
  node.textContent = text;
  return node;
}

/** Open a graph node's own source in pane 3, the same panel a table-view hop
 * or entry-point row opens it in. Unlike a hop, a node is not itself an
 * edge, so this calls `/api/source` directly rather than reusing
 * `renderReferenceSource`. */
async function openNodeSource(node) {
  const container = document.getElementById("reference-source-panel");
  clear(container);
  document.getElementById("reference-source").hidden = false;
  try {
    const view = await apiGetRaw("/api/source", { selector: node.selector, side: node.snapshot });
    container.appendChild(renderSourcePanel(view, null));
  } catch (error) {
    container.appendChild(el("p", { text: `Failed to load source: ${error.message}` }));
    showErrorBanner(error);
  }
}

// ---------------------------------------------------------------------------
// Keyboard map (`?`) and narrow-viewport pane switcher
// ---------------------------------------------------------------------------

function setupPaneSwitcher() {
  for (const button of document.querySelectorAll(".pane-switch-button")) {
    button.addEventListener("click", () => switchToPane(button.dataset.pane));
  }
}

function switchToPane(paneId) {
  const pane = document.getElementById(paneId);
  if (!pane) return;
  pane.scrollIntoView({ behavior: "smooth", block: "start" });
  for (const button of document.querySelectorAll(".pane-switch-button")) {
    button.setAttribute("aria-current", String(button.dataset.pane === paneId));
  }
}

function openKeyboardHelp() {
  document.getElementById("keyboard-help").hidden = false;
  document.getElementById("keyboard-help-close").focus();
}

function closeKeyboardHelp() {
  document.getElementById("keyboard-help").hidden = true;
  document.getElementById("keyboard-help-open").focus();
}

/** Whether `target` is a field that should absorb ordinary keystrokes rather
 * than trigger a global shortcut. */
function isTypingTarget(target) {
  return Boolean(
    target &&
      (target.tagName === "INPUT" ||
        target.tagName === "TEXTAREA" ||
        target.tagName === "SELECT" ||
        target.isContentEditable),
  );
}

function setupKeyboardHelp() {
  const dialog = document.getElementById("keyboard-help");
  document.getElementById("keyboard-help-open").addEventListener("click", openKeyboardHelp);
  document.getElementById("keyboard-help-close").addEventListener("click", closeKeyboardHelp);
  dialog.addEventListener("click", (event) => {
    if (event.target === dialog) closeKeyboardHelp();
  });

  document.addEventListener("keydown", (event) => {
    const typing = isTypingTarget(event.target);

    if (event.key === "?" && !typing) {
      event.preventDefault();
      openKeyboardHelp();
      return;
    }
    if (event.key === "Escape") {
      if (!document.getElementById("search-results").hidden) {
        document.getElementById("search-results").hidden = true;
        return;
      }
      if (!dialog.hidden) {
        closeKeyboardHelp();
        return;
      }
      const lastBanner = document.querySelector("#error-banner-region .error-banner:last-child");
      if (lastBanner) {
        lastBanner.remove();
        return;
      }
      if (focusStack.length > 1) {
        jumpToBreadcrumb(focusStack.length - 2);
      }
      return;
    }
    if (typing) {
      return;
    }
    if (event.key === "/") {
      event.preventDefault();
      document.getElementById("search-input").focus();
      return;
    }
    if (event.key === "g") {
      setView(currentView === "graph" ? "table" : "graph");
      return;
    }
    if (event.key === "Backspace") {
      event.preventDefault();
      clearFilters();
      return;
    }
    if (event.key === "1" || event.key === "2" || event.key === "3") {
      const paneIds = { 1: "pane-changes", 2: "pane-relationships", 3: "pane-source" };
      switchToPane(paneIds[event.key]);
    }
  });
}

main();
