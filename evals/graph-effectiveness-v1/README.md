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
call a provider, change configuration, or publish. The delivered schema-3 runtime is now pinned as `graph-effectiveness-runtime-v2`
(runner 6, broker 5, `prospective-accounting-v1`). The lock retains the original v2
development preparation bindings and their immutable commit/hash. No historical
corpus, report or capture is rewritten. Live admission requires complete independent
truth review, strict root namespace/client qualification and a new operational
freeze with externally recorded operator custody. Preparation grants no provider
or source-transfer authority. Each original capture is replayed independently,
even in a cohort with setup refusals; test-only permits uncontained fixtures and
agent admission always uses strict confinement/resource checks.

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

Requests are validated directly as schema 3 by the delivered shared validator.
There is no schema-2 validation followed by relabeling. Independent root runtime
qualification and an operational freeze are required before live execution. The pin
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

Original installed-plugin capture adaptation requires schema 3,
`installed-plugin-skill-v3`, runner 6 / broker 5 and `prospective-accounting-v1`.
The shared capture loader rechecks file hashes, ledger, provider receipts, safe-text,
lifecycle, normalized usage, timing and outcome for **every non-refused slot**.
Test-only replay relaxes only confinement/resource qualification; it never becomes
live evidence. Live replay uses strict confinement/resource gates even for failed
captures and mixed cohorts. Schema-2 relabels, changed source/runtime pins, forged
ledgers and fixture-as-live input refuse. Missing/duplicate schedule slots refuse.

Setup refusals have exactly `order,status:"setup_refused",error:{code,message},
operator,evidence,elapsed_ms`. Adaptation preserves each record unchanged; the
operator externally records the refusal and its custody. No episode is fabricated.
Failed, truncated, cancelled or invalid original outcomes remain as replayed
outcomes, never success. Missing answer/truth judgments remain unknown; no quality
slot is discarded for incomplete usage. Reviewer tokens and the private registry
stay stable when supplied at scoring, including identical repeated answers.

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
observed/missing independently qualified counters and timings. Schema-3 per-field coverage, diagnostic sums and nullable qualified totals stay
separate. `paired.token_fields` names input, output, cached input, reasoning and total
comparisons separately, with observed/missing pairs and all-pair/all-case
denominators. Legacy paired token fields refer only to qualified `total_tokens`.
Absent totals remain null even when input/output or multi-turn `observed_sum` are
available. Multi-turn/cache semantics remain unqualified; uncached input and
unqualified totals are null. Reasoning/cached counters are never added to other
counters without a qualified semantic contract. All tool attempts and overlapping
versus outside-wall timing observations are preserved separately. Normalized
`wall_ms=setup_ms+provider_ms`: install overlaps setup, graph sync overlaps provider.
For example, wall 27734 = setup 1634 + provider 26100; install 1012 and sync 660
are not added again. `preflight_outside_wall_ms` comes from
`telemetry.timing.outside_wall.preflight_ms`; `preflight_within_setup_ms` comes from
`overlapping.preflight_within_setup_ms`, never the legacy scalar preflight clock.

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
GRAPH_EFFECTIVENESS_ORBIT=/absolute/path/to/orbit \
GRAPH_EFFECTIVENESS_GRAPH=/absolute/path/to/orbit-graph \
GRAPH_EFFECTIVENESS_CODEBASES="$CODEBASES" \
  python3 -B -m unittest discover -s evals/graph-effectiveness-v1 -p 'test_*.py' -v
