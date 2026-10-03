# Live Git commit-text evaluation

Measured on dk-server-1 (15 cores) on 2026-09-26 with a release `orbit-graph`.
Each held-out commit C on `agent-main`'s first-parent chain is queried with C's
subject at target `C^`, in live mode (no cutoff), so Git-only commit text is
eligible. Truth is the files C changed that exist at `C^`. Precision reserves
10 slots. MRR is the mean reciprocal rank of the first hit inside those 10.
The command fails if any recommendation cites a delivery that is not an
ancestor of the target. Both runs reported `leakage_violations: 0`, and every
held-out commit was present in the index, so that zero is exclusion rather
than a missing row.

A subject is **title-restating** when it cites a bracketed task id such as
`[ORB-123]`, the squash-merge marker in this history. No task store is read.
`without_title_restating` is the complement.

`no_commit_text` runs first and pays cold target-symbol extraction. The other
variants reuse that cache. Their lower mean latency is not evidence that
commit text is cheaper; compare recall, precision, and MRR.

The aggregate record is [`commit-text-live.json`](commit-text-live.json).

## Setting

Production stays `0.5 × similarity²`.

The rule fixed before the runs: keep that point unless another point improves
MRR@10 by at least 0.01 on **both** repositories. Use `without_title_restating`
when that cohort has at least 20 truth-bearing cases, otherwise `all`.

orbit-graph's non-title cohort has 19 truth-bearing cases, so its comparison
cohort is `all`. Orbit's has 79, so its comparison cohort is
`without_title_restating`. On those cohorts, `0.25 × s²` gains 0.006 MRR on
orbit-graph (under the margin) and `no_commit_text` wins Orbit's non-title
cohort but loses orbit-graph `all` (0.490 versus 0.516). Nothing clears 0.01
on both. Squared 0.5 is also the best recall on orbit-graph `all` (0.262).

The non-title Orbit cohort is the caveat: there, no commit text (MRR 0.310)
beats squared 0.5 (0.231). Those 79 cases are the minority of a history whose
subjects usually restate task titles (219 of 298 truth-bearing cases). On that
majority, and on the pooled Orbit cohort, squared weight beats no text
(MRR 0.422 and 0.372 versus 0.255 and 0.269).

## orbit-graph

`agent-main` at `d9d8ad4ddd52eccba458eca10b4ec0371686bb6a` has 111 first-parent
commits, so this is the whole holdable history, not a few hundred. Sync indexed
all 110 deliveries (`complete: true`). The walk stopped at `history_exhausted`.
Newest held-out commit `d9d8ad4…`, oldest `187de6e799198d4324cc0db1688a5efbcd621945`
(parent `f1c26cafb27dae3967abfcf4876e162f078e2aec`, the root, which is not a
delivery). One-minute load average 6.08 at the start of scoring and 3.91 at
the end.

```sh
orbit-graph --format json history sync --branch agent-main --limit 1000
orbit-graph --format json evaluate --live --branch agent-main --limit 300 --k 10
```

110 cases, 106 with at least one target-live file (786 relevant files).

| Variant | Recall@10 | Precision@10 | MRR@10 |
| --- | ---: | ---: | ---: |
| no commit text | 0.205 | 0.152 | 0.490 |
| linear 0.5·s | 0.246 | 0.182 | 0.457 |
| squared 0.5·s² | 0.262 | 0.194 | 0.516 |
| squared 0.25·s² | 0.247 | 0.183 | 0.522 |
| linear 1.0·s | 0.247 | 0.183 | 0.433 |

Title-restating subjects: 88 cases, 87 with truth. Without: 22 cases, 19 with
truth.

| Cohort | Variant | Recall@10 | Precision@10 | MRR@10 |
| --- | --- | ---: | ---: | ---: |
| title-restating | no commit text | 0.207 | 0.172 | 0.560 |
| title-restating | linear 0.5·s | 0.242 | 0.202 | 0.518 |
| title-restating | squared 0.5·s² | 0.256 | 0.214 | 0.574 |
| title-restating | squared 0.25·s² | 0.249 | 0.208 | 0.589 |
| title-restating | linear 1.0·s | 0.242 | 0.202 | 0.486 |
| without | no commit text | 0.183 | 0.058 | 0.174 |
| without | linear 0.5·s | 0.283 | 0.089 | 0.178 |
| without | squared 0.5·s² | 0.333 | 0.105 | 0.252 |
| without | squared 0.25·s² | 0.217 | 0.068 | 0.218 |
| without | linear 1.0·s | 0.300 | 0.095 | 0.190 |

## Orbit

Shared clone of the Orbit checkout, `agent-main` at
`56c0924fb2e4380acc6406d58564979ffe6e9ea5`. The history index records the clone
path as its repository identity. Sync indexed the newest 500 first-parent
commits and stopped incomplete (`resume_from`
`45efa5f9aa0dfcfe263efb12f0529342d86057fb`). The newest 300 of those were held
out, so the oldest held-out commit still has 200 older indexed deliveries.
Newest `56c0924…`, oldest `8fed86429821e2344974c5d6ed77996cd3096f41` (parent
`aaed688286f72e6cf3bfaca8c88ca6e76eb109b3`). Load average 5.22 then 6.91.
`limit_reached`. Leakage 0.

```sh
orbit-graph --format json history sync --branch agent-main --limit 500
orbit-graph --format json evaluate --live --branch agent-main --limit 300 --k 10
```

300 cases, 298 with truth (2,591 relevant files).

| Variant | Recall@10 | Precision@10 | MRR@10 |
| --- | ---: | ---: | ---: |
| no commit text | 0.095 | 0.083 | 0.269 |
| linear 0.5·s | 0.120 | 0.105 | 0.298 |
| squared 0.5·s² | 0.142 | 0.123 | 0.372 |
| squared 0.25·s² | 0.152 | 0.132 | 0.391 |
| linear 1.0·s | 0.113 | 0.098 | 0.284 |

Title-restating subjects: 220 cases, 219 with truth. Without: 80 cases, 79
with truth. On the non-title cohort, commit text lowers MRR relative to no
text; on the title-restating majority it raises it.

| Cohort | Variant | Recall@10 | Precision@10 | MRR@10 |
| --- | --- | ---: | ---: | ---: |
| title-restating | no commit text | 0.084 | 0.084 | 0.255 |
| title-restating | linear 0.5·s | 0.126 | 0.127 | 0.343 |
| title-restating | squared 0.5·s² | 0.143 | 0.145 | 0.422 |
| title-restating | squared 0.25·s² | 0.152 | 0.153 | 0.428 |
| title-restating | linear 1.0·s | 0.119 | 0.120 | 0.334 |
| without | no commit text | 0.163 | 0.078 | 0.310 |
| without | linear 0.5·s | 0.089 | 0.043 | 0.174 |
| without | squared 0.5·s² | 0.134 | 0.065 | 0.231 |
| without | squared 0.25·s² | 0.152 | 0.073 | 0.290 |
| without | linear 1.0·s | 0.076 | 0.037 | 0.147 |
