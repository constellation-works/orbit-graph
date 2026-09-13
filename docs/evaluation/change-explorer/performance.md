# Change-explorer performance envelope

Every number here comes from
[`scripts/performance.sh`](scripts/performance.sh) and
[`scripts/progress-cancel.sh`](scripts/progress-cancel.sh) run against
`orbit-graph-explorer` at commit `c1332cf13a4daa7f3695950fa6767622ed61fd28`,
release profile, `EXTRACTOR_VERSION` 6, `STORE_SCHEMA_VERSION` 1, unless marked
otherwise. **Studies 1, 2, 3 and 5 were re-measured on 2026-09-13** against
commit `ab6e3e303689742910c5452d6acc700552428b89` (`agent-main`, after
ORB-12416 and ORB-12417), `EXTRACTOR_VERSION` 8, `STORE_SCHEMA_VERSION` 1
(unchanged), same hardware, same `performance.sh 25`; see *Re-run warm-cache
latency* below. Study 4 and the cold-indexing/corpus-size numbers were not
re-measured (same corpora and commits, no code path either fix touches).

## Reference hardware and environment

| | |
| --- | --- |
| Host | `dk-server-1` |
| CPU | 13th Gen Intel(R) Core(TM) i5-13500H — 14 cores, 1 thread per core, 1 socket |
| Cache | L2 56 MiB (14 instances), L3 16 MiB |
| Memory | 26 GiB total |
| Root filesystem | ext4 on `/dev/mapper/ubuntu--vg-ubuntu--lv`, 391 GB (rotational flag 1 on the LVM device) |
| Scratch | `tmpfs` at `/tmp`, 14 GiB — corpora, snapshot caches and outputs all live here |
| OS | Ubuntu 24.04.4 LTS, Linux 6.8.0-139-generic x86_64 |
| Toolchain | cargo 1.96.0 / rustc 1.96.0 |
| Load | The host runs other services. Measurements were taken with no other explorer or `orbit-graph` process running, but the machine was not otherwise quiesced. |

The snapshot caches, materialized trees and SQLite indexes were all on `tmpfs`.
Cold-index numbers on a spinning or network filesystem will be worse.

## Corpus sizes

Measured with [`scripts/corpus-sizes.sh`](scripts/corpus-sizes.sh) at each study's
head revision (tracked files only; LOC is `wc -l` over the blob contents).

| Study | Repository | Commits | Head | Files | Rust files | Rust LOC | Python files | Python LOC | Other files |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | `orbit-graph` | 35 | `9e5c15986b14` | 203 | 131 | 37 300 | 12 | 54 | 60 |
| 2 | `orbit` | 4 429 | `5a5b45fec9a8` | 2 472 | 1 744 | 475 848 | 17 | 4 928 | 711 |
| 3 | `orbit` | 4 429 | `156dc93d940e` | 2 929 | 1 699 | 446 861 | 17 | 4 878 | 1 213 |
| 4 | `observatory` | 178 | `ae145d1c361a` | 1 443 | 0 | 0 | 156 | 43 650 | 1 287 |
| 5 | `orrery` | 52 | `82c24be9a49d` | 431 | 0 | 0 | 38 | 19 787 | 393 |

Languages the extractor reported during the `orbit` cold build: `config`,
`javascript`, `markdown`, `python`, `rust`, `typescript`. That list is only
observable mid-build — see *Indexing progress* below.

Files the explorer actually indexed per side (from `/api/comparison`):

| Study | Base indexed / written / excluded | Head indexed / written / excluded |
| --- | --- | --- |
| 1 | 177 / 203 / 0 | 177 / 203 / 0 |
| 2 | 2 228 / 2 462 / 10 | 2 228 / 2 462 / 10 |
| 3 | 2 182 / 2 919 / 10 | 2 182 / 2 919 / 10 |
| 4 | 1 263 / 1 388 / 50 | 1 265 / 1 393 / 50 |
| 5 | 162 / 216 / 1 | 354 / 430 / 1 |

## Cold indexing (cache miss) and warm launch (cache hit)

`cold_*` is a `--cache-dir` cleared immediately before launch; `warm_*` is a second
launch against the same cache directory. Per-side `elapsed_ms` is read from
`GET /api/status` at the moment both sides reach `ready`.

