---
title: "Graph store: decisions"
owner: maintainers
last_updated: 2026-11-08
status: Accepted
stage: shipped
next: "—"
feature: graph-store
doc_role: decisions
summary: "Graph store, sync bounds and state containment decisions (ORB-13158, ORB-13156, ORB-13162)."
---

# Graph store: decisions

Decisions for the graph store, folded from the former per-task folders
`graph-store/` (ORB-13158), `sync-bounds/` (ORB-13156) and
`state-containment/` (ORB-13162). Each entry keeps its source task's D-number,
prefixed by its source area so every `##` title stays unique (STD-06@1 §R11).

**Graph store: context.**
Decisions for ORB-13158, which makes read commands and `read_only` plugin
tools never create, initialize or delete index state (`STD-01 §R31`), opens
every index version-aware (`STD-03 §R6`, `§R10`), and reports a missing index
as `index_missing` naming the command that builds it (`STD-02 §R29`). The
user-facing rules are under "Index lifecycle and location" in
[`docs/usage.md`](../../usage.md) and "Code-graph index" in
[`docs/plugin.md`](../../plugin.md).

## Graph store D1. Only `sync` builds and only `sync` and `clean` remove


Query commands open the graph with `Graph::open_existing_read_only`, and
`history status`, `recommend` and the plugin `status` and `recommend` tools
open history with `HistoryIndex::open_read_only`. Neither opener creates a
directory, a database or a sidecar; a missing file is `GraphError::IndexMissing`
with the command that builds it. `Graph::open` and `open_with_revision` no
longer clean up either: `orbit-graph sync` runs `clean_old_databases` before
syncing, and `orbit-graph clean --confirm` runs it on request; `orbit-graph
clean` without `--confirm` runs `plan_clean_old_databases`, which applies the
same predicates and reports them without writing anything (`STD-01 §R5`).
Cleanup resolves the active database without opening it, so it creates
nothing.

Cleanup removes a database family (`.db`, `-wal`, `-shm`, `.db.lock`) only
when its extractor version is strictly older and `File::try_lock` on its
`.db.lock` succeeds; a held lock keeps the family for a later run. A newer
version is never removed (`STD-03 §R10`). An equal-version detached database
is removed only when Git proves its commit unreachable: `find_commit_by_prefix`
reports `NotFound`, or a walk of every ref finds none reaching it. Any other
Git error (an ambiguous prefix, an object-database error, a ref that cannot be
peeled) keeps it and reports it as `unverifiable` (`STD-02 §R31`,
`STD-03 §R29`).


## Graph store D2. Read currency on a read-only index


A read-only SQLite connection to a WAL database still creates `-wal` and
`-shm` next to it when the directory is writable, and cannot open it at all
when the directory is read-only and `-shm` is absent. Reads therefore open
`SQLITE_OPEN_READ_ONLY` with `query_only` and choose the currency from the
files present:

- a rollback-journal database, or a WAL database with both sidecars, opens
  live;
- a WAL database with no `-shm` and an empty or absent `-wal` (a finished
  sync's last connection checkpointed and removed them) opens with
  `immutable=1`, reading the main file alone, which then holds every
  committed transaction;
- a `-wal` holding frames without its `-shm`, or a `-shm` without its `-wal`,
  is refused, because the main file alone would be stale.

This is the approach Orbit's own observational opener takes.


## Graph store D3. First opens serialize on the sync lock


Sixteen concurrent first `sync` processes used to fail with "table files
already exists": each saw an empty file and created the schema. The emptiness
check is now repeated inside the `BEGIN IMMEDIATE` schema transaction. SQLite
also answers a concurrent switch of a new file to WAL with `SQLITE_BUSY`
without calling the busy handler, so an opener that finds the file not yet in
WAL mode takes the database's sync lock (`<db>.lock`, the bounded
`FileLockGuard`) until setup is done. An initialized database skips the lock.


## Graph store D4. Schema identity is validated, not migrated


**Deviation, `STD-03@2 §R23`.** The graph database is a derived index, rebuilt
per version rather than migrated through a migrations registry. The database
file name carries the extractor version, and a new extractor version gets a
new file that `sync` builds from source. Every open instead validates
`meta.schema_version` against `STORE_SCHEMA_VERSION`. A mismatch, a missing
`meta` table or a missing row is `GraphError::IndexIncompatible` naming the
database: a newer one is left for the orbit-graph that wrote it, and an older
or unreadable one is to be deleted and rebuilt with `orbit-graph sync`. No
command writes to it (`STD-03 §R10`). The extractor version is unchanged
because database file names did not change.


