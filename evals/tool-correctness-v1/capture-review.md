# Capture and protocol review

Three complete capture attempts are retained as task artifacts, together with
their original frozen corpus/manifest and raw output. They use candidate
SHA-256 `422c8c92eb5729c2187a4398ace492541a1c1af991ea079913d23ce831079f35`,
source/plugin commit `696855d33922ea02214af83f473fa90316ee4cab`, original manifest
SHA-256 `162b695ec204aca3bca996f47632874f31f6b3bb5c198d55e8a89a9b96bde6a7`,
and real Orbit SHA-256
`4e7f9de8ab8f197e11334451a676145ecb73a10ec8bfa3a4d3bf605c48a81794`.
None qualifies; failures were never replaced by warm successes or another run.

The first run exposed evaluator defects: an empty Git template has no `.git/info`
directory, several standalone CLI spellings differed from plugin arguments, and
some assertions had misread the documented output shape. The source-reviewed
import encodings were corrected from the extractor implementations: Rust
`use std::fmt` stores `(std, fmt)`, C# `using System` names System, Kotlin's import
names abs, and Rust mod declarations are not deps import rows. The ambiguous
body group contains one analysis unit, so it could not test max_symbols=1;
two independently changed functions were added for that bound. These are
post-query protocol errata, not product fixes or a preregistered clean study.
Original corpus bytes were recovered and verified against their capture hashes.

The second run revealed further contract-reading mistakes: version/history
CLI command/branch spellings, overview's plugin `selector` field, missing show
as a null result, the shared `index_missing` error code and failed-file counts
as an object. Those assertions/adapter spellings were corrected from the source.
Cache files-written counts and published generation names are explicitly named
operational differences; raw values are retained. Two positive same-name-owner
cases were added with literal source truth before querying them. The third run
uses 95 cases and compares incremental and fresh full graph query results.

Review found that matching the word "ambiguous" anywhere in a payload could
mistake a fixture path for disclosure. An independent negative control was
recorded failing before the scorer fix. The corrected scorer requires structured
ambiguity/candidate evidence, then replays the saved outputs without rerunning
the candidate. Original measurement scores remain alongside replay scores.

Candidate findings must be assessed using the corrected replay and raw evidence,
not the first two runs' invalid assertions. The dashboard distinguishes missing
samples/setup failure from successfully returned, incorrect outputs. The
duplicate-command challenge prevents index publication with a UNIQUE command
name failure; it is a setup failure for trace, not an exercised trace response.
The retained cold/warm mismatches and source-grounded failures are candidates
for later scoped product repairs. No product code was modified here.

Worker validation needed two scoped environment adaptations. The read-only host
advisory-cache error and nextest 0.9.136 version refusal reproduced on a pristine
clone of the source commit. The CI SHA-256-pinned nextest 0.9.146 was downloaded
only into owned scratch and ran the complete suite successfully. Cargo-deny
passed with an exact copy of repository policy plus only advisories.db-path
pointing to owned scratch. No policy was weakened or global installation made.
The nextest stderr capture is capped; its producer exit 0 is the completion
evidence. The initial installed-plugin conformance log retains result excerpts
and exit 0, rather than claiming a complete byte-exact stream capture.

Native private plugin installation/conformance worked in this worker. Enclosing
memory/PID controller visibility is unavailable (`null` ceilings); no verified
outer-runtime containment claim follows from those reads. Root should confirm
the enclosing runtime limits and independently review frozen truth and the report.

Final review corrected recommendation's installed `result` wrapper in the scorer
and Python's dotted owner identities (`A.run`/`A.work`). The search bound fixture
now queries duplicate work declarations; the library sanitizes FTS operators as
literal terms, so the earlier OR query did not establish a truncation denominator.
A fourth, focused real capture validates these three corrected requests. It
does not qualify the full suite and does not replace earlier failures. The
third capture's JSON corpus snapshot was semantically identical but serialized
UTF-8 differently from the pinned bytes; its original serialized form was kept,
then its snapshot restored from bytes verified against the recorded SHA-256.

The self receiver in Python is a dynamic-binding challenge; demanding exact
ownership from it was too strong. The original fixture is preserved. The final
positive same-name-owner fixture writes Rust A::work/B::work explicitly, with
the extractor's canonical `<A>::work`/`<B>::work` method identities (rust/mod.rs
formats inherent impl owners inside angle brackets). The fifth/sixth focused
captures retain the initial identity mismatch and the final validated requests.
These focused captures remain evidence. The final dashboard uses a complete
capture of the corrected 95-case protocol and retains every earlier capture's
failed qualification. It makes no clean-cohort or prospective effectiveness claim.

