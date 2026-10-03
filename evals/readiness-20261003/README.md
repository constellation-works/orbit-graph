# Integrated candidate and measured agent navigation, 2026-10-03

ORB-13773, execution `jrun-20261003-0550-c2`, certifies the bounded Linux
plugin surfaces at clean source `da5009b7bf8f1a328aa3923252b3b049b1e8bd17`.
The installed CLI/MCP checks and fresh exports pass against the exact measured
Orbit host. **Agent benefit is not established.** Historical provider captures
show equal held-out accuracy in both cohorts, sparse graph use and retained
harness failures. No provider was called during this certification.

[results.json](results.json) retains both complete canonical score outputs,
all 40 episode identities, failures, paired results, usage, scorer versions,
artifact digests, gate outcomes and repair commits. The
[plugin readiness report](../../docs/plugin-readiness.md) gives executable
identities, authority boundaries and exact installation preparation commands.

## Measurements and denominators

Each case has one baseline and one graph episode, in preregistered counterbalanced
order, with fresh sessions, source and cold caches. Model: `gpt-6.1-sol`, medium
reasoning, Codex CLI 0.160.0 with code mode enabled. These are provider captures;
the separate checked-in smoke results are scripted fixtures. The graph treatment
was a confined broker invoking the graph CLI, not the installed plugin plus its
bundled skill. Navigation used graph source `ef8979d7870c0b8b8dc1622cfe4d871cd7490f97`
and binary SHA-256
`465fd959e509a972f65225ac6c053026f63e92fbd17c8ef679cc58abb7307b2d`, not this
certification's final candidate build.

| Cohort / split | Pairs | Baseline correct | Graph correct | Failed/invalid baseline | Failed/invalid graph |
| --- | ---: | ---: | ---: | ---: | ---: |
| Synthetic / all | 12 | 10/12 | 8/12 | 2 | 4 |
| Synthetic / development | 4 | 4/4 | 2/4 | 0 | 2 |
| Synthetic / held-out | 8 | 6/8 | 6/8 | 2 | 2 |
| Real / all | 8 | 1/8 | 2/8 | 2 | 2 |
| Real / development | 3 | 0/3 | 1/3 | 1 | 1 |
| Real / held-out | 5 | 1/5 | 1/5 | 1 | 1 |

Failed/invalid columns count harness or invalid-answer outcomes, not every
incorrect answer. Synthetic raw execution has three graph-arm telemetry failures
and zero baseline harness failures. Scoring additionally retains two invalid
baseline citations and one invalid graph citation as incorrect episodes. Real
execution retains two `telemetry_mismatch` harness failures **per arm**:
`orbit-crew-resolution-entrypoints` and `pulsar-publication-ledger-callbacks`.
All remain in the denominators; no capture, answer, truth or request was rewritten
and no failure was retried or replaced. The synthetic original scorer refusal
and real original adapter refusal are attached alongside the successful canonical
scoring attempts.

Real correctness combines objective identity/citation checks, budget/isolation
checks and source-reviewed semantics. The 12 existing successful-capture reviews
were supplied by `Codex/sol operator ORB-13709`, with arm labels visible. Their
judgment content is preserved unchanged in the canonical audits; the other four
captures have no invented semantic audit. Six successful captures pass semantics,
but only three pass the full correctness check. Real oracle `false_positives`
are 22 baseline / 20 graph: these include real symbols outside the accepted set
and namespace formatting differences, and must not be called fabricated claims.
Audits bind exact records and frozen source excerpts; their hashes authenticate
content consistency, not the reviewer's identity or an independent blind review.

