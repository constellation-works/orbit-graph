# Architecture

The layer model that `scripts/check-dependency-direction.sh` enforces
(STD-02 §R1, §R6). A lower layer never imports a higher one. Adding a crate or
an internal edge changes this file and that script in the same commit; the
script fails when the crate table below and its policy disagree.

## Layers

| Tier | Unit | Owns | May depend on |
|------|------|------|---------------|
| 1. Extraction | `orbit-graph-extract` (library) | Extraction contracts (`ExtractedFile`, the raw rows, `Selector`), the `Extractor` trait and every tree-sitter language extractor (`languages`), and the Git-tree change extraction behind the history index (`history`). A leaf: it knows nothing of the store, sync or queries, and prints nothing. | nothing internal |
| 2. Domain | `orbit-graph` (library) | The SQLite store and its schema (`store`), sync and the file watcher (`sync`), queries (`query`), recommendations (`recommend`), and evaluation (`evaluation`), built on tier 1; it re-exports the extraction types its public API names. Embeddable; it prints nothing (`#![deny(clippy::print_stdout, clippy::print_stderr)]`), reads no environment variable (the CLI installs its process settings once with `install_runtime`) and starts no Orbit processes. Its public `GraphError` is `#[non_exhaustive]`; `GraphError::class()` is the exhaustive class the CLI's one translator maps to CLI and plugin codes. | tier 1 |
| 3. Change analysis | `orbit-graph-changes` (library) | Immutable base/head snapshots (a head may be the working tree), snapshot caching, changed-symbol pairing, evidence paths, filters, JSON reports and the bounded agent-facing analysis (`analysis`), through the public `orbit-graph` API only (D1). No HTTP service or terminal output. | tier 2 |
| 4. Surfaces | `orbit-graph-cli` (binary `orbit-graph`) | Argument parsing, one library call per command, rendering and process exit code; also the Orbit plugin tool protocol. The CLI's `src/output/` is its output layer, with temporary exceptions listed by `scripts/check-terminal-guard.sh`. | tiers 2 and 3 |

The CLI depends on the change-analysis library for one command,
`orbit-graph changes`, and its plugin tool `orbit.graph.changes`; the library
never depends on the CLI. Neither depends on `orbit-graph-extract` directly:
the library reaches extraction types through `orbit-graph`'s re-exports.
`orbit-graph-extract` is its own crate for compile-graph isolation and an
enforceable edge (STD-02 §R7): it owns every tree-sitter grammar, so store,
sync or query changes do not recompile extraction.

## Crates

| Crate | Kind | Internal dependencies | Banned |
|-------|------|-----------------------|--------|
| `orbit-graph-extract` | library | — | `clap`, `tracing-subscriber`, `tiny_http`, `unicode-width` |
| `orbit-graph` | library | `orbit-graph-extract` | `clap`, `tracing-subscriber`, `tiny_http`, `unicode-width` |
| `orbit-graph-cli` | binary `orbit-graph` | `orbit-graph`, `orbit-graph-changes` | — |
| `orbit-graph-changes` | library | `orbit-graph` | `clap`, `tracing-subscriber`, `tiny_http`, `unicode-width` |

"Banned" lists external crates the libraries must not depend on: argument
parsing, log subscribers, HTTP serving and terminal layout do not belong to analysis libraries
(STD-02 §R7). Dev-dependencies are exempt. A third-party dependency that more
than one member uses is declared once in `[workspace.dependencies]`
(STD-02 §R9); the script checks that too.

## `orbit-graph-extract`

| Source unit | Responsibility and allowed imports |
| --- | --- |
| `lib.rs` | Defines extraction contracts and exports the language extractors, selectors and Git history types without dependencies on other workspace crates. |
| `error.rs` | Represents failures during Git history extraction without importing sibling units. |
| `extracted.rs` | Defines raw file, symbol and edge records without importing sibling units. |
| `history.rs` | Extracts Git history, file changes and delivery evidence using the crate's error type. |
| `languages/` | Parses source files into graph rows using the extracted records and the crate's extractor contracts. |
| `selector.rs` | Parses graph selectors and resolves their filesystem anchors without importing sibling units. |

## `orbit-graph`