```

Original-capture controls require those explicit binary/root variables and fail
when missing; they install only under disposable `.orbit/tmp/` roots and use the
shipped fake provider, never a model or live user installation. Golden updates
require `UPDATE_GRAPH_EFFECTIVENESS_GOLDENS=1` on the focused test command; review
the resulting `cli-goldens.json` diff. The tests drive the real CLI, pin all help/error goldens, preserve full source
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

## Root operational freeze and one serial paired development cohort

These are operator instructions, not authorization for the implementation worker
to dispatch a provider. One repetition includes all 24 development cases and 48
serial baseline/product slots. Use a new private `STUDY_DIR` for each cohort; freeze
before any output. Source/answers/reviews/protocol and qualification-only captures
stay outside every mount. The original qualification captures named in the task
are never measured-cohort evidence. No warm treatment or ablation is available.

1. Run the preparation/export commands above with `--repetitions 1`. Build the
   pinned identity frontend after the disk gate. Independently finish all truth
   identity, semantic and diff reviews; retain both initial verdicts and any third
   adjudication. `study validate --export "$STUDY_DIR/source" --truth-reviews
   "$STUDY_DIR/truth-reviews.json"` must show 24 admitted cases and verified diffs.
2. Root independently runs strict namespace/client qualification of the delivered
   runner with the exact installed client/backend/Orbit/bubblewrap binaries,
   model/settings, resource ceilings, source boundary and full inventory/skill.
   Record actual commands, exit statuses and original logs. Inability to create a
   namespace or observe finite ceilings is a capability failure, never a pass.
   Root can repeat the shared fake-provider strict host control with explicit
   binaries (the resulting captures remain qualification-only):

   ```sh
   AGENT_EVAL_ORBIT=/absolute/path/to/orbit \
   AGENT_EVAL_ORBIT_GRAPH=/absolute/path/to/orbit-graph \
   AGENT_EVAL_REQUIRE_BWRAP=1 \
   python3 -B -m unittest discover -s scripts/agent-eval/tests \
     -p test_telemetry.py -k test_strict_prospective_host_qualification -v
   ```

   This is a fake-provider namespace control, not complete qualification of a real
   client/model. Root separately attests the real client and records it in the
   qualification document. Use an existing approved treatment pin (or root's
   authorized private strict `eval_runner.py plugin-inspect` inspection); preparation
   does not mutate the live plugin installation.
3. Write `model-pin.json` with exactly `model:{provider:"codex-cli",name,version,
   settings}` and `provider_binary_sha256`. `version` is the exact installed
   client's `--version`; settings are the explicit shared runner `--setting`
   values (plus `features.code_mode_host:"enabled"` only if separately qualified
   and dispatched with `--allow-code-mode`). Hash the actual provider executable.
   The shared runner reports this client/model identity; it does not prove an
   immutable server-side weights revision. Bind that limitation in root custody.

   ```sh
   python3 -B "$EVAL" study requests --plan "$STUDY_DIR/plan.json" \
     --pin "$STUDY_DIR/treatment-pin.json" --model-pin "$STUDY_DIR/model-pin.json" \
     --destination "$STUDY_DIR/prepared-requests.json"
   python3 -B - "$STUDY_DIR" <<'PY'
   import json, os, sys
   from pathlib import Path
   root=Path(sys.argv[1]);prepared=json.loads((root/'prepared-requests.json').read_text())
   with (root/'request-plan.json').open('x',opener=lambda p,f:os.open(p,f,0o600)) as f:
       json.dump(prepared['request_plan'],f);f.write('\n')
   PY
   ```

The external qualification document has exactly `schema_version:1`, `operator`,
`recorded_at` (UTC RFC3339 seconds), `method:"strict-schema3-namespace-client"`,
`verdict:"qualified"`, `request_plan_sha256`, `lock_sha256`, `tool_versions`
(the exact observed `read`, `rg`, `git` strings including binary SHA-256),
`resource_limits:{"memory.max":positive_integer,"pids.max":positive_integer}`,
`bwrap:{version,sha256}`, `identity_frontend_sha256`, nonempty `evidence:[{path,
sha256}]`, and `qualification_sha256`. Evidence paths are absolute regular files,
at most 64 MiB each, outside agent mounts. Pin the identity frontend at
`.orbit/tmp/source-identity-syntax-target/debug/source-identity-syntax`.

Custody has exactly `schema_version:1`, `study_kind:"agent"`, `operator`,
`recorded_at`, `plan_sha256`, `request_plan_sha256`, `truth_reviews_sha256`,
`qualification_sha256`, `evidence:[{path,sha256}]`, `custody_sha256`, and
`attestations` with every following field explicitly true:
`schedule_frozen_before_output`, `truth_review_identity_independently_attested`,
`serial_dispatch`, `original_capture_custody`, `source_transfer_authorized`,
`non_synthetic_provider`. Root signs/records the corresponding assertions through
its independent external process and retains that record in evidence. Document
reviewer identity verification and temporal custody there, not just display names.
Do not sign assertions for synthetic passing reviews or unqualified controls.

Use the study's existing `source.digest` for document hashes (canonical JSON,
not file SHA-256), and `source.seal(document, 'qualification_sha256')` /
`source.seal(document, 'custody_sha256')` before writing the respective private
JSON files. Qualification precedes custody; every truth review precedes custody.
For example, load the study API with `importlib.util.spec_from_file_location`
under the name `graph_effectiveness`, register it in `sys.modules` and execute
its loader as `test_eval.py` does, then call these shared helpers. File evidence
uses ordinary SHA-256 of bytes. Seal the qualification before binding its digest
in custody. This records evidence, not permission or authentication.

Code checks exact document/source/helper/scorer/runtime/model/treatment/binary
pins, complete attributed source reviews, evidence bytes, timestamp ordering,
and strict original replay. **Hashes do not authenticate a reviewer/operator or
prove temporal custody or that qualification commands actually ran.** Those facts
are independently externally attested by root. A structurally valid external
assertion alone cannot promote a fake/uncontained capture: original replay still
refuses it. The default remains closed without all these inputs.

```sh
python3 -B "$EVAL" study freeze --plan "$STUDY_DIR/plan.json" \
  --request-plan "$STUDY_DIR/request-plan.json" --export "$STUDY_DIR/source" \
  --truth-reviews "$STUDY_DIR/truth-reviews.json" \
  --qualification "$STUDY_DIR/qualification.json" --custody "$STUDY_DIR/custody.json" \
  --destination "$STUDY_DIR/freeze.json"