| Study | Corpus | Cold wall (s) | Base `elapsed_ms` | Head `elapsed_ms` | Head's own build (s) | Warm launch to ready (s) |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | `orbit-graph` | 5.46 | 2 938 | 5 274 | ≈ 2.3 | 0.12 |
| 4 | `observatory` | 6.44 | 3 439 | 6 354 | ≈ 2.9 | 1.03 |
| 5 | `orrery` | 1.52 | 597 | 1 296 | ≈ 0.7 | 0.12 |
| 2 | `orbit` | 441.09 | 219 068 | 441 033 | ≈ 222 | 0.12 |
| 3 | `orbit` | 398.87 | 197 043 | 398 740 | ≈ 202 | 1.03 |

**The two sides are built sequentially, and both report the same `started_at`.**
The head side's `elapsed_ms` therefore includes the base side's build; its own build
time is the difference, given above as "head's own build". This is a reporting
defect, filed as ORB-12412 — the figures above are derived, not read directly.

From the study runs (`scripts/service-study.sh`, which measures launch-to-ready the
same way):

| Study | Corpus | Cold index to ready (s) | Warm index to ready (s) |
| --- | --- | --- | --- |
| 1 | `orbit-graph` | 5.16 | 0.12 |
| 2 | `orbit` | **519.74** | 0.12 |
| 3 | `orbit` | **395.18** | 1.02 |
| 4 | `observatory` | 6.08 | 0.13 |
| 5 | `orrery` | 2.14 | 0.02 |

Cold indexing of the `orbit` corpus (≈ 2 200 indexed files, ≈ 450 000 Rust LOC per
side) costs **6.6 to 8.7 minutes** on this host (398.9 s, 441.1 s, 395.2 s and
519.7 s across the four cold builds measured). That is the dominant first-use
cost: in study 2's cold pass it was 91 % of the 573 s total. Every subsequent launch
against the same two commits is under a second.

## Query latency (warm cache, 25 runs per endpoint)

`curl -w '%{time_total}'`, whole-request wall clock including connection setup and
JSON encoding. The query subject is each study's primary question-4 symbol, named in
the table, so the measurement is on a real neighbourhood rather than a Markdown
heading.

### Study 1 — `orbit-graph`, subject `symbol:src/sync/pass2.rs#resolve_ref:function`

| Endpoint | p50 (ms) | p95 (ms) | max (ms) |
| --- | --- | --- | --- |
| `GET /api/comparison` | 0.4 | 0.6 | 1.0 |
| `GET /api/changed-symbols` | 22.6 | 28.5 | 29.4 |
| `GET /api/search` | 23.2 | 26.1 | 26.8 |
| `GET /api/evidence` depth 1 | 28.5 | 32.5 | 37.9 |
| `GET /api/evidence` depth 2 | 39.2 | 48.6 | 58.6 |
| `GET /api/evidence` depth 3 | 75.6 | 78.0 | 91.1 |
| `GET /api/entry-points` depth 3 | 77.1 | 81.2 | 82.0 |
| `GET /api/candidate-tests` | 97.7 | 108.8 | 110.9 |

### Study 4 — `observatory`, subject `symbol:…/fput/metrics.py#evaluate_metrics:function`

| Endpoint | p50 (ms) | p95 (ms) | max (ms) |
| --- | --- | --- | --- |
| `GET /api/comparison` | 0.7 | 0.9 | 0.9 |
| `GET /api/changed-symbols` | 23.9 | 27.9 | 28.3 |
| `GET /api/search` | 25.2 | 28.2 | 30.2 |
| `GET /api/evidence` depth 1 | 32.9 | 39.6 | 40.1 |
| `GET /api/evidence` depth 2 | 48.7 | 53.8 | 55.3 |
| `GET /api/evidence` depth 3 | 50.0 | 53.7 | 56.6 |
| `GET /api/entry-points` depth 3 | 50.6 | 55.9 | 57.1 |
| `GET /api/candidate-tests` | 72.5 | 83.7 | 86.0 |

### Study 5 — `orrery`, subject `symbol:scripts/research_records.py#supporting_paths:function`

| Endpoint | p50 (ms) | p95 (ms) | max (ms) |
| --- | --- | --- | --- |
| `GET /api/comparison` | 0.5 | 0.7 | 0.9 |
| `GET /api/changed-symbols` | 26.1 | 30.8 | 31.8 |
| `GET /api/search` | 25.7 | 28.5 | 29.1 |
| `GET /api/evidence` depth 1 | 29.7 | 36.6 | 42.4 |
| `GET /api/evidence` depth 2 | 46.0 | 54.8 | 55.6 |
| `GET /api/evidence` depth 3 | 46.8 | 53.1 | 54.1 |
| `GET /api/entry-points` depth 3 | 37.2 | 41.0 | 43.0 |
| `GET /api/candidate-tests` | 57.4 | 61.0 | 61.2 |

