# Sample exports

Two worked examples of `orbit-graph-explorer report`'s output — the machine
contract JSON plus the self-contained static HTML — committed so a developer
can open one without running the tool first. Both pin `--generated-at` so
they are byte-identical if regenerated from the same inputs.

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
materialized into a real temporary Git repository and reported with the real
built binary — no `--selector` filter, so every changed symbol is included:

```sh
orbit-graph-explorer report \
  --repo <materialized direct-call fixture> \
  --base <base commit> --head <head commit> \
  --out docs/evaluation/change-explorer/samples --name direct-call \
  --excerpts controlled --generated-at 2026-09-13T00:00:00Z --force
```

`repository` is `null` in the JSON because `--include-absolute-paths` was not
given — the default omits the host path.

## `orbit-graph-self-study-9e5c15986b14`

The two "most consequential changed symbols" from evaluation study 1 (the
`orbit-graph` repository's own ORB-12372 reference-resolution rewrite; see
[`docs/evaluation/change-explorer/README.md`](../README.md#per-study-what-was-right-what-was-wrong-what-is-unknown)).
Copied verbatim from
[`../reports/1-orbit-graph-9e5c15986b14.json`](../reports/1-orbit-graph-9e5c15986b14.json)
and its `.html` pair, both produced by
[`../scripts/export-reports.sh`](../scripts/export-reports.sh) at `--depth 1`
with `--excerpts none` — the two selectors for study 1's question 4, not
every changed symbol in that comparison (61 rows; see the size cap above for
why). Regenerate with `DEST=docs/evaluation/change-explorer/samples
scripts/export-reports.sh 1`, or a wider `--depth` for a bigger, non-cap-fitting
sample.
