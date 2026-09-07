# Recommendation evaluation

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

## Recorded real prospective cohort

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
run start is the corresponding prospective lower bound. The current corrective
delivery remains outside the corpus until this run lands.

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
orbit-graph evaluate \
  --input docs/evaluation/orbit-prospective-20260907-input.json
```

Wall-clock latency varies by machine; compare the stable corpus digest,
coverage, revisions, truth counts, hits, recall, precision, and stale rates.
