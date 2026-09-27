# Orbit plugin

orbit-graph ships an Orbit v2 plugin (`plugin.yaml`) that exposes read-only
code-graph queries, leakage-safe change recommendations, and index maintenance
as Orbit tools.

## Install

The v2 Orbit plugin installs from a tagged repository release. Its manifest
declares `publisher: constellation-works` and `origin: orbit`, which Orbit
honours only for a verified first-party source: installed with
`orbit plugin add git+https://github.com/constellation-works/orbit-graph#<tag>`,
the tools register as `orbit.graph.version`, `orbit.graph.status`,
`orbit.graph.recommend`, `orbit.graph.maintain`, and the query tools
`orbit.graph.search`, `show`, `refs`, `callees`, `impact`, `trace`, `deps`, and
`overview` (MCP `orbit_graph_*`) with the derived `orbit graph <verb>` command
group. Orbit refuses the `origin`
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

What the launcher may run is bound to the consent the operator gave: the
manifest's `spec.backend.args`, which the manifest digest Orbit records at
`orbit plugin add` covers, carries exactly one of

- `--backend-sha256 <hex>`: run only an executable with that SHA-256. The
  digest is checked before the executable runs at all, so an impostor on
  `PATH` that answers the version probe correctly is still refused with
  `incompatible_binary`, whose `detail` gives `path`, `expected_sha256` and
  `actual_sha256`;
