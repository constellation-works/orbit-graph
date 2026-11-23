# Recommendation indexes

Maintenance is deliberate. `orbit.graph.maintain` supports:

- `history_sync`: bounded, atomic, resumable first-parent Git-only evidence;
- `import`: one public DeliveryImport v2 envelope;
- `orbit_sync`: bounded explicit `task_runs` pairs (`{"task_id", "run_id"}`)
  and/or `task_ids` (each task's current `job_run_id`), read through public
  `orbit.workspace.list` and `orbit.task.show` (`field: "delivery"` for a
  run), then checked against Git. Bare `run_ids` are
  retired and refused: no public read binds a run to its task.
- `graph_sync`: build the code-graph index within `budget_ms` (1000-110000,
  default 90000) and publish it only when complete. Incremental by default;
  `full: true` re-extracts every file. `coverage.state: budget_exhausted` means
  nothing was published and the previous index is unchanged: retry with a
  larger budget, or keep working without structure. A second concurrent call
  fails with `graph_error` while the first is building.

`orbit_sync` is idempotent but reports partial coverage because current Orbit
does not expose a cursor-paginated delivery feed. Resume by resubmitting omitted
pairs or task IDs.

Repeated Git sync calls follow `resume_from` until
`complete:true`, then become a no-op; the complete cursor does not advance
during partial bootstrap. `history rebuild --branch <name>` previews the scope
and its verified delivery count without changing it. Add `--confirm` to rebuild;
verified deliveries are preserved and re-extracted by default.
`--discard-verified` requires `--confirm` and explicitly removes them.

The calling activity must allow the callback tools it uses:
`orbit.workspace.list`, `orbit.task.show`, and `orbit.search`. Delivery
evidence is `orbit.task.show` with `field: "delivery"`. Orbit's policy still applies to the adapter's
nested public calls. For offline strict replay, pass an eligible earlier public
`task_snapshot` with `cutoff` and use lexical hits. A live supplied snapshot
still requires task/workspace authority; it cannot bypass a refused callback.

Workspace discovery is MCP-only: the adapter calls `orbit.workspace.list`
through a short-lived `orbit mcp serve` stdio session (never `--operator`),
under the same timeout and output bound as its `orbit tool run` calls.

Orbit
normalizes registered workspace names and IDs to absolute checkout paths before
calling a plugin. The adapter verifies that path matches `repository`, then
reads the task with the same workspace filter. Its public owner ID and name
select the discovery row; shared remotes never establish task ownership.

The repository's `origin` must also match that row's `git_remote`. This remote
match is an accident guard, since the checkout owner can rewrite it; Orbit's
per-call authorization is the security boundary. A missing owner, inactive
workspace, absent remote or conflicting repository is refused.

Only a host-reported `delivery_status: landed` whose commits Git verifies on the
landing branch is imported.

`committed` (no verified landing), `no_change`,
`in_progress`, `not_delivered`, `unavailable`, a task with no current run, an
answer for another workspace or repository, and landed evidence Git cannot
confirm are `excluded` with a reason naming the status or check. The answer's
`repository` must equal the identity recomputed from the routed `origin`
(`owner/name` on GitHub, else `git:` and the SHA-256 of the exact origin URL).

A squash landing is accepted only when `landed_commit` is on the branch, builds
on `base_sha` and makes exactly the tree changes of `base_sha..head_sha` (same
paths, object IDs and modes on both sides).

A foreign task/run pair, which Orbit refuses with `invalid_input`, is `failed`. So are an
answer naming another task or run, an unsupported `schema_version`, malformed
evidence and a missing local commit. `failed` is counted in `coverage.failed`
and `excluded` in `coverage.excluded`.

Orbit 0.25.1 can emit a diagnostic before its refusal JSON; Graph then reports `graph_error` with the refusal in the
message rather than structured `orbit_refused`. Read the per-item outcomes, not
just the outer successful batch response. `import` stores supplied envelopes as
caller-attested evidence; it does not establish host-verified delivery status.