### Study 2 — `orbit`, subject `symbol:crates/orbit-cli/src/command/workspace/teardown.rs#resolve_teardown_target:function`

| Endpoint | p50 (ms) | p95 (ms) | max (ms) |
| --- | --- | --- | --- |
| `GET /api/comparison` | 0.5 | 0.6 | 0.6 |
| `GET /api/changed-symbols` | 129.4 | 175.8 | 186.4 |
| `GET /api/search` | 109.9 | 156.4 | 168.4 |
| `GET /api/evidence` depth 1 | 175.6 | 199.7 | 201.8 |
| `GET /api/evidence` depth 2 | 275.1 | 291.5 | 292.9 |
| `GET /api/evidence` depth 3 | 279.6 | 299.8 | 322.4 |
| `GET /api/entry-points` depth 3 | 284.7 | 401.1 | 405.6 |
| `GET /api/candidate-tests` | **741.5** | **769.3** | **803.0** |

### Study 3 — `orbit`, subject `symbol:crates/orbit-types/src/task/model.rs#TaskComplexity:enum`

| Endpoint | p50 (ms) | p95 (ms) | max (ms) |
| --- | --- | --- | --- |
| `GET /api/comparison` | 0.5 | 0.7 | 0.7 |
| `GET /api/search` | 125.5 | 270.5 | 304.5 |
| `GET /api/changed-symbols` | 306.2 | 397.2 | 398.6 |
| `GET /api/evidence` depth 1 | 298.8 | 487.8 | 500.7 |
| `GET /api/evidence` depth 2 | 441.4 | 879.6 | 922.4 |
| `GET /api/evidence` depth 3 | **1 141.2** | **1 340.1** | **1 417.9** |
| `GET /api/entry-points` depth 3 | **1 065.4** | **1 186.5** | **1 580.2** |
| `GET /api/candidate-tests` | **1 473.9** | **2 017.1** | **2 032.8** |

## Re-run warm-cache latency (2026-09-13, after ORB-12416 / ORB-12417)

Same method as above (`curl -w '%{time_total}'`, 25 runs per endpoint, warm
cache, no cold build in the measurement), same query subjects, run against
`orbit-graph-explorer` at `ab6e3e303689742910c5452d6acc700552428b89`,
`EXTRACTOR_VERSION` 8. Cold-index and corpus-size numbers were not re-measured
(same corpora, same commits, no code path either fix touches); cold walls
observed incidentally while re-measuring (`cold_wall_seconds` from
`scripts/performance.sh`) were 3.51 s (study 1), 469.44 s (study 2), 431.89 s
(study 3) and 1.18 s (study 5) — consistent with the original 5.46/441.09/398.87/1.52 s
range, confirming cold-indexing cost is unaffected by either fix.

### Study 1 — `orbit-graph`, subject `symbol:src/sync/pass2.rs#resolve_ref:function`

| Endpoint | p50 (ms) | p95 (ms) | max (ms) | vs. original p50 |
| --- | --- | --- | --- | --- |
| `GET /api/comparison` | 0.4 | 0.6 | 0.6 | unchanged |
| `GET /api/changed-symbols` | 24.3 | 29.8 | 31.3 | +1.7 ms |
| `GET /api/search` | 21.6 | 25.9 | 26.4 | −1.6 ms |
| `GET /api/evidence` depth 1 | 27.4 | 32.0 | 33.3 | −1.1 ms |
| `GET /api/evidence` depth 2 | 35.8 | 39.1 | 39.2 | −3.4 ms |
| `GET /api/evidence` depth 3 | 73.8 | 81.3 | 83.1 | −1.8 ms |
| `GET /api/entry-points` depth 3 | 78.0 | 82.4 | 86.8 | +0.9 ms |
| `GET /api/candidate-tests` | 98.1 | 105.4 | 110.8 | +0.4 ms |

### Study 2 — `orbit`, subject `symbol:crates/orbit-cli/src/command/workspace/teardown.rs#resolve_teardown_target:function`

