---
name: graph-recommendations
description: Navigate code (search, show, refs, callees, impact, trace, deps, overview) and query leakage-safe file or symbol recommendations from verified delivered changes through the Orbit graph plugin.
---

# Orbit graph code navigation and recommendations

Tool names: a verified first-party install (`orbit plugin add
git+https://github.com/constellation-works/orbit-graph#<tag>`) registers
`orbit.graph.*` (MCP `orbit_graph_*`, CLI `orbit graph <verb>`). A copy
installed from any other source registers bare `graph.*` (MCP `graph_*`)
instead; use whichever spelling your tool list shows. The names below use the
first-party spelling.

## Code navigation

Eight read-only tools answer precise structural questions from the plugin's
code-graph index. Prefer them over grep when you need exact callers, callees,
or blast radius:

| Question | Tool | Key input |
|---|---|---|
| Where is a symbol, string, or config key? | `orbit.graph.search` | `query`, optional `kind`, `lang` |
| What does this symbol's source say? | `orbit.graph.show` | `selector`, `max_bytes` |
| Who calls or uses this symbol? | `orbit.graph.refs` | `selector`, `confidence`, `kind` |
| What does this function call? | `orbit.graph.callees` | `selector` |
| What breaks if I change this? | `orbit.graph.impact` | `selector`, `direction`, `depth` |
| What runs under this CLI command? | `orbit.graph.trace` | `command`, `depth` |
| What does this file or directory import? | `orbit.graph.deps` | `file:` or `dir:` `selector` |
| What is in this repository or directory? | `orbit.graph.overview` | optional `selector`, `format` |

Start with `search` or `overview` to find selectors, then use them in the
other tools. Selectors look like `symbol:src/lib.rs#parse:function`,
`file:src/lib.rs`, or `dir:src`. The default `confidence` is `same_module`.
When `refs` finds nothing at that level, it returns name-only matches under
`fallback`. Pass `confidence: fuzzy_name` to include callers that go through
re-exports.

Each response carries `index`, which names the revision it describes. When
`index.fresh` is false, the results describe older code: run the call in
`index.stale.fix` (`graph_sync`) and retry if the difference matters. A tool
fails with `index_missing` until `graph_sync` has run once, and with
`index_incompatible` when the index needs a full rebuild (`full: true`).
Arrays are capped by `limit` (default 50, search 20) and a 256 KiB ceiling.
`truncated` and `truncation` report every cut, so narrow the query instead of
raising limits.

## Recommendations

Call `orbit.graph.status` first when freshness matters. Always pass an explicit
absolute `repository`; never treat tool cwd or `ORBIT_TOOL_WORKSPACE_ROOT` as an
authority selector. Pass the owning Orbit `workspace` for task-ID lookup,
hybrid search, or Orbit synchronization. The plugin lets the host's `orbit`
executable resolve its own global root.

Use `orbit.graph.recommend` with `schema_version: 1` and exactly one of `query`
or `task_id`. Set `level` to `file` for planning a change surface or `symbol`
for implementation navigation. Results are ranked evidence, not an instruction
to mutate a task's `context_files`. Inspect `supporting_task_ids`,
`supporting_delivery_ids`, contribution `reasons`, `source_freshness`, and
`fallbacks`. A `file:` result in symbol mode is deliberate fallback evidence.

Structural evidence (`structure_applied: true`) comes from the plugin's
code-graph index, which only `graph_sync` builds. `orbit.graph.status` reports
it as `code_index` with `fresh: true` when it matches the checkout. If a
recommendation's `fallbacks` include `structure_index_missing` or
`structure_index_stale`, run `orbit.graph.maintain` with `operation:
graph_sync`; for `structure_index_incompatible`, add `full: true`. Lexical and
history evidence still apply meanwhile.

For live requests, current started/completed task text is usable after its
observation but remains honestly labeled post-execution. Supplying `cutoff`
selects strict replay, where only a versioned snapshot attested before execution
and cutoff is eligible. Live supplied snapshots still trigger public authority,
workspace, task, and repository verification. `hybrid: true` asks the configured
public `orbit.search` surface for ranked task hits. If it is unavailable, the
response labels the deterministic lexical fallback.

A failed call returns `error.code` `invalid_request` (fix the request: unknown
field, unsupported `schema_version`, out-of-range bound, missing required
field), `repository_unavailable` (the routed `repository` is missing or not a
Git repository), `index_missing` / `index_incompatible` (query tools: run
`graph_sync`, with `full: true` for incompatible), or `graph_error` (an index,
Git, or callback failure; read the message).

Maintenance is deliberate. `orbit.graph.maintain` supports:

- `history_sync`: bounded, atomic, resumable first-parent Git-only evidence;
- `import`: one public DeliveryImport v2 envelope;
- `orbit_sync`: a bounded explicit list of run IDs and/or task IDs, using public
  `orbit.workspace.list`, `orbit.task.show`, `orbit.workflow.run.show`, and Git
  reachability checks.
- `graph_sync`: build the code-graph index within `budget_ms` (1000-110000,
  default 90000) and publish it only when complete. Incremental by default;
  `full: true` re-extracts every file. `coverage.state: budget_exhausted` means
  nothing was published and the previous index is unchanged: retry with a
  larger budget, or keep working without structure. A second concurrent call
  fails with `graph_error` while the first is building.

`orbit_sync` is idempotent but reports partial coverage because current Orbit
does not expose a cursor-paginated detailed delivery feed. Resume by resubmitting
omitted IDs. Repeated Git sync calls follow `resume_from` until `complete:true`,
then become a no-op; the complete cursor does not advance during partial
bootstrap. Do not call `history rebuild` casually: verified envelopes must be
replayed afterward.

The calling activity must allow the callback tools it uses:
`orbit.workspace.list`, `orbit.task.show`, `orbit.search`, and
`orbit.workflow.run.show`. Orbit's policy still applies to the adapter's nested
public calls. When task or search tools are not granted, pass an earlier public
`task_snapshot` and use lexical/offline hits.

Workspace discovery is MCP-only: the adapter calls `orbit.workspace.list`
through a short-lived `orbit mcp serve` stdio session (never `--operator`),
under the same timeout and output bound as its `orbit tool run` calls. Discovery
publishes no checkout path, so the requested `repository` is bound to the
workspace by matching its `origin` remote to the workspace's `git_remote`; a
workspace without `git_remote`, or a foreign `origin`, is refused.
`orbit.workflow.run.show` is an operator-only Orbit operation. A plugin backend
does not hold `operator`, so `orbit_sync` reports each run `excluded` with
Orbit's `capability_denied` reason instead of importing it; an empty or
all-excluded sync is not evidence of a verified delivery. Until Orbit exposes a
sanctioned non-operator run read, import verified envelopes with `import`.
