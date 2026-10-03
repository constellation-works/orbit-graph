# Source-first review record

Reviewer: Codex executor for ORB-13797, 2026-10-03. Review used explicit physical
Git roots under `/home/daniel/workspace/constellation/codebases/` and pinned
`git show COMMIT:PATH` reads, before any graph query or provider output. No graph
results, model answers, historical scores or candidate performance selected these
questions. Local fake-provider tests exercise the frozen cases only after this
selection; they are scripted test fixtures, not observations of agents.

The original commits are graph `ac6d5b91f973874325bac789e9c433a71c805766`,
Orbit `0db9356eef631ada11569d0ca4e1e407fad5bee3`, and pulsar
`6d33fb9d4609618c13d46deb2df6af53b7f504a6`. `corpus.json` records each required
identity's exact definition line, owner context, permitted module/owner aliases,
and semantic evidence with full excerpt SHA-256. `corpus.lock.json` records all
selected file hashes, original blob OIDs/modes, commits/trees and content revisions.
Only the new study changes; prior files are read-only dependencies.

| New case | Source review and explicit required behavior |
| --- | --- |
| graph-lexical-selector-paths | `crates/orbit-graph-extract/src/selector.rs`: FromStr 92–134, wrapper 171–179, normalizer 181–218. Grammar splits and lexical normalization examples; success cannot attest physical containment. |
| graph-trace-expansion-budget | `crates/orbit-graph/src/query/trace.rs`: traversal 13–77, materialization 153–165; `query/tests/trace.rs` 89–109. FIFO/depth/node-cap semantics, unresolved leaves and repeated paths; tree size is not runtime completeness. |
| orbit-interval-catchup-projection | `crates/orbit-automation/src/auto_tasks/schedule.rs`: entry/projection 58–105, due 131–149, next interval 156–165, latest slot 187–199. Exclusive floor versus fixed anchor, collapsed catch-up and uncertain future execution. |
| orbit-indexed-overlap-candidates | `crates/orbit-common/src/fs/overlap_index.rs`: insert 62–76, query 84–134; `fs/selector.rs` 303–378, 429–473. Candidate generation/recheck, relative-root filtering, anchor equality, no semantic resolution. |
| pulsar-account-selection-uncertainty | `src/pulsar/app/core/account/registry.py`: resolve/error 367–414, cached identity 417–423, status constants 92–95. Precedence, ambiguous defaults, revoked versus reauth fallback, binding-limited cache trust. |
| pulsar-readonly-wal-fallback | `src/pulsar/app/core/ledger/connection.py`: read 43–113, error classification 127–131, fingerprint 134–139, attempts 20–21. WAL refusal, immutable retries, metadata-only guarantees and SQLite sidecars. |

Every required identity is explicitly requested by its role/name in the prompt.
Every rubric claim answers an explicit prompt clause. Additional supporting
symbols are optional, judged with attributable source evidence, and do not add
hidden requirements. Qualified aliases refer to the same declaration, not just a
matching short name. Owner context is retained for methods; duplicate short names
cannot collapse into one identity. Source checks reject incorrect namespaces,
owners, kinds, files and quotes. Supporting declarations outside the conservative
parser's recognized shapes are refused explicitly; a reviewer cannot guess an
alias into validity after seeing outcomes.

All required definition lines and behavioral excerpts survive the delivered
`safe-tool-text-v1` shape redactor unchanged. The protocol forbids secret-valued
provider-env bindings for this cohort and requires the empty host-value policy;
shape masking remains active. Thus no required answer relies on reconstructing
masked strings. Other test source can still be masked: manifests identify the
original objects, while raw proofs identify actual delivered safe text. Exact
original definition-line citation is the only convention accepted here.

## Distinctness from exposed questions

Both full prompts and required behaviors were compared, not merely IDs. The lock
pins all three old corpus files. The deterministic ID/prompt disjointness check
is a guard against accidental copying; this source review establishes behavioral
distinctness and remains a judgment, not a claim of statistical independence.

| Exposed real-study case | Boundary from these new questions |
| --- | --- |
| graph-inbound-identity | Inbound row-hint predicate/consumers; v2 trace asks expansion budgets, not target identity repair. |
| graph-sync-preserved-timestamp | Incremental file replacement detection; v2 has no sync freshness case. |
| graph-impact-test-selection | Impact confidence/inbound regressions; v2 trace's branching cap is a different traversal and regression. |
| orbit-crew-resolution-entrypoints | Activity crew resolver dispatch; v2 has no crew-selection question. |
| pulsar-publication-ledger-callbacks | Publication reservation/approval callbacks; v2 ledger reads never claim a publication. |
| pulsar-media-deferred-callbacks | Deferred upload/content validation; v2 has no media case. |
| pulsar-bounded-error-commit | Historical provider-error commit diff; v2 has no history/provider-error case. |
| pulsar-channel-dispatch-abstention | Dynamic publication channel callee; v2 uncertainty is account-state selection and cache trust, not channel dispatch. |

| Exposed installed-v1 case | Boundary from these new questions |
| --- | --- |
| graph-live-source-bounds | FIFO/UTF-8 bounded file I/O; lexical parser and trace-tree budget have no live-source reader requirement. |
| graph-trait-name-resolution | Trait short-name SQL resolution; lexical selector grammar does not ask about implementors or trait matching. |
| orbit-artifact-text-policy | Persistence redaction action policy; no v2 artifact sanitizer question. |
| orbit-reverse-log-boundaries | Reverse-block UTF-8 log iteration; no v2 log-reader question. |
| pulsar-read-metering | Paid-read admission/count/cost race; SQLite read-only fallback asks storage consistency, not API usage. |
| pulsar-text-weight-normalization | Unicode/URL weighted text and price; no v2 text validator question. |

The original synthetic Rust/Python cases asked price definition, direct callers,
direct callees, transitively affected tests, changed bodies, external consumers,
and dynamic callback resolution. V2 does not rephrase these fixtures or retry
their identities. Its source-defined behavior remains bounded and purposive.
Repositories were previously inspected; development and held-out repositories
are disjoint only within this new study. Six pairs cannot establish superiority,
p-values or unseen-repository generalization.

## Reuse boundary

The v2 helper imports read-only source/export/oracle functions from the current
v1 helper and its dependencies. Its freeze/adapt/CLI plumbing and test patterns
are adapted locally from those same in-repository files; it does not recover
additional external historical code. No runner is duplicated or patched. The
new oracle adds conservative declaration/owner checking; runtime admission adds
explicit runner5/broker4/four-module and safe-text-policy requirements. The final
provider, backend, tool inventory and shipped skill still require an operator
runtime freeze and external custody before any live execution.

Operator source review, posted 2026-10-03 11:55Z, independently checked all 31
rubric excerpts and topic distinctness before measured model output. Its three
clarifications are incorporated: tree materialization and unknown-account error
details are explicit prompt requests, and before-baseline projection is stated
as baseline PLUS ONE PERIOD. This changes clarity, not source truth or cases.
