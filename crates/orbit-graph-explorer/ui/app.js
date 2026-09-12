// orbit-graph change explorer — UI.
//
// Framework-free. No build step, no CDN, no network access beyond this
// service's own `/api/*` loopback endpoints. Source text from the API is
// always inserted as text nodes (`textContent` / `createTextNode`), never
// `innerHTML`, so repository content can never become markup.
//
// Filter state (`confidence`, `language`, `change_kind`, `depth`, `scope`)
// lives in the URL fragment as ordinary `URLSearchParams` pairs, so it
// survives a reload. The per-launch bearer `token` is read from the same
// fragment once at startup and is never written back into it: `writeFragment`
// only ever serializes `FILTER_KEYS`, so a copied or bookmarked URL after the
// first render carries filters but never the credential.
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
//                truncated, truncated_by, bounds_hit[].bound,
//                bounds_hit[].value, filtered_out,
//                paths[].from.label, paths[].to.label, paths[].distance,
//                paths[].category, paths[].truncated, paths[].truncated_by,
//                paths[].edges[].from, paths[].edges[].from_selector,
//                paths[].edges[].from_origin, paths[].edges[].to,
//                paths[].edges[].relationship, paths[].edges[].category,
//                paths[].edges[].confidence, paths[].edges[].snapshot,
//                paths[].edges[].source.file, paths[].edges[].source.line,
//                paths[].edges[].note, no_path_reasons
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

const POLL_INTERVAL_MS = 500;

/** The per-launch bearer token, held in memory only for the life of this page. */
let authToken = null;

/** Current filter selection, as fragment/query string values (all strings). */
let currentFilters = {};

/**
 * Breadcrumb trail of focused symbols in pane 2, root first. The last entry
 * is the current focus. `changedSymbol` is set only for the trail's root,
 * which came from a pane-1 row and carries both sides for pane-3 defaults.
 */
let focusStack = [];

function main() {
  const rawHash = window.location.hash.replace(/^#/, "");
  const initialParams = new URLSearchParams(rawHash);
  authToken = initialParams.get("token");
  if (!authToken) {
    document.getElementById("token-gate").hidden = false;
    return;
  }
  initialParams.delete("token");
  currentFilters = decodeFragment(initialParams.toString());
  // Strip the token from the visible URL and history right away, and persist
  // whatever filters (if any) were already present — never the credential.
  writeFragment(currentFilters);

  document.getElementById("app").hidden = false;
  populateFilterForm(currentFilters);
  setupFilterBar();
  enableArrowNavigation(document.getElementById("change-groups"));
  enableArrowNavigation(document.getElementById("evidence-primary"));
  enableArrowNavigation(document.getElementById("evidence-heuristic"));
  enableArrowNavigation(document.getElementById("entry-points-list"));
  enableArrowNavigation(document.getElementById("candidate-tests"));

  run().catch((error) => {
    setText(document.getElementById("indexing-notice"), `Failed to load: ${error.message}`);
  });
}

async function run() {
  const comparison = await pollComparisonUntilReady();
  renderHeader(comparison);
  const changed = await apiGet("/api/changed-symbols", activeFilterParams(currentFilters));
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

/** Parse known filter keys out of a fragment (or query) string. Unknown keys,
 * including a stray `token`, are ignored. */
function decodeFragment(raw) {
  const params = new URLSearchParams(raw);
  const filters = {};
  for (const key of FILTER_KEYS) {
    const value = params.get(key);
    if (value !== null && value !== "") {
      filters[key] = value;
    }
  }
  return filters;
}

/** Serialize only `FILTER_KEYS`. The token never round-trips through here. */
function encodeFragment(filters) {
  const params = new URLSearchParams();
  for (const key of FILTER_KEYS) {
    const value = filters[key];
    if (value !== undefined && value !== null && String(value).length > 0) {
      params.set(key, value);
    }
  }
  return params.toString();
}

function writeFragment(filters) {
  const query = encodeFragment(filters);
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
    writeFragment(currentFilters);
    refreshAfterFilterChange();
  });
  document.getElementById("filter-clear").addEventListener("click", () => {
    currentFilters = {};
    populateFilterForm(currentFilters);
    writeFragment(currentFilters);
    refreshAfterFilterChange();
  });
}

/** Re-query pane 1, and pane 2 if a symbol is currently focused, under the
 * current filters. Clearing filters restores everything the same way. */
function refreshAfterFilterChange() {
  apiGet("/api/changed-symbols", activeFilterParams(currentFilters))
    .then(renderChangeList)
    .catch((error) => {
      setText(document.getElementById("indexing-notice"), `Failed to reload changed symbols: ${error.message}`);
    });

  if (focusStack.length > 0) {
    const top = focusStack[focusStack.length - 1];
    if (top.selector) {
      loadFocusEvidence(top).catch((error) => {
        setText(document.getElementById("evidence-bounds"), `Failed to load evidence: ${error.message}`);
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

/** Fetch JSON, returning the parsed body whatever the HTTP status was. */
async function apiGetRaw(path, params) {
  const response = await fetch(buildUrl(path, params), {
    method: "GET",
    headers: { Authorization: `Bearer ${authToken}` },
    cache: "no-store",
    credentials: "same-origin",
  });
  return response.json();
}

/** Fetch JSON and throw on an error-shaped body. */
async function apiGet(path, params) {
  const body = await apiGetRaw(path, params);
  if (body && body.error) {
    throw new Error(body.error.message || body.error.code || "request failed");
  }
  return body;
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

function renderChangeList(changed) {
  renderFilteredOut(document.getElementById("change-hidden-by-filters"), changed.filtered_out);

  const container = document.getElementById("change-groups");
  clear(container);

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
    clear(document.getElementById("entry-points-list"));
    setText(document.getElementById("entry-points-bounds"), "");
    return;
  }

  await loadFocusEvidence(entry);
}

async function loadFocusEvidence(entry) {
  const params = { selector: entry.selector, side: entry.side, ...activeFilterParams(currentFilters) };
  const [evidence, entryPoints, candidates] = await Promise.all([
    apiGet("/api/evidence", params),
    apiGet("/api/entry-points", params),
    apiGet("/api/candidate-tests", params),
  ]);
  renderEvidence(evidence);
  renderEntryPoints(entryPoints);
  renderCandidateTests(candidates);
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
    setText(noPath, evidence.no_path_reasons.join(" "));
    noPath.hidden = false;
  } else {
    noPath.hidden = true;
  }
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

main();
