# Recommendations from verified changes

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
and cutoff is eligible.

Live supplied snapshots still trigger public authority,
workspace, task, and repository verification. `hybrid: true` asks the configured
public `orbit.search` surface for ranked task hits. If it is unavailable, the
response labels the deterministic lexical fallback.

For live free-text `query` requests over Git-only history, a delivery's commit
message can stand in for missing task text. Such contributions have the reason
kind `historical_change_commit_text`, are labelled post-execution, are
down-weighted, and the response lists the `git_commit_text_used` fallback. Task
IDs cited in a message are hints, not `supporting_task_ids`. Strict replay
(`cutoff`) and task-ID requests never use commit text.

A failed call returns `error.code`:
- `invalid_request` (fix the request: unknown field, unsupported `schema_version`, out-of-range bound, missing required field), `repository_unavailable` (the routed `repository` is missing or not a Git repository), `index_missing` / `index_incompatible` (query tools: run `graph_sync`, with `full: true` for incompatible), or `not_found` (a named revision does not exist).
- `timeout` (a lock wait or Orbit callback ran out of time; `retryable: true`, so retry), `orbit_refused` (Orbit refused a callback; its own code is `error.orbit.code`), or `graph_error` (an index, Git, or subprocess failure; read the message). Retry only when `error.retryable` is true.

`timeout` (a lock wait or Orbit callback ran out of
time; `retryable: true`, so retry), `orbit_refused` (Orbit refused a callback;
its own code is `error.orbit.code`), or `graph_error` (an index, Git, or
subprocess failure; read the message). Retry only when `error.retryable` is
true.

