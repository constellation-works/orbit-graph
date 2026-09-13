# orbit-graph-explorer

`orbit-graph-explorer` explains one Git change — a base revision and a head
revision — through source relationships: what changed, which callers and
entry points may be affected, and which tests have a defensible connection.
It is built strictly on the public `orbit_graph` library API described in
[`crates/orbit-graph/src/lib.rs`](../orbit-graph/src/lib.rs) and reads no
Orbit control-plane state. See
[the design document](../../docs/design/change-explorer.md) for the full
evidence contract, snapshot semantics, and service/API reference, and
[the evaluation](../../docs/evaluation/change-explorer/README.md) for what the
tool gets right, gets wrong, and does not know across five real changes.

## Prerequisites

- Rust 1.89 or newer (CI pins exactly `1.89.0`; see
  [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml) and
  [`CONTRIBUTING.md`](../../CONTRIBUTING.md)). The repository carries no
  `rust-toolchain` file, so `rustup` will use whatever toolchain is already
  active; install one at least that new if `rustc --version` reports older.
- Git, on `PATH`. The explorer drives it through `git2` and argument-vector
  process invocation, never through a shell string.
- A clone of a Git repository to inspect. It does not need to be this
  repository — point `--repo` (or the working directory) at any Git worktree.

## Install

From a checkout of this repository:

```sh
cargo install --path crates/orbit-graph-explorer --locked
cargo install --path crates/orbit-graph-cli --locked
```

The first command installs the `orbit-graph-explorer` binary this document
describes. The second installs the `orbit-graph` CLI
([root README](../../README.md)), which is not required to run the explorer
but is the fastest way to query the same index directly (`orbit-graph search`,
`orbit-graph refs`, and so on) while reading its output.

For development, build without installing:

```sh
cargo build --workspace --locked
```

which places the binary at `target/debug/orbit-graph-explorer`.

## First launch

```sh
orbit-graph-explorer serve --repo /path/to/your/repo --base main --head HEAD
```

`--repo` defaults to the current directory, so running the command from
inside the repository you want to explore only needs `--base` and `--head`.
Both are resolved once, at launch, to immutable commit SHAs — the comparison
never moves even if the named refs do.

On success the service prints a banner to standard error and then blocks,
serving requests until the process is stopped (`Ctrl-C`):

```text
orbit-graph-explorer listening on http://127.0.0.1:<port>
Open: http://127.0.0.1:<port>/#token=<64-hex-character token>
Authorization: Bearer <64-hex-character token>
Loopback only; the token is per-launch and is not written to disk.
```

- **The URL** opens the embedded UI in a browser. It binds `127.0.0.1` only —
  never a network-reachable address — and, unless `--port` is given, an
  ephemeral port chosen by the operating system.
- **The token** is 32 bytes of operating-system randomness, hex-encoded,
  generated fresh for this one launch. Every `/api/*` request must carry it as
  `Authorization: Bearer <token>`; the three embedded UI assets (`/`,
  `/ui/app.css`, `/ui/app.js`) are the one exception, because the browser
  cannot attach a custom header to a plain navigation. The UI instead reads
  the token from the URL fragment (`#token=…`), which a browser never sends
  back to the server. The token lives only for this process: it is never
  written to disk, and stopping the service invalidates it — the next launch
  prints a new one. A request whose `Host`, `Origin`, or `Referer` does not
  match the service's own loopback origin is refused before any graph work,
  independent of the token.
- Every request must also name the launch-scoped repository; a request for
  any other repository is refused as out of scope. The scope is fixed at
  launch and never inferred from a request.

Stopping the process (`Ctrl-C`, or any signal that kills it) ends the
session; both revisions' temporary snapshot trees and their indexes are
removed with it, unless caching is in effect (below).

## The `snapshot` and `report` subcommands

`snapshot` prints a human-readable diagnostic of the two resolved snapshots —
files indexed, cache outcome, and, with `--selector`, one selector's refs and
impact in both revisions. It is explicitly not a machine contract; script
against `serve`'s JSON routes or `report`'s JSON export instead.

```sh
orbit-graph-explorer snapshot --repo /path/to/your/repo --base main --head HEAD \
  --selector 'symbol:src/lib.rs#entry:function'
```

`report` writes the same evidence `serve` would answer over HTTP as two
static files: `<name>.json` (the machine-contract export) and `<name>.html`
(a self-contained rendering — readable from disk, no scripts, no service).
It refuses to overwrite either file unless `--force` is given, and it never
embeds the whole repository: only the changed symbols in scope are queried,
and by default every excerpt is `controlled` — a bounded window around each
cited line, not the whole file.

```sh
orbit-graph-explorer report --repo /path/to/your/repo --base main --head HEAD \
  --out /tmp/change-report --name my-change --generated-at 2026-09-13T00:00:00Z
```

`--generated-at` pins the export's `generated_at` field so repeated runs over
the same inputs produce byte-identical output — useful for committing a
sample export, or for diffing two runs. See
[the sample exports](../../docs/evaluation/change-explorer/samples/) for a
worked example, and
[the demo walkthrough](../../docs/evaluation/change-explorer/demo.md) for the
full seven-step workflow this binary supports end to end.

## Cache directory and `clean`

