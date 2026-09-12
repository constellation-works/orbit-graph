// orbit-graph change explorer — v0 UI.
//
// Framework-free. No build step, no CDN, no network access beyond this
// service's own `/api/*` loopback endpoints. Source text from the API is
// always inserted as text nodes (`textContent` / `createTextNode`), never
// `innerHTML`, so repository content can never become markup.
//
// Data-contract fields this module reads from the service JSON (kept here so
// a Rust test can assert the same names against the real serializers):
//
//   comparison:  mode, base.commit_sha, head.commit_sha, working_tree.dirty,
//                working_tree.notice, indexing_status, indexing_error
//   changed-symbols: schema_version, symbols[].status, symbols[].pairing,
//                symbols[].base.selector, symbols[].head.selector,
//                symbols[].supporting_snapshots, symbols[].base_path,
//                symbols[].head_path, symbols[].note,
//                symbols[].uncertain_candidates[].selector,
//                symbols[].uncertain_candidates[].snapshot,
//                symbols[].uncertain_candidates[].reason,
//                out_of_scope[].path, out_of_scope[].reason,
//                out_of_scope[].snapshot
//   evidence:    target.selector, commit_sha, query_options.depth,
//                query_options.min_confidence, query_options.source_max_bytes,
//                paths[].truncated, paths[].truncated_by,
//                paths[].edges[].from, paths[].edges[].from_selector,
//                paths[].edges[].relationship, paths[].edges[].category,
//                paths[].edges[].confidence, paths[].edges[].snapshot,
//                paths[].edges[].source.file, paths[].edges[].source.line,
//                paths[].edges[].note, no_path_reasons
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

const POLL_INTERVAL_MS = 500;

/** The per-launch bearer token, held in memory only for the life of this page. */
let authToken = null;

function main() {
  authToken = extractToken();
  if (!authToken) {
    document.getElementById("token-gate").hidden = false;
    return;
  }
  // The fragment is never sent to the server, but stripping it here also
  // keeps the credential out of the visible address bar and browser history
  // going forward.
  history.replaceState(null, "", window.location.pathname + window.location.search);

  document.getElementById("app").hidden = false;
  enableArrowNavigation(document.getElementById("change-groups"));
  enableArrowNavigation(document.getElementById("evidence-primary"));
  enableArrowNavigation(document.getElementById("evidence-heuristic"));
  enableArrowNavigation(document.getElementById("candidate-tests"));

  run().catch((error) => {
    setText(document.getElementById("indexing-notice"), `Failed to load: ${error.message}`);
  });
}

function extractToken() {
  const hash = window.location.hash.replace(/^#/, "");
  const params = new URLSearchParams(hash);
  const token = params.get("token");
  return token && token.length > 0 ? token : null;
}

async function run() {
  const comparison = await pollComparisonUntilReady();
  renderHeader(comparison);
  const changed = await apiGet("/api/changed-symbols");
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

/** Roving-focus arrow-key navigation across every `.row-button` in `container`. */
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

// ---------------------------------------------------------------------------
// Header
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
    setText(indexingNotice, "Indexing both revisions…");
  }
}

function shortSha(sha) {
  return typeof sha === "string" && sha.length > 0 ? sha.slice(0, 7) : "unknown";
}

// ---------------------------------------------------------------------------
// Pane 1 — change list
// ---------------------------------------------------------------------------

function renderChangeList(changed) {
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
    list.appendChild(el("li", null, [button]));
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
  loadRelationshipView(symbol).catch((error) => {
    setText(document.getElementById("evidence-bounds"), `Failed to load evidence: ${error.message}`);
  });
}

// ---------------------------------------------------------------------------
// Pane 2 — focused relationship view
// ---------------------------------------------------------------------------

function primaryReference(symbol) {
  if (symbol.head) return { selector: symbol.head.selector, side: "head" };
  if (symbol.base) return { selector: symbol.base.selector, side: "base" };
  return null;
}

