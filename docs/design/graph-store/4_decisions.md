# Graph store: decisions

Decisions for ORB-13158, which makes read commands and `read_only` plugin
tools never create, initialize or delete index state (`STD-01 §R31`), opens
every index version-aware (`STD-03 §R6`, `§R10`), and reports a missing index
as `index_missing` naming the command that builds it (`STD-02 §R29`). The
user-facing rules are under "Index lifecycle and location" in
[`docs/usage.md`](../../usage.md) and "Code-graph index" in
[`docs/plugin.md`](../../plugin.md).

## D1. Only `sync` builds and only `sync` and `clean` remove

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

## D2. Read currency on a read-only index

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

## D3. First opens serialize on the sync lock

Sixteen concurrent first `sync` processes used to fail with "table files
already exists": each saw an empty file and created the schema. The emptiness
check is now repeated inside the `BEGIN IMMEDIATE` schema transaction. SQLite
also answers a concurrent switch of a new file to WAL with `SQLITE_BUSY`
without calling the busy handler, so an opener that finds the file not yet in
WAL mode takes the database's sync lock (`<db>.lock`, the bounded
`FileLockGuard`) until setup is done. An initialized database skips the lock.

## D4. Schema identity is validated, not migrated

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

## D5. HEAD read failures are errors

Choosing the database needs the branch. Only an unborn branch or a directory
outside Git selects the `HEAD` family now; any other failure to open the
repository or read `HEAD` (a corrupt ref, for example) is an error naming the
repair, instead of silently selecting the `HEAD` database.

## D6. The recommendation target-symbol cache

**Deviation, `STD-01@2 §R31`.** `recommend` still writes, touches and prunes
`recommend-target.<extractor>.<tree>.json` files beside the history index
(ORB-13091). They are a performance cache of what extraction produces from
immutable Git objects, not index state: cached and uncached answers are
identical, every cache write is best effort, and on a read-only directory the
write fails silently and the request succeeds unchanged. Moving the cache out
of the index directory is left to a follow-up.
