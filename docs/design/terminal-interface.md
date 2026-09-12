# orbit-graph terminal interface

This document records the terminal behavior of the standalone `orbit-graph`
binary. It adapts Orbit's current terminal-interface contract without importing
Orbit control-plane configuration, runtime state, or rendering crates.

## Output sink and modes

`crates/orbit-graph-cli/src/output/sink.rs` owns the one sink resolved for an
invocation. It reads
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

The migrated recommendation, history, evaluation, and index commands use the
following views:

- `recommend` renders one row per destination with rank, selector, score,
  concise score evidence and fallbacks, and history freshness. An empty result
  explains its freshness and fallbacks on stderr.
- `history import`, `history sync`, `history status`, and `history rebuild`
  render field/value summaries. Status includes the cursor and bootstrap
  coverage state plus verified, Git-only, and task-association counts.
- `evaluate` renders corpus coverage, one row per variant/level metric, and one
  admission row per case so exclusions remain visible.
- `sync`, `clean`, `db-path`, and `version` render their counts, locations, and
  versions through the same borderless table path. `clean` writes its empty
  diagnostic to stderr.

The exploration and relationship commands use these views:

- `overview` renders aggregate counts, language and symbol-kind counts, files,
  and (for `--format full`) a file-contextual symbol list.
- `search` renders one row per match with kind, complete match text, path, and
  one-based source line. `show` renders resolved metadata followed by source;
  non-UTF-8 source directs the reader to the byte-preserving JSON view.
- `refs` combines textual references, structural relations, and explicitly
  labelled fallback references without dropping any of the three sets.
  `callees`, `implementors`, and `deps` render one row per returned edge or
  implementation.
- `trace` flattens every node in preorder with its depth and complete ancestor
  traversal. `impact` labels primary and fallback traversal sets and preserves
  their breadth-first order.

Empty list results write a command-specific diagnostic to stderr and nothing to
stdout. Full values for every potentially truncated field are available from
the same invocation with `--format json`; source paths and symbol selectors can
also be followed with `show file:PATH` or `show symbol:PATH#NAME:KIND`.

## Shared table width and record safety

Table widths use terminal display columns and extended grapheme boundaries, so
wide characters, emoji, and combining sequences are padded and truncated
without splitting a visible character. Numeric, status, kind, confidence, and
other fixed columns do not shrink. Flexible prose shrinks at the tail and paths
shrink through the middle with `…`; if all flexible columns reach eight display
columns and the row still does not fit, flexible columns are omitted from the
right and stderr names them. A terminal narrower than the remaining fixed
columns receives a separate diagnostic; fixed values are still emitted in
full.

Table and plain cells are always one physical line. The renderer escapes `\\`
as `\\\\`, tab as `\\t`, newline as `\\n`, carriage return as `\\r`, byte-sized
controls as `\\xNN`, and other Unicode controls as `\\u{NNNN}`. This makes the
tab separator in plain output unambiguous and reversible while leaving JSON and
NDJSON values untouched.

### Declared NDJSON records

Commands without a declared stream continue to emit their complete JSON
document as one NDJSON record. In particular, each `history` operation,
`sync`, `db-path`, and `version` is one detail record.

The commands with natural repeated records declare these boundaries:

- `recommend`: one `recommendation_context` record containing every top-level
  field except `recommendations`, followed by one full `recommendation` record
  per ranked destination.
- `evaluate`: one `evaluation_context` record containing every top-level field
  except `metrics` and `cases`, followed by the full `evaluation_metric`
  records and then the full `evaluation_case` records, in document order.
- `clean`: one `clean_context` record containing `graph_dir`, followed by one
  `deleted_database` record per deleted path.
- `overview`: one `overview_context` record containing every top-level field
  except `files`, followed by one `overview_file` record per complete file
  object (including its nested symbols).
- `search`: one unchanged match object per result. An empty result emits no
  records.
- `show`: one unchanged detail document, including `null` for an unresolved
  selector.
- `refs`: one `refs_context`, then the unchanged textual-reference and
  structural-relation records; an optional `refs_fallback_context` precedes the
  unchanged fallback-reference records.
- `callees`: one unchanged callee edge per record.
- `implementors`: one `implementors_context` carrying `trait_name`, followed by
  one complete `implementor` record per implementation.
- `deps`: one `deps_context` carrying `scope`, followed by one complete
  `import` record per source import.
- `trace`: one `trace_context` carrying traversal counts and truncation,
  followed, when present, by one complete `trace_root` record containing the
  lossless nested tree.
- `impact`: one `impact_context`, then the primary impact records; an optional
  `impact_fallback_context` precedes the fallback impact records.

The wrapper's `record_type` identifies how to reconstruct the original list;
the nested `context`, `recommendation`, `metric`, and `case` values retain the
existing field names and types. A zero-item result still emits its context
record, so provenance and coverage are never lost.

## Help, diagnostics, streams, and exits

Top-level help is a human-readable Clap template in
`crates/orbit-graph-cli/src/command/mod.rs`. It uses
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

## Executable compatibility matrix

`crates/orbit-graph-cli/tests/cli_smoke.rs` runs the built `orbit-graph`
executable against disposable Git fixtures. The command-inventory assertion in
`crates/orbit-graph-cli/src/tests/inventory.rs` keeps this table synchronized
with Clap registration — the binary has no library target, so that assertion
lives in the crate; adding a command requires both an inventory entry and
real-workflow coverage. The plugin boundary is exercised separately by
`crates/orbit-graph-cli/tests/plugin_integration.rs` with `ORBIT_TOOL_NAME` and
JSON stdin.

| Registered path | Executable behavior covered |
| --- | --- |
| `sync` | Full indexing; plain summary, JSON document, and one NDJSON detail record. |
| `history` | Namespace help and the missing-subcommand usage error. |
| `history import` | Valid import plus malformed, repository-mismatched, and invalid-time failures. |
| `history sync` | Git-only first-parent synchronization, human summary, JSON, and NDJSON. |
| `history status` | Cursor/evidence counts in human, JSON, and NDJSON output. |
| `history rebuild` | Atomic rebuild summary in human, JSON, and NDJSON output. |
| `recommend` | File and symbol results, invalid inputs, empty state, table, JSON, and record-stream output. |
| `evaluate` | Isolated chronological evaluation, human summary, JSON, and metric/case NDJSON records. |
| `search` | Match, empty state, plain, table, JSON, and per-match NDJSON output. |
| `show` | Resolved source/detail output, malformed-selector failure, JSON, and one detail record. |
| `refs` | Filtered references, missing-argument failure, table, JSON, and context/reference NDJSON output. |
| `callees` | Returned calls and empty-state behavior in all three output formats. |
| `impact` | Bounded traversal in table, JSON, and context/impact NDJSON output. |
| `trace` | Discovered command traversal, missing-argument failure, and lossless root record. |
| `overview` | Summary/full views and the root output-format versus local detail-format compatibility rule. |
| `implementors` | Implementations and empty-state behavior in table, JSON, and NDJSON output. |
| `deps` | Import edges in table, JSON, and context/import NDJSON output. |
| `db-path` | Human, JSON, and one NDJSON detail record. |
| `clean` | Deleted-database records, human summary, and empty diagnostic routing. |
| `version` | Environment/explicit mode precedence, color controls, JSON, and one NDJSON detail record. |

The same suite also checks top-level and nested help, no-argument help,
unknown-command and usage exits, TTY width adaptation, redirected plain output,
color suppression, and a closed stdout pipe. These are representative fixture
workflows rather than assertions against private live state.
