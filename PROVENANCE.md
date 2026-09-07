# Source provenance

This repository recovers the graph implementation from the public
[`constellation-works/orbit`](https://github.com/constellation-works/orbit)
history under the upstream MIT license.

## Primary snapshot

- Immutable commit: `c85e38b3e0820f1e468db70c3465ea6c2b14ace9`
- Source paths: `crates/orbit-graph/Cargo.toml` and
  `crates/orbit-graph/src/**`
- License path: `LICENSE.md`
- Recovery rule: the graph crate files and their focused unit tests were copied
  from that commit, which is the parent of graph deletion commit
  `199c0202bc5aebfcaacc42eb230133847c80d35f`.

At the primary snapshot, the former `orbit-graph-extract` and
`orbit-graph-cli` crates had already been consolidated into
`crates/orbit-graph/src/extract/**` and `crates/orbit-graph/src/cli/**`.

## Shared code and executable entry point

- The selector grammar originated at
  `c85e38b3e0820f1e468db70c3465ea6c2b14ace9:crates/orbit-common/src/utility/selector.rs`.
  This repository retains only the `Selector`, `SelectorParseError`, parsing,
  display, and path-normalization pieces used by graph callers.
- `src/main.rs` is adapted from the last standalone JSON entry point at
  `4f868826c47763a18946f1485aabedd3f53c8922:crates/orbit-graph-cli/src/main.rs`.
  The package import and executable name changed to the consolidated
  `orbit-graph` crate.
- SQLite setup was already locally implemented in
  `c85e38b3e0820f1e468db70c3465ea6c2b14ace9:crates/orbit-graph/src/store/mod.rs`.
  No `orbit-common` SQLite code was copied.

## Standalone adaptation boundary

The recovery intentionally changes only repository packaging and dependencies:

- workspace-inherited package metadata and dependency versions became explicit;
- the `orbit-common` dependency and graph-to-control-plane error adapter were
  removed;
- the shared selector implementation was reduced to the graph's required API;
- the executable is named `orbit-graph`, with real-binary smoke tests added;
- graph scratch databases moved from `.orbit/graph/` to `.orbit-graph/` so they
  do not share Orbit control-plane state;
- upstream comments that treated Orbit tasks or design records as authority
  were converted to local behavior descriptions where touched;
- standalone documentation, repository metadata, CI, and dependency update
  configuration were added.

The extraction, schema, synchronization, and query implementations and their
focused test suites otherwise remain the primary snapshot's code. No Orbit
runtime, service, repository-relative path dependency, or private crate is
required.
