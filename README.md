# orbit-graph

`orbit-graph` builds a local SQLite index of a source tree and exposes graph
queries through a Rust library and a JSON-only command-line interface. It is a
standalone recovery of Orbit's historical graph implementation: it does not
need an Orbit checkout, runtime, configuration, or private dependency.

## Install

Rust 1.89 or newer and Git are required. From a checkout:

```sh
cargo install --path . --locked
```

For development, build without installing:

```sh
cargo build --locked
```

## Quick start

Commands operate on the Git worktree containing the current directory. If the
current directory is not in a Git worktree, it is treated as the root.

```sh
fixture="$(mktemp -d)"
git -C "$fixture" init -b main
mkdir -p "$fixture/src"
printf 'pub fn helper() -> i32 { 1 }\npub fn entry() -> i32 { helper() }\n' > "$fixture/src/lib.rs"

cd "$fixture"
orbit-graph sync --full
orbit-graph search helper --kind symbol --limit 5
orbit-graph show 'symbol:src/lib.rs#entry:function' --max-bytes 1024
orbit-graph refs 'symbol:src/lib.rs#helper:function' --confidence fuzzy --kind call
orbit-graph callees 'symbol:src/lib.rs#entry:function'
```

Every successful data command writes one JSON value to stdout; `--help` uses
conventional text help. Failures return a nonzero status and write a JSON
object with `error.code` and `error.message` to stderr. Set `RUST_LOG` to enable
diagnostic tracing on stderr.

## Index lifecycle and location

Synchronization is explicit. Query commands never refresh the index; run
`orbit-graph sync` after source changes. Incremental sync compares metadata and
content hashes. `orbit-graph sync --full` rehashes and re-extracts every
supported file.

Each worktree stores scratch state under `.orbit-graph/` in its root. Attached
branches use `.orbit-graph/<sanitized-branch>.4.db`; detached worktrees use a
commit-prefixed database name. SQLite WAL and lock sidecars live beside the
database. This directory is independent of Orbit's `.orbit/` control-plane
state and should not be committed. `orbit-graph db-path` prints the exact path,
and `orbit-graph clean` removes obsolete extractor versions and unreachable
detached-commit indexes.

The scanner respects Git ignore rules and optional `.orbitignore` files. The
latter is a source-scanning ignore format retained for compatibility; it is not
an Orbit runtime configuration dependency.

Delivered-change history is a separate rebuildable index at
`.orbit-graph/change-history.1.sqlite3`. It learns only from immutable Git tree
differences supplied by the public delivery contract or from explicitly weaker
first-parent Git sync evidence. It never reads task `context_files` or Orbit
private state. See [the design and v1 contract](docs/design/change-recommendations.md).

## Commands

| Command | Purpose |
| --- | --- |
| `sync [--full]` | Incrementally update or fully rebuild the index. |
| `history import --input <path\|->` | Import a validated v1 delivery JSON envelope. |
| `history sync --branch <name> [--limit <n>]` | Atomically index new first-parent commits as Git-only evidence. |
| `history status --branch <name>` | Report history versions, cursor, evidence, and association counts. |
| `history rebuild --branch <name> [--limit <n>]` | Atomically recreate one scope from Git-only history. |
| `search <query> [--kind symbol|string|config] [--lang <id>] [--limit <n>]` | Full-text search indexed definitions, strings, or config keys. |
| `show <selector> [--max-bytes <n>]` | Return metadata and a bounded source slice. |
| `refs <symbol> [--confidence <level>] [--kind <kind>]` | Return inbound references and relations. |
| `callees <symbol>` | Return calls made by a symbol. |
| `impact <selector> [--depth <n>] [--confidence <level>]` | Traverse the bounded reverse dependency graph. |
| `trace <command> [--depth <n>] [--confidence <level>]` | Trace a discovered CLI command handler and its calls. |
| `overview [<file-or-dir-selector>] [--format summary|full]` | Summarize indexed files and symbols. |
| `implementors <trait-selector>` | Find concrete implementations of a trait-like symbol. |
| `deps <file-or-dir-selector>` | List source-level module/import edges. |
| `db-path` | Show the current database path and extractor version. |
| `clean` | Delete obsolete graph databases. |
| `version` | Show crate and extractor versions. |

Confidence levels are `exact`, `import`, `same_module`, and `fuzzy`. Reference
kinds are `call`, `type`, `use`, `trait_bound`, `impl`, `extends`, and
`implements`.

Selectors use one of these forms:

```text
dir:<path>
file:<path>
symbol:<path>#<name>:<kind>
module:<qualified-name>
command:<name>
```

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
results. It also exposes `HistoryIndex`, the v1 import/provenance and extracted
change types, current-symbol resolution, and independent schema/extractor
version constants. A minimal manual-sync embedding looks like:

```rust,no_run
use std::path::Path;
use orbit_graph::{Graph, SearchQuery, SyncMode, SyncPolicy};

let graph = Graph::open(Path::new("."), SyncPolicy::Manual)?;
graph.sync(SyncMode::Full)?;
let matches = graph.search(&SearchQuery::new("helper"))?;
# Ok::<(), orbit_graph::GraphError>(())
```

See [PROVENANCE.md](PROVENANCE.md) for the exact recovered source and adaptation
boundary, and [CONTRIBUTING.md](CONTRIBUTING.md) for validation requirements.

## License

MIT. See [LICENSE.md](LICENSE.md).
