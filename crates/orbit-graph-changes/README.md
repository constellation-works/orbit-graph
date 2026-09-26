# orbit-graph-changes

`orbit-graph-changes` is a library for explaining a Git change. It compares
immutable base and head snapshots, pairs changed symbols, traces bounded caller
and entry-point evidence, suggests candidate tests, and builds a deterministic
JSON report. A working-tree comparison mode includes uncommitted changes.

The library uses only the public `orbit_graph` API. It has no executable,
HTTP service, or UI. The follow-up task will expose it through
`orbit-graph changes` and the `orbit.graph.changes` plugin tool.

See [the design contract](../../docs/design/change-explorer.md) for snapshot
semantics, changed-symbol identity, evidence categories and JSON fields.
The [evaluation](../../docs/evaluation/change-explorer/README.md) records
observed strengths and gaps. Historical service and UI decisions in those
documents were retired on 2026-09-26 by ORB-13255.

Run the retained analysis tests with:

```sh
cargo test -p orbit-graph-changes --locked
```