```

After root's authorization and freeze, the following recipe dispatches the entire
48-slot cohort serially through the shared runner and adapts it. It contains no
new harness. Root first writes private `operator-paths.json` with explicit absolute
paths for `codex`, `orbit`, `orbit_graph`, `rg`, `git`, `python`, `bwrap`, `auth_file`,
and `source_roots` mapping each of the four repository names to its original Git
root. Root's bounded job runtime owns this recipe and its descendants. It never
retries a slot. Captured failed/invalid/timeout episodes remain outcomes. Only an
explicit shared-runner refusal with `provider_started:false` becomes a setup
refusal, preserving its original response/log as evidence. Other uncertainty
stops the recipe for investigation and retains all outputs.

```sh
python3 -B - "$EVAL" "$STUDY_DIR" <<'PY'
import importlib.util, json, sys, time
from pathlib import Path
spec=importlib.util.spec_from_file_location('graph_effectiveness',sys.argv[1])
ev=importlib.util.module_from_spec(spec);sys.modules[spec.name]=ev;spec.loader.exec_module(ev)
root=Path(sys.argv[2]).resolve(strict=True)
c,p,lock=ev.documents();plan=ev.load(root/'plan.json');rp=ev.load(root/'request-plan.json')
freeze=ev.load(root/'freeze.json');paths=ev.load(root/'operator-paths.json')
views=ev.check_export(c,lock,root/'source')
ev.runtime.checked_freeze(ev,freeze,plan,rp,c,p,lock,root/'source',views)
cases={case['id']:case for case in c['cases']}
raw,refusals=[],[]
for slot,request in zip(plan['slots'],rp['requests']):
    order=slot['order'];case=cases[slot['case_id']]
    req=root/f'request-{order}.json';out=root/f'run-{order}'
    ev.write(req,request)
    argv=[paths['python'],'-B',str(ev.REPO/'scripts/agent-eval/eval_runner.py'),'run',
          '--request',str(req),'--out',str(out),'--head',str(root/'source/views'/case['head_view']),
          '--base',str(root/'source/views'/case['base_view']),
          '--source-repo',paths['source_roots'][case['repository']],'--plugin-repo',str(ev.REPO),
          '--codex',paths['codex'],'--model',rp['model']['name'],'--rg',paths['rg'],'--git',paths['git'],
          '--python',paths['python'],'--orbit',paths['orbit'],'--orbit-graph',paths['orbit_graph'],
          '--containment','bwrap','--bwrap',paths['bwrap'],'--auth-file',paths['auth_file'],
          '--truth-path',str(root),'--truth-path',str(ev.REPO/'evals')]
    for key,value in rp['model']['settings'].items():
        argv += ['--allow-code-mode'] if key==ev.runner.CODE_MODE_FEATURE else ['--setting',key+'='+value]
    started=time.monotonic()
    result=ev.runner.broker.run_child(argv,str(root),
        dict(PATH='/usr/bin:/bin',HOME=str(root),LC_ALL='C.UTF-8',TZ='UTC',PYTHONDONTWRITEBYTECODE='1'),
        600,capture_limit=64*1024*1024)
    elapsed=int((time.monotonic()-started)*1000)
    evidence=root/f'dispatch-{order}.json';ev.write(evidence,ev.plugin.evidence(result))
    ev.require(result['stopped'] is None and not result['stderr_truncated'] and
               result['supervision']['survivors']==[], 'dispatch supervision incomplete; inspect original evidence')
    if (out/'episode.json').is_file():
        raw.append(str(out))
    else:
        report=ev.source.parse(result['stdout'])
        ev.require(report.get('status')=='refused' and report.get('provider_started') is False and
                   report.get('request_digest')==ev.runner.digest(request),
                   'no original capture or attributable setup refusal; investigate, never retry silently')
        refusal=report['refusal']
        refusals.append(dict(order=order,status='setup_refused',error={k:refusal[k] for k in ('code','message')},
            operator=freeze['custody']['operator'],evidence=dict(path=str(evidence),sha256=ev.sha(evidence.read_bytes()),
            original_refusal=report),elapsed_ms=elapsed))