async function loadRelationshipView(symbol) {
  const reference = primaryReference(symbol);
  document.getElementById("relationship-empty").hidden = true;
  document.getElementById("relationship-content").hidden = false;

  const identity = reference ? parseSelector(reference.selector) : { name: "(no resolvable side)" };
  setText(document.getElementById("selected-symbol-heading"), identity.name || "(unnamed)");
  setText(document.getElementById("selected-symbol-selector"), reference ? reference.selector : "");

  await renderSymbolSource(symbol, reference);

  if (!reference) {
    setText(document.getElementById("evidence-bounds"), "No resolvable side for this entry.");
    return;
  }

  const [evidence, candidates] = await Promise.all([
    apiGet("/api/evidence", { selector: reference.selector, side: reference.side }),
    apiGet("/api/candidate-tests", { selector: reference.selector, side: reference.side }),
  ]);
  renderEvidence(evidence, reference.side);
  renderCandidateTests(candidates);
}

function renderEvidence(evidence, side) {
  const bounds = evidence.query_options || {};
  setText(
    document.getElementById("evidence-bounds"),
    `Bounds in force: depth ${bounds.depth}, confidence ≥ ${bounds.min_confidence}, ` +
      `source excerpts ≤ ${bounds.source_max_bytes} bytes` +
      (evidence.skipped_low_confidence
        ? `; ${evidence.skipped_low_confidence} candidate(s) excluded by the confidence floor`
        : ""),
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
    const edge = path.edges[0];
    if (!edge) continue;
    const row = renderEvidenceRow(path, edge, side);
    if (edge.category === "heuristic_match") {
      heuristicList.appendChild(row);
      heuristicCount += 1;
    } else {
      primaryList.appendChild(row);
      primaryCount += 1;
    }
  }

  if (primaryCount > 0) primary.appendChild(primaryList);

  if (heuristicCount > 0) {
    const group = el("div", { className: "evidence-heuristic-group" }, [
      el("h5", { text: "Heuristic / fallback matches" }),
      heuristicList,
    ]);
    heuristic.appendChild(group);
  }

  const noPath = document.getElementById("evidence-no-path");
  if ((evidence.paths || []).length === 0 && (evidence.no_path_reasons || []).length > 0) {
    setText(noPath, evidence.no_path_reasons.join(" "));
    noPath.hidden = false;
  } else {
    noPath.hidden = true;
  }
}

function renderEvidenceRow(path, edge, side) {
  const line = [
    el("span", { className: "row-name", text: edge.from }),
    badge(edge.relationship),
    badge(edge.category.replace(/_/g, " ")),
    badge(edge.confidence),
    badge(edge.snapshot),
  ];
  if (path.truncated) {
    line.push(el("span", { className: "truncation-marker", text: `[truncated by ${path.truncated_by}]` }));
  }
  const fileLine = el("span", {
    className: "row-file",
    text: `${edge.source.file}${edge.source.line ? `:${edge.source.line}` : ""}${edge.note ? ` — ${edge.note}` : ""}`,
  });
  const button = el(
    "button",
    { className: "row-button", attrs: { type: "button", "aria-pressed": "false" } },
    [el("span", { className: "row-line" }, line), fileLine],
  );
  button.addEventListener("click", () => selectEvidenceRow(edge, button));
  return el("li", null, [button]);
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
  clear(panels);

  const sides = [];
  if (symbol.base) sides.push({ selector: symbol.base.selector, side: "base" });
  if (symbol.head) sides.push({ selector: symbol.head.selector, side: "head" });
  if (sides.length === 0 && reference) sides.push(reference);

  if (sides.length === 0) {
    symbolSourceNode.hidden = true;
    emptyNode.hidden = false;
    return;
  }
  emptyNode.hidden = true;
  symbolSourceNode.hidden = false;
  panels.className = sides.length > 1 ? "source-side-by-side" : "";

  const views = await Promise.all(
    sides.map((entry) => apiGetRaw("/api/source", { selector: entry.selector, side: entry.side })),
  );
  for (const view of views) {
    panels.appendChild(renderSourcePanel(view, null));
  }
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
