# Prospective graph effectiveness development study v1

This is an offline development deliverable: **24 fresh source-authored cases**, four
in each of six families, eight Python and sixteen Rust cases across four pinned
public repositories. It contains a prospective confirmatory protocol, full source
provenance, independent review preparation and executable offline scoring. It
contains **no provider measurements and no effectiveness claim**. Historical
studies are immutable and their findings have not changed.

All cases are exposed development material. They can never become final holdout.
The planning envelope of 48 fresh cases / eight fresh repositories / three
repetitions per arm is a starting budget, **not an established power guarantee**.
See [protocol.json](protocol.json) for the fixed final decision policy, multiplicity,
precision planning, futility, downstream patch/hidden-test and chronological
history tracks, unavailable ablations, and separate warm-cache study.

[Source review](source-review.md) maps each question to pinned source. The lock
records original path, commit, tree, Git blob OID/mode and content SHA-256 for
**12 complete source views**, including four accessible historical base/head
comparisons. No graph query or evaluated model answer informed these questions.
The source universe is the established `source.snapshot` allowlist, preserved in
full across all questions/arms. No `.orbit-plugin` duplicate source, instructions,
evaluations, truth, docs or credential state is exported. No script extension was
needed: the Python questions use ordinary Pulsar `src` modules.

Live runtime admission is closed. Author metadata retains its pending independent
review status; external attributed root reviews are supplied with `--truth-reviews`,
and the CLI verifies their exact packet/source bindings. All 48 required
identities currently have unsupported automatic coverage under the unchanged
`source-identity-v1` helper: Rust views contain multiple crate roots; Pulsar's full
Python manifests exceed 128 files. Other ordinary syntax can also refuse a whole
language index. We preserve those files and ambiguities. Unsupported is neither
verified nor hallucinated. A frozen independent source review must verify each
written declaration/citation, owner, module and trait across the full universe;
semantic claims are reviewed separately. Missing reviews stay pending.

The study reuses the established source exporter, runner request validator,
installed-plugin profile capture replay and identity helper read-only, pinned in
[corpus.lock.json](corpus.lock.json). It does not clone a runner, install a plugin,
call a provider, change configuration, or publish. Future runtime admission needs
telemetry/lifecycle work, strict confinement/capture qualification, a new versioned
runtime freeze and external operator custody. The offline plan does not grant
provider/source-transfer authority. Agent adaptation refuses until that future
prospective version; test-only capture replay preserves the current strict checks.

## Prepare and verify

Use Python **3.14** on Linux, the prospectively pinned AST/platform. Source exports
are staged, fsynced and atomically published with no-replace rename; JSON artifacts
are fsynced and published without overwriting. A failed export may retain an
unpublished `.source-stage-*` directory for diagnosis. All successful command
stdout is JSON; operational errors are JSON on stderr with exit 1; usage errors
exit 2. `--help` is text. Each command accepts only its documented flags. Writes
require a fresh destination with an existing physical parent; no overwrites.
Commands have a 600-second deadline; imported Git reads have a 30-second deadline.

From the repository root, set explicit Git roots. Replace `CODEBASES` when the
four pinned repositories live elsewhere. These commands read pinned Git objects,
not moving checkout contents. They never fetch or run evaluated source.

```sh
EVAL="$PWD/evals/graph-effectiveness-v1/eval.py"
CODEBASES=/home/daniel/workspace/constellation/codebases
STUDY_DIR="$PWD/.orbit/tmp/graph-effectiveness-preparation"
mkdir -p "$STUDY_DIR"
python3 -B "$EVAL" study validate \
  --repo "orbit-graph=$CODEBASES/orbit-graph" --repo "orbit=$CODEBASES/orbit" \
  --repo "pulsar=$CODEBASES/pulsar" --repo "nebula=$CODEBASES/nebula"
python3 -B "$EVAL" study export \
  --repo "orbit-graph=$CODEBASES/orbit-graph" --repo "orbit=$CODEBASES/orbit" \
  --repo "pulsar=$CODEBASES/pulsar" --repo "nebula=$CODEBASES/nebula" \
  --destination "$STUDY_DIR/source"
python3 -B "$EVAL" study validate --export "$STUDY_DIR/source"
python3 -B "$EVAL" study plan --study-id graph-effectiveness-dev-v1 \
  --repetitions 3 --destination "$STUDY_DIR/plan.json"
python3 -B "$EVAL" study truth-packets --export "$STUDY_DIR/source" \
  --destination "$STUDY_DIR/truth-packets.json"
```

