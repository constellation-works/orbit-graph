---
name: orbit-graph
description: Navigate code (search, show, refs, callees, impact, trace, deps, overview), analyse a change for affected callers and tests to run (changes), and query leakage-safe file or symbol recommendations from verified delivered changes through the Orbit graph plugin.
---

# Orbit graph code navigation and recommendations

Tool names: a verified first-party install (`orbit plugin add
git+https://github.com/constellation-works/orbit-graph#<tag>`) registers
`orbit.graph.*` (MCP `orbit_graph_*`, CLI `orbit graph <verb>`). A copy
installed from any other source registers bare `graph.*` (MCP `graph_*`)
instead; use whichever spelling your tool list shows. The names below use the
first-party spelling.

If the tools are missing, fail with `incompatible_binary`, or every call fails
with `index_missing`, follow [references/setup.md](references/setup.md) first.

## Code navigation

Eight read-only tools answer precise structural questions from the plugin's
code-graph index. Prefer them over grep when you need exact callers, callees,
or blast radius:

| Question | Tool | Key input |
|---|---|---|
| Where is a symbol, string, or config key? | `orbit.graph.search` | `query`, optional `kind`, `lang` |
| What does this symbol's source say? | `orbit.graph.show` | `selector`, `max_bytes` |
| Who calls or uses this symbol? | `orbit.graph.refs` | `selector`, `confidence`, `kind` |
| What does this function call? | `orbit.graph.callees` | `selector`, `include_unresolved` |
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
Every array, nested ones included, is capped by `limit` (default 50, search
20) and a 256 KiB ceiling. `truncated` and `truncation` report every cut by
field path (for example `files[].symbols`), so narrow the query instead of
raising limits. `callees` omits unresolved calls with no indexed definition
(standard-library calls such as `map_err`) and counts them in
`hidden_unresolved`; pass `include_unresolved: true` to see them.

## Change analysis

`orbit.graph.changes` answers "what else could this break?" and "which tests
should I run?" for a whole change in one call. Call it:

- **after implementing**, before you call the work done: read each changed
  symbol's `callers` and `entry_points` and check the ones your edit could
  break;
- **before review**, so the review starts from the affected surface and its
  evidence rather than from the diff alone;
- **to pick tests**: run the entries of `result.tests`, strongest first, and
  say which you ran. A candidate is what the evidence points at, not proof of
  coverage.

With no `base`/`head` it compares the working tree, untracked files included,
against the merge base with the upstream (else `origin/HEAD`, `main`,
`master`); `base` alone compares the working tree against it; `base` and
`head` compare two revisions. It indexes both sides itself, so it needs no
`graph_sync`, but a cold call can take tens of seconds on a large repository;
repeat calls over the same commits reuse cached snapshots.

`changes` needs writable plugin scratch/cache state, even though it does not
change repository files or Git state. This is the documented cache exception
to read-only execution; it is not a promise that a cold call works with plugin
state made read-only.

Trust labels, not list position. Every caller, entry point and test carries a
`source` (`call_path`, `import_relationship`, `reference_path`,
`changed_symbol`, `naming_heuristic`, `runtime_invocation`) and a
`confidence`. `fuzzy_name`, `name_only` and `file_import` are name or file
matches, often to an unrelated symbol that shares the name; confirm them in
the source before acting on them. When `complete` is false, the budget ran out:
`incomplete` says where, and `not_analysed` lists what was skipped. Every cut
is in `truncation`: narrow with `symbols` (selectors of changed symbols),
`scope` or `language` before raising `max_*`.

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

For live free-text `query` requests over Git-only history, a delivery's commit
message can stand in for missing task text. Such contributions have the reason
kind `historical_change_commit_text`, are labelled post-execution, are
down-weighted, and the response lists the `git_commit_text_used` fallback. Task
IDs cited in a message are hints, not `supporting_task_ids`. Strict replay
(`cutoff`) and task-ID requests never use commit text.