- `--allow-unbound-backend`: the named operator override. The launcher runs a
  compatible executable whatever its digest, writes
  `orbit-graph launcher: backend override: running <path> unverified …` to
  stderr, and every response carries a top-level `backend_override` naming the
  executable (Orbit reads only `ok` and `output`, so the field is visible to a
  direct caller and in the backend's stderr log).

Anything else in `spec.backend.args` (neither, both, a malformed digest, an
unknown argument) runs nothing and returns `incompatible_binary`. A release
carries the override, because a release ships no executable to bind.

The preferred release path bundles the executable, so every Orbit service on
the host runs the binary built from the installed tag regardless of its `PATH`:

```sh
cargo install --git https://github.com/constellation-works/orbit-graph --tag <tag> --locked orbit-graph-cli
orbit plugin add git+https://github.com/constellation-works/orbit-graph#<tag> --enable --grant fs,orbit_tools
# Copy (never link) that executable into the installed tree as bin/orbit-graph.bin.
plugin_root=$(orbit plugin show graph | sed -n 's/^Install path: //p')
sh "$plugin_root/scripts/bundle-plugin-binary.sh" --unbound --binary "$HOME/.cargo/bin/orbit-graph"
orbit plugin test "$plugin_root"   # certifies the installed digest
orbit plugin show graph
```

`scripts/bundle-plugin-binary.sh` probes the candidate against the launcher's
pinned `extractor_version` and `plugin_schema_version` and refuses an
incompatible one. By default it also binds the tree: it records the copied
executable's SHA-256 as `--backend-sha256` in that tree's `plugin.yaml`. That
changes the manifest digest, so Orbit treats the tree as a new manifest until
the operator approves it again with `orbit plugin add <tree> --force`; the
bundler prints that step. A tree installed from `git+` cannot be re-added in
place (its `origin: orbit` claim is honoured only for the `git+` source), so
bundle a first-party install with `--unbound`, as above: the manifest keeps
`--allow-unbound-backend` and every call reports the override. Bind a tree you
install from a local directory, which drops the `origin` claim. `orbit plugin
add` and `orbit plugin upgrade` replace the whole installed tree, so repeat the
bundle step after each. Without a bundled executable the plugin falls back to
`PATH`, which must then resolve to the same tagged build in every service that
calls the plugin. The `fs` and `orbit_tools` grants are required for the
requested workspace/index access and bounded callbacks; the plugin requests no
network access.

For a checkout, `make plugin-check` builds the executable and runs
`orbit plugin validate --first-party` on both manifests and
`orbit plugin test --first-party .` with the fresh build first on `PATH`
(`--first-party` checks the checkout as it would load after a verified `git+`
install); `make plugin-bundle` bundles a release build as the git-ignored
`bin/orbit-graph.bin` and binds the checkout's `plugin.yaml` to it, a local
change not to commit.

The older `orbit tool add` installation path and
`scripts/install-orbit-plugin.sh` / `scripts/uninstall-orbit-plugin.sh` are
deprecated and remain available for one compatibility release. They register
only the three v1 sidecars. The installer uses the bundled executable when
present, then `--binary`, then `PATH`, verifies the v2 envelope, and applies
the launcher's binding rule from `plugin/plugin.yaml` before registering
anything: a recorded `--backend-sha256` must match (otherwise it exits 1 with
`incompatible_binary`), and the `--allow-unbound-backend` override is reported
on stderr as `backend override: registering <path> unverified …`. It refuses a
set `ORBIT_GRAPH_BIN` (exit 2) rather than silently honouring or ignoring it.
Pass `--binary /absolute/path/to/orbit-graph` for a development build and
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

The protocol lives in the executable's crate, `crates/orbit-graph-cli`
(`src/plugin.rs` and `src/plugin/`): tool names, envelopes, dispatch, error
codes, the Orbit subprocess adapter and the plugin-state code-graph index. The
`orbit-graph` library has no plugin API and reads neither `ORBIT_PLUGIN_STATE`
nor `GRAPH_ORBIT_TIMEOUT_SECONDS`.

## Configuration

The plugin reads one `[plugins.graph]` key, which Orbit passes to the backend
as the envelope's `context.config`:

| Key | Default | Meaning |
|---|---|---|
| `branch` | `main` | Landing branch whose first-parent history `status`, `recommend` and `maintain` (`history_sync`, `import`, `orbit_sync`) work on when the call names no `branch`, including the seeded `history-sync` routine. A call's own `branch` wins. |

For example, `branch = "agent-main"` makes the seeded routine and any
`maintain` call without `branch` sync `agent-main`; each such response reports
the `branch` it used. The former `index_dir` key is gone: no code path honoured
it (the history index lives under the repository's `.orbit-graph/`, the code
graph under plugin state). The config schema is closed, so Orbit refuses a
config that still sets `index_dir` and names the key; delete it. A key the
backend itself does not read is named on its stderr and otherwise ignored.

## Usage

Every plugin request takes an explicit absolute `repository` (defaulting to
the envelope's workspace root). `schema_version` is optional and must be 1
when present. Task-ID and hybrid queries also require the owning `workspace`;
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

Every `maintain` operation refuses, with `invalid_request` and before any index
is opened, each field it does not read, naming them all:

| `operation` | Reads |
|---|---|
| `history_sync` | `branch`, `limit` |
| `import` | `branch`, `delivery` |
| `orbit_sync` | `branch`, `limit`, `workspace`, `task_ids`, `run_ids`, `task_snapshots` |
| `graph_sync` | `full`, `budget_ms` |

`recommend` likewise refuses `hybrid_limit` without `hybrid: true`, and every
tool refuses an empty string as a field's value (or an item of a list field)
rather than reading it as absent; omit the field instead.
`result.failed` and `result.skipped` have the same shape as the CLI `sync`
output: a `count` and one entry per path the build could not read or extract,
or deliberately left out (see [usage](usage.md#index-lifecycle-and-location)).
They are `null` when the budget ran out before the build reported them.

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
directory or its lock file is refused. A refusal is a `graph_error` whose
message starts `refusing orbit-graph state path <path>:` and names the reason;
the plugin error codes are unchanged. Every other index file is opened without
following a symbolic link, and the pointer files are replaced atomically.

The response's `code_index` names the plugin state `directory` it wrote, and
`orbit.graph.status` reports the same object: `state` is `missing`,
`incompatible`, or `ready`, and `fresh` is true when a ready index was built at
the checkout's `HEAD`. A published generation uses a rollback journal rather
than WAL, so `status` and `recommend` open it read-only and create no files
next to it. Neither tool builds a history index either: without one they fail
with `index_missing`, naming `orbit.graph.maintain` (or `graph.maintain`, in
the caller's spelling) with `{"operation":"history_sync","branch":"<branch>"}`,
and create nothing. A recommendation applies structure only when the target
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

## Code-graph queries

Eight `read_only` tools answer the orbit-graph library's queries from the
published code-graph index: `search`, `show`, `refs`, `callees`, `impact`,
`trace`, `deps`, and `overview`. Their inputs mirror the CLI commands of the
same names (see [usage](usage.md)); `schemas/<verb>.request.json` describes
every field, enum, and default. The derived CLI takes the main argument
positionally:

```sh
orbit graph search parse
orbit graph refs 'symbol:src/parser.rs#helper:function'
orbit tool run orbit.graph.impact --input '{
  "schema_version":1,
  "repository":"/work/widgets",
  "selector":"symbol:src/parser.rs#helper:function",
  "direction":"inbound",
  "depth":2
}' --full
```

Each response has this shape:

| Field | Contents |
|---|---|
| `result` | The library result, unshaped, so fields the library adds reach callers unchanged. |
| `index` | `revision`, `checkout_revision`, `fresh`, `worktree_dirty`, `synced_at`, and `files`. |
| `index.stale` | Present only when `fresh` is false: the reason, and a structured `fix` (`{"tool":"orbit.graph.maintain","input":{"operation":"graph_sync"}}`). Stale results are still returned. |
| `truncated` / `truncation` | `limit` (1–500, default 50; search 20) caps every result array, nested ones included. While the result exceeds 256 KiB the cap is halved; a result that cannot fit even with every array emptied fails with `graph_error`. `truncation` reports `{returned, total}` per cut field path, with `[]` for each entry of an array (`files[].symbols`, `fallback.touched`); a nested total counts only the returned parents. Search fetches one match past `limit`, so its cut reports `total_at_least` instead of `total`. |

`show` returns at most `max_bytes` (default 16384, at most 65536) of source,
and `result` is null when the selector names nothing; `limit` does not apply to
it. `callees` hides unresolved calls with no indexed definition (such as
`map_err` or `Ok`) and counts them in `hidden_unresolved`, like the CLI;
`include_unresolved: true` lists them. `impact` and `trace` accept `depth`
1–10 and keep the library's 200-node cap; `trace` caps each node's children
at 50.

A query never builds or migrates an index, and it creates no files, so it works
against a read-only state directory. A query fails with a stable code instead
of returning an empty result:

| Code | Cause | Fix |
|---|---|---|
| `index_missing` | Nothing has been published | Run `graph_sync` |
| `index_incompatible` | Another extractor built the index | Run `graph_sync` with `"full": true` |

The error message names the exact call, in the caller's own tool spelling. The
query tools use the existing `fs` read grant and request no new permissions.
The deprecated `plugin/` compatibility tree advertises every tool of the root
manifest; its v1 sidecars register only `recommend`, `status` and `maintain`.

The `plugin/` tree is generated from the root `plugin.yaml` (schemas inlined,
no routines, jobs or config, so it seeds no schedule), and its skill mirrors
`skills/orbit-graph/` exactly. `plugin_contract` fails when either drifts; see
CONTRIBUTING.md for the regeneration command.

## Change analysis

`changes` is a `read_only` tool that answers two questions about a change in
one call: what else could it break, and which tests should run. It is the
plugin form of `orbit-graph changes` (see [usage](usage.md#change-analysis)).
Call it:

- **after implementing**, to see the callers and entry points your edit
  reaches before you call the work done;
- **before review**, to hand the reviewer the affected surface with evidence;
- **to pick tests**, from `result.tests` (each test lists the changed symbols
  it covers) rather than from file names.

```sh
orbit tool run orbit.graph.changes --input '{
  "schema_version":1,
  "repository":"/work/widgets"
}' --full

orbit tool run orbit.graph.changes --input '{
  "schema_version":1,
  "repository":"/work/widgets",
  "base":"main",
  "head":"HEAD",
  "max_tests":10
}' --full
```

Without `base` and `head` the tool compares the working tree (staged,
unstaged and untracked files) against the merge base of `HEAD` with the
branch it will land on: its upstream, else `origin/HEAD`, else `main`, else
`master`. Nothing is fetched. `base` alone compares the working tree against
that revision; `base` and `head` compare two revisions. `head` without `base`
is refused.

Unlike the query tools, `changes` does not read the published code-graph
index: it indexes both sides of the comparison itself. Committed snapshots
are cached under the plugin state directory (`<state>/<repository
hash>/changes-snapshots`), never in the repository, so a second call over
the same commits skips indexing; the working tree is never cached. Without
`ORBIT_PLUGIN_STATE` nothing is cached and a notice says so. Temporary trees
are built under `changes-scratch` beside the cache and removed when the call
ends. On an Orbit clone a cold call took 25–43 s and a warm call 19–40 s:
analysis, not indexing, dominates
([evaluation](evaluation/changes-command/README.md#latency)).

`result` is the same document the CLI prints with `--json`
(`schemas/changes.response.json`, `schema_version` 1):

| Field | Contents |
|---|---|
| `symbols[]` | Each analysed changed symbol: `status`, `pairing` and its evidence, then `callers`, `entry_points` and `candidate_tests`, each with a `*_found` count before its cap. |
| `callers[]`, `entry_points[]` | `source` (`call_path`, `import_relationship`, `reference_path`, or `changed_symbol` at distance 0), the weakest `category` and `confidence` on the path, and the path itself with a `file:line@sha` per hop. |
| `candidate_tests[]` | `source` (`call_path`, `import_relationship`, `naming_heuristic`, `runtime_invocation`), `confidence` (the path's weakest, or `file_import` / `name_only`), and the evidence path when there is one. A candidate is what the evidence points at, not proof of coverage. |
| `tests[]` | Distinct candidate tests across all symbols, strongest first, each with the changed symbols it covers. |
| `not_analysed`, `unmatched_selection`, `unresolved`, `out_of_scope`, `filtered_out` | Everything not answered, and why. |
| `complete` / `incomplete` | `false` with the `phase` and reason when `budget_ms` ran out. |
| `truncated` / `truncation` | Every bound that cut the answer, by name: `max_symbols`, `max_callers`, `max_entry_points`, `max_tests`, `depth`, `impact_node_cap`, `time_budget_ms`, `max_response_bytes`. |

The plugin defaults are smaller than the CLI's so a response stays readable:
25 symbols, 5 callers, 3 entry points and 5 tests per symbol, `same_module`
confidence floor, depth 3, 200 nodes per traversal. `budget_ms` (1000 to
110000, default 90000) bounds the whole call, indexing included, below the
120 s Orbit tool timeout: when it runs out the tool returns what it has with
`complete: false` rather than being killed. A `result` over 512 KiB is shrunk
in recorded steps (per-symbol lists, then symbols, then the standing lists),
each adding a `max_response_bytes` flag.

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
`schema_version`, out-of-range bound, missing required field, empty string, a
field the operation does not read, or a malformed `context.config`) or, for
`changes`, before anything is indexed (a `base` or `head` that does not
resolve, or no default base to compare the working tree against),
`repository_unavailable` when the routed `repository` is missing or not a Git
repository, `index_missing` or `index_incompatible` when a query tool has no
usable code-graph index, `index_missing` when `status` or `recommend` finds no
history index, `incompatible_binary` from the launcher when the executable is
stale or not the one `spec.backend.args` binds, and `graph_error` for every
other index, Git, or callback failure.


The bundled agent guidance is in
[`skills/orbit-graph/SKILL.md`](../skills/orbit-graph/SKILL.md).