Mount only `source/views/<view-name>`. Manifests/completion markers are outside
those directories; corpus/truth/reviews remain outside every agent mount. Hash
validation verifies the full file set, rejects added/missing/tampered files and
symlinks, and checks the exact source evidence and actual diff pins. Validation
can succeed offline with pending independent review: its `offline_ready` and
`runtime_admitted` fields explicitly distinguish those states. It refuses invalid
citations, contradictory absence claims, or ambiguous required truth.

Order is balanced within repository and family, reversed on alternating
repetitions. The schedule has one immutable `attempt_id` per arm/case/repetition,
with `pair_id=case_id--rNN` for compatibility with the existing profile's two-arm
replay contract. Every scheduled attempt, including setup refusals, failures,
invalid answers and timeouts, remains in the denominator. Missing/duplicate
attempts are integrity errors. Warm cache and experimental treatments refuse.

Given an **existing** treatment pin JSON from the shipped profile's inspection,
prepare prospective schema-3 request templates without installing anything:

```sh
python3 -B "$EVAL" study requests --plan "$STUDY_DIR/plan.json" \
  --pin /path/to/existing-treatment-pin.json \
  --destination "$STUDY_DIR/requests.json"
```

Common request fields are checked with the pinned v2 reference validator. Emitted
schema-3 templates require ORB-14003 delivery, qualified version dispatch and an
independent root runtime freeze before execution. They are not a certified runtime. The pin
has `commit`, `backend_sha256`, `orbit_sha256`, `inventory_sha256`, `skill_sha256`.
Format validation alone does not qualify the runtime. The primary product has the
complete shipped inventory/skill, optional adoption and matched baseline tools,
model/settings, source and budgets. Unavailable inventory/skill/tool-family
ablations require separate qualified treatments; never silently filter product.

## Review and score

Truth packets have separate `truth-identity`, `truth-semantic` and `truth-diff`
records. Each binds the full relevant source universe and exact evidence. A
review document has `schema_version:1`, `reviews:[]`, `adjudications:[]`. Each
review has exactly:

- `packet_sha256`, attributed `reviewer` distinct from the author,
  `reviewed_at` (UTC RFC3339 seconds), `method`, honest `blinding`, `verdict`,
  `rationale`, `answer_quote`, `source_evidence`, `identity`,
  `initial_review_sha256`, `review_sha256`.
- Methods are `source-wide-independent`, `semantic-source-review`, or
  `diff-source-review`. Verdicts are `verified`, `contradicted`, `ambiguous`,
  `unsupported`. Blinding is `source-only`, `arm-blinded`, or `unblinded`.
- Identity reviews verify `identity:{owner,module,trait,declaration}`; declaration
  is the written selector. Include declaration and module/owner/trait evidence
  from the full pinned view, never graph output. Semantic/diff identity is null.
  Truth `answer_quote` is null; answer reviews require a verbatim quote from
  the packet's original answer text. Every required evidence object is retained
  exactly, and all additional ranges must match frozen source bytes.
- Initial reviews have `initial_review_sha256:[]`. Consequential ambiguity or
  disagreement requires two independently attributed initial reviews and a third
  independent adjudicator. Its `initial_review_sha256` lists both original review
  hashes in order; both verdicts remain present. Missing adjudication is pending.

Seal completed reviews with `source.seal(review, 'review_sha256')` from `eval.py`.
The seal detects edits; it does not authenticate a reviewer or establish custody.
Test-generated passing reviews are **synthetic controls**, not completed root
review. No live truth waiver exists. A verified review that contradicts required
identity/evidence or a contradictory semantic/diff truth verdict refuses admission.

Original **test-only** installed-plugin capture adaptation requires schema 3,
`installed-plugin-skill-v3`, runner 6 / broker 5 and `prospective-accounting-v1`.
This checkout pins the v2 primitives as read-only references and refuses raw
capture adaptation until the qualified dependency is delivered and root freezes
new runtime pins. Historical schema-2 captures cannot become prospective data.
All-refusal schema-3 registries and explicit scorer fixtures remain runnable offline.
Setup refusals have exactly `order,status:"setup_refused",error:{code,message},
operator,evidence,elapsed_ms`. No episode is fabricated for a refused setup.
`--study-kind agent` always refuses in this offline version. Future original
capture admission retains per-episode evidence, safe-text, lifecycle, containment
and accounting gates even when another slot has a setup refusal.

For a validated bundle, the offline flow is:

```sh
python3 -B "$EVAL" study review --plan "$STUDY_DIR/plan.json" \
  --export "$STUDY_DIR/source" --bundle /path/to/test-only-bundle.json \
  --destination "$STUDY_DIR/answer-packets.json"
python3 -B "$EVAL" study score --plan "$STUDY_DIR/plan.json" \
  --export "$STUDY_DIR/source" --bundle /path/to/test-only-bundle.json \
  --review-packets "$STUDY_DIR/answer-packets.json" \
  --reviews /path/to/answer-reviews.json --truth-reviews /path/to/truth-reviews.json \
  --destination "$STUDY_DIR/report.json"
python3 -B "$EVAL" study precision --report "$STUDY_DIR/report.json"
```

