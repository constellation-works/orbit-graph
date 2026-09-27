# State containment: decisions

Decisions for ORB-13162: repository content can neither redirect nor
pre-populate index, history, plugin or snapshot-cache state; that state is
created owner-only; and every durable replacement goes through one
`atomic_write` helper (`STD-05 §R6`–`§R9`, `STD-03 §R5`). The user-facing rules
are under "Index lifecycle and location" in [`docs/usage.md`](../../usage.md),
"Code-graph index" in [`docs/plugin.md`](../../plugin.md), and "Snapshot
caching" in [`docs/design/change-explorer.md`](../change-explorer.md).

## D1. One module decides every state path

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

## D2. A symlink directly in `.orbit-graph/` refuses the whole directory

The acceptance matrix requires `history status` to refuse a symlinked graph
database or lock, although it never opens the graph database. Rather than
teach every command about every other command's files, the scratch directory
itself is refused when it directly holds a symlink: orbit-graph never creates
one there, so any link is content standing in for state. The check is one
`read_dir` of a directory with a handful of entries. Deeper levels are covered
by the per-component walk and by `O_NOFOLLOW` opens.

## D3. SQLite opens the physical path with `SQLITE_OPEN_NOFOLLOW`

`SQLITE_OPEN_NOFOLLOW` refuses a symlink in any component of the path it is
given, so it is only safe on a physical path. `store::open_at_path` resolves
the verified directory to its physical path, carries it as
`OpenedGraph::physical_db`, and opens the writer and the read connection on it
through `store::open_writer`. The spelled `GraphDbPath` stays what commands
report, so JSON output, test hooks and sync coalescing keyed by path are
unchanged; on macOS, where temporary directories live under the `/var ->
/private/var` link, the spelled and physical paths differ and both keep
working.

## D4. Readers validate; writers repair (deviation from `STD-05@1 §R9`)

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

## D5. Every durable replacement is `atomic_write`

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

## D6. The snapshot cache trusts an entry only by its provenance

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

## D7. A refused snapshot cache falls back instead of failing

`Comparison::open*` already treats an unusable cache directory as a reason to
index both sides into task-owned temporary trees, reporting it in
`cache_note`. A refused cache path does the same: the comparison succeeds,
`cache_outcome` is `disabled`, `cache_dir()` is `None`, and the note carries
the `refusing orbit-graph state path <path>: <reason>` text. A per-entry
refusal (a symlinked lock inside an entry) falls back for that side only.
Failing the comparison would make an inspectable repository uninspectable
because of a directory it does not need.

## D8. The snapshot tree's `.git` anchor keeps libgit2's modes

Every materialized directory and blob of a snapshot tree is `0700`/`0600`.
The empty repository `anchor_git_discovery` initialises at the tree root is
created by libgit2, which applies the process umask to `.git/`. It sits inside
the `0700` entry and tree directories, so no other user can reach it; the
umask test skips `.git` for that reason.

## D9. Test allowances for the `fs::write` ban

Test fixtures write hundreds of files with `fs::write`. Library and binary
crates allow `clippy::disallowed_methods` under `cfg(test)`, and each
integration-test crate that writes fixtures allows it at its root with a
`reason`. The allowance also covers the `std::sync::mpsc::channel` ban in test
code; no test uses that constructor today.

## D10. Umask tests run in a child

`STD-04@1 §R6` forbids changing process-wide state in the shared test process.
The CLI tests run the binary through `sh -c 'umask 0002; exec "$@"'`. The
snapshot-cache umask test re-executes its own test binary through the same
shell with a marker variable, and the child builds the cache and checks the
modes.
