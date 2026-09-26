# Orbit plugin

orbit-graph ships an Orbit v2 plugin (`plugin.yaml`) that exposes
leakage-safe change recommendations and history maintenance as Orbit tools.

## Install

The v2 Orbit plugin installs from a tagged repository release. Its manifest
declares `publisher: constellation-works` and `origin: orbit`, which Orbit
honours only for a verified first-party source: installed with
`orbit plugin add git+https://github.com/constellation-works/orbit-graph#<tag>`,
the tools register as `orbit.graph.version`, `orbit.graph.status`,
`orbit.graph.recommend`, and `orbit.graph.maintain` (MCP `orbit_graph_*`) with
the derived `orbit graph <verb>` command group. Orbit refuses the `origin`
claim from any other source (a local directory, an archive, a fork); such a
copy must drop `origin: orbit` and then registers bare `graph.*` names. The
executable accepts both spellings.

The launcher `bin/orbit-graph` selects the executable in this order and probes
it with a v2 version envelope before forwarding the request:

1. `bin/orbit-graph.bin` beside the launcher, when present;
2. the first `orbit-graph` on the backend's `PATH`.

A stale or incompatible executable returns an `incompatible_binary` JSON error
that names its path. Orbit runs plugin backends with a cleared environment and
this plugin requests no `env_pass`, so no environment variable can redirect the
launcher; a service's `PATH` is whatever that service was started with (for
example, `orbit web serve` under systemd often lacks `~/.cargo/bin`).

The preferred release path bundles the executable, so every Orbit service on
the host runs the binary built from the installed tag regardless of its `PATH`:

```sh
cargo install --git https://github.com/constellation-works/orbit-graph --tag <tag> --locked orbit-graph-cli
orbit plugin add git+https://github.com/constellation-works/orbit-graph#<tag> --enable --grant fs,orbit_tools
# Copy (never link) that executable into the installed tree as bin/orbit-graph.bin.
plugin_root=$(orbit plugin show graph | sed -n 's/^Install path: //p')
sh "$plugin_root/scripts/bundle-plugin-binary.sh" --binary "$HOME/.cargo/bin/orbit-graph"
orbit plugin test "$plugin_root"   # certifies the installed digest
orbit plugin show graph
```

`scripts/bundle-plugin-binary.sh` probes the candidate against the launcher's
pinned `extractor_version` and `plugin_schema_version` and refuses an
incompatible one. `orbit plugin add` and `orbit plugin upgrade` replace the
whole installed tree, so repeat the bundle step after each. Without a bundled
executable the plugin falls back to `PATH`, which must then resolve to the same
tagged build in every service that calls the plugin. The `fs` and
`orbit_tools` grants are required for the requested workspace/index access and
bounded callbacks; the plugin requests no network access.

For a checkout, `make plugin-check` builds the executable and runs
`orbit plugin validate --first-party .` and `orbit plugin test --first-party .`
with the fresh build first on `PATH` (`--first-party` checks the checkout as it
would load after a verified `git+` install); `make plugin-bundle` bundles a
release build as the git-ignored `bin/orbit-graph.bin` instead.

The older `orbit tool add` installation path and
`scripts/install-orbit-plugin.sh` / `scripts/uninstall-orbit-plugin.sh` are
deprecated and remain available for one compatibility release. They register
only the three v1 sidecars. The installer uses the bundled executable when
present, then `--binary` (or `ORBIT_GRAPH_BIN` in the installing shell when
`--binary` is absent), then `PATH`, and verifies the v2 envelope before
registration. Pass `--binary /absolute/path/to/orbit-graph` for a development
build and
`--orbit-root /absolute/path/to/.orbit` for a non-default Orbit authority.
Removing either the plugin or the legacy registrations deliberately retains
derived `.orbit-graph/` indexes.

### Plugin versions

`metadata.version` always equals the crate version that `orbit.graph.version`
reports. Bump both (the workspace `version` in `Cargo.toml` and both manifests)
whenever the launcher's pinned `extractor_version` or `plugin_schema_version`
changes after a release; a host that installed the previous version keeps its
own launcher pin, so reusing a version would mix incompatible trees. Released
contracts are recorded in `tests/plugin-releases.json` (append an entry when a
version is tagged or installed), and
`crates/orbit-graph-cli/tests/plugin_contract.rs` fails in CI when the launcher,
installer, goldens, or manifests disagree with the crate, or when a released
version's contract changes.

## Usage

Every plugin request requires `schema_version: 1` and an explicit absolute
`repository`. Task-ID and hybrid queries also require the owning `workspace`;
the adapter never infers authority from cwd or `ORBIT_TOOL_WORKSPACE_ROOT`.