Distribute **only** answer `packets` to reviewers; `operator_bindings` carries
attempt linkage and stays with the operator. Each attempt receives a random
256-bit opaque token. Packet groups are presented in random-token order, independent
of the public schedule, and replay preserves that order; freeze this registry once and supply it with `--review-packets`
at score time. Repeated identical answers keep every arm/repetition binding.
Automation cannot override pending, unsupported or ambiguous manual review. Packets omit arm, tools, timing and
usage. Answers can reveal treatment; reviewers report that honestly. Required and
additional written identities receive separate source review, never implicit
semantic credit. Contradicted and ambiguous automation are distinguished from
unsupported coverage. Pending answer or truth reviews yield unknown correctness;
failures remain incorrect. Successful scoring reports all-attempt accuracy bounds,
identity/citation recall, semantic outcomes, coverage, adoption/calls/bytes,
observed/missing tokens and timings. Schema-3 per-field coverage, diagnostic sums and nullable qualified totals stay
separate. Multi-turn/cache semantics remain unqualified; uncached input and
unqualified totals are null. Reasoning/cached counters are never added to other
counters without a qualified semantic contract. All tool attempts and overlapping
versus outside-wall timing observations are preserved separately.

Paired results average repetitions within case; incomplete case clusters are
reported explicitly and do not disappear from all-attempt denominators. Both-correct
time is conditional and includes its all-pair denominator. Development bootstrap
intervals, family reports and leave-one-repository-out and component sensitivity
are descriptive; repeated component problems are not independent sampling.
Precision planning refuses pending quality judgments. It uses observed discordance
and case variance and multiplicity-adjusted normal approximations. Constant or
no-discordance samples and fewer than eight clusters leave intervals/power/widths
null; a variance floor is not a statistical bound. It requires independent joint simulation and
fresh-sample freeze. It never declares 48 sufficient or a confirmatory go outcome.

## Deterministic offline controls

To exercise scoring without a provider/installation/capture, `review` and `score`
accept `--fixture` instead of `--bundle`. This is an explicit separate
`offline-scoring-fixture-v1` schema, always `study_kind:"test-only"`, with the
current `plan_sha256` and every ordered fixture record. It contains synthetic
outcomes/answers, calls, timing and nullable observed usage, **not original runner
captures**. Fixture outputs disclose that raw replay did not occur and always
report `effectiveness_evidence:false` / `decision:"offline-development-only"`.
Changing a fixture to `agent` or presenting it as an original bundle refuses.
The tests demonstrate the complete schema and all command paths.

Build the unchanged identity frontend from cached dependencies before focused
Rust identity checks. Check disk before every new build directory; at 80% or more,
create nothing new and follow the workspace disk-pressure runbook.

```sh
df --output=pcent .orbit/tmp | tail -1
CARGO_TARGET_DIR="$PWD/.orbit/tmp/source-identity-syntax-target" \
  cargo build --offline --locked --manifest-path evals/source-identity-v1/syntax/Cargo.toml
GRAPH_EFFECTIVENESS_EXPORT="$STUDY_DIR/source" \
  python3 -B -m unittest discover -s evals/graph-effectiveness-v1 -p 'test_*.py' -v
```

The tests drive the real CLI, pin all help/error goldens, preserve full source
universes, check qualified Python/Rust identities, wrong lines/quotes/names,
normal unsupported syntax, ambiguous truth, attribution/disagreement/adjudication,
missing telemetry, failed episodes, setup refusals, missing/duplicate attempts,
malformed raw captures, fake-live refusal, counterbalancing/repetitions and
precision refusal. Synthetic fixtures are never effectiveness evidence.
Run the complete repository sequence in `CONTRIBUTING.md` as well; ignored live
plugin conformance remains a maintainer operation. Keep logs/source/review scratch
under `.orbit/tmp/`, graph caches under `.orbit-graph/`, and leave changes uncommitted.

Three concentrated Bluesky cases were replaced source-first by ledger state aggregation,
error serialization and credential-value declaration problems. The primary bank stays
24 cases (four/family, eight Python and sixteen Rust); full universes are unchanged.
Component/required-identity overlap is explicit in corpus.json and source-review.md.
Root source/semantic/diff reviews live in external task artifacts; without them the
default remains pending. The final CLI verified all 108 external records against
unchanged source packets: 24 admitted cases, 48 independently verified identities
with automatic coverage still unsupported, and four independently verified diffs.
Runtime admission remains closed.
