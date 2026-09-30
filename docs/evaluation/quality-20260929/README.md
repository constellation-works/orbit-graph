# CLI, graph, and plugin quality validation

This audit covers the standalone CLI, public graph/change-analysis APIs, and the
canonical Orbit plugin on macOS arm64. The starting revision is `e361af50`.
Repository release versions, storage schemas, extractor versions, tags, and
installed user plugins are unchanged.

## Public executable surfaces

The CLI has 20 leaf commands: `sync`, `search`, `show`, `refs`, `callees`,
`impact`, `changes`, `recommend`, `history import`, `history sync`,
`history status`, `history rebuild`, `evaluate`, `trace`, `overview`,
`implementors`, `deps`, `db-path`, `clean`, and `version`. Each has executable
success coverage. Help goldens cover those commands, the root, and the `history`
namespace (22 pages); a version JSON golden pins a representative machine result.

The composed tests also exercise invalid selectors and bounds, persisted-data
failures, read-only behavior, empty results in all four formats, corpus evaluation,
closed stdout, and native macOS PTY rendering. Source inspection escapes terminal
controls for human output while retaining the original source in JSON/NDJSON.

## Installed plugin surfaces

`tests/plugin_v2.rs` installs the canonical `.orbit-plugin/` export in a private
HOME and Orbit root, bundles the actual graph binary, and exercises all 13 tools
through both installed CLI and stdio MCP. It checks nonempty query evidence,
a real discovered command handler, unknown fields, and mutation authority.
The two legacy installed-tool tests remain covered separately.

The installed v2 test passed against both Orbit 0.24.0 and 0.25.0. Orbit 0.24
requires the direct `.orbit-plugin/` source path; Orbit 0.25 also discovers it
from a local export root. The pinned CI Orbit version remains 0.24.0.
A fresh `7ff9730` export additionally passed all 42 canonical conformance goldens
against Orbit 0.25.0. No live plugin or credential installation was inspected or
modified.

Schema/runtime parity checks reject explicit null across every advertised tool
field. Executable request tests accept exactly 1 MiB and refuse larger input
before dispatch. Launcher tests cover JSON control characters, trailing newlines,
UTF-8, and digest errors.

## Regression evidence and gate behavior

Focused regressions failed before their fixes for stale/reused inbound hints,
corrupt persisted enum values, zero history limits creating state, unsafe launcher
JSON, terminal control rendering, timestamp-preserving size changes, qualified
symbol identity, and legacy short selector selection. They passed afterward.
Short aliases resolve independently on each snapshot using the public graph
resolver; original caller selections and unmatched requests remain visible.

The pristine baseline ran 651 tests outside the outer execution sandbox:
650 passed, one signal fixture reached its 1-second callback deadline, and two
installed-Orbit tests were skipped by default. Those installed tests passed when
explicitly invoked. Native watchers require host filesystem notifications and
fail inside the outer execution sandbox; they run outside it for this gate.
The signal fixture passes on its own; no production signal-reaping bug was proved.

An intermediate run with nextest 0.9.136 falsely reported a capture-pipe leak on
`default_bounds_are_valid`, a pure validation test that exited successfully and
spawns no process. This matches the upstream concurrent pipe-inheritance bug
[fixed in nextest 0.9.145](https://www.nexte.st/changelog/).
CI and local instructions now pin checksum-verified nextest 0.9.146; the config
refuses older runners. Leak detection and all timeouts remain enabled, with no
retries configured. The same complete change-analysis suite then passed 113/113.

The final native Mac `make ci` passed at `eb4077e`: 677 tests passed with the
three installed-Orbit tests skipped by default, and all 14 doctests passed.
Formatting, all-target workspace Clippy, supply-chain checks, the 52 seeded
repository-guard cases, documentation with warnings denied, the locked build,
and whitespace checks passed. The signal fixture passed in this complete run.
An explicit final-source run against Orbit 0.25.0 then passed all three installed
tests (two legacy and one canonical v2), with none ignored.

## Reproducible inbound benchmark

The [locked harness](benchmarks/inbound/run.py) calls the actual release-mode
`Graph::refs` and inbound `Graph::impact_with_direction` APIs. Each process creates
a fresh on-disk SQLite database with 300,000 references, two files/symbols, and
exactly one matching reference. Every warmup and measured call asserts the
expected result. There is one warmup and 30 samples per query; results report
upper medians in microseconds (sorted sample at index 15 of 30). Source reads
and result materialization are included.

| Public query | Original baseline | Original fix | Fresh-build baseline | Fresh-build fix |
| --- | ---: | ---: | ---: | ---: |
| Exact refs | 26,156.084 | 136.083 | 18,669.458 | 47.250 |
| Exact inbound impact | 17,359.584 | 122.083 | 22,273.250 | 60.541 |
| Fuzzy refs | 32,010.292 | 42.583 | 32,238.333 | 47.625 |
| Fuzzy inbound impact | 22,995.333 | 52.625 | 24,412.875 | 61.042 |

The public harness uses bundled SQLite 3.53.2. A separate SQL-only Python probe
uses SQLite 3.51.0, checks the query plan, and measures the old scan and new target
index lookup; its timing excludes all public API work. Machine-readable results
and methodology are in [results.json](benchmarks/inbound/results.json).
These are synthetic selective-query measurements, not repository-wide throughput
claims. A separate statement-cache trial was discarded because repeated timings
did not establish a stable gain.

Run the same harness against two source checkouts or clean exports:

```sh
python3 docs/evaluation/quality-20260929/benchmarks/inbound/run.py /path/to/baseline --offline
python3 docs/evaluation/quality-20260929/benchmarks/inbound/run.py /path/to/current --offline
python3 docs/evaluation/quality-20260929/benchmarks/inbound/sql_only.py
```

`run.py` creates and retains a temporary locked Cargo project, prints its path,
and builds outside the source checkout. Omit `--offline` if dependencies are not
cached. Use the same host and repository lockfile when comparing runs.
