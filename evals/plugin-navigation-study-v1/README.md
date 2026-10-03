# Fresh installed-plugin qualification, version 1

This is a runnable **offline preparation**, not an effectiveness result and not
a successful follow-up to the old 24+16 episodes. No provider measurement, source
transfer to a provider, live plugin grant, or runtime qualification is performed
by this helper. The operator owns approval and later execution.

`corpus.json` freezes six new source-reviewed questions, required identities,
module/owner aliases, semantic claims and exact source ranges. `protocol.json`
freezes limits, split, order, metrics, failure policy, limitations and the
follow-up consideration rule. `corpus.lock.json` binds both files, every selected
file's SHA-256 and Git blob/mode, original commits/trees, content revisions and
read-only helper dependencies. Its canonical SHA-256 is pinned in `eval.py`.
There is no re-lock command. Any change after exposure to results requires a new
study version/cohort, including a proposed improvement to the rubric.

| Cases (in pair order) | Split | Original source |
| --- | --- | --- |
| graph-live-source-bounds; graph-trait-name-resolution | development | orbit-graph `ac6d5b91f973874325bac789e9c433a71c805766` |
| orbit-artifact-text-policy; orbit-reverse-log-boundaries | held-out | Orbit `0db9356eef631ada11569d0ca4e1e407fad5bee3` |
| pulsar-read-metering; pulsar-text-weight-normalization | held-out | pulsar `6d33fb9d4609618c13d46deb2df6af53b7f504a6` |

Questions were selected by reading these source blobs before graph queries or
provider answers. They address different behavior from the previous questions:
query I/O bounds, structural trait matching, artifact sanitization, reverse log
boundaries, read metering and Unicode text validation. There is no history case.
Graph is development-only; Orbit and pulsar are held-out-only in this study.
These repositories were examined in earlier work: this is **not** evidence of
unseen-repository generalization. Six purposive pairs support descriptive
observations, no superiority, p-value or generalization claim.

Pairs alternate baseline-first and graph-first (three each), with one episode
per arm/case, no replacements or retries in the cohort. Both arms receive
identical questions and answer format, settings, source views and budgets.
Neither prompt requires graph usage. The treatment is exactly the delivered
[installed-plugin-skill-v2 profile](../plugin-agent-navigation/README.md),
including its measured cold private installation and shipped skill context.
Index construction remains an agent choice. Limits: 300000 ms total wall,
60 calls, 1048576 returned tool/final bytes, 65536 bytes/call, 32 answer items,
4096 UTF-8 reason bytes. Setup has a 60000 ms / 8388608 byte cap, with its time
inside total wall and its bytes reported separately. No silent clamping.

## Source validation and export (offline)

Python 3.10+, Linux and `/usr/bin/git` are required. Use explicit physical Git
checkout roots, not parent discovery, bare repositories or worktree contents.
The helper imports the existing source exporter primitives and profile evaluator
read-only; historical corpus files and reports are never rewritten.

```sh
EVAL=evals/plugin-navigation-study-v1/eval.py
GRAPH_REPO=/home/daniel/workspace/constellation/codebases/orbit-graph
ORBIT_REPO=/home/daniel/workspace/constellation/codebases/orbit
PULSAR_REPO=/home/daniel/workspace/constellation/codebases/pulsar
STUDY="$PWD/.orbit/tmp/plugin-study-v1"
mkdir -p "$STUDY"
python3 -B "$EVAL" validate --repo "orbit-graph=$GRAPH_REPO" \
  --repo "Orbit=$ORBIT_REPO" --repo "pulsar=$PULSAR_REPO"
python3 -B "$EVAL" export --repo "orbit-graph=$GRAPH_REPO" \
  --repo "Orbit=$ORBIT_REPO" --repo "pulsar=$PULSAR_REPO" \
  --destination "$STUDY/source"
STUDY_SOURCE_ROOT=/home/daniel/workspace/constellation/codebases \
  python3 -B -m unittest discover -s evals/plugin-navigation-study-v1 -p test_eval.py -v
```

Exports retain the historical narrow allowlist (root Cargo/Python manifests,
Rust/Python source and Cargo manifests under `src/`, `crates/`, `tests/`) and
caps (4 MiB/blob, 128 MiB/tree, 10000 files), and additionally require the shipped
profile's source-path guard. Both guards must agree; there is no silent removal.
Hidden paths, documentation/instructions/skills, eval/truth/answers, generated,
vendor and credential directories are excluded. Source regression tests remain,
including inert credential-looking test strings. This is not a full runnable
reconstruction of each project. Pinned exports contain 241 graph, 2242 Orbit,
and 124 pulsar files. No ambient config, original history or working-tree data
is copied. Replacement Git objects are disabled.

