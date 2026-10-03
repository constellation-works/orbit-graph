# Terminal interface: decisions

Decisions for ORB-13164, which brings the CLI output contract in line with
STD-01 (`§R7`, `§R11`, `§R12`, `§R17`, `§R19`, `§R20`, `§R28`, `§R29`, `§R32`,
`§R34`, `§R35`). The user-facing rules are under "Output contract" in
[`docs/usage.md`](../../usage.md); the design is
[`terminal-interface.md`](../terminal-interface.md). Every point is pinned by
`crates/orbit-graph-cli/tests/output_contract.rs` against the built binary.

## D1. `ORBIT_TOOL_NAME` is the only signal for the plugin protocol

The binary used to read stdin whenever it had no arguments and stdin was not a
terminal, then sniff the bytes for a request envelope. A bare `orbit-graph`
under a pipe that stays open therefore hung instead of printing help
(`§R28`). Stdin is now read only when `ORBIT_TOOL_NAME` is set. Orbit's exec
and external backends set it for every tool call, so the plugin manifests and
the host need no change; an envelope sent without it now gets help, as any
bare invocation does. `ORBIT_TOOL_NAME` together with arguments is a usage
error (exit 2, nothing on stdout) naming both inputs, rather than silently
ignoring one of them.

## D2. A capped search reports `total: null`

`search` queries `LIMIT n+1` to learn whether more matches exist without
counting them all, so a truncated search knows only "more than n": `total` is
`null` and the stderr notice says `showing n of more than n search matches`.
An uncapped result reports the exact `total`. `recommend` ranks every
candidate before cutting, so its `total` is always the ranked count. `impact`
and `trace` stop at a node cap, so they report `total: null` when truncated and
the visited count otherwise. The plugin `search` tool, which already reported
`truncation.matches`, now takes the library's own truncation instead of
querying one extra row itself.

## D3. Machine errors drop `details`

The flat `§R19` object is exactly `{"error", "code"}`. The former nested
object's optional `details` repeated the underlying reason, which the
`Display` message already includes, so it was dropped rather than added as a
third key. The plugin's `{"ok": false, "error": {code, message, retryable}}`
envelope on stdout is unchanged. The error taxonomy itself (which failures get
which code) is ORB-13167; this task adds only `argument_error` for usage
failures detected after parsing and `not_found` for an unresolved `show`.

## D4. Logging and streams live in the output layer

The tracing subscriber moved from `main.rs` to `output/log.rs`, with the
default filter raised from off to `warn` so byte-cap and similar warnings reach
stderr (`§R12`). With the stdin probe gone (D1) and every write routed through
`output/`, `main.rs` has no stream access left: the terminal guard's allow list
is empty, and its self-test seeds an entry into a scratch copy to keep the
stale-entry and hit-count checks proven.

## D5. Input structs keep `skip_serializing_if`

"No output struct uses `skip_serializing_if`" covers the structs a command
prints. Request and storage records that are only serialized to be read back
(`RecommendationRequest`, `EvaluationCase`, recommend's on-disk
`CachedSymbol`, the history provenance records in
`orbit-graph-extract`, and the change-explorer cache in `orbit-graph-changes`,
which ORB-13168 owns) keep their attributes: removing them would change
persisted and hashed forms without changing any command's output.

## D6. Preserve the graph command grammar

`orbit-graph` operates on one graph resource, so its established top-level
commands omit the noun level under `STD-01@2 §R1`. The history maintenance
namespace keeps `history import|sync|status|rebuild`; this existing subgroup is
a compatibility deviation from a uniform dropped-noun level (`§R1`). We keep
these shipped names under `STD-01@2 §R35`, without introducing a second
verb/noun order or renaming query commands. Root help groups navigation by
intent, but reads every description from the composed clap parser (`§R25`);
its grammar assertion runs after output arguments are installed (`§R26`).

## D7. Plain details and heterogeneous rows

Under `STD-01@2 §R6`, `§R9` and `§R16`, redirected `show` projects only its
UTF-8 source slice, while table mode retains metadata and source. Empty source
stays empty and terminal controls stay escaped. Non-UTF-8 source is available
through the existing JSON byte payload and is diagnosed rather than displayed
as an empty valid slice. Multi-table commands prefix plain rows with the
record types documented in usage, including optional sync failure/skip tables
and both evaluation modes. Empty blocks add no plain blank lines. This
deliberately changes the prior plain display; the named-field JSON, NDJSON
and plugin contracts remain intact (`§R10`, `§R35`).

## D8. Golden checks capture the shipped surface

The existing help/version fixtures remain under `fixtures/help`; the extended
real-executable matrix under `fixtures/output` pins exit codes and both streams
for representative graph queries, index reports, changes and errors
(`STD-01@2 §R24`). `make goldens` checks without writing unless
`UPDATE_GOLDENS=1` is explicit. Fixed commit identities/times keep revision
evidence stable; only fixture roots and specified clock values are normalized.
The nextest workspace gate already runs these checks, so `make ci` needs no
second test pass. Help-description and JSON-field mutation failures are
recorded as gate evidence (`STD-04@1 §R2`, `§R10`).
