# Using orbit-graph

Reference for the `orbit-graph` command-line interface and library. For the
Orbit plugin, see [plugin.md](plugin.md).

## Quick start

Commands operate on the Git worktree containing the current directory. If the
current directory is not in a Git worktree, it is treated as the root.

```sh
fixture="$(mktemp -d)"
git -C "$fixture" init -b main
mkdir -p "$fixture/src"
printf 'pub fn helper() -> i32 { 1 }\npub fn entry() -> i32 { helper() }\n' > "$fixture/src/lib.rs"

cd "$fixture"
orbit-graph --format json sync --full
orbit-graph --format json search helper --kind symbol --limit 5
orbit-graph --format json show 'symbol:src/lib.rs#entry:function' --max-bytes 1024
orbit-graph --format json refs 'symbol:src/lib.rs#helper:function' --confidence fuzzy --kind call
orbit-graph --format json callees 'symbol:src/lib.rs#entry:function'
```

The default is deliberately human-oriented: a terminal receives a headed table
and a redirected command receives lossless, tab-separated plain rows. Scripts
must select the stable machine contract explicitly with `--format json`; use
`--format ndjson` for one complete JSON record per line. `--help` remains
conventional text help. Failures return a nonzero status, keep stdout empty,
and write either a readable diagnostic or, in explicit JSON/NDJSON mode, a JSON
object with `error.code` and `error.message` to stderr. Set `RUST_LOG` to enable
diagnostic tracing on stderr.

The grouped help layout, stream contracts, styling rules, and compatibility
boundaries are documented in [the terminal-interface design](design/terminal-interface.md).

## Index lifecycle and location

Synchronization is explicit. Query commands never refresh the index; run
`orbit-graph sync` after source changes. Incremental sync compares metadata and
content hashes. `orbit-graph sync --full` rehashes and re-extracts every
supported file.

Each worktree stores scratch state under `.orbit-graph/` in its root. Attached
branches use `.orbit-graph/<sanitized-branch>.<extractor-version>.db`; detached
worktrees use a commit-prefixed database name. SQLite WAL and lock sidecars
live beside the database. This directory is independent of Orbit's `.orbit/`
control-plane state and should not be committed. `orbit-graph db-path` prints
the exact path, and `orbit-graph clean` removes obsolete extractor versions and
unreachable detached-commit indexes.

The scanner respects Git ignore rules and optional `.orbitignore` files. The
latter is a source-scanning ignore format retained for compatibility; it is not
an Orbit runtime configuration dependency.

Delivered-change history is a separate rebuildable index at
`.orbit-graph/change-history.<schema-version>.sqlite3`. It learns only from
immutable Git tree differences supplied by the public delivery contract or from
explicitly weaker first-parent Git sync evidence. It never reads task
`context_files` or Orbit private state. Delivery, ingestion, task creation, and
snapshot availability times remain distinct and carry explicit
certainty/provenance. See [the design and v2 contract](design/change-recommendations.md).

## Commands

| Command | Purpose |
| --- | --- |
| `sync [--full]` | Incrementally update or fully rebuild the index. |
| `history import --input <path\|->` | Import a validated v2 delivery JSON envelope. |
| `history sync --branch <name> [--limit <n>]` | Atomically index new first-parent commits as Git-only evidence. |
| `history status --branch <name>` | Report history versions, cursor, evidence, and association counts. |
| `history rebuild --branch <name> [--limit <n>]` | Atomically recreate one scope from Git-only history. |
| `recommend --query <text>\|--task-id <id> [--level file\|symbol]` | Rank current destinations with evidence and freshness. |
| `evaluate --input <corpus.json>` | Compare four ranking variants chronologically. |
| `search <query> [--kind symbol|string|config] [--lang <id>] [--limit <n>]` | Full-text search indexed definitions, strings, or config keys. |
| `show <selector> [--max-bytes <n>]` | Return metadata and a bounded source slice. |
| `refs <symbol> [--confidence <level>] [--kind <kind>]` | Return inbound references and relations. |
| `callees <symbol>` | Return calls made by a symbol. |
| `impact <selector> [--depth <n>] [--confidence <level>] [--direction inbound\|outbound\|both]` | Traverse callers, callees, or both around a selector (default: both). |
| `trace <command> [--depth <n>] [--confidence <level>]` | Trace a discovered CLI command handler and its calls. |
| `overview [<file-or-dir-selector>] [--format summary|full]` | Summarize indexed files and symbols. |
| `implementors <trait-selector>` | Find concrete implementations of a trait-like symbol. |
| `deps <file-or-dir-selector>` | List source-level module/import edges. |
| `db-path` | Show the current database path and extractor version. |
| `clean` | Delete obsolete graph databases. |
| `version` | Show crate, extractor, and store schema versions. |