A failed call returns `error.code` `invalid_request` (fix the request: unknown
field, unsupported `schema_version`, out-of-range bound, missing required
field), `repository_unavailable` (the routed `repository` is missing or not a
Git repository), `index_missing` / `index_incompatible` (query tools: run
`graph_sync`, with `full: true` for incompatible), `not_found` (a named
revision does not exist), `timeout` (a lock wait or Orbit callback ran out of
time; `retryable: true`, so retry), `orbit_refused` (Orbit refused a callback;
its own code is `error.orbit.code`), or `graph_error` (an index, Git, or
subprocess failure; read the message). Retry only when `error.retryable` is
true.

Maintenance is deliberate. `orbit.graph.maintain` supports:

- `history_sync`: bounded, atomic, resumable first-parent Git-only evidence;
- `import`: one public DeliveryImport v2 envelope;
- `orbit_sync`: bounded explicit `task_runs` pairs (`{"task_id", "run_id"}`)
  and/or `task_ids` (each task's current `job_run_id`), read through public
  `orbit.workspace.list`, `orbit.task.show` and the task-scoped
  `orbit.workflow.run.delivery`, then checked against Git. Bare `run_ids` are
  retired and refused: no public read binds a run to its task.
- `graph_sync`: build the code-graph index within `budget_ms` (1000-110000,
  default 90000) and publish it only when complete. Incremental by default;
  `full: true` re-extracts every file. `coverage.state: budget_exhausted` means
  nothing was published and the previous index is unchanged: retry with a
  larger budget, or keep working without structure. A second concurrent call
  fails with `graph_error` while the first is building.

`orbit_sync` is idempotent but reports partial coverage because current Orbit
does not expose a cursor-paginated delivery feed. Resume by resubmitting omitted
pairs or task IDs. Repeated Git sync calls follow `resume_from` until
`complete:true`, then become a no-op; the complete cursor does not advance
during partial bootstrap. `history rebuild --branch <name>` previews the scope
and its verified delivery count without changing it. Add `--confirm` to rebuild;
verified deliveries are preserved and re-extracted by default.
`--discard-verified` requires `--confirm` and explicitly removes them.

The calling activity must allow the callback tools it uses:
`orbit.workspace.list`, `orbit.task.show`, `orbit.search`, and
`orbit.workflow.run.delivery`. Orbit's policy still applies to the adapter's
nested public calls. For offline strict replay, pass an eligible earlier public
`task_snapshot` with `cutoff` and use lexical hits. A live supplied snapshot
still requires task/workspace authority; it cannot bypass a refused callback.

Workspace discovery is MCP-only: the adapter calls `orbit.workspace.list`
through a short-lived `orbit mcp serve` stdio session (never `--operator`),
under the same timeout and output bound as its `orbit tool run` calls. Orbit
normalizes registered workspace names and IDs to absolute checkout paths before
calling a plugin. The adapter verifies that path matches `repository`, then
reads the task with the same workspace filter. Its public owner ID and name
select the discovery row; shared remotes never establish task ownership. The
repository's `origin` must also match that row's `git_remote`. This remote
match is an accident guard, since the checkout owner can rewrite it; Orbit's
per-call authorization is the security boundary. A missing owner, inactive
workspace, absent remote or conflicting repository is refused.

Only a host-reported `delivery_status: landed` whose commits Git verifies on the
landing branch is imported. `committed` (no verified landing), `no_change`,
`in_progress`, `not_delivered`, `unavailable`, a task with no current run, an
answer for another workspace or repository, and landed evidence Git cannot
confirm are `excluded` with a reason naming the status or check. The answer's
`repository` must equal the identity recomputed from the routed `origin`
(`owner/name` on GitHub, else `git:` and the SHA-256 of the exact origin URL).
A squash landing is accepted only when `landed_commit` is on the branch, builds
on `base_sha` and makes exactly the tree changes of `base_sha..head_sha` (same
paths, object IDs and modes on both sides). A foreign
task/run pair, which Orbit refuses with `invalid_input`, is `failed`. So are an
answer naming another task or run, an unsupported `schema_version`, malformed
evidence and a missing local commit. `failed` is counted in `coverage.failed`
and `excluded` in `coverage.excluded`. Orbit 0.25.1 can emit a diagnostic before
its refusal JSON; Graph then reports `graph_error` with the refusal in the
message rather than structured `orbit_refused`. Read the per-item outcomes, not
just the outer successful batch response. `import` stores supplied envelopes as
caller-attested evidence; it does not establish host-verified delivery status.
