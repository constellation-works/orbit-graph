# Architecture

The layer model that `scripts/check-dependency-direction.sh` enforces
(STD-02 §R1, §R6). A lower layer never imports a higher one. Adding a crate or
an internal edge changes this file and that script in the same commit; the
script fails when the crate table below and its policy disagree.

## Layers

| Tier | Unit | Owns | May depend on |
|------|------|------|---------------|
| 1. Extraction | `orbit-graph-extract` (library) | Extraction contracts (`ExtractedFile`, the raw rows, `Selector`), the `Extractor` trait and every tree-sitter language extractor (`languages`), and the Git-tree change extraction behind the history index (`history`). A leaf: it knows nothing of the store, sync or queries, and prints nothing. | nothing internal |
| 2. Domain | `orbit-graph` (library) | The SQLite store and its schema (`store`), sync and the file watcher (`sync`), queries (`query`), recommendations (`recommend`), evaluation (`evaluation`), and the Orbit plugin tool contract (`plugin`), built on tier 1; it re-exports the extraction types its public API names. Embeddable; it prints nothing (`#![deny(clippy::print_stdout, clippy::print_stderr)]`). | tier 1 |
| 3. Explorer domain | `orbit-graph-explorer` (library) | Snapshots of two revisions, the snapshot cache, changed-symbol pairing, evidence paths, filters, the exported report and the loopback HTTP service, built on the public `orbit-graph` API only (`docs/design/change-explorer.md` D1). | `orbit-graph` |
| 4. Surfaces | `orbit-graph-cli` (binary `orbit-graph`); `orbit-graph-explorer`'s `src/main.rs` (binary `orbit-graph-explorer`) | Argument parsing, one library call per command, rendering and the process exit code. These are the output layers: the CLI's `src/output/` and the explorer's `src/main.rs` are the only code that writes to stdout or stderr or checks for a TTY (`scripts/check-terminal-guard.sh`, which lists the temporary exceptions). | tier 2; the explorer binary also its own library (tier 3) |

The CLI and the explorer are siblings: neither depends on the other, and
neither depends on `orbit-graph-extract` directly; they reach extraction types
through `orbit-graph`'s re-exports. `orbit-graph-extract` is its own crate for
compile-graph isolation and an enforceable edge (STD-02 §R7): it owns every
tree-sitter grammar, so a change to the store, sync or queries does not
recompile extraction, and the direction check keeps the leaf free of them.

## Crates

| Crate | Kind | Internal dependencies | Banned |
|-------|------|-----------------------|--------|
| `orbit-graph-extract` | library | — | `clap`, `tracing-subscriber`, `tiny_http`, `unicode-width` |
| `orbit-graph` | library | `orbit-graph-extract` | `clap`, `tracing-subscriber`, `tiny_http`, `unicode-width` |
| `orbit-graph-cli` | binary `orbit-graph` | `orbit-graph` | — |
| `orbit-graph-explorer` | library and binary `orbit-graph-explorer` | `orbit-graph` | — |

"Banned" lists external crates the libraries must not depend on: argument
parsing, log subscribers, HTTP serving and terminal layout belong to surfaces
(STD-02 §R7). Dev-dependencies are exempt. A third-party dependency that more
than one member uses is declared once in `[workspace.dependencies]`
(STD-02 §R9); the script checks that too.

## Repository gates

Each runs in `make ci` and in CI (`.github/workflows/ci.yml`):

| Gate | Rule | Fails on |
|------|------|----------|
| `scripts/check-dependency-direction.sh` | STD-02 §R1–§R7, §R9 | an unlisted crate, an unlisted internal edge, a banned crate in its crate, a crate table here that disagrees with the script, a third-party dependency used by two or more members that any of them declares locally; and, failing closed, any dependency form it does not read (a `[dependencies.<name>]` or otherwise unrecognized dependency header, a dotted or quoted key, a `package =` rename; whitespace inside brackets and around dots is normalized first) |
| `scripts/check-terminal-guard.sh` | STD-02 §R15 | stdout/stderr, `print!`-family macros or TTY checks outside an output layer, except allow-listed write sites (each a file, a line pattern and an exact hit count, with a reason and a removing task); an entry whose count no longer matches, including a site that is gone; grep itself failing |
| `scripts/check-orphan-modules.sh` | STD-02 §R19 | a `src/**/tests/*.rs` file its `tests/mod.rs` does not declare, or a `tests/` directory its parent module does not declare |
| `cargo deny --locked check` (`deny.toml`) | STD-02 §R23, STD-05 §R23/§R24 | an open advisory, a yanked crate, a license outside the allow-list, a registry other than crates.io, a git source |
| `scripts/test-repo-gates.sh` | STD-04 §R10 | any of the three scripts above passing on a seeded violation or on an empty tree |
| `crates/orbit-graph-explorer/tests/derived_artifacts.rs` | STD-04 §R11, §R13 | the explorer README's "Every flag" block differing from `orbit-graph-explorer --help`, or the committed `direct-call` sample export differing from a fresh `report` (regenerate both with `UPDATE_GOLDENS=1`) |
| `docs/usage.md` doctest (`cargo test --doc`) | STD-04 §R14 | the library example in `docs/usage.md` no longer compiling |
