# Current plugin evidence, 2026-10-03

The [current readiness and installation handoff](../../docs/plugin-readiness.md)
identifies candidate `1523af82c7a9055adb5fd474035ee9a05b505732`. Its runtime
qualification is bounded; **agent benefit is not demonstrated**. This directory
is a report, not a new cohort or a scorer change. [results.json](results.json)
contains the canonical report's complete groups, episodes and pairs, reviewer
attribution, limitations, candidate/export identities and artifact SHA-256s.
Raw captures, complete answer quotes/source reviews and source archives remain
in their original durable task artifacts.

## Completed installed-plugin study v2

Root ran twelve once-only episodes on six fresh, counterbalanced question pairs:
two graph development questions and four Orbit/pulsar held-out questions. Both
arms received the same approved 2,607 source files. Runtime: Codex **0.160.0**,
`gpt-6.1-sol`, medium reasoning, code mode, bubblewrap with 4 GiB / 256 PID limits,
and preserved **debug** graph binary `81ffdbda…`. Graph use was optional; the
treatment included the installed plugin and shipped skill. These repositories
had been inspected in prior work, so held-out does not mean unseen repositories.

All 12 captures were accepted: 10 completed and two graph attempts failed with
`tool_execution_failed` during long MCP calls. No retries or replacements.
Frozen combined correctness requires both objective identity/citation checks and
semantic review; successful execution alone is not a correct answer.

| All-attempt measure | Baseline (6 attempts) | Graph (6 attempts) |
| --- | ---: | ---: |
| Combined correct | 3/6 | 2/6 |
| Development correct | 2/2 | 1/2 |
| Held-out correct | 1/4 | 1/4 |
| Completed / failed | 6 / 0 | 4 / 2 |
| Graph adoption attempted | 0/6 | 6/6 |
| At least one successful graph call | 0/6 | 5/6 |
| Graph adoption among completed episodes | 0/6 | 4/4 |
| Graph calls / successful graph calls | 0 / 0 | 32 / 23 |
| All tool calls | 42 | 63 |
| Wall time, ms | 421,483 | 532,679 |
| Provider time, ms | 416,687 | 521,098 |
| Setup time, ms | 4,796 | 11,581 |
| Plugin install time, ms | 0 | 6,125 |
| Graph sync time, ms | 0 | 36,554 |
| Preflight time, ms | 1,402 | 1,494 |
| Tool output bytes | 408,098 | 494,087 |
| Setup output bytes | 0 | 1,789,962 |
| Observed input tokens (6/6 each) | 620,889 | 1,315,521 |
| Observed cached input tokens (6/6 each) | 438,144 | 1,078,400 |
| Observed output tokens (6/6 each) | 9,849 | 10,296 |
| Total tokens / USD (0/6 observed each) | null / null | null / null |

Timings preserve the harness's fields: wall = setup + provider; installation,
preflight and graph sync are component observations, not additional disjoint
costs to add to wall. Cached tokens are separately reported observations, not an
extra additive token total. Missing total-token and USD observations remain
**null**, never zero or inferred prices. The per-arm accumulated wall limit is
1,800,000 ms (300,000 per episode). Failures remain in every all-attempt total.

| Pair | Baseline status / strict correct / wall ms | Graph status / strict correct / wall ms |
| --- | --- | --- |
| graph-lexical-selector-paths | ok / yes / 39,037 | ok / no / 67,044 |
| graph-trace-expansion-budget | ok / yes / 72,505 | ok / yes / 81,890 |
| orbit-interval-catchup-projection | ok / no / 85,552 | failed / no / 88,699 |
| orbit-indexed-overlap-candidates | ok / no / 100,800 | failed / no / 122,427 |
| pulsar-account-selection-uncertainty | ok / yes / 53,300 | ok / yes / 76,454 |
| pulsar-readonly-wal-fallback | ok / no / 70,289 | ok / no / 96,165 |

Only the trace and account pairs are correct in both arms and eligible for speed
comparison. Graph is slower by **9,385 ms** and **23,154 ms** respectively;
baseline/graph wall ratios are 0.885395 and 0.697151. Other pair speed fields stay
null. Six purposive pairs are descriptive, not powered evidence of superiority,
generalization or production latency/cost. `follow_up_consideration` is **false**;
this report ran no further cohort.

## Review, frozen score and prospective improvement