Unless `--no-cache` is given, `serve`, `snapshot`, and `report` cache each
revision's materialized snapshot tree and index under
`<repo>/.orbit-graph/explorer/snapshots` (override with `--cache-dir`), keyed
by commit SHA, extractor version, and store schema version. A cache entry
whose key does not match the running binary is rebuilt, never reused, so
upgrading `orbit-graph-explorer` cannot silently serve a stale index. This
directory is scratch state: like the root crate's own `.orbit-graph/`, it
should not be committed (see the
[root README's index-lifecycle section](../../README.md#index-lifecycle-and-location)).

```sh
orbit-graph-explorer clean --repo /path/to/your/repo
```

`clean` removes cache entries whose key this binary can no longer use and
entries for commits the repository no longer has (for example, after a
rebase or a force-push drops a commit this repository once compared).
Anything else in the cache directory — a staging directory an interrupted
build left behind is the one exception, also removed — is reported and left
alone. `clean` touches nothing outside the cache directory; it does not
affect the root crate's own `.orbit-graph/` index for this repository.

## Every flag

Generated from the real binary's own `--help` (`orbit-graph-explorer --help`);
regenerate this section if the binary's usage text changes.

```text
orbit-graph-explorer: explain one Git change through source relationships

Usage:
  orbit-graph-explorer serve --base <REF> --head <REF> [--repo <PATH>] [--port <PORT>]
  orbit-graph-explorer snapshot --base <REF> --head <REF> [--repo <PATH>] [--selector <SELECTOR>]
  orbit-graph-explorer report --base <REF> --head <REF> --out <DIR> --name <NAME>
                               [--repo <PATH>] [--force] [--selector <SELECTOR>]...
                               [--excerpts none|controlled|full-span] [--generated-at <RFC3339>]
                               [--include-absolute-paths] [--depth <N>] [--confidence <LEVEL>]
                               [--node-cap <N>] [--time-budget-ms <MS>] [--language <LANG>]
                               [--change-kind <KIND,...>] [--scope <PREFIX>]
  orbit-graph-explorer clean [--repo <PATH>] [--cache-dir <PATH>]

Options:
  --repo <PATH>            Repository to inspect (default: current directory).
  --base <REF>             Base revision; resolved to an immutable commit SHA.
  --head <REF>             Head revision; resolved to an immutable commit SHA.
  --port <PORT>            Port for `serve` (default: an ephemeral port).
  --selector <SELECTOR>    For `snapshot`, a symbol selector queried in both
                           snapshots. For `report`, may be repeated to select
                           specific changed symbols (default: every changed
                           symbol).
  --out <DIR>              Directory `report` writes `<name>.json` and
                           `<name>.html` into. Created if missing.
  --name <NAME>            Base file name for `report`'s two output files.
  --force                  Let `report` overwrite existing output files.
  --excerpts <MODE>        `report` excerpt policy: `none` (references only),
                           `controlled` (default; a bounded window around each
                           cited line), or `full-span` (the whole bounded file).
  --generated-at <RFC3339> Pin `report`'s `generated_at` field, for
                           byte-identical output across runs.
  --include-absolute-paths
                           Let `report` emit the repository's absolute host
                           path. Omitted by default.
  --depth <N>              Evidence and entry-point traversal depth for
                           `serve` and `report`.
  --confidence <LEVEL>     Confidence floor for `report`: `exact`,
                           `import_resolved`, `same_module` (default), or
                           `fuzzy_name`.
  --language <LANG>        `report` presentation filter: keep only items whose
                           file is in this language.
  --change-kind <KINDS>    `report` presentation filter: comma-separated
                           changed-symbol statuses to keep.
  --scope <PREFIX>         `report` presentation filter: keep only items whose
                           path starts with this prefix.
  --cache-dir <PATH>       Snapshot cache directory
                           (default: <repo>/.orbit-graph/explorer/snapshots).
  --no-cache               Index into task-owned temporary trees; reuse nothing.
  --time-budget-ms <MS>    Per-request traversal budget for `serve` and
                           `report`; `0` answers nothing and reports the budget
                           as the bound.
  --node-cap <N>           Traversal node cap for `serve` and `report`.
  -h, --help               Print this message.

`serve` binds 127.0.0.1 only, fixes the repository scope at launch, and prints a
per-launch bearer token to standard error once. Every request must carry that
token, and any Origin or Referer that is not the service's own origin is
refused.

Snapshot trees and indexes are cached per commit, keyed by commit SHA, extractor
version, and store schema version. A key that does not match this binary is
rebuilt, never reused. `clean` removes stale-key entries and entries for commits
the repository no longer has, and nothing outside the cache directory.

`snapshot` output is a human diagnostic, not a stable machine contract.

`report` writes a machine-contract JSON export plus a self-contained, static
HTML rendering (readable from disk, no scripts, no service). It never includes
the whole repository: only the changed symbols in scope are queried, and
every cited source location is either a bounded excerpt or a precise
`file:line-span@sha` reference. It refuses to overwrite `<name>.json` or
`<name>.html` unless `--force` is given.
```

## Limitations

See [the design document's limitations section](../../docs/design/change-explorer.md#limitations)
for what this tool gets wrong, does not know, and does not attempt.