The source-only directories are `source/orbit-graph`, `source/Orbit`, and
`source/pulsar`. Manifests and `complete.json` are outside them. Only those three
source roots may be supplied as head/base views; all truth, locks, manifests,
review packets, requests and runtime freeze files stay outside agent mounts.
The runner creates a fresh repository, HOME, session and graph cache for every
episode even though the immutable input view is shared. Original commit/blob
IDs are distinct from the runner's deterministic synthetic snapshot commits.
Content revision is SHA-256 of sorted compact `ensure_ascii=True` JSON mapping
relative paths to UTF-8 text; request/lock seals use the same canonical encoding.

Destinations must be new, with an existing physical parent. Outputs are private,
exclusive-create files. `complete.json` is written last; interrupted exports
are retained without completion and cannot be frozen or overwritten. Choose a
new destination after investigating a failure. Hashes prove local integrity,
not authentication or historical creation time.

## Operator runtime freeze and admission

The shipped corpus/protocol are frozen; **runtime pins are not filled in**.
Before any live episode the root operator must qualify the final repaired graph
binary (including the independently tracked concurrent-first-sync repair), and
approve an exact model, credential mount, source transfer plan, budget and run
plan. Keep the historical source commits above unchanged. Do not qualify a
runtime by silently relaxing confinement or borrowing old capture evidence.

After that approval, first inspect the real installed treatment without a model:

```sh
RUNNER=scripts/agent-eval/eval_runner.py
# Set absolute reviewed binaries and a full immutable plugin source commit.
export ORBIT_BIN=/absolute/orbit GRAPH_BIN=/absolute/orbit-graph
export PROVIDER_BIN=/absolute/codex RG_BIN=/absolute/rg
export PLUGIN_COMMIT=FULL_40_HEX_COMMIT
python3 -B "$RUNNER" plugin-inspect --plugin-repo "$GRAPH_REPO" \
  --plugin-commit "$PLUGIN_COMMIT" --orbit "$ORBIT_BIN" \
  --orbit-graph "$GRAPH_BIN" --out "$STUDY/inspection" \
  > "$STUDY/inspection.json"
```

Use strict containment (the default). Inspection does not start a provider or
build an index. Its sealed `plugin-treatment.json` must remain at the reported
`out` path. `freeze` verifies its seal and exact inventory/pin correspondence.
Never substitute guessed inventory/tool/skill hashes. The installed profile
exports only into a new private host; do not install into the user's live host.

Create `$STUDY/runtime.json` with exactly these fields, replacing the descriptions
with observed values. Strings called SHA256 are full lowercase 64-hex hashes,
computed from actual executable bytes. This is a schema template, not a valid
preregistration. All settings must be the runner's recorded dictionary, e.g.
`{"model_reasoning_effort":"high"}`. Use the same settings in every invocation.

```json
{
  "schema_version": 1,
  "study_kind": "agent",
  "operator": "accountable operator identity",
  "frozen_at": "RFC3339 UTC seconds",
  "approval_reference": "external approval and qualified-runtime evidence",
  "model": {"provider":"codex-cli", "name":"approved model", "version":"exact provider --version stdout", "settings":{"model_reasoning_effort":"high"}},
  "provider_binary_sha256": "SHA256",
  "harness": {"eval_runner.py":"SHA256", "eval_broker.py":"SHA256", "plugin_profile.py":"SHA256"},
  "baseline_tool_versions": {"read":"agent-eval-broker 1 sha256:SHA256", "rg":"exact runner-normalized version sha256:SHA256", "git":"exact git --version stdout sha256:SHA256"},
  "resource_limits": {"memory.max":4294967296, "pids.max":256},
  "binary_paths": {"provider":"/absolute/codex", "orbit":"/absolute/orbit", "backend":"/absolute/orbit-graph", "git":"/usr/bin/git", "rg":"/absolute/rg", "python":"/usr/bin/python3", "bwrap":"/usr/bin/bwrap"}
}
```