ev.write(root/'refusals.json',refusals)
bundle=ev.adapt(plan,rp,raw,refusals,lock,c,'agent',freeze,root/'source',views)
ev.write(root/'bundle.json',bundle)
PY
python3 -B "$EVAL" study review --plan "$STUDY_DIR/plan.json" \
  --export "$STUDY_DIR/source" --bundle "$STUDY_DIR/bundle.json" \
  --destination "$STUDY_DIR/answer-packets.json"
python3 -B "$EVAL" study score --plan "$STUDY_DIR/plan.json" \
  --export "$STUDY_DIR/source" --bundle "$STUDY_DIR/bundle.json" \
  --review-packets "$STUDY_DIR/answer-packets.json" \
  --reviews "$STUDY_DIR/answer-reviews.json" \
  --destination "$STUDY_DIR/report.json"
```

For separate manual adaptation, call `study adapt --plan PLAN --request-plan RP
--study-kind agent --freeze FREEZE --export SOURCE --raw RUN` with a repeated
`--raw` for every original, `--refusals REFUSALS --destination BUNDLE`. Missing
slots refuse; pass `[]` refusals when every slot has an original. Preserve cancelled
and interrupted originals and every tool attempt. Root externally attests dispatch
order and custody; adaptation validates the complete slot set and per-capture
start time after freeze, not an independently trusted event clock.

Distribute only `answer-packets.packets`, retain the private bindings, complete
independent blinded review/adjudication, then score. Agent bundles carry frozen
truth reviews; an explicit `--truth-reviews` must match those bytes exactly.
Review and score recheck the freeze, evidence and every original capture. Preserve
all raw directories and external evidence for that replay. Even admitted live
development reports return `effectiveness_evidence:false` and
`decision:"offline-development-only"`: development observations support planning,
never confirmatory effectiveness, general provider superiority or patch quality.
