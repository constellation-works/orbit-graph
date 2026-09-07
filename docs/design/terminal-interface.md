# orbit-graph terminal interface

This document records the terminal behavior of the standalone `orbit-graph`
binary. It adapts Orbit's current terminal-interface principles to a small
JSON CLI; it does not import Orbit runtime, configuration, or rendering
dependencies.

## Help

Top-level help is a human-readable Clap template in
[`src/cli/mod.rs`](../../src/cli/mod.rs). It uses named, borderless purpose
sections:

- Explore code
- Follow relationships
- Recommendations and history
- Index and utilities

The command enum remains flat, so command names and argument parsing do not
change. Each command's one-line description is Clap metadata on its registered
variant. The smoke test invokes the built executable and checks that every
registered top-level command appears in the grouped template with its
description. When adding a command, place its row in the matching help section,
add its Clap description, and extend the executable coverage if needed.

The template keeps `Usage`, purpose sections, `Options`, a per-command help
hint, and a few valid starting examples in a stable order. `orbit-graph`,
`orbit-graph --help`, and `orbit-graph help` are human help paths; a command's
own `--help` remains Clap-generated and includes its arguments and options.

## Streams and protocols

Successful data commands emit exactly one JSON value followed by a newline on
stdout. Failures exit nonzero and emit a JSON error object on stderr. Help is
the intentional human-text exception and is emitted on stdout.

The `ORBIT_TOOL_NAME` check runs before normal argument parsing. Recognized
external tools read one JSON request from stdin and write one JSON response to
stdout with no command-line arguments. Their schemas and output modes are
unchanged by the help design.

## Styling and redirection

Help uses plain text, aligned command/description rows, and no decorative
borders or glyphs. Help redirected to a file or pipe is complete text without
ANSI escape sequences. `NO_COLOR` and `TERM=dumb` likewise produce readable
plain help. Color is not used to carry meaning in the current help surface.

These rules apply to help only. JSON data output remains the stable machine
interface and does not gain a human table renderer as part of this change.

## Future scope

Future work may add a separate human table presentation for selected data
commands, or explicit `auto`, `table`, `json`, and `ndjson` modes. Those are
proposals, not implemented behavior, and would require a separate compatibility
and output-contract design. This document makes no claim that those modes
exist today.
