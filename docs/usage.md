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

Synchronization is explicit. Query commands never write the index; run
`orbit-graph sync` after source changes. Incremental sync compares metadata and
content hashes, re-extracts the changed files, and re-resolves references in
other files that name a definition the changed or removed files added, removed
or renamed, so its references match what a full rebuild stores.
`orbit-graph sync --full` rehashes and re-extracts every supported file.

The sync output names the database it wrote (`database_path`) and the branch
that database indexes (`branch`). It also lists the paths the sync did not
index:

- **`failed`** holds a `count` and one entry per directory or file the sync
  could not read or extract, with its `path`, the `operation` that failed, a
  stable `error_kind` (`permission_denied`, `not_found`, `io`, `invalid_data`,
  `parse_timeout`, `unsupported` or `panic`) and the `message`. One bad path
  does not fail the sync: every other file is indexed, and anything already
  indexed at or under the failed path keeps its rows. A one-line stderr notice
  gives the count. The sync exits 1 only when paths failed and nothing is
  indexed.
- **`skipped`** holds a `count` and one entry per file deliberately left out,
  with its `path` and `reason` (`oversize` for a file above the byte cap).

An interrupted sync never leaves stale references behind. A file becomes
current only when pass 2 commits its references. After a crash, a kill, an
error or a cancellation, the next sync, incremental or full, extracts every
file the interrupted one touched again and re-resolves every stored reference,
so its references match what a full rebuild stores.

Each worktree stores scratch state under `.orbit-graph/` in its root. Attached
branches use `.orbit-graph/<sanitized-branch>.<extractor-version>.db`; detached
worktrees use a commit-prefixed database name. SQLite WAL and lock sidecars
live beside the database. This directory is independent of Orbit's `.orbit/`
control-plane state and should not be committed: orbit-graph writes a
`.gitignore` containing `*` into it, so it never shows up in `git status`.
Only two directories are marked this way: a real `.orbit-graph/` directory
directly in the worktree root (not a symlink, and not one that resolves
elsewhere), and orbit-graph's per-repository directory under
`$ORBIT_PLUGIN_STATE`, which only the `orbit-graph` executable reads when it
serves Orbit plugin tools. The library reads no plugin environment: a library
caller passes a database path instead. A database directory passed by a
library caller is never marked, and neither is any parent created on the way. An existing
`.gitignore` is left as it is. `orbit-graph db-path` prints
the exact path and whether a database `exists` there, without creating one.

Only `sync` builds an index. Every other command (`search`, `show`, `refs`,
`callees`, `impact`, `trace`, `overview`, `implementors`, `deps`, `db-path`,
`recommend` and `history status`) only reads: it creates, initializes and
deletes nothing, so it also works when `.orbit-graph/` is read-only. A WAL
database without its `-shm` sidecar, as a finished sync leaves it, is then
read from the main file alone (SQLite `immutable=1`); a `-wal` holding
frames without its `-shm` is refused rather than read stale. On a worktree
that was never synced, a read exits 1 with code `index_missing` and names the
command that builds the missing index:

```text
no graph index for /work/widgets at /work/widgets/.orbit-graph/main.18.db; run `orbit-graph sync`
```

`recommend` and `history status` name `orbit-graph history sync --branch
<branch>` for a missing history index instead. A database whose stored
`schema_version` differs from the one this orbit-graph writes is never used:
reads and `sync` fail with code `index_incompatible`, naming the database. For
an older one, delete it and sync; a newer one belongs to a newer orbit-graph
and is never modified. When `HEAD` cannot be read for any reason other than an
unborn branch, commands fail instead of guessing which database to use.

Only `sync` and `orbit-graph clean --confirm` remove databases. They remove a
database from an older extractor version, with its WAL, shared-memory and lock
sidecars, only when no other process holds its lock; a locked one is kept
until a later run. A database from a newer extractor version is never
removed. They also remove a detached-commit index whose commit Git reports
not found, or that no local ref reaches. A detached index whose commit Git
cannot look up for any other reason (an ambiguous prefix, an unreadable
object, an unreadable ref) is kept and reported as `unverifiable`, with a
`detail` naming the failure.