| All episodes, including failures | Synthetic baseline | Synthetic graph | Real baseline | Real graph |
| --- | ---: | ---: | ---: | ---: |
| Wall time including cold setup (ms) | 301,595 | 313,355 | 572,428 | 561,311 |
| Tool calls | 56 | 47 | 100 | 98 |
| Returned tool/final bytes | 24,043 | 21,865 | 654,072 | 610,525 |
| Provider input tokens | 793,074 | 719,232 | 1,242,376 | 1,282,025 |
| Provider output tokens | 4,165 | 4,144 | 14,412 | 13,880 |
| Graph calls | unavailable in baseline | 1 search | unavailable in baseline | 2 sync + 2 search |
| Cost (USD) | unknown | unknown | unknown | unknown |

Usage is observed in all 24 synthetic and 16 real episodes through
`codex exec --json turn.completed.usage`. Adapted usage does not establish
billing, cached-token discounts or cost-effectiveness; missing cost stays `null`.
Only one real pair is correct in both arms (`graph-impact-test-selection`):
baseline 43,226 ms, graph 40,764 ms, ratio 1.0604. That single descriptive ratio
cannot establish a speedup. Aggregate times include failures and cold setup and
are not a quality-controlled speed comparison. The model's server weights
revision is unknown.

## Immutable inputs and scoring

The cohorts remain separate. The synthetic scorer's
`agent_effectiveness_evidence: true` classifies captured agent rather than scripted
input; it is not a finding of benefit. The real scorer explicitly sets both
`effectiveness_claim_permitted` and
`follow_up_larger_independent_repo_cohort` to `false`.

| Input | Immutable identity |
| --- | --- |
| Synthetic corpus canonical SHA-256 | `2f98c3eb9ca92b625d6c1859bf02a6a988c8e3b93c189852e7fe11483c747e24` |
| Synthetic bundle canonical SHA-256 | `f63a2c94d41094743003a488ba72c3300430e70c5414998f53e415fbb502392f` |
| Synthetic scorer commit | `ce7405d3511c6a23d009b71ba17a73a1351d9a69` |
| Synthetic scorer file SHA-256 | `523ff1b2d1bfc8a591d16cffd8e4fb84628d6224b8cd621cb80d4a9c761d54bd` |
| Real corpus canonical SHA-256 | `fcd754711b1b477eabd19f660c215d84430794c9fc90075b8b2d9a2ca74af21b` |
| Real records canonical SHA-256 | `dd17f7e8b363519e21da90bd3161e7967d57772d84985528858a7c595b0c421e` |
| Real audits canonical SHA-256 | `f16413f61051afb8a2bf8c95ff8d77efe81ac26a432b46d50c4a95e36907f493` |
| Real scorer commit | `3486e4b1857bcc34c38fa75bd828b727fa6d2d30` |
| Real scorer file SHA-256 | `a6e933cbb17e13dc482f89277be3becf03eb791ea0a68332260205ba8364b769` |

Both scorer files in the integrated candidate match those exact bytes. Offline
re-scoring reproduced the supplied JSON objects exactly, including every failure.
Verification checked 130 synthetic and 46 real preregistration file hashes,
40 raw episode seals and all 240 capture-file byte counts/digests. It also checked
all 12 audit judgment sets against the original source notes. File digests and
canonical JSON digests are distinct and explicitly named in `results.json`.

Original operator paths are under the main checkout's `.orbit/tmp/`:
`ORB-13709-synthetic-cohort-01/` and `ORB-13709-real-cohort-01/`.
Durable **ORB-13773 task artifacts** under `certification/inputs/` contain the
unchanged bundle, preregistrations, scores, compressed real records, audits,
source-review notes, summaries and original refused attempts.
`raw-captures-manifest.json` binds two ordered archive parts containing the
original `episode.json` and six captured files for every episode. Concatenate the
parts in manifest order and verify the whole archive SHA-256 before extraction.
The split satisfies the task artifact tool's 1 MiB per-file limit.
`certification/input-verification.json` retains the full file verification ledger;
`certification/logs/{synthetic-score,real-score}.json` retain exact commands,
producer status and output. Re-scoring reads the frozen Git objects for the real
source corpus; it does not call a provider.

## Delivery evidence and repairs