## Graph store D5. HEAD read failures are errors


Choosing the database needs the branch. Only an unborn branch or a directory
outside Git selects the `HEAD` family now; any other failure to open the
repository or read `HEAD` (a corrupt ref, for example) is an error naming the
repair, instead of silently selecting the `HEAD` database.


## Graph store D6. The recommendation target-symbol cache


**Deviation, `STD-01@2 §R31`.** `recommend` still writes, touches and prunes
`recommend-target.<extractor>.<tree>.json` files beside the history index
(ORB-13091). They are a performance cache of what extraction produces from
immutable Git objects, not index state: cached and uncached answers are
identical, every cache write is best effort, and on a read-only directory the
write fails silently and the request succeeds unchanged. Moving the cache out
of the index directory is left to a follow-up.

**Sync bounds: context.**
Decisions for ORB-13156, which makes a graph sync finish or fail within
stated bounds (`STD-03 §R2`, `§R4`, `§R7`, `§R15`, `§R22`; `STD-05 §R10`).
The user-facing limits are listed under "Sync bounds" in
[`docs/usage.md`](../../usage.md).

## Sync bounds D1. Git ignore rules are matched in process


The scanner no longer starts `git check-ignore --stdin`. It matches with
libgit2 (`Repository::is_path_ignored`), which reads nested `.gitignore`
files, `.git/info/exclude` and `core.excludesFile`, and skips paths in the
index, as `check-ignore` without `--no-index` does. With no child process,
the pipe deadlock, the unbounded wait and the inherited environment are gone,
and nothing the repository configures (`core.fsmonitor`, hooks) can run.
The `STD-03 §R11`–`§R15` child rules therefore have nothing to apply to here.


## Sync bounds D2. One bounded lock helper with a diagnostic holder record


`crates/orbit-graph/src/lock.rs` polls `File::try_lock` until a deadline (30 s
by default, `ORBIT_GRAPH_LOCK_TIMEOUT_MS` to override) and writes the holder's
PID, acquisition time and label into the lock file after acquiring it. A
timeout names that holder, or says no readable record exists; either way it is
contention. ORB-13160 reuses the helper for the history lock.

**Deviation, `STD-03@2 §R5`.** The holder record is rewritten in place inside
the lock file rather than replaced by rename. The kernel lock is held on that
file's inode, so renaming a new file over it would detach the record from the
lock. The record is diagnostic only (`STD-03 §R7`): a torn or stale record
reads as "holder unknown" and never decides ownership.


## Sync bounds D3. Coalesced followers wait as long as a lock waiter


A caller that joins an in-flight sync of the same database in the same process
waits for the same deadline as the database lock, and then fails naming the
leader thread. A follower is in the same position as a second process waiting
on the lock, so both get one bound, even though a legitimately long leader can
outlast it. A `LeaderGuard` publishes a failure and removes the in-flight entry
if the leader unwinds (`STD-03 §R4`).


## Sync bounds D4. Pass-1 chunks bound extracted rows, not refs


Pass 1 extracts and writes at most 128 files or 32 MiB of source per chunk,
so symbol, import, relation, string and config rows are held for one chunk at
a time.

**Deviation, `STD-03@2 §R22`.** Each written file's raw refs and command rows
are still carried in memory to pass 2 and to the final command transaction.
Pass 2 resolves refs against the whole change set, and the frozen store schema
does not stage raw refs. The byte cap bounds each file's contribution, so the
total is proportional to the changed source. Staging refs on disk is a
separate change.


## Sync bounds D5. The parse deadline covers tree-sitter parsing


A per-file deadline (10 s) is enforced through tree-sitter's progress
callback, which runs every hundred or so parse operations, and an expired
deadline makes the file an extraction failure. The walk over the parsed tree,
and the Markdown and configuration extractors, which do not use tree-sitter,
are linear in the input and bounded by the 4 MiB byte cap instead.


## Sync bounds D6. Watcher overflow drops events and rescans