```sh
orbit tool run orbit.graph.recommend --input '{
  "schema_version":1,
  "repository":"/work/widgets",
  "workspace":"ws_widgets",
  "task_id":"TASK-123",
  "level":"symbol",
  "hybrid":true,
  "limit":10
}' --full

orbit tool run orbit.graph.recommend --input '{
  "schema_version":1,
  "repository":"/work/widgets",
  "query":"repair parser cache",
  "level":"file"
}' --full

orbit tool run orbit.graph.status --input '{
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
Over Git-only history, a live free-text `query` request may also rank from
commit messages: those contributions use the reason kind
`historical_change_commit_text`, are labelled post-execution, are down-weighted
against task text, and add the `git_commit_text_used` fallback; strict replay
and task-ID requests never use them.
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
orbit tool run orbit.graph.maintain --input '{
  "schema_version":1,
  "operation":"orbit_sync",
  "repository":"/work/widgets",
  "workspace":"ws_widgets",
  "branch":"main",
  "run_ids":["jrun-20260907-0339-3"],
  "limit":25
}' --full
```


## Code-graph index

Combined and graph-only recommendations add structural evidence (callers,
callees, and references of the matched symbols) only from the plugin's own
code-graph index, which lives in the plugin state directory, never in the
repository's `.orbit-graph/`. Only the mutating `graph_sync` maintenance
operation builds it:

```sh
orbit tool run orbit.graph.maintain --input '{
  "schema_version":1,
  "operation":"graph_sync",
  "repository":"/work/widgets"
}' --full
```

`graph_sync` is incremental by default: it copies the published index and
re-extracts only changed files. `"full": true` re-extracts every file, which an
index from another extractor version requires. `budget_ms` (1000 to 110000,
default 90000) bounds the whole call below the plugin's 120 s backend timeout.
Reference extraction stops starting new files at 60% of the budget so that
resolution can finish in the rest. Resolution itself is not interrupted, so if
it runs past the budget the call still answers at the budget with
`budget_exhausted` and the unfinished build is discarded. `graph_sync` indexes the checkout as it is
and rejects the history fields (`branch`, `limit`, `delivery`, `workspace`,
`task_ids`, `run_ids`, `task_snapshots`) rather than ignoring them.

Readers only ever see a complete index. Each build writes a new generation
database that nothing references yet, and a finished build publishes it by
atomically replacing the pointer file `graph.current.json`. If the budget runs
out, the response reports `coverage.complete: false` and
`coverage.state: "budget_exhausted"` with the `phase` it reached and a
`resume` hint, and the build is discarded. The previously published index is
unchanged, and so are recommendations. A killed process, a failed build, or a
second concurrent `graph_sync` (refused with `graph_error` while the first
holds the build lock, and named in the refusal) also leaves the published
index as it was. Each build records the generation it creates in
`graph.owned.json` before writing it, and the next build deletes only recorded
generations that are no longer published. It never deletes a graph database it
did not record, such as the empty `graph.<extractor>.db` older plugin versions
left in plugin state. It reports those files in `result.unowned_files` instead.
The index directory is created owner-only (`0700`) and every file in it
`0600`, whatever the process umask, and a symbolic link in place of the
directory or its lock file is refused.

The response's `code_index` names the plugin state `directory` it wrote, and
`orbit.graph.status` reports the same object: `state` is `missing`,
`incompatible`, or `ready`, and `fresh` is true when a ready index was built at
the checkout's `HEAD`. A published generation uses a rollback journal rather
than WAL, so `status` and `recommend` open it read-only and create no files
next to it. A recommendation applies structure only when the target
revision is the checkout `HEAD` and the published index was built at it; the
index also covers uncommitted changes present when it was built
(`published.worktree_dirty`). Otherwise the response keeps lexical and history
evidence, sets `structure_applied: false`, and names the fix in `fallbacks`:

| `fallbacks[].kind` | Meaning | Fix |
|---|---|---|
| `structure_index_missing` | Nothing published yet | Run `graph_sync` |
| `structure_index_stale` | Built at another commit | Run `graph_sync` |
| `structure_index_incompatible` | Built by another extractor or schema version | Run `graph_sync` with `"full": true` |
| `structure_unavailable_for_revision` | Target is not the checkout `HEAD` | Recommend for `HEAD`, or accept no structure |

## Scheduled history synchronization

The plugin also ships a disabled `history-sync` routine. Enabling the plugin
seeds it as `.orbit/routines/graph-history-sync.yaml`; review that file and set
`enabled: true` to run the plugin's `history_sync` maintenance operation on its
daily schedule. The routine invokes only the bundled
`graph_history_sync_pipeline` job, so it never enables a schedule by default
or reaches another plugin's jobs.

A second disabled routine, `code-sync`, seeds as
`.orbit/routines/graph-code-sync.yaml` and runs the bundled
`graph_code_sync_pipeline` job at 03:30, after history synchronization. Its
activity calls `graph_sync` with the default budget. A run that reports
`budget_exhausted` has changed nothing, so enable it only for a repository
whose incremental build fits the budget (see the timing note in the
`graph_sync` response's `result.timings`).

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

Failed calls return a stable `error.code`: `invalid_request` when the request
is refused before any repository is read (unknown tool or field, unsupported
`schema_version`, out-of-range bound, missing required field),
`repository_unavailable` when the routed `repository` is missing or not a Git
repository, and `graph_error` for every other index, Git, or callback failure.


The bundled agent guidance is in
[`skills/recommendations/SKILL.md`](../skills/recommendations/SKILL.md).