Use observed finite cgroup limits, not the example numbers. The read version
string is `eval_broker.BROKER_NAME + ' ' + eval_broker.BROKER_VERSION +
' sha256:' + sha256(eval_broker.py)`; rg is its `--version` stdout stripped and
split at the first ` (`; Git is stripped `--version`. These are the shipped
runner's `tool_versions` rules. Hash all three harness files under
`scripts/agent-eval/`. `freeze` checks local executable/harness hashes, inventory,
source bytes and all twelve complete requests with the delivered validator.
Subsequent adaptation checks captured model/settings, read/rg/git versions and
finite limits against this freeze, rather than merely checking equality between
arms. All study files are also frozen by hash. Archive the freeze externally
before any provider output; hashes alone cannot establish ordering or custody.

```sh
python3 -B "$EVAL" freeze --runtime "$STUDY/runtime.json" \
  --inspection "$STUDY/inspection.json" --export "$STUDY/source" \
  --destination "$STUDY/frozen"
```

The output is `plan.json` in the delivered evaluator's exact schema, twelve
`request-NN.json` files, and `freeze.json` written last. No placeholders are
produced as supposedly frozen requests. For deliberately fake captures use
`study_kind:test-only`; diagnostic containment never becomes agent evidence.
Known fake provider versions/names are rejected for agent freezes. External
operator custody remains necessary: a checksum cannot identify an adversary
who fabricates an entire sealed evidence set.

## Run, adapt, review and score (operator only)

Run the strict fake-provider admission suite against the approved binaries
before live execution. It must have zero skips. This calls only the existing
local fake provider with synthetic fixture source:

```sh
AGENT_EVAL_REQUIRE_BWRAP=1 AGENT_EVAL_ORBIT="$ORBIT_BIN" \
AGENT_EVAL_ORBIT_GRAPH="$GRAPH_BIN" \
  python3 -B -m unittest discover -s scripts/agent-eval/tests -p test_plugin_profile.py -v
```

Strict schema-2 `run` performs its own final-context feature/MCP preflight.
Standalone schema-1 `preflight` is not a substitute. Capability refusal before
an episode exists is a blocked cohort, not an observation to replace. Once an
episode exists, retain every success, failure, invalid answer and timeout.
Integrity failures stop scoring; they must not disappear from the cohort.

After explicit live approval, invoke `run` for requests 00 through 11 **once in
numeric order**, to fresh output paths. For each request, read `fixture` to
select its source root and original Git repository; use `arm` to determine
whether graph flags apply. This concrete single-episode example is request 00
(graph source, baseline arm); substitute the matching paths for later requests:

```sh
python3 -B "$RUNNER" run --request "$STUDY/frozen/request-00.json" \
  --head "$STUDY/source/orbit-graph" --base "$STUDY/source/orbit-graph" \
  --source-repo "$GRAPH_REPO" --out "$STUDY/run-00" \
  --codex "$PROVIDER_BIN" --model APPROVED_MODEL \
  --setting model_reasoning_effort=high --auth-file /approved/provider-auth.json \
  --git /usr/bin/git --rg "$RG_BIN" --python /usr/bin/python3 \
  --bwrap /usr/bin/bwrap --containment bwrap \
  --truth-path "$PWD/evals" --truth-path "$STUDY"
# Graph arms additionally: --plugin-repo "$GRAPH_REPO" \
#   --orbit "$ORBIT_BIN" --orbit-graph "$GRAPH_BIN"
```

The original Git source root is verified outside the agent mount. Only the
selected files are materialized. Credentials use the runner's documented
read-only binding, never this helper. Inspect frozen request order rather than
guessing alternation within held-out repositories. Do not parallelize the pairs,
prewarm graph caches, rerun a failure, or change settings midway.

Prepare `custody.json` with exactly `operator`, `attested_at`, `freeze_sha256`,
`captures` (mapping every captured `run_id` to its `artifact_sha256`) and a
nonempty `attestation` stating these are original runner captures, who held them,
and that the freeze preceded execution. Preserve originals and all setup/error
files. Custody text is attributed evidence, not a cryptographic signature.

```sh
raw_args=()
for n in $(seq -w 0 11); do raw_args+=(--raw "$STUDY/run-$n"); done
python3 -B "$EVAL" adapt --frozen "$STUDY/frozen" "${raw_args[@]}" \
  --custody "$STUDY/custody.json" --destination "$STUDY/bundle.json"
python3 -B "$EVAL" review-packets --frozen "$STUDY/frozen" \
  --bundle "$STUDY/bundle.json" --destination "$STUDY/review-packets.json"
```

`adapt` calls the delivered profile replay, retains complete raw artifacts,
errors/setup/skill/tool hashes, and additionally checks selected Git blob sets.
Every score and packet operation replays original captures again. It never
accepts the synthetic dictionaries used in unit tests as raw captures.

