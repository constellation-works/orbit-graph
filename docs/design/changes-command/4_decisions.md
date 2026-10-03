# Changes command: decisions

Decisions for ORB-13256, which exposes the `orbit-graph-changes` analysis to
agents as `orbit-graph changes` and the `orbit.graph.changes` plugin tool
(v2 alias `graph.changes`). The user-facing contract is in
[`docs/usage.md`](../../usage.md#change-analysis) and
[`docs/plugin.md`](../../plugin.md#change-analysis); the measurements behind
D1 are in [`evals/changes-command/`](../../../evals/changes-command/README.md).
The evidence contract underneath (snapshots, pairing, evidence categories) is
unchanged from [`change-explorer.md`](../change-explorer.md).

## D1. Cost: analysis, not indexing, dominates; every call is budgeted

The task expected about 12 s for a cold call (two full syncs of Orbit) and a
cache hit when warm. Measured on an Orbit clone (studies 2 and 3, release
build, shared 15-core host):

| | Cold | Warm |
| --- | --- | --- |
| Indexing both sides (`prepare_ms`) | 6.0–7.5 s | < 0.2 s (cache hit) |
| Analysis (`analysis_ms`) | 18–36 s | 19–40 s |
| Whole call | 25–43 s | 19–40 s |

Indexing matched the estimate. The analysis did not: each changed symbol runs
a bounded inbound traversal per side, and on Orbit every traversal costs at
least ~150 ms inside `orbit_graph`'s impact query, even when it finds nothing.
Heavy symbols cost about 1 s. Two cheap fixes landed here. One traversal per
symbol and side now serves evidence, entry points and call-path candidate
tests (memoized by query). Test-file imports and runtime invocations are read
once per snapshot, not once per changed symbol: `runtime_invocations()` alone
takes ~3 s per side on Orbit. What remains is the per-traversal floor in the
core graph; see D11.

So a call is bounded by time, not by an assumed cost. `--budget-ms` (CLI,
none by default) and `budget_ms` (plugin, default 90 000, range
1 000–110 000) cover the whole call, indexing included:

- a deadline cancels indexing through the comparison's progress hook;
- each traversal's own budget is capped at the time remaining;
- symbols not reached are listed in `not_analysed` with reason
  `time_budget_ms`;
- the result says `complete: false` with the `phase` it reached.

The plugin's ceiling, 110 000 ms, leaves 10 s under Orbit's 120 s tool
timeout for work that cannot be interrupted mid-query (one traversal, one
`runtime_invocations` read) and for serialization. A call therefore returns
an explicit incomplete result instead of being killed.

## D2. Default base: merge base with the branch the work will land on

Without a range, the working tree is compared against
`merge-base(HEAD, X)`, where X is the first of these that resolves:

1. the branch's upstream;
2. `origin/HEAD`;
3. `main`;
4. `master`.

The merge base, not X's tip, is the base, so other people's commits on X since
the branch point never show up as "your" changes. Nothing is fetched: a stale
remote-tracking ref gives an older merge base, not a network call. The choice
is reported in `default_base` (`reference`, `source`, `merge_base`). When none
resolves, the command fails with `no_default_base` and names every ref it
tried. `<base>` alone compares the working tree against that revision, and
`<base>..<head>` compares two revisions. A separate `--worktree` flag was not
added: the bare `<base>` form already says it, and `...` (merge-base syntax)
is refused rather than guessed at.

## D3. Default bounds, and every bound is named when hit

| Bound | CLI default | Plugin default | Range |
| --- | --- | --- | --- |
| `max_symbols` (changed symbols analysed) | 50 | 25 | 1–1000 |
| `max_callers` per symbol | 10 | 5 | 1–500 |
| `max_entry_points` per symbol | 5 | 3 | 1–100 |
| `max_tests` per symbol | 10 | 5 | 1–500 |
| `depth` | 3 | 3 | 1–10 |
| `node_cap` per traversal | 200 | 200 | 1–2000 |
| `query_budget_ms` per traversal | 5000 | 5000 | 100–60000 |
| `budget_ms` whole call | none | 90 000 | 1000–3 600 000 (plugin ≤ 110 000) |
| response ceiling | none | 512 KiB | — |

Depth, node cap and per-traversal budget keep the library defaults the
explorer evaluation measured. The per-symbol caps are what make the answer
readable: study 2's `teardown.rs#execute` has 94 callers and 115 candidate
tests, almost all of them the fuzzy `execute` fan-out. The plugin halves them
because an agent's context is the scarce resource. Lists are ordered before a cap applies:

- callers by evidence category, then distance, then confidence;
- entry points by distance, then category, then confidence;
- tests by source, then category, then confidence.

A cap never drops a stronger item for a weaker one of the same kind. Each list reports `*_found`, and every cap that cut something
adds a `truncation` flag naming the bound. A value outside its range is
refused (`argument_error` / `invalid_request`), not clamped.

The plugin's `result` must also fit 512 KiB. Callers carry their evidence path
(~1.5 KB each), so the 256 KiB ceiling of the query tools would cut even
default answers. Above the ceiling the document is shrunk in recorded steps,
each adding a `max_response_bytes` flag:

1. halve every per-symbol list;
2. move symbols to `not_analysed`;
3. halve the standing lists.

A document that cannot fit even when empty fails with `graph_error`.

## D4. Contract: the library's report types, one new document, `schema_version` 1

The document is `orbit_graph_changes::analysis::ChangesDocument`
(`schemas/changes.response.json`). It is the report's own content regrouped
per changed symbol, not a new vocabulary. It reuses:

- `EndpointRef`, `ReportEvidencePath` (every hop with `file:line@sha`),
  `EvidenceCategory` and `CandidateSource` for the evidence;
- the pairing types for changed symbols;
- `ComparisonView`, `TruncationFlag`, `UnresolvedArea` and
  `OutOfScopeEntry` for everything else.

It adds `schema_version: 1` (`CHANGES_SCHEMA_VERSION`), independent of the
explorer report's version. The CLI prints the document itself. The plugin
wraps it as `{schema_version, operation: "changes", repository, complete,
truncated, result}` like its other tools, and `result` is byte-for-byte the
CLI document.

Each caller, entry point and test is labelled on two axes:

- **`source`, how it reaches the change.**
  - Callers and entry points: `call_path` (every hop a call),
    `import_relationship` (a hop is an import), `reference_path` (type or
    other references), or `changed_symbol` (distance 0: the changed symbol is
    itself the entry point, for example a changed test).
  - Tests: the library's `call_path`, `import_relationship`,
    `naming_heuristic` or `runtime_invocation`.
- **`confidence`, how well it resolved.**
  - The weakest `orbit_graph` confidence on the path (`exact` at distance 0).
  - For a test with no path: `file_import` for an import match, `name_only`
    for a naming or runtime match.

`PLUGIN_SCHEMA_VERSION`, the conformance `version.yaml` and the launcher pins
are unchanged. As with ORB-13095's query tools, a new tool is additive: no
existing request or response changes shape, and the new tool's schemas are
referenced from its own manifest entry.

## D5. Confidence floor `same_module`, fuzzy fallbacks labelled

The default floor is `same_module`, the same as `refs` and `impact`. The
library still returns a call whose receiver type is unknown at `fuzzy_name`
rather than dropping it (a genuine call through an unknown receiver must not
vanish). Such items are therefore labelled, sorted last, and first to go under
a cap. They are not hidden. `--confidence fuzzy` / `"confidence":"fuzzy_name"`
admits every name-only match; the evaluation runs both profiles.

## D6. Where snapshots are cached

**CLI.** Committed snapshots are cached in
`.orbit-graph/explorer/snapshots`, the library's existing default under the
graph's own scratch directory. `--cache-dir` moves the cache, and `--no-cache`
builds in temporary directories and writes nothing.

**Plugin.** Snapshots are cached in `<plugin state>/<repository
hash>/changes-snapshots`, next to the plugin's code-graph index, and never in
the repository. Temporary trees go under `changes-scratch` beside it
(`ComparisonOptions::scratch_dir`), because the manifest grants writes to
`{{workspace}}/.orbit-graph` and `{{plugin_state}}` only, not to the system
temporary directory. Without `ORBIT_PLUGIN_STATE` the tool caches nothing and
adds a notice. It never falls back to the repository.

**Working tree.** The working-tree head is never cached. Its content has no
commit to key on, so it is rebuilt every call, and the cache records
`head_cache: "disabled"` with a note.

**Deviation, `STD-01@2 §R31`.** `changes` only reports, yet by default it
writes a snapshot cache (and the plugin tool is declared `read_only` while
writing to plugin state). A cold call is otherwise re-paid in full every time
(D1), and review and test selection call it repeatedly over the same commits.
The mitigations:

- the cache is scratch state in the tool's own directory, never the user's
  files, index or refs;
- entries are immutable and keyed by commit, extractor and store schema, and
  published by atomic rename;
- a cache that cannot be created or written makes the call index into
  temporary trees and say so in `cache_note`, so the command still works on a
  read-only checkout;
- `--no-cache` writes nothing;
- the working tree and Git index are never modified (tested).

## D7. Validation before indexing; typed errors (ORB-13168 carry-over)

Every input is checked before the first snapshot is materialized
(`STD-02@2 §R34`):

- range syntax;
- every bound's range;
- that each `--symbol` is a `symbol:` selector;
- that both revisions resolve.

Failures are typed (`AnalysisError`, wrapping `SnapshotError`, both
`#[non_exhaustive]`) and map to stable codes:

| Failure | CLI code | Plugin code |
| --- | --- | --- |
| Malformed range, bound or selector | `argument_error` (exit 2) | `invalid_request` |
| Revision that does not resolve | `revision_not_found` | `invalid_request` |
| No default base | `no_default_base` | `invalid_request` |
| Not a repository | `repository_unavailable` | `repository_unavailable` |
| Anything else | `changes_error` | `graph_error` |

The other carry-over items:

- **No false "unchanged".** The working-tree diff is taken against the index
  and working directory together, untracked files included, so an uncommitted
  or new file is a change. A clean tree against `HEAD` reports no changes, and
  ambiguous rows stay `uncertain` instead of being dropped.
- **No leaked staging directories.** A cache entry staged but never published
  (build failed, cancelled, or a panic) is removed by its drop guard, and
  temporary trees are `TempDir`s under the scratch parent.

## D8. The CLI follows the output contract

`--json` and `--format json|ndjson|table|auto` behave as for every other
command. The table view has two tables, symbols and tests. NDJSON emits one
`changes_context` record, then a `changed_symbol` record per symbol and a
`candidate_test` record per test. Notices (cuts, `incomplete`, unmatched
selectors, cache notes) go to stderr in every mode. The command is not in
`json_flag_is_byte_identical_to_format_json_for_every_command`, because its
document carries `generated_at` and wall-clock `timings`. Its own test compares
`--json` with `--format json` with those two fields removed.

## D9. Where the agent guidance lives

The task named `plugin/skills/orbit-graph/SKILL.md` and/or
`skills/recommendations/SKILL.md`. The recommendations skill became
`skills/orbit-graph/` in #111, and the `plugin/` tree is the deprecated
compatibility install: it lists no query tools and its manifest is
unchanged. The guidance on when to call the tool (after implementing, before
review, to pick tests) is in `skills/orbit-graph/SKILL.md`, `docs/plugin.md`
and the tool's manifest description. The tool is declared only in the root
`plugin.yaml`.

## D10. Layering

`orbit-graph-cli` (tier 4) now depends on `orbit-graph-changes` (tier 3), as
`ARCHITECTURE.md` and `scripts/check-dependency-direction.sh` record.
`orbit-graph-changes` still uses only `orbit_graph`'s public API.

## D11. Follow-ups

- **Orbit's review pipeline** (ws_orbit): calling `orbit.graph.changes` on a
  delivery before review, and passing `result.tests` to the test step, is out
  of scope here.
- **Per-traversal cost in `orbit_graph`.** An inbound impact traversal costs
  ≥ 150 ms on Orbit even with no result.
  `Graph::runtime_invocations` runs a correlated subquery per
  `runtime_invocation` ref and takes ~3 s on Orbit. Both are core-graph work;
  fixing them would bring a warm call toward the few seconds the task
  expected.
- **Citing the right line of a fuzzy dispatch.** When several arms of one
  dispatcher fuzzy-match the changed method, the caller is found but the
  evidence cites the first arm's line, not the matching arm's (study 2,
  [comparison](../../../evals/changes-command/README.md#study-2)).
