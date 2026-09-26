---
name: graph-recommendations
description: Query leakage-safe file or symbol recommendations from verified delivered changes through the Orbit graph plugin.
---

# Orbit graph recommendations

Tool names: a verified first-party install (`orbit plugin add
git+https://github.com/constellation-works/orbit-graph#<tag>`) registers
`orbit.graph.*` (MCP `orbit_graph_*`, CLI `orbit graph <verb>`). A copy
installed from any other source registers bare `graph.*` (MCP `graph_*`)
instead; use whichever spelling your tool list shows. The names below use the
first-party spelling.

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
Git repository), or `graph_error` (an index, Git, or callback failure; read the
message).

Maintenance is deliberate. `orbit.graph.maintain` supports:

- `history_sync`: bounded, atomic, resumable first-parent Git-only evidence;
- `import`: one public DeliveryImport v2 envelope;
- `orbit_sync`: a bounded explicit list of run IDs and/or task IDs, using public
  `orbit.workspace.list`, `orbit.task.show`, `orbit.workflow.run.show`, and Git
  reachability checks.

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