| Endpoint | p50 (ms) | p95 (ms) | max (ms) | vs. original p50 |
| --- | --- | --- | --- | --- |
| `GET /api/comparison` | 0.5 | 0.8 | 0.8 | unchanged |
| `GET /api/changed-symbols` | 120.1 | 140.9 | 151.1 | −9.3 ms |
| `GET /api/search` | 117.7 | 134.9 | 136.0 | +7.8 ms |
| `GET /api/evidence` depth 1 | 203.0 | 218.2 | 225.0 | +27.4 ms |
| `GET /api/evidence` depth 2 | 321.9 | 355.9 | 376.8 | +46.8 ms |
| `GET /api/evidence` depth 3 | 321.7 | 442.8 | 449.3 | +42.1 ms |
| `GET /api/entry-points` depth 3 | 319.7 | 349.5 | 353.5 | +35.0 ms |
| `GET /api/candidate-tests` | **800.0** | 994.1 | 1173.5 | +58.5 ms |

### Study 3 — `orbit`, subject `symbol:crates/orbit-types/src/task/model.rs#TaskComplexity:enum`

| Endpoint | p50 (ms) | p95 (ms) | max (ms) | vs. original p50 |
| --- | --- | --- | --- | --- |
| `GET /api/comparison` | 0.5 | 0.8 | 0.9 | unchanged |
| `GET /api/search` | 124.2 | 133.5 | 136.0 | −1.3 ms |
| `GET /api/changed-symbols` | 127.3 | 176.1 | 176.7 | **−178.9 ms** |
| `GET /api/evidence` depth 1 | 235.8 | 334.6 | 338.0 | −63.0 ms |
| `GET /api/evidence` depth 2 | 863.5 | 1055.2 | 1215.9 | **+422.1 ms** |
| `GET /api/evidence` depth 3 | **854.0** | 882.0 | 1055.1 | **−287.2 ms** |
| `GET /api/entry-points` depth 3 | **882.8** | 924.4 | 1227.7 | **−182.6 ms** |
| `GET /api/candidate-tests` | **1298.2** | 1687.2 | 1818.4 | **−175.7 ms** |

### Study 5 — `orrery`, subject `symbol:scripts/research_records.py#supporting_paths:function`

| Endpoint | p50 (ms) | p95 (ms) | max (ms) | vs. original p50 |
| --- | --- | --- | --- | --- |
| `GET /api/comparison` | 0.5 | 0.6 | 0.8 | unchanged |
| `GET /api/changed-symbols` | 25.0 | 29.0 | 33.4 | −1.1 ms |
| `GET /api/search` | 24.7 | 28.5 | 29.6 | −1.0 ms |
| `GET /api/evidence` depth 1 | 27.3 | 29.4 | 29.9 | −2.4 ms |
| `GET /api/evidence` depth 2 | 29.4 | 31.8 | 32.1 | −16.6 ms |
| `GET /api/evidence` depth 3 | 29.3 | 31.6 | 31.6 | −17.5 ms |
| `GET /api/entry-points` depth 3 | 29.9 | 32.4 | 33.6 | −7.3 ms |
| `GET /api/candidate-tests` | 51.8 | 59.0 | 60.5 | −5.6 ms |