**`clean` reports by default.** This is a deliberate change: earlier releases
deleted as soon as `clean` ran. Without `--confirm`, `clean` prints
`{graph_dir, would_delete: [{path, reason}], kept: [{path, reason, detail?}],
applied: false, deleted: []}`, exits 0, and creates, writes or removes nothing;
a stderr notice names `--confirm` when there is something to delete. With
`--confirm` it deletes exactly the files the report lists, sets `applied` to
`true`, and lists them again in `deleted`. The reasons are
`older_extractor_version` and `unreachable_detached_commit` for deletion, and
`current`, `newer_extractor_version`, `locked` and `unverifiable` for a kept
database.

The scanner respects Git ignore rules and optional `.orbitignore` files. The
latter is a source-scanning ignore format retained for compatibility; it is not
an Orbit runtime configuration dependency. Git ignore rules (nested
`.gitignore` files, `.git/info/exclude` and `core.excludesFile`) are matched
in process through libgit2, as `git check-ignore` would match them: tracked
files are always indexed. Sync starts no Git process, so repository-configured
programs such as `core.fsmonitor` or hooks never run.

### Sync bounds

A sync finishes or fails within stated bounds:

- **Lock wait.** One sync at a time holds a database's `.lock` sidecar. A
  second sync waits at most 30 seconds, then fails with an error naming the
  holder's PID, the time it took the lock and its label. Set
  `ORBIT_GRAPH_LOCK_TIMEOUT_MS` (whole milliseconds; `0` tries once) to change
  the wait. Concurrent syncs of one database inside a process share one run,
  and a caller joining it waits for the same bound. If that run panics, its
  waiters get an error and the next sync starts afresh.
- **File size.** A supported file larger than 4 MiB (the change explorer's
  blob cap) is skipped and listed in `skipped`, and gets no rows; a file that
  grows past the cap loses its rows at the next sync.
- **Memory.** Pass 1 extracts and writes changed files in chunks of at most
  128 files or 32 MiB of source, so it holds one chunk of extracted rows at a
  time. References and command rows still wait in memory for pass 2.
- **Parse time.** Each file's tree-sitter parse has a 10-second deadline. A
  file that exceeds it counts as an extraction failure and is listed in
  `failed`, like a file whose extractor panics.
- **Watcher.** A watching `Graph` buffers at most 1,024 file events. When the
  buffer is full, further events are dropped and counted, and the watcher
  schedules a sync, which rescans the whole worktree, so no change is lost.
  Dropping the graph waits up to 5 seconds for the watcher thread, then
  detaches it with a warning.

Delivered-change history is a separate rebuildable index at
`.orbit-graph/change-history.<schema-version>.sqlite3`. It learns only from
immutable Git tree differences supplied by the public delivery contract or from
explicitly weaker first-parent Git sync evidence. It never reads task
`context_files` or Orbit private state. Delivery, ingestion, task creation, and
snapshot availability times remain distinct and carry explicit
certainty/provenance. See [the design and v2 contract](design/change-recommendations.md).
`recommend` also caches the symbols it extracts from a target revision beside
that index, as `recommend-target.<extractor-version>.<tree-id>.json` (owner-only,
a few recent entries kept). The cache is best effort, not index state: it is
skipped when the directory is read-only, and deleting these files is always
safe.

If the history extractor or import version changes, history reads fail with
`version_mismatch`, naming the stored and expected values, database path, and
rebuild command. Run `history rebuild --branch <name>` to inspect the scope,
then add `--confirm` to repair it. Verified deliveries are re-extracted from
their stored envelopes and retained unless `--discard-verified` is explicit.

## Commands