Assign packet hashes to a reviewer without arm/tools/timing metadata; answers
may themselves reveal a tool, so record `arm-blinded` or `unblinded` honestly.
Write `audits.json` as `{"schema_version":1,"audits":[...]}` with exactly one
review for each successful raw episode, none for failed outcomes. Bind packet
hash back to `run_id` after review. Each review has exactly:

- `run_id`, `artifact_sha256`, `reviewer`, `signed_at` (UTC RFC3339 seconds),
  `blinding`, nonempty `attestation`, `judgments`, `supporting_symbols`, `audit_sha256`.
- Every frozen claim appears once in `judgments`, each with `claim_id`, boolean
  `pass`, `rationale`, nonempty verbatim `answer_quote` from the reason, and
  `source_evidence` copied exactly from that claim's frozen evidence. Name-only
  answers do not establish behavior. No helper invents passing judgments.
- For extra symbols outside frozen identities/aliases, `supporting_symbols`
  contains `item`, boolean `accepted`, `rationale`, `declaration` (exact `file`,
  `line`, `quote`), and `identity_evidence` (source range objects with the same
  fields/hash encoding as rubric evidence). Reviewer attests declaration,
  module/owner identity and relevance. Source bytes, declaration spelling,
  module path and ambiguity are checked. Unknown/unsupported items without
  an accepted source review remain incorrect, not automatically fabricated.
  Ambiguous unqualified duplicates cannot be rescued by a guessed alias.

Seal each completed review with `eval.py`'s `seal(review, 'audit_sha256')`.
This is an edit-detection hash, not proof of reviewer identity. Source evidence
hashes cover joined original lines plus a final newline, without stripping;
`quote` is the stripped first line. Supporting methods must include owner
context. Conservatively refused ambiguous additional declarations require a
new prospective reviewed corpus; never revise this oracle on exposed results.

```sh
python3 -B "$EVAL" score --repo "orbit-graph=$GRAPH_REPO" \
  --repo "Orbit=$ORBIT_REPO" --repo "pulsar=$PULSAR_REPO" \
  --frozen "$STUDY/frozen" --bundle "$STUDY/bundle.json" \
  --audits "$STUDY/audits.json" > "$STUDY/report.json"
```

Required identity recall/citation coverage and semantic correctness are separate.
Exact required identities plus module/owner aliases receive equal credit; wrong
files/quotes do not. Accurate reviewed support is allowed without requiring it.
Failures are incorrect in 6-per-arm all, 4-per-arm held-out and 2-per-arm
development denominators. Reports retain every outcome/error, adoption among
all and successful episodes, attempted/successful graph calls, setup and total
wall, calls/bytes, and nullable observed usage with observed/total denominators.
Cached/total tokens are reported only if captured; USD remains null when the
profile does not observe it. Missing values never become zero. Speed ratios and
deltas appear only for both-correct pairs. Setup time is a component of wall,
not an extra term to add twice. Cold index sync time remains a separate component.

The frozen follow-up rule permits **consideration only** of a separately
approved fresh cohort: complete genuine strict evidence, no held-out accuracy
regression, at least one held-out improvement, and no unsupported held-out graph
identities. Test-only fixtures can never satisfy it. Failure to meet the rule
is a result; it is not permission to tune the oracle or repeat exposed cases.

Commands have a 600-second total deadline; each Git read is bounded at 30 seconds.
Successful stdout is JSON, operational errors are JSON on stderr/exit 1, argparse
usage errors exit 2. Exports/freezes/bundles/packets require fresh destinations;
validation/scoring write nothing. Keep scratch/logs under `.orbit/tmp/` and
runner graph caches under its private `.orbit-graph/`. No releases or tags.

For a full offline study smoke after export/build, run the same test suite with
`STUDY_FAKE_PROFILE=1`, `STUDY_EXPORT` pointing at the validated source export,
and `AGENT_EVAL_ORBIT` / `AGENT_EVAL_ORBIT_GRAPH` pointing at local binaries.
It freezes **test-only** requests, uses the shipped local fake provider for all
twelve captures (including actual invalid/failed outcomes), then drives the real
freeze/adapt/packet/score CLI. It retains diagnostic evidence in
`.orbit/tmp/study-profile-smoke/`. It never contacts a provider and cannot prove
strict containment or effectiveness. Without those opt-in inputs that smoke
skips visibly; the deterministic source/oracle tests still run.