**Codex root ORB-13709** reviewed all ten complete answers against original
source and preregistered rubrics at `2026-10-03T12:53:50Z`. All ten passed semantic
review. This review was **UNBLINDED**: the reviewer also operated the cohort and
had seen arms, timings and artifact metadata. The two failed episodes have no
completed-answer review. Per-run artifact and audit seals, claim judgments and
reviewer attestations are retained in results; full quotes and source evidence
are in ORB-13709 `evals/plugin-study-v2-audits.json`.

The frozen checker rejects some source-supported spellings: trait-qualified
`<Selector as FromStr>::from_str`, Python `READ_ATTEMPTS` assignment, duplicate
method/function leaf names despite owner qualification, Rust dot spellings and
trait-impl owner recognition. `review-limitations.json` distinguishes these
cases. Unsupported entries are **not automatically hallucinations**, and the
unblinded semantic pass does not overwrite the strict 3/6 versus 2/6 score.

The delivered [source-identity-v1 helper](../source-identity-v1/README.md)
(ORB-13803/13804) is **prospective only**. It parses source independently of graph
output and does not execute source. Its isolated locked Rust frontend uses
`syn 2.0.119` / `proc-macro2 1.0.106`; Python uses AST declarations and supported
literal assignments. Verified owner/trait/generic spellings can share canonical
identity while citations are scored separately and `semantic_review` stays
pending. Wrong, ambiguous, conditional or unsupported context remains strict;
Rust dot spellings are explicitly rejected. Macro/attribute/module restrictions,
closed source scope, parser-versus-typechecking limits and an offline cached
frontend build prerequisite remain part of the contract. Root's 34 tests and 12
independent CLI cases qualify that bounded contract, not agent effectiveness.
The test-only example (identity 2/3, citation 1/3, semantic pending) is not a
rescoring of these or any historical answers.

## Provenance and retrieval

| Canonical record | SHA-256 |
| --- | --- |
| `report.json` bytes | `68c2a0dc22e946091f315a9d7faca605b72dfb0e28daa3dd7ee02903f40eb7fb` |
| `audits.json` bytes | `caf973433a895a059eca9850b16163073194dcf7051af1b54e79ad0cd964aba2` |
| `review-limitations.json` bytes | `bb6ed598219582dafd0688f848f12c31f85fed6c9ad5942743ddc3d0dd0aeb26` |
| Runtime freeze content seal | `772bb58d54be8e2083403f9b217bcc947d676ac7bbf2f315da904237618ef915` |
| Accepted bundle content seal | `4b76d26d5c9342c536ffef8cc953f1a6f9ff3df110f3576f0d2fc5a26dc3b8d2` |
| Prospective scorer admission bytes | `394cb786ad7a0bbbbcbb3a0c0153a832ae08a63e1387b0682ec45a66b0f2bfa0` |

Content seals hash canonical JSON excluding the seal field; they are not file-byte
hashes. They establish local integrity, not authentication or capture origin.
The separate operator custody and pre-run records remain necessary.
`results.json.sources` names owning tasks, exact artifact paths, byte hashes and
sizes. Retrieve a source through the granted task tool, for example:

```sh
orbit tool run orbit.task.artifact.get --input '{"id":"ORB-13709","model":"codex","path":"evals/plugin-study-v2-report.json"}'
```

The canonical local study is
`/home/daniel/workspace/constellation/codebases/orbit-graph/.orbit/tmp/ORB-13709-plugin-study-v2`.
Replay uses its exact archived **148-file** `replay-eee73b5` evaluator and archived
`score-command.json` / `score-provenance.json`. Durable
`evals/plugin-study-v2-replay-parts.json` locates the evaluator archive;
`bundle-parts.json` and `raw-index.json` locate all accepted and raw captures.
The current repaired harness intentionally differs and must refuse the old lock;
do not replay the frozen cohort through it or silently replace its score.

The [historical report](../readiness-20261003/README.md), corpora, protocols,
locks, helpers and captures remain byte-identical. The 24 synthetic episodes
scored held-out 6/8 each; 16 real episodes scored held-out 1/5 each. Neither
showed advantage. Installed v1 retained all 12 failed attempts; its provenance adapter
refused the capture set: **no accepted quality score**, neither zero nor
success. Its refusal and raw captures remain ORB-13709 `evals/plugin-study-v1-*`
artifacts. Later repairs never convert historical failure or refusal into a pass.