| Command | Purpose |
| --- | --- |
| `sync [--full]` | Incrementally update or fully rebuild the index. |
| `history import --input <path\|->` | Import a validated v2 delivery JSON envelope. |
| `history sync --branch <name> [--limit <n>]` | Atomically index new first-parent commits as Git-only evidence. |
| `history status --branch <name>` | Report history versions, cursor, evidence, and association counts. |
| `history rebuild --branch <name> [--limit <n>] [--confirm] [--discard-verified]` | Preview one scope, then atomically re-extract Git-only history while retaining verified deliveries by default. |
| `recommend --query <text>\|--task-id <id> [--level file\|symbol]` | Rank current destinations with evidence and freshness. |
| `evaluate --input <corpus.json>` | Compare four ranking variants chronologically. |
| `evaluate --live --branch <name> [--limit <n>] [--k <n>] [--revision <rev>]` | Hold out first-parent commits and score Git-only commit-text relevance. Requires `history sync` first. |
| `search <query> [--kind symbol|string|config] [--lang <id>] [--limit <n>]` | Full-text search indexed definitions, strings, or config keys. |
| `show <selector> [--max-bytes <n>]` | Return metadata and a bounded source slice. |
| `refs <symbol> [--confidence <level>] [--kind <kind>]` | Return inbound references and relations. |
| `callees <symbol> [--include-unresolved]` | Return calls made by a symbol. Unresolved calls whose name has no indexed definition (standard-library and prelude calls) are hidden by default, counted in `hidden_unresolved`, and noted on stderr. |
| `impact <selector> [--depth <n>] [--confidence <level>] [--direction inbound\|outbound\|both]` | Traverse callers, callees, or both around a selector (default: both). |
| `trace <command> [--depth <n>] [--confidence <level>]` | Trace a discovered CLI command handler and its calls. |
| `overview [<file-or-dir-selector>] [--format summary|full]` | Summarize indexed files and symbols. |
| `implementors <trait-selector>` | Find concrete implementations of a trait-like symbol. |
| `deps <file-or-dir-selector>` | List source-level module/import edges. |
| `db-path` | Show the current database path, extractor version, and whether it exists. |
| `clean [--confirm]` | Report obsolete graph databases; delete them only with `--confirm`. |
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

Rust method calls resolve by type when the extractor can read the receiver's
type: a parameter or `let` with a type annotation (`runtime: &OrbitRuntime`),
a constructor (`T::new()`, `T::default()`, a `T { .. }` literal), or
`self`/`Self`. `&T`, `Box`/`Arc`/`Rc<T>` and `use .. as Alias` names are seen
through, and `dyn Trait`/`impl Trait` receivers resolve to the trait's
declaration. A receiver typed by a type parameter (`x: T`) stays unknown. The
type's member is found by first narrowing to the module the call names (a
written path such as `crate::config::Config::load()`, or the import that
brings `Config` into the file), then preferring an inherent method over a
trait impl over a trait declaration, as Rust's method lookup does. A member
the type gets from a trait's default body resolves to that trait's
declaration. A type named through a path or import that the index does not
contain (an external crate's `reqwest::Client`) reports `fuzzy`, never a local
type of the same name.

A written `Type::member(..)` path never falls back to a bare-name match: when
no indexed type of that name has the member, it reports `fuzzy`. A free
function call never resolves to a same-named method, and a module path outside
the crate (`std::process::id()`) only matches a `same_module` item under that
path; paths starting with `crate::`, `self::` or `super::` keep the plain
same-module rule.

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

## Live Git commit-text evaluation

`orbit-graph evaluate --live` measures the Git-only commit-message signal that
strict replay never reads. It walks the named branch's first-parent history
from `--revision` (default: the branch tip), newest first, and holds out up to
`--limit` commits (default 300). For each commit C with a non-empty subject
whose delivery is already in the history index, the query is that subject and
the target revision is C's first parent. No `--cutoff` is set, so the request
is live. Truth is the files C changed that still exist at that parent. Added
files are omitted from the denominator.

The command reads an existing history index and does not sync or import. Sync
through the tip first (`history sync --branch <name>`) so the held-out commit
and later deliveries are present and the ancestry check can see them. Each case
fails the command if a recommendation cites a delivery that is not an ancestor
of the target. Five scoring points are reported for the combined file-level
ranker: no commit text, `0.5·s`, `0.5·s²`, `0.25·s²`, and `1.0·s`. Cohorts are
`all`, `title_restating` (the subject cites a bracketed task id such as
`[ORB-123]`, the squash-merge marker; no task store is consulted), and
`without_title_restating`. Metrics are precision@k with k slots reserved,
recall@k, and MRR@k. A metric with no cases or no relevant truth is `null`.

Like `recommend`, a live query may write a best-effort target-symbol cache
beside the history index. That cache is not history evidence. The recorded
runs are in [`docs/evaluation/`](evaluation/README.md).

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