The installed private fixture seeds host run/checkpoint rows, then exercises
ordinary public callbacks through all thirteen CLI and MCP tools. Its positive
landed import, unlanded exclusion, foreign-pair refusal and idempotent replay
prove that fixture's installed path; it is not an executor-produced delivery.

Separately, operator evidence for the **final source candidate** uses backend
SHA-256 `af2fe5682eaa4871b0c6aa5ef4572253076cecc9c43da59d2ccbe65e3579293b`,
ordinary public Orbit callbacks and fresh private graph state, with no host seeds
or installed wrapper. Four actual historical squash deliveries were inserted:
ORB-13170, ORB-13159, ORB-13158 and ORB-13156. ORB-13157 and ORB-13229 remain
excluded because recorded landed postimages differ from the checkpoint head.
Replay returns four `already_indexed` outcomes and the same two exclusions.
This worker also read ORB-13170's actual public delivery and verified landing
reachability and matching postimages for all 28 changed paths. Worker and
operator graph hashes identify distinct builds of the same candidate; they are
not interchangeable binary certifications. The evidence and verification are
attached under `certification/actual-delivery-*.json`.

`results.json` maps historical failures to full landed repair commits:
doctest isolation (ORB-13737), structured callback refusals (ORB-13738),
usable confined tooling (ORB-13745), citation/telemetry integrity (ORB-13748),
parallel call matching (ORB-13762), cache annotations/cleanup (ORB-13757),
episode-local invalid citation scoring (ORB-13764), broker diagnostics/supervision
(ORB-13769), raw redaction compatibility (ORB-13771), and public delivery import
(ORB-13763). The operator's final-candidate runner suite records 72 passes and
zero skips. These repairs have behavioral evidence; historical harness failures
remain failures, and no new effectiveness cohort tests the repaired runner.

## Limits and a future protocol

These eight real questions share three repositories across development and
held-out splits; the twelve synthetic questions use tiny fixtures. Treatment
exposure is sparse and does not test skill loading, live task recommendations,
plugin sandbox callbacks or scheduled use. The real scorer's preregistered
follow-up eligibility threshold is unmet: the rule requires verified isolation,
no held-out pair regression, at least one net additional correct held-out answer
and zero graph-arm held-out oracle false positives. The observed net accuracy
gain is zero and that arm has nine held-out oracle false positives. The evidence does not support
superiority, production task generalization or a larger effectiveness claim.

Before any separately authorized new study:

1. Freeze a new, unseen corpus/version with independently held-out repositories,
   reviewed source truth, scoring rules, budgets and failure policy. Keep these
   exposed held-out cases as historical diagnostics; do not retune their truth.
2. Preflight the actual installed bundled-plugin treatment in disposable roots:
   verify its manifest/binary/host hashes, skill delivery, visible tool inventory,
   ordinary public callbacks, cold-cache setup and valid source citations. A
   treatment-fidelity failure is recorded before provider work begins.
3. Preregister baseline read/rg/git versus the actual installed plugin and skill,
   with a separately labeled broker-proxy arm only if its comparison is wanted.
   Match model/settings/budgets, counterbalance order, isolate source/session/cache,
   and log actual graph selection and tool availability. Distinguish compulsory
   setup from optional agent use; do not force graph use and call it adoption.
4. Preserve every failed, invalid, denied and timed-out episode. Attribute
   infrastructure failures separately from answer quality, with no replacement
   episodes. Review semantics with arm labels hidden from independent reviewers;
   bind reviews to exact answers, frozen source and rubric before unblinding.
5. Report accuracy and omission/false-positive categories first, then paired
   quality-qualified latency, tool errors/calls/bytes, observed token categories
   and cost only when available. Predeclare sample size, uncertainty and the
   eligibility/effect threshold; the existing small cohorts cannot supply it.

This is a protocol handoff, not authorization to call providers, widen live
grants, install on the live host, publish a release, or claim effectiveness.
