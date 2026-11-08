# Prospective warm navigation v1

Status: proposed evaluation capability; **no measured study output or effectiveness
claim**. Cold profiles 1–3 and their captures remain cold. Root owns independent
review, cold-measurement prioritization, strict host/client rehearsal, approvals,
and any later cohort. None of the commands below authorizes model network calls,
production installation/grants/configuration, publication or releases.

The distinct request schema **5**, `installed-plugin-warm-batch-v1`, runs through
`scripts/agent-eval/eval_runner.py run-batch`. It shares the existing installation,
containment probe, broker, lossless safe replies, pidfd supervision, version-8
raw episode replay and field-qualified schema-3 telemetry. `warm_batch.py` owns
only the batch lifecycle. Broker schema 3 is deliberately unchanged. The ordinary
cold replay route rejects warm artifacts; only batch replay admits them.

## State and experiment contract

Each single-use batch starts empty. Its fixed ordered requests require at least
two unchanged-source tasks followed by a different independently pinned source
view. Every task has a fresh `--ephemeral` model process, HOME/CODEX_HOME/tmp,
runtime helper directory, logs, and selected-source two-commit Git view. No
conversation, provider session, answer, runtime log, hidden warmup or prior batch
state enters another task. The graph tools remain an observed choice under the
unchanged shipped skill; scripted qualification calls are **test-only**.

Only these batch-private roots survive:

| Root | Purpose |
| --- | --- |
| `product/installation` | Pinned Orbit/backend binaries and plugin source export; read-only after initial installation |
| `product/orbit-home/.orbit` | Orbit-created private registration, consent, installation and product databases; plugin graph caches live under `state/plugins/graph` |
| `product/orbit-home/.agents/skills`, `.claude/skills` | Orbit-generated installation projections, only four named skill links into that private `.orbit` tree |
| `product/repo/.orbit` and `.orbit-graph` | Private workspace registration/state and graph scratch; held between tasks under `product/held` |

The source/Git view is archived after each task and never reused. Source paths
remain read-only in strict execution. `.orbit`/`.orbit-graph` remain hidden from
broker reads. Orbit needs its generated private installation resources, registry,
workspace metadata, generation locks and local product state; no Orbit control
plane dependency is introduced into the Cargo workspace. Generated private
resources are retained as data; no routines, jobs or auto-tasks are dispatched.
Installation/plugin/grant/skill/resource bytes are checked for drift across
handoffs. Manifests use `lstat`, reject undeclared roots, special/hard links and
escaping projections, and record each file's SHA-256/size. A source pin, helper,
binary, runtime setting, manifest or provenance mismatch refuses the batch.
No in-place resume or retry replaces an outcome.

Batch evidence includes the full schedule before launch, immutable checkpoints,
original per-task captures, before/after state manifests, source commit/content
and backend/skill/inventory identities, every sync attempt and complete product
facts (including extractor/schema/database identity when reported). An index's
`fresh` flag and repeated invocation **do not prove a cache hit**. The present
product has no qualified hit/miss signal, so `cache` is `unknown`; raw sync
`seeded_from_published`, publication and stale-index facts remain separate.

Seals detect drift; they are not authentication against a same-user editor who
can rewrite evidence and seals. Uncontained execution is an accident guard and
is accepted only with an explicit diagnostic flag, never as effectiveness
or OS containment evidence. Strict replay requires the original successful
in-sandbox probe and finite cgroup process/memory limits.

## Offline preparation and qualification

The committed `fixtures/` and `corpus.lock.json` are source-only, test-only truth,
authored before candidate queries. The export verifies their hashes, creates
reproducible Git commit pins, and never copies a solution or upstream history.
All output destinations must be new and have an existing parent. Use
`.orbit/tmp/` for run evidence and perform the task's disk gate before builds.