| Source unit | Responsibility and allowed imports |
| --- | --- |
| `lib.rs` | Exposes graph indexing, SQLite storage, query and recommendation operations using the extraction crate. |
| `clean.rs` | Plans and removes obsolete database families using the store, lock, state directory, graph errors and extractor version. |
| `db_path/` | Resolves worktree database identities and legacy paths using the store and graph errors. |
| `error.rs` | Defines graph failures and their public classes using extraction errors. |
| `evaluation.rs` | Evaluates recommendation variants against dated delivery corpora using extraction, graph, store, sync, recommendation, runtime and error contracts. |
| `graph/` | Manages graph handles and their sync policy using database paths, errors, queries, storage, synchronization and extraction selectors. |
| `live_evaluation.rs` | Evaluates held-out Git commits using extraction history, the history store, recommendations and graph errors. |
| `lock.rs` | Acquires bounded database and directory locks using runtime settings, state directory access, extraction timestamps and graph errors. |
| `query/` | Reads indexed symbols and relationships using the graph handle, sync and extraction contracts, and graph errors. |
| `recommend.rs` | Ranks change destinations using graph queries, history storage, extraction, redaction, state directory writes and graph errors. |
| `redaction.rs` | Redacts sensitive values before they reach graph records or reports without importing sibling units. |
| `runtime.rs` | Stores process settings supplied by the caller without importing sibling units. |
| `state_dir.rs` | Creates private state directories and writes durable files using graph errors. |
| `store/` | Opens SQLite graph and history storage using database paths, errors, locks, redaction, state directory access and extraction contracts. |
| `sync/` | Scans, indexes and resolves changed files using graph connections, storage, extraction, locking, redaction, runtime settings and graph errors. |

## `orbit-graph-changes`

| Source unit | Responsibility and allowed imports |
| --- | --- |
| `lib.rs` | Exposes bounded change analysis and its report types using the public graph library. |
| `analysis.rs` | Builds the agent-facing change analysis using changes, evidence, filters, reports, selection, snapshots and the graph library. |
| `cache.rs` | Caches immutable snapshot trees and indexes using the graph library's version and state directory APIs. |
| `changes.rs` | Pairs changed symbols across snapshots using filters and the graph library. |
| `evidence.rs` | Finds evidence paths and candidate tests for changed symbols using changes, filters, snapshots and graph queries. |
| `filters.rs` | Defines report filters and accounts for omitted results without importing sibling units. |
| `report.rs` | Assembles bounded change reports using changes, evidence, filters, selection, snapshots and graph queries. |
| `selection.rs` | Resolves requested symbol aliases using snapshots, evidence and the graph library's selectors. |
| `snapshot.rs` | Materializes and indexes revision and working-tree snapshots using the cache and graph library. |
| `test_fixtures/` | Builds deterministic Git repositories for change-analysis tests without importing sibling units. |

## `orbit-graph-cli`

| Source unit | Responsibility and allowed imports |
| --- | --- |
| `main.rs` | Configures and dispatches the command-line and plugin surfaces using the graph and change-analysis libraries. |
| `command/` | Parses and dispatches graph commands using CLI errors, output views and the graph and change-analysis libraries. |
| `environment.rs` | Declares process environment keys and their documentation without importing sibling units. |
| `error.rs` | Maps graph and change-analysis failures to CLI and plugin errors without importing CLI siblings. |
| `output/` | Renders human and machine output and configures streams and logging using CLI environment keys and errors. |
| `plugin.rs` | Implements the Orbit external-tool protocol using CLI environment keys, errors and the graph and change-analysis libraries. |

## Repository gates

Each runs in `make ci` and in CI (`.github/workflows/ci.yml`):

| Gate | Rule | Fails on |
|------|------|----------|
| `scripts/check-dependency-direction.sh` | STD-02 §R1–§R7, §R9 | an unlisted crate, an unlisted internal edge, a banned crate in its crate, a crate table here that disagrees with the script, a third-party dependency used by two or more members that any of them declares locally; and, failing closed, any dependency form it does not read (a `[dependencies.<name>]` or otherwise unrecognized dependency header, a dotted or quoted key, a `package =` rename; whitespace inside brackets and around dots is normalized first) |
| `scripts/check-unit-map.sh` | STD-02 §R43 | a workspace crate or top-level module missing from its section, a documented path absent from disk, or a crate root or module missing its opening doc comment |
| `scripts/check-terminal-guard.sh` | STD-02 §R15 | stdout/stderr, `print!`-family macros or TTY checks outside an output layer, except allow-listed write sites (each a file, a line pattern and an exact hit count, with a reason and a removing task); an entry whose count no longer matches, including a site that is gone; grep itself failing |
| `scripts/check-orphan-modules.sh` | STD-02 §R19 | a `src/**/tests/*.rs` file its `tests/mod.rs` does not declare, or a `tests/` directory its parent module does not declare |
| `scripts/check-env-vars.sh` | STD-01 §R25 | a production env read naming a string literal or undocumented constant, a missing env doc comment (`Default:` / `Overridden flag:`), or `docs/usage.md` disagreeing with `crates/orbit-graph-cli/src/environment.rs` |
| `cargo deny --locked check` (`deny.toml`) | STD-02 §R23, STD-05 §R23/§R24 | an open advisory, a yanked crate, a license outside the allow-list, a registry other than crates.io, a git source |
| `scripts/test-repo-gates.sh` | STD-04 §R10 | a repository gate passing on a seeded violation |
| `crates/orbit-graph-changes/src/report/tests/derived_artifacts.rs` | STD-04 §R11, §R13 | the committed `direct-call` JSON sample differing from a fresh library-built report (regenerate with `UPDATE_GOLDENS=1`) |
| `docs/usage.md` doctest (`cargo test --doc`) | STD-04 §R14 | the library example in `docs/usage.md` no longer compiling |
