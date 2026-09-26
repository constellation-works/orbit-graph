# orbit-graph

`orbit-graph` builds a local SQLite index of a source tree and answers graph
queries — search, references, callees, impact, command traces — from a Rust
library and a CLI. It is standalone: no Orbit checkout, runtime, or
configuration is required.

## Install

Requires Rust 1.89+ and Git.

```sh
cargo install --path crates/orbit-graph-cli --locked
```

As an Orbit plugin, the same queries (search, show, refs, callees, impact,
trace, deps, overview) are read-only `orbit.graph.*` tools that answer from an
index the plugin's `graph_sync` maintenance builds; see
[docs/plugin.md](docs/plugin.md).

## Quick start

```sh
cd /path/to/repo
orbit-graph sync
orbit-graph search helper --kind symbol
orbit-graph refs 'symbol:src/lib.rs#helper:function'
orbit-graph callees 'symbol:src/lib.rs#entry:function'
```

Pass `--format json` for machine-readable output. `orbit-graph --help` lists
every command.

## Index lifecycle and location

Queries never refresh the index; run `orbit-graph sync` after source changes
(`--full` to rebuild). The index lives in `.orbit-graph/` at the worktree root
and should not be committed; `orbit-graph db-path` prints its location and
`orbit-graph clean` reports obsolete databases (`clean --confirm` removes
them).

## Documentation

- [Usage](docs/usage.md) — commands, selectors, confidence levels, languages, library API
- [Orbit plugin](docs/plugin.md) — install, query and recommendation tools, index maintenance
- [Change analysis library](crates/orbit-graph-changes/README.md) — snapshots, changed symbols, evidence and JSON reports
- Design: [terminal interface](docs/design/terminal-interface.md),
  [change recommendations](docs/design/change-recommendations.md),
  [change explorer](docs/design/change-explorer.md)
- [Evaluation](docs/evaluation/README.md) ·
  [Contributing](CONTRIBUTING.md) · [Provenance](PROVENANCE.md)

## License

MIT. See [LICENSE.md](LICENSE.md).
