---
name: orbit-graph-recommendations
description: Query leakage-safe file or symbol recommendations from verified delivered changes through the Orbit external tools.
---

# Orbit graph recommendations

Call `orbit.graph.status` first when freshness matters. Always pass an explicit
absolute `repository`; never treat tool cwd or `ORBIT_TOOL_WORKSPACE_ROOT` as an
authority selector. Pass the owning Orbit `workspace` for task-ID lookup or
hybrid search.

Use `orbit.graph.recommend` with `schema_version: 1` and exactly one of `query`
or `task_id`. Set `level` to `file` for planning a change surface or `symbol`
for implementation navigation. Results are ranked evidence, not an instruction
to mutate a task's `context_files`. Inspect `supporting_task_ids`,
`supporting_delivery_ids`, contribution `reasons`, `source_freshness`, and
`fallbacks`. A `file:` result in symbol mode is deliberate fallback evidence.

For a pending task, call by task ID before execution begins so the public task
observation is attestably pre-execution. During or after execution, provide a
previously captured versioned `task_snapshot`; current task text cannot prove
what text existed before the run. `hybrid: true` asks the configured public
`orbit.search` surface for ranked task hits. If it is unavailable, the response
labels the deterministic lexical fallback.

Maintenance is deliberate. `orbit.graph.maintain` supports:

- `history_sync`: bounded, atomic first-parent Git-only evidence;
- `import`: one public DeliveryImport v2 envelope;
- `orbit_sync`: a bounded explicit list of run IDs and/or task IDs, using public
  `orbit.task.show`, `orbit run show`, and Git reachability checks.

`orbit_sync` is idempotent but reports partial coverage because current Orbit
does not expose a cursor-paginated detailed delivery feed. Resume by resubmitting
omitted IDs. Do not call `history rebuild` casually: verified envelopes must be
replayed afterward.

The calling activity must allow `orbit.task.show` for live task lookup and
`orbit.search` for hybrid retrieval in addition to the external tool; Orbit's
policy still applies to the adapter's nested public calls. When those tools are
not granted, pass an earlier public `task_snapshot` and use lexical/offline hits.