```sh
python3 -B evals/warm-navigation-v1/eval.py export --destination .orbit/tmp/warm-export
python3 -B scripts/agent-eval/eval_runner.py plugin-inspect \
  --plugin-repo "$PWD" --plugin-commit "$(git rev-parse HEAD)" \
  --orbit /absolute/path/to/orbit --orbit-graph "$PWD/target/debug/orbit-graph" \
  --containment none --out .orbit/tmp/warm-inspection
```

Inspection is a disposable private real install, not a host install or a provider
call. It produces a `pin` and exact inventory/skill/setup evidence. The integration
tests drive export, inspection, freeze, batch execution, replay, and scoring with
the real installed plugin and fake provider:

```sh
AGENT_EVAL_ORBIT=/absolute/path/to/orbit \
AGENT_EVAL_ORBIT_GRAPH="$PWD/target/debug/orbit-graph" \
  python3 -B -m unittest discover -s evals/warm-navigation-v1 -p 'test_*.py' -v
```

The tests cover cold-to-warm reuse, stale source then incremental invalidation,
empty-state semantic parity including a newly added symbol, equal paired controls,
all-failed and partially refused batches, cancellation, missing usage/total,
wrong backend identity, source-export drift, undeclared state and tampered
cache/provenance. CI runs this diagnostic integration in `plugin-conformance`.
They never invoke a measured provider. The existing runner suite supplies the
shared broker's containment, permission, output-limit and process-cleanup controls.

## Freeze, export and scoring interfaces

