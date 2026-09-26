# Sync bounds: decisions

Decisions for ORB-13156, which makes a graph sync finish or fail within
stated bounds (`STD-03 §R2`, `§R4`, `§R7`, `§R15`, `§R22`; `STD-05 §R10`).
The user-facing limits are listed under "Sync bounds" in
[`docs/usage.md`](../../usage.md).

## D1. Git ignore rules are matched in process

The scanner no longer starts `git check-ignore --stdin`. It matches with
libgit2 (`Repository::is_path_ignored`), which reads nested `.gitignore`
files, `.git/info/exclude` and `core.excludesFile`, and skips paths in the
index, as `check-ignore` without `--no-index` does. With no child process,
the pipe deadlock, the unbounded wait and the inherited environment are gone,
and nothing the repository configures (`core.fsmonitor`, hooks) can run.
The `STD-03 §R11`–`§R15` child rules therefore have nothing to apply to here.

## D2. One bounded lock helper with a diagnostic holder record

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

## D3. Coalesced followers wait as long as a lock waiter

A caller that joins an in-flight sync of the same database in the same process
waits for the same deadline as the database lock, and then fails naming the
leader thread. A follower is in the same position as a second process waiting
on the lock, so both get one bound, even though a legitimately long leader can
outlast it. A `LeaderGuard` publishes a failure and removes the in-flight entry
if the leader unwinds (`STD-03 §R4`).

## D4. Pass-1 chunks bound extracted rows, not refs

Pass 1 extracts and writes at most 128 files or 32 MiB of source per chunk,
so symbol, import, relation, string and config rows are held for one chunk at
a time.

**Deviation, `STD-03@2 §R22`.** Each written file's raw refs and command rows
are still carried in memory to pass 2 and to the final command transaction.
Pass 2 resolves refs against the whole change set, and the frozen store schema
does not stage raw refs. The byte cap bounds each file's contribution, so the
total is proportional to the changed source. Staging refs on disk is a
separate change.

## D5. The parse deadline covers tree-sitter parsing

A per-file deadline (10 s) is enforced through tree-sitter's progress
callback, which runs every hundred or so parse operations, and an expired
deadline makes the file an extraction failure. The walk over the parsed tree,
and the Markdown and configuration extractors, which do not use tree-sitter,
are linear in the input and bounded by the 4 MiB byte cap instead.

## D6. Watcher overflow drops events and rescans

The watcher's event channel holds 1,024 events. When it is full, the notify
callback drops the event and counts it, and the sync thread schedules a sync
(`STD-03 §R2`, drop with a counted signal). Every watcher sync diffs the whole
worktree, so the rescan recovers every dropped change. Dropping the watcher
waits at most 5 s for its thread and then detaches it with a warning; a
detached thread can at most finish a sync that is itself bounded by the lock
wait.

## D7. Extractor version 17

The byte cap and the parse deadline change which files get rows, so
`EXTRACTOR_VERSION` moves from 16 to 17 and every pin moves with it.
