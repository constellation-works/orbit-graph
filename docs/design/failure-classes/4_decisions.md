# Failure classes: decisions

Decisions for ORB-13167. Failures now keep a typed class all the way from where
they happen to the CLI's `code` and the plugin envelope's `code` and `retryable`
(`STD-02 §R10`–`§R12`, `§R30`). The plugin tool table has one definition
(`§R24`). The library reads no process input (`§R3`). `orbit_sync` isolates bad
items (`§R32`). The Orbit adapter supervises its children as process groups
(`STD-03 §R11`–`§R15`). The user-facing codes are listed in
[`docs/plugin.md`](../../plugin.md).

## D1. An exhaustive class beside a non-exhaustive error

`GraphError` is public, so it is `#[non_exhaustive]` (`STD-02@2 §R11`). As a
result the CLI crate could only match it with a wildcard, and `§R27` forbids
that for an in-workspace enum. So `GraphError::class()` returns
`GraphErrorClass`, a plain enum with one variant per class. It is not
`#[non_exhaustive]`, and `Io` carries whether the failure is transient. The CLI
has one translator, `report_graph_error` in
`crates/orbit-graph-cli/src/command/mod.rs`. It matches that class exhaustively
and returns the CLI code, the plugin code and `retryable` together. Adding a
class therefore breaks the build at the translator, not at a silent default.
`ToolError::graph`, `CliError::code` and `CliError::retryable` all go through
it.

`retryable` is true only for `Timeout` and for I/O errors of kind
`Interrupted`, `WouldBlock` or `TimedOut`. A transient I/O failure keeps the
existing `graph_error` code, so existing codes are unchanged and only new ones
are added.

## D2. Process settings are installed once, not threaded through every call

**Deviation, `STD-02@2 §R3`.** The library needs four settings: the lock wait
(`ORBIT_GRAPH_LOCK_TIMEOUT_MS`), the program name written into lock holder
records, the temporary directory for evaluation checkouts, and the test-only
sync fault point (`ORBIT_GRAPH_FAULT_INJECT`). It used to read these from the
environment and argv at the point of use. Now `main` resolves and validates
them once and installs them with `orbit_graph::install_runtime`, which sets a
process-wide `OnceLock<RuntimeConfig>`. They are not passed down as arguments
through every `Graph`, `HistoryIndex` and sync entry point. Doing that would
change most of the public API to carry values that are constant for the life of
the process. The part `§R3` cares about still holds: the library reads no
environment variable or argv itself (`tests/process_input.rs` enforces this),
and a malformed value is refused at start-up, naming its variable (`§R28`).

An embedder that installs nothing gets `RuntimeConfig::default()`. That
default's `temp_dir: None` leaves the choice to the `tempfile` crate, which
consults `TMPDIR`. The CLI always installs an explicit directory.

## D3. The plugin environment is resolved before any tool runs

`ORBIT_PLUGIN_STATE` and `GRAPH_ORBIT_TIMEOUT_SECONDS` are resolved once into
`PluginEnvironment` and handed to every tool through `ToolCall`. Query tools,
`changes` and the adapter no longer read them. An empty state root, or a timeout
outside 1–60 seconds, is refused as `invalid_request` for every tool, including
`version`. Before this change, a bad timeout failed only the call that reached
an Orbit callback, and an empty state root was quietly treated as unset.

## D4. `orbit_sync`: verdicts are `excluded`, infrastructure is `failed`

A run that was examined and judged ineligible is `excluded`, with the check it
failed as its `reason`. Examples: not successful, no commit output, not a
strict descendant, not reachable from the landing branch, or prepared in
another repository. A task or run that could not be examined at all is
`failed`, with a structured `error` (`code`, `message`, `retryable` and, for an
Orbit refusal, `orbit.{tool,code}`). Examples: Orbit refused the read, it timed
out, or Git could not be opened. The batch continues after either outcome, and
`coverage` counts them separately.

An authority failure is the request's own input: the named workspace is not
registered or not active, it publishes no `git_remote`, or the repository's
origin does not match that remote. It is `InvalidInput` and makes each run
`excluded`. Orbit answered, and the answer is that the run does not belong to
that authority. For `recommend` the same failure is now `invalid_request`
instead of `graph_error`.

Under the plugin, `orbit.workflow.run.show` still needs Orbit's `operator`
capability, so each run read is refused. That refusal is now `failed` with
`orbit_refused` rather than `excluded`: it is a denied read, not a verdict on
the run.

## D5. Group supervision observes exit without reaping

Each `orbit` child is spawned with `process_group(0)`. Exit is observed with
`waitid(P_PID, …, WEXITED | WNOHANG | WNOWAIT)`, which leaves the leader
waitable. The leader is reaped only after the last `killpg` to its group, so the
group ID can never refer to a reused PID (`STD-03@2 §R14`).

While the leader is an unreaped zombie, a `killpg(pgid, 0)` probe always finds
the group occupied, so the probe cannot show that the group has emptied. On a
timeout the supervisor therefore sends SIGTERM to the group, waits up to 5 s for
the leader to exit, and then sends SIGKILL to the group unconditionally before
reaping (`§R12`: the kill is sent whether or not anything survived, never
skipped on the strength of the leader's exit). A clean exit gets the same
unconditional SIGKILL sweep before the leader is reaped and the pipe readers
are joined (`§R13`). Reader joins wait no longer than the call's deadline.