The watcher's event channel holds 1,024 events. When it is full, the notify
callback drops the event and counts it, and the sync thread schedules a sync
(`STD-03 §R2`, drop with a counted signal). Every watcher sync diffs the whole
worktree, so the rescan recovers every dropped change. Dropping the watcher
waits at most 5 s for its thread and then detaches it with a warning; a
detached thread can at most finish a sync that is itself bounded by the lock
wait.


## Sync bounds D7. Extractor version 17


The byte cap and the parse deadline change which files get rows, so
`EXTRACTOR_VERSION` moves from 16 to 17 and every pin moves with it.

**State containment: context.**
Decisions for ORB-13162: repository content can neither redirect nor
pre-populate index, history, plugin or snapshot-cache state; that state is
created owner-only; and every durable replacement goes through one
`atomic_write` helper (`STD-05 §R6`–`§R9`, `STD-03 §R5`). The user-facing rules
are under "Index lifecycle and location" in [`docs/usage.md`](../../usage.md),
"Code-graph index" in [`docs/plugin.md`](../../plugin.md), and "Snapshot
caching" in [`docs/design/changes/2_design.md`](../changes/2_design.md).

## State containment D1. One module decides every state path


`crates/orbit-graph/src/state_dir.rs` is the only code that resolves a state
directory or opens a state file:

- `scratch_state_dir` resolves `<worktree>/.orbit-graph/<sub>` from the
  canonical worktree root, `lstat`s every component, and refuses a symlink
  (dangling or not), a non-directory, or a directory another user owns. It
  also refuses the whole directory when the Git index tracks `.orbit-graph` or
  anything below it (`Index::get_path`, then `find_prefix` on
  `.orbit-graph/`), and when `.orbit-graph/` directly holds any symlink.
- `private_state_dir` does the same below a trusted root (the plugin state
  root), and `chosen_state_dir` below the nearest existing ancestor of a
  caller-chosen directory such as a snapshot `cache_dir`, applying the
  worktree rules to the part inside a worktree.
- Missing directories are created one at a time with `DirBuilder` mode `0700`
  and re-`lstat`ed after creation (`§R7`).
- `open_state_file` opens with `O_NOFOLLOW | O_CLOEXEC` and mode `0600`,
  checks the descriptor with `fstat` (regular file, owned by the euid), and a
  writer repairs group or other bits with `fchmod` through that descriptor.

Every refusal is `GraphError::UnsafeStatePath { path, reason }`, displayed as
`refusing orbit-graph state path <path>: <reason>` and reported by the CLI
with the new error code `unsafe_state_path` (an added code; no existing code
changed meaning). The plugin keeps reporting it as `graph_error`, because the
plugin error-code set is a published enum; the message carries the path and
reason. The snapshot cache maps it to the added
`CacheError::UnsafeStatePath` variant (`CacheError` is `#[non_exhaustive]`).


## State containment D2. A symlink directly in `.orbit-graph/` refuses the whole directory


The acceptance matrix requires `history status` to refuse a symlinked graph
database or lock, although it never opens the graph database. Rather than
teach every command about every other command's files, the scratch directory
itself is refused when it directly holds a symlink: orbit-graph never creates
one there, so any link is content standing in for state. The check is one
`read_dir` of a directory with a handful of entries. Deeper levels are covered
by the per-component walk and by `O_NOFOLLOW` opens.


## State containment D3. SQLite opens the physical path with `SQLITE_OPEN_NOFOLLOW`


`SQLITE_OPEN_NOFOLLOW` refuses a symlink in any component of the path it is
given, so it is only safe on a physical path. `store::open_at_path` resolves
the verified directory to its physical path, carries it as
`OpenedGraph::physical_db`, and opens the writer and the read connection on it
through `store::open_writer`. The spelled `GraphDbPath` stays what commands
report, so JSON output, test hooks and sync coalescing keyed by path are
unchanged; on macOS, where temporary directories live under the `/var ->
/private/var` link, the spelled and physical paths differ and both keep
working.


## State containment D4. Readers validate; writers repair (deviation from `STD-05@1 §R9`)


`STD-05@1 §R9` asks for state to be validated when it is loaded: owned by the
current user, not group- or world-writable, not reached through a symlink, and
otherwise refused or repaired with a warning. `STD-01@2 §R31` forbids a
reporting command from writing anything, a `chmod` included. The two are
reconciled this way:

- every opener, reader or writer, refuses a symlink and another user's file or
  directory;
