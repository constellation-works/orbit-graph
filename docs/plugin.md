# Orbit plugin

orbit-graph ships an Orbit v2 plugin (`plugin.yaml`) that exposes
leakage-safe change recommendations and history maintenance as Orbit tools.

## Install

The v2 Orbit plugin installs from a tagged repository release. The launcher
selects `bin/orbit-graph.bin` beside itself when a release bundles one, then
`ORBIT_GRAPH_BIN`, and finally the first `orbit-graph` on the caller's `PATH`.
It probes the selected executable with a v2 version envelope before forwarding
the request. A stale or incompatible binary returns an `incompatible_binary`
JSON error that names its path. Install the executable first:

```sh
cargo install --path crates/orbit-graph-cli --locked
orbit plugin add git+https://github.com/constellation-works/orbit-graph#<tag> --enable --grant fs,orbit_tools
orbit plugin show graph
```

For a service whose `PATH` puts an older `~/.orbit/bin/orbit-graph` before
`~/.cargo/bin`, set `ORBIT_GRAPH_BIN` to the absolute current executable path
in that service's environment (for example, `$HOME/.cargo/bin/orbit-graph`).
An interactive shell's environment does not configure `orbit web serve`.
Until a release bundles `bin/orbit-graph.bin`, a service with no override uses
its own `PATH`; an incompatible selection fails with a structured error.

Enabling the plugin provides `graph.version`, `graph.status`,
`graph.recommend`, and `graph.maintain`, plus the derived
`orbit graph` command group. The `fs` and `orbit_tools` grants are required for
the requested workspace/index access and bounded callbacks; the plugin requests
no network access.

The older `orbit tool add` installation path and
`scripts/install-orbit-plugin.sh` / `scripts/uninstall-orbit-plugin.sh` are
deprecated and remain available for one compatibility release. They register
only the three v1 sidecars. The installer uses the bundled executable when
present, then `--binary` (or `ORBIT_GRAPH_BIN` when `--binary` is absent), then
`PATH`, and verifies the v2 envelope before registration. Pass
`--binary /absolute/path/to/orbit-graph` for a development build and
`--orbit-root /absolute/path/to/.orbit` for a non-default Orbit authority.
Removing either the plugin or the legacy registrations deliberately retains
derived `.orbit-graph/` indexes.

## Usage

Every plugin request requires `schema_version: 1` and an explicit absolute
`repository`. Task-ID and hybrid queries also require the owning `workspace`;
the adapter never infers authority from cwd or `ORBIT_TOOL_WORKSPACE_ROOT`.

```sh
orbit tool run graph.recommend --input '{
  "schema_version":1,
  "repository":"/work/widgets",
  "workspace":"ws_widgets",
  "task_id":"TASK-123",
  "level":"symbol",
  "hybrid":true,
  "limit":10
}' --full

orbit tool run graph.recommend --input '{
  "schema_version":1,
  "repository":"/work/widgets",
  "query":"repair parser cache",
  "level":"file"
}' --full

orbit tool run graph.status --input '{
  "schema_version":1,
  "repository":"/work/widgets",
  "branch":"main"
}' --full
```

Live task-ID lookup uses the current public `orbit.task.show` response, honestly
labels started/completed text as post-execution, and may use that current
observation for a recommendation made afterward. An explicit `cutoff` switches
to strict historical replay: only text attested `known_pre_execution` and
strictly before that cutoff is eligible. A supplied snapshot on a live request
still verifies the task, workspace, and repository through the public API.
`hybrid: true`
uses public `orbit.search`; failure is surfaced and local lexical fallback is
named in `adapter.warnings`.
The calling activity must allow `orbit.workspace.list`, `orbit.task.show`,
`orbit.search`, and `orbit.workflow.run.show` for the callback operations it
uses; the adapter does not bypass Orbit policy. `orbit.workspace.list` is served
only over MCP, so the adapter reaches it through a short-lived, bounded
`orbit mcp serve` stdio session and binds the requested repository to the
workspace by matching the repository's `origin` to the published `git_remote`.
With narrower grants, pass an earlier public snapshot and use lexical/offline
hits.

Maintenance is deliberately separate from querying:

```sh
orbit tool run graph.maintain --input '{
  "schema_version":1,
  "operation":"orbit_sync",
  "repository":"/work/widgets",
  "workspace":"ws_widgets",
  "branch":"main",
  "run_ids":["jrun-20260907-0339-3"],
  "limit":25
}' --full
```


## Scheduled history synchronization

The plugin also ships a disabled `history-sync` routine. Enabling the plugin
seeds it as `.orbit/routines/graph-history-sync.yaml`; review that file and set
`enabled: true` to run the plugin's `history_sync` maintenance operation on its
daily schedule. The routine invokes only the bundled
`graph_history_sync_pipeline` job, so it never enables a schedule by default
or reaches another plugin's jobs.

`orbit_sync` reads only public `orbit.workspace.list`, `orbit.task.show`, and
`orbit.workflow.run.show` tool responses, then verifies full commit objects,
strict base ancestry, and landing-branch reachability in the explicitly routed
Git repository. `orbit.workflow.run.show` requires Orbit's `operator`
capability, which a plugin backend does not hold, so under the plugin each run
is currently reported `excluded` with Orbit's `capability_denied` reason rather
than imported. It reports partial
coverage: current Orbit has no cursor-paginated detailed delivery feed, so only
explicit run IDs and each requested task's current `job_run_id` are processed.
Retrying or submitting omitted IDs is safe because the immutable first-observed
envelope is preserved for a stable delivery ID; changed boundaries still fail.
Run completion time remains `uncertain` delivery-time evidence when the public
response does not attest the exact landing instant. `history_sync` is a bounded,
resumable newest-first Git-only bootstrap: partial responses expose a frozen
`snapshot_tip` and `resume_from`, keep the complete cursor unchanged, and reach
a no-op caught-up state after repeated calls. `import` accepts one public
DeliveryImport v2 envelope.


The bundled agent guidance is in
[`skills/recommendations/SKILL.md`](../skills/recommendations/SKILL.md).