`fixture-slots --export DIR --pin PIN_JSON --arm baseline|graph` emits the
qualification schedule (the pin file contains the inspection's `pin` object).
For a later cohort, root supplies independently source-reviewed requests and
truth instead. The generic `freeze --spec SPEC_JSON --destination PLAN_JSON`
accepts exactly these spec fields:

- `study_kind`: `test-only` or separately approved `agent`.
- `batch_id`, `arm`, `slots`: unique batch ID, arm and ordered objects containing
  `request`, `head_view`, `base_view`. View names select code-only directories
  under the exported source root. Requests pin both upstream commits, both
  content revisions, exact prompt, budgets, tools and plugin identity.
- `runtime`: `model` (`provider`, `name`, reported CLI `version`, `settings`),
  `provider_binary_sha256`, `harness` (all six files in `warm_batch.HARNESS`),
  `binaries` (SHA-256 for `codex`, `orbit`, `orbit_graph`, `python`, `git`, `rg`,
  `bwrap`; null only for unused bwrap), `settings`, `containment`,
  `allow_code_mode`, `provider_env`, `resource_limits` (`memory.max`, `pids.max`),
  and `approval_reference`. An `agent` freeze requires strict containment,
  finite limits, a real client version and a non-pending approval reference.

Freeze includes the policy, protocol, fixture lock, study implementation and
all helper/binary/runtime/source pins before any task output. A frozen file is
never edited; drift requires a **new prospective batch**, preserving the old
attempt. Prepare **all** batch plans before the first launch. `pair --left
BASELINE_PLAN --right GRAPH_PLAN` verifies identical ordered questions, source
views, Git visibility, pins, settings and budgets, allowing only the declared
arm/tool/skill treatment differences.

`run-batch` takes `--plan PLAN_JSON --export SOURCE_VIEWS --out NEW_BATCH_DIR`
plus the shared runner's binary/model/source/plugin/containment options. It
uses the same upstream source repository for commit verification. The provider
CLI is only started after pin/installation/surface checks. Model credentials,
if separately approved for a later study, use the original runner's read-only
credential mount; this work performs no auth write.

```sh
python3 -B evals/warm-navigation-v1/eval.py replay --batch BATCH_DIR --diagnostic
python3 -B evals/warm-navigation-v1/eval.py score --batch BATCH_DIR \
  --audits AUDITS_JSON --diagnostic
```

Omit `--diagnostic` for strict evidence. Audits contain `schema_version: 1`,
`freeze_sha256`, and `reviews` for exactly the successful slots. Each review has
`slot`, original `artifact_sha256`, boolean `correct`, `reviewer`, and nonempty
`source_evidence`. Root must supply independent source-based review; the
qualification's scripted positive audits are test-only. Success is not correctness.

The score retains every scheduled slot and per-slot original telemetry. Primary
outputs are all-attempt correctness, measured batch wall and tool bytes. Missing
capture bytes yield a null measured total plus partial observed bytes. Batch
usage/cost totals stay null. Per-field usage retains coverage, diagnostic sums,
unknown counter scope/cache semantics, and raw event provenance; input+output
never creates a total and cached counters never create an uncached estimate.

One batch wall starts before initial layout/installation and ends after the
last slot's finalization/checkpoint; it includes preflights, verification, source
materialization, initial index choice, later syncs and failures. Per-task setup,
provider, installation, preflight and sync spans **overlap** this wall and are
not added to it. Amortized wall divides by **all scheduled slots**, including
refused/not-run slots. Source export, prior runtime qualification and independent
review are outside batch wall, separately unknown unless measured independently.
A refused batch retains partial setup logs; unavailable setup bytes stay null.
A cancelled batch finalizes the current supervised task, marks remaining slots
not-run and never launches them. Abrupt SIGKILL leaves checkpoints, not a valid
final batch; replay refuses missing completion evidence rather than guessing.

## Root-owned strict and client rehearsal

Before actual study output, root reviews the protocol/lock/source truth, pins the
real client/binaries/helpers/model/settings, approves source/provider transfer,
and supplies independent review. Rehearse with fake provider first on a Linux
host with user namespaces and finite cgroup limits. This required strict route
fails for missing binaries/capabilities instead of silently skipping:

```sh
WARM_REQUIRE_STRICT=1 AGENT_EVAL_ORBIT=/absolute/path/to/orbit \
AGENT_EVAL_ORBIT_GRAPH="$PWD/target/debug/orbit-graph" \
  python3 -B -m unittest discover -s evals/warm-navigation-v1 -p 'test_*.py' -v
```

A root-owned release checklist must run this route before admitting the profile
for study use (STD-04 §R8). Diagnostic passes cannot substitute for it. The actual
client rehearsal then uses the same `run-batch` surface with independently frozen
agent plans and approved credentials; it is not run by this worker. After those
approvals and freeze checks, root's command template is:

```sh
python3 -B scripts/agent-eval/eval_runner.py run-batch \
  --plan FROZEN_AGENT_PLAN --export SOURCE_ONLY_VIEWS --out NEW_BATCH_DIR \
  --source-repo PINNED_SOURCE_REPOSITORY --plugin-repo "$PWD" \
  --codex /absolute/path/to/codex --model FROZEN_MODEL \
  --orbit /absolute/path/to/orbit --orbit-graph /absolute/path/to/orbit-graph \
  --rg /usr/bin/rg --containment bwrap --truth-path INDEPENDENT_TRUTH_DIR \
  --auth-file /absolute/path/to/provider/auth.json
```

Pass exactly the settings/code-mode/provider-environment controls frozen in the
plan. `SOURCE_ONLY_VIEWS` contains the plan's named source directories, not truth.
The template's capitalized arguments and binary paths require root substitution;
it is not authorization to run a provider.

For a later cohort, use the frozen order `baseline, graph, graph, baseline`,
all four fresh batches on one host boot, no host cache flushing or selective
warmup. Record competing load/cache limitations. Check chronological, disjoint
batch boundaries and identical paired controls with:

```sh
python3 -B evals/warm-navigation-v1/eval.py cohort-check \
  --capture BASELINE_1 --capture GRAPH_1 --capture GRAPH_2 --capture BASELINE_2
```

This gate refuses incomplete batch lists, reused identities, reordered or
overlapping batch clocks and unequal controls. It reports incomplete outcomes
without dropping them. OS page-cache contents remain uncontrolled; counterbalanced
order reduces exposure bias and never certifies equal cache contents.
