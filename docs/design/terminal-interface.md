# orbit-graph terminal interface

This document records the terminal behavior of the standalone `orbit-graph`
binary. It adapts Orbit's current terminal-interface contract without importing
Orbit control-plane configuration, runtime state, or rendering crates.

## Output sink and modes

`src/cli/output.rs` owns the one sink resolved for an invocation. It reads
whether stdout is a terminal, terminal width, color controls, and output mode
once. Command code does not inspect those process properties or write its
records directly.

The supported modes are `auto`, `table`, `json`, and `ndjson`. Resolution uses
the first applicable source:

1. an explicit `--format <MODE>`;
2. the standalone `ORBIT_GRAPH_FORMAT` environment variable;
3. `auto`.

An invalid environment value is ignored and resolves as `auto`; an invalid
command-line value is a usage error. `auto` becomes `table` when stdout is a
TTY and plain output otherwise. Plain is the redirected form of a human table:
it has no header, ANSI, borders, or truncation and separates fields with one
tab. `--format table` requests the headed human view even when redirected.

The sink has width zero when stdout is not a TTY, regardless of `COLUMNS`.
Zero means no truncation. On a TTY, a positive `COLUMNS` value precedes the
terminal query; an absent or invalid result also becomes zero rather than a
guessed width. Color is never allowed off a TTY. On a TTY, `NO_COLOR` with a
non-empty value and `TERM=dumb` disable it; `CLICOLOR_FORCE` cannot override
`TERM=dumb`. No current view emits color, so these are policy inputs for later
view migrations rather than a promise of colored output.

### The overview `--format` spelling

`overview --format summary|full` predates output modes and retains its exact
detail-selection meaning. Choose an output mode at the root when invoking that
command:

```text
orbit-graph --format json overview --format full
```

For commands without a local collision, the shared output option is accepted
at the root or after the command. This placement rule makes the two meanings
unambiguous and preserves existing overview invocations.

## Machine contracts

`--format json` emits exactly one JSON document followed by a newline. Its
success document is the same `serde_json::Value` commands returned before
output modes were introduced; field names, nesting, and value types are not
changed by rendering. JSON is compact when redirected and pretty on a TTY.

`--format ndjson` emits one complete compact JSON value per line and flushes
after each record. A command that supplies record units through
`CommandOutput::with_ndjson_records` emits those values in order. Until a
command declares such units, its entire existing success document is one
record. This default avoids guessing that an arbitrary nested array is the
command's record stream.

The no-argument plugin protocol is separate from terminal rendering.
`ORBIT_TOOL_NAME` recognition still precedes CLI parsing; recognized plugins
read their JSON request from stdin and emit the same compact JSON response or
error envelope as before. Output flags and `ORBIT_GRAPH_FORMAT` do not affect
that protocol.

## Human views and migration seam

A command hands the renderer a `CommandOutput`: its stable JSON `document`, a
`View`, and optionally explicit NDJSON records. Human views are sequences of
`ViewBlock::Text` and `ViewBlock::Table`. `TableView` owns ordered `Column`
metadata and rows; the shared renderer applies headers, alignment, width,
truncation, the tab-separated plain form, empty-state stderr routing, and all
writes. A command migration should therefore:

1. build the existing JSON document exactly as it does today;
2. build a `View::Blocks` from the same collected records;
3. return `CommandOutput::with_view(document, view)`;
4. add `with_ndjson_records(records)` only after identifying the command's
   stable record unit.

`View::Document` is the explicit migration boundary. Commands not yet assigned
a dedicated human view use it and render their complete JSON document in
human modes. This keeps their information and schema intact while the two
dependent command-group migrations add purposeful text and tables. New or
migrated commands should not use `View::Document` merely to avoid defining a
human view.

## Help, diagnostics, streams, and exits

Top-level help is a human-readable Clap template in `src/cli/mod.rs`. It uses
named, borderless purpose sections. `orbit-graph`, `orbit-graph --help`, and
`orbit-graph help` print that text to stdout and exit 0. Nested help does the
same. Redirected help is complete and contains no ANSI styling.

Successful payloads go to stdout. Diagnostics and errors go to stderr. Default
usage errors are Clap's readable human text and usage, including bare command
namespaces such as `history`; they exit 2. `refs` and `trace` without their
required argument follow the same rule. In explicit JSON or NDJSON mode,
usage and command errors instead use the existing object with nested
`error.code` and `error.message`, plus `details` when available. Usage still
exits 2, command failures exit 1, and stdout stays empty on failure.

A closed stdout pipe is a successful, silent stop. Both direct I/O errors and
broken pipes reported through JSON serialization are recognized at the process
boundary. A closed stderr is not reclassified as a successful command.
