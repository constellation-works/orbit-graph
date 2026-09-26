# Sample exports

Two worked examples of `orbit-graph-explorer report`'s output — the machine
contract JSON plus the self-contained static HTML — committed so a developer
can open one without running the tool first.

- `direct-call` is **generated and checked**: the test
  `crates/orbit-graph-explorer/tests/derived_artifacts.rs` regenerates it with
  the current binary and fails if either file differs byte for byte. After an
  intended change to the report output (a new field, an `EXTRACTOR_VERSION`
  bump, an index-identity change), regenerate it in the same change:
  `UPDATE_GOLDENS=1 cargo test -p orbit-graph-explorer --test derived_artifacts --locked`.
- `orbit-graph-self-study-9e5c15986b14` is a **historical snapshot**, pinned to
  extractor version 6 and export schema version 1. Nothing regenerates or
  checks it, and it will not match what the current binary produces.

**Size cap.** A sample is committed here only if its JSON+HTML pair together
are under 250 KiB; `orbit-graph-explorer report` with no `--selector` and the
default `controlled` excerpt mode can otherwise produce several megabytes per
export (see [`docs/evaluation/change-explorer/README.md`](../README.md), which
documents the same cap for the five study reports under
[`../reports/`](../reports/)).

| File | Cap | Actual |
| --- | --- | --- |
| `direct-call.json` + `.html` | 250 KiB | 37.8 KiB |
| `orbit-graph-self-study-9e5c15986b14.json` + `.html` | 250 KiB | 146.9 KiB |

## `direct-call`

The single-symbol, single-caller, single-test fixture case from
`crates/orbit-graph-cli/tests/fixtures/change-explorer/direct-call/` (see
[the demo](../demo.md) and
[the changed-symbol identity section](../../../design/change-explorer.md#changed-symbol-identity)),
materialized into a real temporary Git repository with fixed commit times
(so its commit SHAs are stable) and reported with the real built binary — no
`--selector` filter, so every changed symbol is included. The test runs the
equivalent of:

```sh
orbit-graph-explorer report \
  --repo <materialized direct-call fixture> \
  --base <base commit> --head <head commit> \
  --out <fresh directory> --name direct-call \
  --excerpts controlled --generated-at 2026-09-13T00:00:00Z
```

`repository` is `null` in the JSON because `--include-absolute-paths` was not
given — the default omits the host path.

## `orbit-graph-self-study-9e5c15986b14`

**Historical snapshot at extractor version 6.** The two "most consequential
changed symbols" from evaluation study 1 (the `orbit-graph` repository's own
ORB-12372 reference-resolution rewrite; see
[`docs/evaluation/change-explorer/README.md`](../README.md#per-study-what-was-right-what-was-wrong-what-is-unknown)),
as [`../scripts/export-reports.sh`](../scripts/export-reports.sh) exported them
at `--depth 1` with `--excerpts none` when the explorer indexed with extractor
version 6 — the two selectors for study 1's question 4, not every changed
symbol in that comparison (61 rows; see the size cap above for why). It was
then a verbatim copy of
[`../reports/1-orbit-graph-9e5c15986b14.json`](../reports/1-orbit-graph-9e5c15986b14.json)
and its `.html` pair; those reports have since been re-exported at extractor
version 8, so the two no longer match, and this snapshot predates fields such
as `outbound_paths`. To produce a current export of the same comparison, run
`DEST=<directory> scripts/export-reports.sh 1` (a wider `--depth` gives a
bigger, non-cap-fitting export).
