# Setting up orbit-graph

Read this when the `orbit.graph.*` tools are missing or return
`incompatible_binary`, or when every query fails with `index_missing`. Also read
it when preparing a repository for code navigation and recommendations.
[docs/plugin.md](../../../docs/plugin.md) is the full reference.

## 1. Check what is installed

```sh
orbit plugin show graph
orbit tool run orbit.graph.version --input '{}' --full
```

A verified first-party install registers `orbit.graph.*` (MCP `orbit_graph_*`).
A copy installed from any other source registers bare `graph.*`. Use whichever
spelling your tool list shows. `version` reports the crate version, the
`extractor_version` and the `plugin_schema_version`. An `incompatible_binary`
error names the executable the launcher found, and that executable was built
from a different tag.

## 2. Install the plugin

Install from a tagged release, and bundle the executable built from the same
tag. Every Orbit service then runs that binary, whatever its `PATH` is:

```sh
cargo install --git https://github.com/constellation-works/orbit-graph --tag <tag> --locked orbit-graph-cli
orbit plugin add git+https://github.com/constellation-works/orbit-graph#<tag> --enable --grant fs,orbit_tools
plugin_root=$(orbit plugin show graph | sed -n 's/^Install path: //p')
sh "$plugin_root/scripts/bundle-plugin-binary.sh" --binary "$HOME/.cargo/bin/orbit-graph"
orbit plugin test "$plugin_root"
```

- The `fs` and `orbit_tools` grants are required. The plugin requests no
  network access.
- `orbit plugin add` and `orbit plugin upgrade` replace the whole installed
  tree, so repeat the bundle step after either.
- Without a bundled binary, the launcher falls back to the first `orbit-graph`
  on the calling service's `PATH`. Services under systemd often lack
  `~/.cargo/bin`.

Installing, upgrading and bundling change the host. Only do them when the user
has asked for it.

## 3. Build the indexes

Every request needs `schema_version: 1` and an absolute `repository`. The
plugin keeps two indexes, and each has its own build step.

**Code-graph index**, for the query tools and structural evidence:

```sh
orbit tool run orbit.graph.maintain --input '{"schema_version":1,"operation":"graph_sync","repository":"/abs/repo"}' --full
```

This is incremental by default. Add `"full": true` after `index_incompatible`,
or after an extractor upgrade. If the response reports
`coverage.state: "budget_exhausted"`, nothing was published: retry with a
larger `budget_ms`, up to 110000.

**History index**, for recommendations:

```sh
orbit tool run orbit.graph.maintain --input '{"schema_version":1,"operation":"history_sync","repository":"/abs/repo","branch":"main"}' --full
```

`history_sync` is bounded and newest-first. Repeat the same call until the
response reports `complete: true`. Later calls are then no-ops until new
commits land. `status` and `recommend` fail with `index_missing` until the first
`history_sync` has run.

## 4. Verify

```sh
orbit tool run orbit.graph.status --input '{"schema_version":1,"repository":"/abs/repo","branch":"main"}' --full
```

The indexes are ready when `code_index.state` is `ready`, `code_index.fresh` is
`true` and `status.complete` is `true`.

## 5. Keep the indexes fresh

Queries never refresh an index. Enabling the plugin seeds two disabled
routines into the workspace:

| Routine file | Runs | Schedule |
|---|---|---|
| `.orbit/routines/graph-history-sync.yaml` | `history_sync` | 03:00 daily |
| `.orbit/routines/graph-code-sync.yaml` | `graph_sync` | 03:30 daily |

To turn one on, set `enabled: true` in its file. Only enable the code-sync
routine for a repository whose incremental build fits the default budget. Check
`result.timings` in a `graph_sync` response to see how long a build takes.

## Standalone CLI

The CLI needs no Orbit install. Its index lives in `.orbit-graph/` at the
worktree root, which is separate from the plugin's index in plugin state.

```sh
cargo install --path crates/orbit-graph-cli --locked
orbit-graph sync
orbit-graph --help
```

## Failure codes

| Symptom | Fix |
|---|---|
| Tool not listed | Install and enable the plugin (step 2) |
| `incompatible_binary` | Bundle the binary built from the installed tag (step 2) |
| `index_missing` from a query tool | Run `graph_sync` |
| `index_missing` from `status` or `recommend` | Run `history_sync` |
| `index_incompatible` | Run `graph_sync` with `"full": true` |
| `structure_index_stale` or `structure_index_missing` in `fallbacks` | Run `graph_sync` |
| `graph_error` naming a lock holder | Another `graph_sync` is running. Wait, then retry |
