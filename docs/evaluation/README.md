# Evaluation

This directory holds two unrelated evaluations.

* **Recommendation evaluation** — below: `orbit-graph evaluate`'s chronological,
  fail-closed backtest of change-destination recommendations.
* **[Change explorer](change-explorer/README.md)** — the Milestone 5 evidence for
  `orbit-graph-explorer`: five real change studies across Rust and Python, a
  measured performance envelope, and the limitations that follow from them.

## Recommendation evaluation

`orbit-graph evaluate --input <corpus.json>` runs a chronological, fail-closed
backtest. Every case supplies a task-text snapshot attested before its cutoff,
an immutable repository revision observed at query time, and a later public
delivery envelope used only as truth. The evaluator previews the held-out Git
diff without importing it, refuses a target boundary declared in the corpus
training set, and applies the same strict cutoff to combined, task-search-only,
graph-only, and frequency rankings.

Each case runs in a disposable local clone. Its history contains exactly the
declared corpus deliveries and its graph is built from the immutable target
tree, so neither operational index contamination nor later HEAD structure can
affect non-latency results. Held-out truth must be verified and proven later
than the cutoff by a known landing time or an explicit prospective lower bound.
A known landing time that predates the cutoff is authoritative and rejects the
case even when a prospective lower bound claims a later start (contradictory
chronology). Prospective lower bounds only admit uncertain/unknown landing times.
Hybrid hits likewise require pre-cutoff observation provenance. Added files and
symbols, unsupported languages, and unresolved pre-target identities remain in
the report as omitted truth coverage rather than disappearing from denominators.

The report emits file and symbol recall@K and precision@K, stale-result rate,
mean/maximum latency, case and training coverage, exclusions, input digest,
source provenance, and exact revisions. Precision reserves K slots per case;
an engine returning fewer results does not receive artificial credit.

### Recorded real prospective cohort

[`orbit-prospective-20260907-input.json`](orbit-prospective-20260907-input.json)
is derived from two public `orbit.task.show` observations captured before
execution at 2026-09-07 04:15:17 and 04:15:21 UTC (seconds-resolution wall times).
Conservative evaluation cutoffs are therefore `2026-09-07T04:15:18Z` and
`2026-09-07T04:15:22Z` — one second after each observation — rather than invented
nanosecond offsets. Their immutable observed Git revision is
`4d84b3b7a61aa8d3a6035a6fe92bd4526b746679`. The later ORB-11483 delivery chain
is attested by successful public run outputs for `jrun-20260907-0441-3`,
`jrun-20260907-0500-3`, and `jrun-20260907-0516-3`, whose verified commit
boundaries form `7d128b8a… -> e4c047ce… -> 558cc638… -> 8ca24afb…`; Git confirms
the composite boundary is reachable from `agent-main`. Run completion time is
kept `uncertain` as delivery time because the public output does not expose an
exact landing timestamp.

ORB-11484's initial delivery is independently attested by successful run
`jrun-20260907-0525-3` at boundary `8ca24afb… -> 3311ab82…`; its 05:25:04 UTC
run start is the corresponding prospective lower bound. The frozen corpus
records that ORB-11484's rework was unavailable at capture time and includes
only the initial delivery.

The public start of the first delivery run at 04:41:20 UTC is retained
separately as a trustworthy prospective lower bound: a performed commit
delivery cannot precede its execution start. It proves post-cutoff ordering
without pretending to be the landing timestamp.

[`orbit-prospective-20260907-result.json`](orbit-prospective-20260907-result.json)
records the resulting two-case evaluation. Combined and graph-only each found
three of 13 file destinations at K=10 (recall 0.231, precision 0.15); every
symbol result and both historical baselines had zero hits. All stale rates were
zero. The frozen target graphs were materialized but supplied no applicable structural neighbor; that
absence is recorded rather than treating HEAD or a lexical row as graph
evidence. Across both cases, truth coverage reports 13 eligible and 18 omitted
file changes, plus 14 eligible and 200 omitted symbol changes, with every
omission class retained per case. This tiny cohort has no attested earlier
training task text. Its corpus digest is
`dce41885bec60a107416630ff205809dd4d806ed40ca0f53263ee8f9c20bf7e2`.
The result supports no ranking-superiority claim.

Reproduce from a checkout containing the named commits:

```sh
orbit-graph --format json evaluate \
  --input docs/evaluation/orbit-prospective-20260907-input.json
```

Wall-clock latency varies by machine; compare the stable corpus digest,
coverage, revisions, truth counts, hits, recall, precision, and stale rates.

### Recommendation latency with indexed history

Measured on dk-server-1 (15 cores) against a `--no-hardlinks` clone of Orbit
at `9591bbbd1` (2,752 files) after
`orbit-graph history sync --branch agent-main --limit 200`, with release
builds and the query
`recommend --query "auto-task delete durable opt-out for shipped defaults" --level file --branch agent-main`.

| Build | Host load (1-min avg) | Wall time | User CPU |
| --- | --- | --- | --- |
| Before, `279091f` (orchestrator profile) | quieter | 333 s | — |
| Before, `279091f` | 32–54 | 1,391 s | — |
| After (path lineage) | 36–42 | 15.0–21.1 s | 9.5–10.0 s |
| After, same query with an empty history scope | 40 | 11.7–15.9 s | 9.1–9.5 s |

Before the change, 134 of the call's deliveries needed a whole-tree
`diff_tree_to_tree` plus rename-and-copy `find_similar` against the target
(about 2.45 s each, 328 s of the 333 s profile), repeated for only 47 distinct
revisions. Recommendation now chains the rename and deletion steps persisted
at ingest. On this index (207 steps: 102 renames and 105 deletions) it runs no
query-time diff, because the history cursor equals the target. The remaining
latency is the per-call target-tree symbol extraction that every recommend
call pays even with no history. It dominates both "after" rows, and 200
commits of history add about 0.5 s of CPU on top of it. The ranked output of
the combined query (selectors, scores, reasons) is identical to the
pre-change output. The `--variant frequency --limit 100` ranking, which
resolves every indexed delivery's paths, keeps 98 of 100 selectors with
identical scores (947 s before, 27 s after, under the same load). The two
dropped rows come from ORB-12958, which split `tests/callback.rs` and
`tests/loader.rs` into several files. That delivery's own diff classified the
old files as deleted rather than renamed, and lineage records what the
delivery's diff classified. The old whole-tree diff against the target had
paired each old file with one of its split parts. Upgrading the
existing v3 index copied its 200 deliveries and derived lineage in 2.1 s on
first open. The box was shared with concurrent builds during these runs, so
compare CPU times, or re-measure on an idle host, before quoting wall times.
