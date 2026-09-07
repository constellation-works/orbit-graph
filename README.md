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

### Install the Orbit external tools

Orbit 0.19.2 or newer can register the installed binary under three versioned
sidecar manifests. Registration is local configuration; maintainers retain
production registration ownership.

```sh
cargo install --path . --locked
./scripts/install-orbit-plugin.sh
orbit tool show orbit.graph.recommend
orbit tool show orbit.graph.status
orbit tool show orbit.graph.maintain
```

Use `--binary /absolute/path/to/orbit-graph` to register a development build,
and `--orbit-root /absolute/path/to/.orbit` to select a non-default Orbit
authority. Remove only the registrations with
`./scripts/uninstall-orbit-plugin.sh`; derived `.orbit-graph/` indexes are
deliberately retained. The executable recognizes the three registered
`ORBIT_TOOL_NAME` values and reads one JSON object from stdin with no argv,
matching Orbit's external-tool execution protocol.

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
`.orbit-graph/change-history.2.sqlite3`. It learns only from immutable Git tree
differences supplied by the public delivery contract or from explicitly weaker
first-parent Git sync evidence. It never reads task `context_files` or Orbit
private state. Delivery, ingestion, task creation, and snapshot availability
times remain distinct and carry explicit certainty/provenance. See
[the design and v2 contract](docs/design/change-recommendations.md).

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

## Orbit recommendation plugin

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

For a pending task, task-ID lookup uses the public `orbit.task.show` response
and is eligible only when observed before execution. During or after execution,
pass an attested earlier `task_snapshot`; the adapter labels a current read as
post-execution instead of inventing historical availability. `hybrid: true`
uses public `orbit.search`; failure is surfaced and local lexical fallback is
named in `adapter.warnings`.
The calling activity must also allow `orbit.task.show` for live lookup and
`orbit.search` for hybrid retrieval; the adapter does not bypass Orbit policy.
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

`orbit_sync` reads only public `orbit.task.show` and `orbit run show` responses,
then verifies full commit objects, strict base ancestry, and landing-branch
reachability in the explicitly routed Git repository. It reports partial
coverage: current Orbit has no cursor-paginated detailed delivery feed, so only
explicit run IDs and each requested task's current `job_run_id` are processed.
Retrying or submitting omitted IDs is safe because delivery IDs are idempotent.
Run completion time remains `uncertain` delivery-time evidence when the public
response does not attest the exact landing instant. `history_sync` is a bounded
Git-only fallback; `import` accepts one public DeliveryImport v2 envelope.

The bundled agent guidance is in
[`plugin/skills/orbit-graph/SKILL.md`](plugin/skills/orbit-graph/SKILL.md).

## Chronological evaluation

`orbit-graph evaluate` consumes a versioned public corpus and runs combined,
task-search-only, graph-only, and frequency ranking at file and symbol levels.
Held-out delivery diffs are previewed as truth but never imported. Task text and
training deliveries must be strictly before the cutoff; a target boundary
already present in the index is excluded fail-closed. Reports include
recall@K, precision@K, stale-result rate, mean/maximum latency, coverage,
exclusions, source provenance, input digest, and exact revisions.

The synthetic adversarial executable fixture runs in CI. The bounded real Orbit
prospective input, measured result, and its no-superiority limitation are
documented in [`docs/evaluation/`](docs/evaluation/README.md).

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

See [PROVENANCE.md](PROVENANCE.md) for the exact recovered source and adaptation
boundary, and [CONTRIBUTING.md](CONTRIBUTING.md) for validation requirements.

## License

MIT. See [LICENSE.md](LICENSE.md).