## Independent operator review reconciliation

The independent source-only reviews attached to ORB-14004 under
`operator/source-truth-20261005T0057/` reviewed 93 original scenarios without
candidate outputs. The operator observed the two already-written owner cases
at 01:18 UTC and authorized retaining 95, with no further scenarios. All 95
identifiers are preserved. The root's 01:38 scorer recheck is retained as proof
that the original missing-identity and invalid-metric controls falsely passed;
it was read before the final correction, together with all three reviews.

| Review finding | Correction and evidence |
| --- | --- |
| changes/history: uncertain rows omit all source identities | `uncertain_pairing` requires the exact multiset of old/new selector, snapshot and SHA occurrences, including uncertain candidates. Added/removed sides require proper null opposite sides. Complete uncertainty remains valid; missing, duplicate, invented and overconfident identities fail. |
| changes/history: arbitrary comparison SHAs | Every ordinary changes comparison uses exact frozen `$base`/`$head` assertions; `changes_provenance` also checks every occurrence/candidate SHA and side. Budget comparison pins are checked whenever indexing finishes. |
| changes/history: ranking ranges/formulas unchecked | Eight hand-specified variant/level count rows require K=10, cases=1, relevant=1, TP=1 and returned=1. Metric formulas require recall=1, precision=0.1 and stale fraction=0; invalid values, inconsistent counts, duplicate variants and nonfinite latency fail. |
| changes/history: no budget delta | Existing `changes-budget` now uses a leaf body change with a hub caller. `budget_ms=1000`, `query_budget_ms=100` and `node_cap=1` survive scoring; completed comparisons account for the leaf and traversal cuts, or structured indexing/analysis exhaustion discloses uncertainty. |
| graph: OR search and empty lifecycle equality | Search bounds uses the source-known duplicate literal `work`. Lifecycle runs separate literal keep/remove/added/moved queries. A corrected lifecycle fixture writes keep->remove in base and keep->added in head; original no-call bytes remain preserved. Each full/incremental snapshot independently satisfies complete overview, search, callee and reference truth before equality is tested. Same-count stale paths/edges and two empty search outputs fail controls. |
| graph: scope and failed result shapes | Overview submits `selector: file:src/helper.rs`; maintain checks `failed.count=0`. |
| graph: ambiguity keywords and false missing freshness | Structured ambiguity/candidates are required; fixture path words cannot pass. Missing status expects `index_missing`, never an invented fresh state. |
| language: import encoding addendum | Root's 01:19 source recheck supersedes initial null-symbol assumptions for Rust std/fmt, C# System/System and Kotlin kotlin.math.abs/abs. Relevant literal import lines and product source remain unchanged. |
| language/history: scope qualifications | JSX/TSX test extension routing without markup; source extraction is not compilation/runtime semantics. Unsupported/omitted schema.proto are one physical file. Future Git objects exist, while later truth is never imported into ranking; cutoff behavior is not physical containment. |

The corrected truth, manifest, scorer, tests and runner were attached as
`tool-correctness-v1/truth-operator-corrected.tar.gz` before final candidate
queries. Twenty-five focused tests include the exact three operator invalid
output controls and valid uncertainty. Capture `attempt-007` uses the same
candidate/source/runtime identities as earlier attempts, with complete frozen
corpus and raw streams. Its source-quality failures remain findings; no product
fixes are mixed into this evaluator. Operator source review is evidenced;
corrected final approval is not self-attested by this executor.

The operator subsequently verified the corrected scorer at SHA-256
`1c2dce65791f8ae8dfcf97339c181fabffe8724cd3634312c6f2534d2eb425a9`
using complete valid/invalid controls (scorer-controls-20261005T0153.json),
including an incorrect in-range precision. Independent final source review
(final-source-reconciliation-20261005T0155.json) verifies all 95 source bindings,
the two static-owner cases, actual lifecycle/budget deltas and fixture hashes.
All 49 initial fixture files remain unchanged; ten added files support the
authorized corrections. That source/control admission makes no blanket approval
of the captured report.

Root's separate outer-runtime-observation-20261005T0204.json identifies the
enclosing worker scope and observes memory.max=65074413568 bytes,
memory.high=52059529216 bytes and pids.max=4096, with no OOM events. This is a
current external host observation. Historical capture visibility remains null;
it establishes neither per-episode 4 GiB/256 PID limits nor comparable timing
conditions. Raw captures are preserved unchanged and cost remains diagnostic.
