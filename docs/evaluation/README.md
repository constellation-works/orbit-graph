# Recommendation evaluation

`orbit-graph evaluate --input <corpus.json>` runs a chronological, fail-closed
backtest. Every case supplies a task-text snapshot attested before its cutoff,
an immutable repository revision observed at query time, and a later public
delivery envelope used only as truth. The evaluator previews the held-out Git
diff without importing it, refuses a target boundary already in the training
index, and applies the same strict cutoff to combined, task-search-only,
graph-only, and frequency rankings.

The report emits file and symbol recall@K and precision@K, stale-result rate,
mean/maximum latency, case and training coverage, exclusions, input digest,
source provenance, and exact revisions. Precision reserves K slots per case;
an engine returning fewer results does not receive artificial credit.

## Recorded real prospective cohort

[`orbit-prospective-20260907-input.json`](orbit-prospective-20260907-input.json)
is derived from the public `orbit.task.show` observation captured before
execution at 2026-09-07 04:15:17 UTC. Its immutable observed Git revision is
`4d84b3b7a61aa8d3a6035a6fe92bd4526b746679`. The later ORB-11483 delivery chain
is attested by successful public run outputs for `jrun-20260907-0441-3`,
`jrun-20260907-0500-3`, and `jrun-20260907-0516-3`, whose verified commit
boundaries form `7d128b8a… -> e4c047ce… -> 558cc638… -> 8ca24afb…`; Git confirms
the composite boundary is reachable from `agent-main`. Run completion time is
kept `uncertain` as delivery time because the public output does not expose an
exact landing timestamp.

[`orbit-prospective-20260907-result.json`](orbit-prospective-20260907-result.json)
records the resulting one-case evaluation. Combined and graph-only each found
one of six file destinations at K=10; every symbol result and both historical
baselines had zero hits. All stale rates were zero. This tiny cohort has no
attested earlier training task text, and ORB-11484 has no delivery until this
work lands, so it is intentionally absent from metrics. The result supports no
ranking-superiority claim. An orchestrator can append ORB-11484 after landing
using its preserved pre-execution observation, without regenerating either
observation timestamp or observed revision.

Reproduce from a checkout containing the named commits:

```sh
orbit-graph evaluate \
  --input docs/evaluation/orbit-prospective-20260907-input.json
```

Wall-clock latency varies by machine; compare the stable corpus digest,
coverage, revisions, truth counts, hits, recall, precision, and stale rates.