Confidence levels are `exact`, `import`, `same_module`, and `fuzzy`. Reference
kinds are `call`, `type`, `use`, `trait_bound`, `impl`, `extends`, and
`implements`.

Qualified cross-file calls such as `a::run()` and `pkg.mod.run()` report
`exact` when the qualifier and indexed file-module path identify one symbol.
Explicit imports report `import` (`import_resolved` in JSON and storage) when
they identify one symbol; ambiguous qualifiers or imports remain `fuzzy`.

A method call whose receiver type the extractor cannot determine
(`args.execute()`, `rows.append(1)`) is never matched on its bare method name:
it resolves only through an import or a qualified path, and otherwise reports
`fuzzy`. A dispatcher's own same-named method is therefore not a match for the
calls it dispatches. Receivers that name the enclosing definition (`self`,
`Self`, `cls`) keep resolving within the defining file.

Selectors use one of these forms:

```text
dir:<path>
file:<path>
symbol:<path>#<name>:<kind>
module:<qualified-name>
command:<name>
```

## Chronological evaluation

`orbit-graph evaluate` consumes a versioned public corpus and runs combined,
task-search-only, graph-only, and frequency ranking at file and symbol levels.
Each case runs in a disposable clone with an isolated history index and a graph
materialized from the exact target tree; operational indexes and the caller
checkout are never read or mutated. Held-out delivery diffs are previewed as
truth but never imported. Task text and training deliveries must be strictly
before the cutoff. Held-out evidence must be verified and proven after the
cutoff by an exact time or an explicit trustworthy prospective lower bound.
Reports include
recall@K, precision@K, stale-result rate, mean/maximum latency, coverage,
per-level omitted-truth reasons, exclusions, source provenance, input digest,
and exact graph revisions. A metric with no data behind it (no relevant truth,
no evaluated case, no returned result or no query) is `null` in the JSON
report (schema version 2) and `n/a` in the table, never `0`. The corpus input
keeps its own schema version, 1.

The synthetic adversarial executable fixture runs in CI. The bounded real Orbit
prospective input, measured result, and its no-superiority limitation are
documented in [`docs/evaluation/`](evaluation/README.md).

## Language coverage

Tree-sitter extractors cover Rust, C, C#, Go, Java, JavaScript/JSX, TypeScript/
TSX, Kotlin, Python, and Ruby. Markdown headings and fenced code, plus JSON,
YAML, TOML, and dotenv-style configuration keys, are also indexed.

This is a static, syntax-driven graph rather than a compiler or language
server. Dynamic dispatch, generated code, macro expansion, runtime imports,
and ambiguous same-name symbols can produce missing or lower-confidence edges.
Cross-file and cross-language resolution is deliberately conservative. Binary,
archive, font, PDF, and lock files are skipped.

## Library

The crate exposes `Graph`, `Selector`, synchronization policies, and typed query
results. It also exposes `HistoryIndex`, the v2 import/provenance and extracted
change types, including explicit temporal certainty and pre-execution task-text
availability, current-symbol resolution, and independent schema/extractor
version constants. Chronological consumers must use only task snapshots marked
`known_pre_execution` and must exclude uncertain or unavailable timing rather
than inferring it from capture or Git commit time. A minimal manual-sync
embedding looks like:

```rust,no_run
use std::path::Path;
use orbit_graph::{Graph, SearchQuery, SyncMode, SyncPolicy};

let graph = Graph::open(Path::new("."), SyncPolicy::Manual)?;
graph.sync(SyncMode::Full)?;
let matches = graph.search(&SearchQuery::new("helper"))?;
# Ok::<(), orbit_graph::GraphError>(())
```