- a writer (`sync`, `history sync`, `clean --confirm`, a plugin `maintain`
  operation, a snapshot build) repairs group or other bits to `0600`/`0700`
  and logs a warning;
- a reader (queries, `history status`, `recommend`, `clean` without
  `--confirm`) reads a loose-mode file as it is and does not refuse it.

Refusing loose modes in readers would make every index created by an earlier
release under a permissive umask unreadable until the next `sync`, for no
gain: the graph index grants no authority and holds no secret, and the next
writer repairs it. The snapshot cache is the exception (D6), because its
entries are reused as evidence without being rebuilt.


## State containment D5. Every durable replacement is `atomic_write`


`orbit_graph::atomic_write` writes a `0600` temp file named
`<name>.tmp-<pid>-<seq>` in the target's directory (created `O_EXCL`, so never
through a link), `fsync`s it, renames it over the target, and `fsync`s the
directory. It replaces the plugin's `write_durably` (`graph.current.json`,
`graph.owned.json`) and `recommend.rs`'s `write_private_file` (the
target-symbol cache), and the snapshot cache's `entry.json` and `last_used`.
The reclaimers of those directories still recognise the older temp-name
grammars (`<name>.<pid>.tmp` and `json.tmp-<pid>`) so leftovers from an earlier
release are still cleaned. `clippy.toml` bans `std::fs::write`.

The task also names report exports. No file-writing report export exists any
more: the explorer binary that wrote them was removed in ORB-13255, and the
`orbit-graph-changes` report module returns values to its caller. There is
nothing to route through the helper; the ban keeps any new one on it.


## State containment D6. The snapshot cache trusts an entry only by its provenance


A cache hit reuses an entry's tree and index without rebuilding them, so an
entry is trusted only when its directory is a real directory owned like the
cache directory (which was verified to be the euid's) with no group or other
bits, and its `entry.json` is a regular file with the same properties. An
entry of ours with looser modes (an earlier release under a permissive umask,
or `chmod`) is rebuilt by `lookup` and removed by `clean` with the added
`CleanReason::Untrusted` (`untrusted`); an entry that is a symlink or another
user's is kept and never read, and `lookup` answers `Kept { Untrusted }` so the
side falls back to a temporary tree. `CACHE_SCHEMA_VERSION` is unchanged:
existing entries are rebuilt through this check rather than a key bump, which
also covers entries whose key is current. A `last_used` that is not a
complete newline-terminated number, empty or torn, counts as absent, so the
age falls back to `published_at` rather than reading a truncated number as a
much older time.

These owner and mode checks are an accident guard against another user and a
permissive umask, not a boundary against a process running as the same user,
which can rewrite the cache at will (`STD-05@1 §R5`).


## State containment D7. A refused snapshot cache falls back instead of failing


`Comparison::open*` already treats an unusable cache directory as a reason to
index both sides into task-owned temporary trees, reporting it in
`cache_note`. A refused cache path does the same: the comparison succeeds,
`cache_outcome` is `disabled`, `cache_dir()` is `None`, and the note carries
the `refusing orbit-graph state path <path>: <reason>` text. A per-entry
refusal (a symlinked lock inside an entry) falls back for that side only.
Failing the comparison would make an inspectable repository uninspectable
because of a directory it does not need.


## State containment D8. The snapshot tree's `.git` anchor keeps libgit2's modes


Every materialized directory and blob of a snapshot tree is `0700`/`0600`.
The empty repository `anchor_git_discovery` initialises at the tree root is
created by libgit2, which applies the process umask to `.git/`. It sits inside
the `0700` entry and tree directories, so no other user can reach it; the
umask test skips `.git` for that reason.


## State containment D9. Test allowances for the `fs::write` ban


Test fixtures write hundreds of files with `fs::write`. Library and binary
crates allow `clippy::disallowed_methods` under `cfg(test)`, and each
integration-test crate that writes fixtures allows it at its root with a
`reason`. The allowance also covers the `std::sync::mpsc::channel` ban in test
code; no test uses that constructor today.


## State containment D10. Umask tests run in a child


`STD-04@1 §R6` forbids changing process-wide state in the shared test process.
The CLI tests run the binary through `sh -c 'umask 0002; exec "$@"'`. The
snapshot-cache umask test re-executes its own test binary through the same
shell with a marker variable, and the child builds the cache and checks the
modes.