**Reading the deltas.** Study 3's move is the most informative one: `evidence`
depth 3, `entry-points` and `candidate-tests` p50 all dropped by 175–290 ms even
though the fixed `TaskComplexity` refs are still categorized `heuristic_match`
(see the study's After section) — the drop is consistent with the resolver no
longer walking a null-target dead end for every one of the 56 references before
falling back, though this report did not instrument the resolver internals to
confirm that mechanism directly. Studies 1 and 5 (small corpora, few affected
symbols) move by single-digit milliseconds in both directions — noise at this
sample size. Study 2 moved consistently *up* by tens of milliseconds on
`evidence`/`entry-points`/`candidate-tests`; plausibly the extra
`resolved_call`/`observed_reference` categorization work now needed to
distinguish the (fixed) self-loop-free paths from the (unfixed) fuzzy fan-out,
but this report did not isolate the cause. **The one-second-target verdict is
unchanged**: still met on the three small corpora, still missed on `orbit`
(`candidate-tests` p50 800.0 ms / 1298.2 ms on studies 2 and 3, both under the
original's 741.5 ms / 1473.9 ms baseline but still within the same order of
magnitude), for the same underlying reason — the 200-node budget on `orbit` is
still filled predominantly by name-only matches, just slightly fewer of them.

## Memory

`VmHWM` and `VmRSS` from `/proc/<pid>/status`, read from the live service.

| Study | Corpus | Peak RSS after the full latency sweep (MiB) | RSS at warm launch, before any query (MiB) |
| --- | --- | --- | --- |
| 1 | `orbit-graph` | 62.0 | 8.2 |
| 4 | `observatory` | 196.9 | 11.8 |
| 5 | `orrery` | 147.9 | 9.4 |
| 2 | `orbit` | 375.8 | 13.6 |
| 3 | `orbit` | 366.7 | 14.1 |

The "warm launch" column is measured immediately after both sides report `ready` and
before any query endpoint is called, so it is a floor, not a peak. The peak column
is after cold indexing plus the 200-request latency sweep plus the truncation census.

## Truncation

How often each bound cut an evidence query, over every **head-side** changed symbol
at each depth (`scripts/performance.sh`, `truncated_evidence_d*`), and which bound
fired first (`scripts/summarize-study.sh`, `truncated_by`). The denominators are
head-side selectors only, so they are smaller than the study reports' counts, which
include the base-side query issued for each `removed` symbol.

| Study | d1 truncated | d2 truncated | d3 truncated | Dominant bound at d3 |
| --- | --- | --- | --- | --- |
| 1 `orbit-graph` | 37 / 54 | 37 / 54 | 36 / 54 | `depth` (39 of 41 queries in the study pass) |
| 4 `observatory` | 9 / 43 | 2 / 43 | 1 / 43 | `depth` |
| 5 `orrery` | 48 / 76 | 38 / 76 | 23 / 76 | `depth` 29, `impact_node_cap` 2 |
| 2 `orbit` | 14 / 25 | 12 / 25 | 12 / 25 | `impact_node_cap` 9, `depth` 3 |
| 3 `orbit` | 14 / 26 | 14 / 26 | 13 / 26 | `impact_node_cap` 12, `depth` 1 |

The **time budget (`DEFAULT_TIME_BUDGET_MS` = 5000) never fired** in any study at any
depth, and `skipped_low_confidence` was 0 everywhere. The `source_max_bytes` bound
(64 KiB) fired on no changed symbol. On the small corpora `depth` is the bound that
stops the traversal; on `orbit` the 200-node `impact_node_cap` takes over, which
matters because on that corpus the node budget is consumed by name-only matches
(see ORB-12416, ORB-12417 and study 2 question 5).

## Against the "cached neighbourhood interaction under one second" target

**Met on the three small corpora, with an order of magnitude to spare.** The slowest
p95 anywhere on `orbit-graph`, `observatory` or `orrery` is 108.8 ms
(`/api/candidate-tests` on `orbit-graph`). A full three-pane refresh — comparison,
changed symbols, evidence at depth 3, entry points, candidate tests, issued
serially — sums to roughly 275 ms p95 on `orbit-graph` and 220 ms p95 on
`observatory`.

**Missed on `orbit`, and by a wide margin on the study-3 subject.** Three endpoints
exceed one second on a warm cache, for a single symbol, with no cold build involved:

| Endpoint | Study 3 subject `TaskComplexity:enum` | Over target |
| --- | --- | --- |
| `GET /api/candidate-tests` | 1 473.9 ms p50 / 2 017.1 ms p95 / 2 032.8 ms max | yes, at p50 |
| `GET /api/evidence` depth 3 | 1 141.2 ms p50 / 1 340.1 ms p95 / 1 417.9 ms max | yes, at p50 |
| `GET /api/entry-points` depth 3 | 1 065.4 ms p50 / 1 186.5 ms p95 / 1 580.2 ms max | yes, at p50 |
| `GET /api/evidence` depth 2 | 441.4 ms p50 / 879.6 ms p95 / 922.4 ms max | not at p50; p95 is 88 % of the budget |

On study 2's subject the same corpus stays inside one second per endpoint —
`/api/candidate-tests` peaks at 769.3 ms p95 — but a serial three-pane refresh there
sums to roughly 1.65 s p95, so the whole-pane interaction misses the target as well.
On study 3's subject the serial sum is roughly 5.2 s p95.

The cause is not corpus size as such. Both `orbit` subjects are dominated by
name-only fan-out: study 2's `.execute()` matching and study 3's unresolved
cross-crate `TaskComplexity` references fill the 200-node budget with `fuzzy_name`
matches that then have to be walked, sorted and serialized. The resolver defects
filed as **ORB-12416** and **ORB-12417** are therefore the main latency cost on this
corpus as well as the main correctness cost — fixing them should shrink both the
answer and the time to produce it.

**Where the target does not apply at all:** the first launch against a pair of
commits, on a corpus the size of `orbit`, is not an interaction — it is a 6.6–8.7
minute build during which every query endpoint answers 409 `side_not_ready`.
`GET /api/health` and `GET /api/status` stay responsive throughout, so the UI can
show progress, but no neighbourhood is available. That is a cache-miss cost, not a
query cost.

## Indexing progress and cancellation on the large corpus

`scripts/progress-cancel.sh 2` runs against `orbit`
`1ca6416e0ba2c6a713e42561b7b35a72b045aa25` → `5a5b45fec9a83b765b485043398d3abadaa468de`.

Phase 1 samples `GET /api/status` once a second through a full cold build. Phase 2
clears the cache, starts a second cold build, probes a query endpoint while the base
side is `indexing`, posts `/api/cancel`, and watches the transition.

Observed behaviour, with the raw trace in `progress-trace.tsv` and `cancel.txt` under
the scratch output directory:

**Progress reporting works, but saturates almost immediately.** The base side's
counters over the first seconds of a cold build:

| seconds | `base.state` | `files_seen` | `files_indexed` |
| --- | --- | --- | --- |
| 0 | `materializing` | 0 | 0 |
| 1 | `indexing` | 2 228 | 1 269 |
| 2 | `indexing` | 2 228 | 2 228 |
| 3 … 213 | `indexing` | 2 228 | 2 228 |

`files_seen` is set to the side's total at the first `indexing` sample rather than
counted up, and `files_indexed` reaches that total after **two seconds** — then holds
for the remaining 211 s of that side's build, because reference resolution and FTS
population are not reflected in any counter. A progress bar driven
by these fields shows 100 % for almost the whole build on this corpus. Recorded on
ORB-12412.

`languages` does behave as documented *during* the build, growing from
`config,markdown,rust` at t=1 to
`config,javascript,markdown,python,rust,typescript` by t=51 — and is then emptied
when the side reaches `ready`.

Phase 1's full trace: `materializing` at t=0, base `indexing` from t=1 to t=213,
base `ready` and head `indexing` from t=214, both `ready` at t=428. The head side
sat at `pending` with `files_indexed: 0` for the entire base build — the two sides
are strictly sequential.

**Phase 2 — cancellation, verbatim from `cancel.txt`.** With the base side
`indexing` and the head side `pending`:

```
changed_symbols_while_indexing (expect 409 side_not_ready):
http_status=409
{"code":"side_not_ready",
 "message":"At least one side is not ready yet. `GET /api/health` stays responsive
            while this runs; `GET /api/status` reports per-side progress.
            Retry once both sides report `ready`."}

cancel_response:
{"cancelling":true,"indexing":{"base":{...,"state":"indexing"},
                               "head":{...,"state":"pending"}},"schema_version":1}
http_status=200

after_cancel_states (polled up to 60s):
indexing,pending
cancelled,cancelled

cache_entries_published_after_cancel:
0
```

Everything the contract specifies for this path holds: the query endpoint refuses
with 409 `side_not_ready` rather than blocking, `POST /api/cancel` is non-blocking
and returns 200 with the live `indexing` object, **both** sides transition to
`cancelled` (including the one that had not started), and no cache entry is
published for the cancelled build.

Contract deviations found here, all filed as **ORB-12412**: the shared `started_at`
(so a `pending` side's `elapsed_ms` ticks and the second side's build time is
overstated), the emptied `languages` list at `ready`, and the saturating
`files_indexed` counter above.

## Reproduce

```sh
cargo build --workspace --locked --release
docs/evaluation/change-explorer/scripts/clone-corpora.sh
docs/evaluation/change-explorer/scripts/corpus-sizes.sh
for s in 1 2 3 4 5; do docs/evaluation/change-explorer/scripts/performance.sh "$s" 25; done
docs/evaluation/change-explorer/scripts/progress-cancel.sh 2
```

Latency varies with machine and filesystem. The stable comparisons are the shape —
`comparison` ≪ `changed-symbols` ≈ `search` < `evidence` < `entry-points` <
`candidate-tests`, and cold indexing dominating everything — and the truncation
counts, which depend on the corpus and the bounds rather than on the host.
