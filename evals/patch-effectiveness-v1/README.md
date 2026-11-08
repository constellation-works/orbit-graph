# One-shot patch effectiveness v1 — proposed, qualification only

This extends the existing read-only harness, not agent permissions. Schema 4,
`one-shot-patch-v1`, runner 7 uses the unchanged schema-3 broker, optional full
installed-plugin graph inventory/skill and prospective accounting. Both arms
receive the same pinned full permitted source universe, task specification,
visible tests, patch format/path limits and budgets. The graph arm alone gets
the existing graph treatment. Source-only exports are **not** runnable closures.
Two materialized commits do not recover original chronological history.

The model emits only a bounded unified patch. An external evaluator loads and
replays the original sealed capture after model/broker termination, reconstructs
a fresh private two-commit clone from identical source bytes, checks all patch
headers and declared paths, and applies exact hunks. No headers reach Git or a
shell. Existing regular Python source files only: no additions, deletions,
renames, binary/mode changes, config, hidden tests, evaluator, history or graph
state. Parent directory FDs and final opens use `O_NOFOLLOW`; linked/hardlinked
and changed targets are refused. All replacements are prepared before writing.
Empty patches and context-only edits are tested and reported as no-op.

The initial qualified interface is a stdlib Python **JSON stdin/stdout program**.
Every hidden input gets a fresh process; the trusted host compares JSON against
independently authored expectations. Exit zero alone never means correctness.
The hidden suite, expected values, evaluator and reports stay outside all agent
mounts. Candidate mounts contain only its reconstructed source, a read-only
pinned offline runtime and private proc/dev/tmp namespaces. They contain neither
hidden test code/expected values nor credentials. There is no build/debug or
hidden feedback loop. Rust/Cargo and other closures require a separate future
reconstruction/validator qualification; this implementation refuses missing
closures rather than pretending source exports are buildable.

`fixtures/review.md` records specification-grounded behavioral and regression
obligations written before control patches and fake-provider outputs. These
public synthetic fixtures are test-only. A different root-appointed author and
reviewer must own **real** development truth before any model output. This
worker's fixture authorship is not independent real-task custody.

## Offline checks and root qualification

From the repository root (Python 3.10+ and Linux):

```sh
python3 -B -m unittest discover -s evals/patch-effectiveness-v1 -v
python3 -B -m unittest discover -s scripts/agent-eval/tests -v
python3 -B evals/patch-effectiveness-v1/check_lock.py
```

The first command runs the real runner, real broker, deterministic source
materialization, fake provider and original-capture replay. Correct, wrong,
no-op, partial, malformed/invalid-UTF-8, unsafe-path, symlink, bad-context, timeout, bounded
output, custody drift and capture tampering controls are assertions. Diagnostic
execution is for these manually reviewed trusted controls only; it offers no OS
security and can never yield an effectiveness rate. Do not run arbitrary model
patches with `--diagnostic` or `--containment none`.

Before **each** new worktree, target, runtime or build fixture, run
`df --output=pcent <parent-dir> | tail -1`; at >=80% create nothing and follow
RB-11. The builder and evaluator also enforce this threshold. Put all generated
qualification material under `.orbit/tmp/`; use fresh private directories.

Root performs strict qualification on a host with unprivileged user namespaces
and finite enclosing cgroup-v2 `memory.max` and `pids.max`, with no model calls:

```sh
mkdir -m 700 .orbit/tmp/patch-qualification
# Check disk first. Use system Python; no installs/downloads or inherited packages.
/usr/bin/python3 -B evals/patch-effectiveness-v1/build_closure.py \
  --out .orbit/tmp/patch-qualification/rootfs
# Review rootfs.lock.json, Python identity and exact shared-library/stdlib bytes.
python3 -B evals/patch-effectiveness-v1/eval.py identity \
  --closure .orbit/tmp/patch-qualification/rootfs
AGENT_EVAL_ORBIT="$(command -v orbit)" \
AGENT_EVAL_ORBIT_GRAPH="$PWD/target/debug/orbit-graph" \
PATCH_EVAL_REQUIRE_STRICT=1 \
PATCH_EVAL_CLOSURE="$PWD/.orbit/tmp/patch-qualification/rootfs" \
  python3 -B -m unittest discover -s evals/patch-effectiveness-v1 -v
AGENT_EVAL_REQUIRE_BWRAP=1 \
  python3 -B -m unittest discover -s scripts/agent-eval/tests -k LiveContainment -v
```

With the pinned installed binaries supplied above, strict tests fail instead
of skipping unavailable namespace/closure capabilities. The installed schema-4
paired fake-provider test switches both capture and external evaluation to
strict containment. These tests exercise offline
closure reconstruction/canary, good behavior, timeout, output caps, fork/session
controls and descendant cleanup. The sandbox clears the environment, drops
capabilities, unshares all namespaces (network included), uses read-only
rootfs/source, and sets CPU/address-space/file-size/FD limits. The enclosing
cgroup bounds memory and process counts. Bubblewrap PID namespace teardown
reclaims descendants including new sessions; the existing supervisor bounds
streams/time and sweeps/reaps its process group. No host policy is changed. The configured test output budget covers stdout and
stderr together across all vectors; exhausting it leaves remaining tests
unsuccessful, rather than resetting the budget per vector.
Root must additionally run the real installed-host navigation tests with
`AGENT_EVAL_ORBIT` and `AGENT_EVAL_ORBIT_GRAPH` set as documented in the shared
runner README, and retain the strict schema-4 paired rehearsal logs from the command above
with those exact installed artifacts. Finally root owns an **actual client synthetic**
rehearsal under approved provider authority, outside this worker and outside
any measured cohort. Do not infer that rehearsal from fake-provider tests.

A denied namespace or missing cgroup/closure is an admission failure. Record
strict tests as **not run** when visibly skipped, never as passed. Root's release
qualification checklist above must pass before any runtime admission. This
profile remains proposed pending navigation results and root prioritization.

## Freeze, run, evaluate, report (root only after qualification)

Prepare `seed-plan.json` using the existing schema-3 plan envelope (keys
`schema_version`, `profile`, `model`, `provider_binary_sha256`, `harness`,
`requests`). It contains one matched baseline/graph pair with orders 0/1 and
**patch task** prompts, exact original source commits/content revisions,
installed treatment pins, visible fixtures, tools, limits and setup limits.
Export every permitted src/crates/tests/manifest blob in each pinned view using
the existing source export boundary; schema 4 additionally verifies completeness.
The full source selection is identical across arms. Suite and custody files must
be owned by the operator with mode 0600. Independently specify and
review the private JSON suite with both `behavior` and `regression` vectors
before candidate output. Suite schema is illustrated by `fixtures/suite.json`;
use purpose `independent-development` for real development tasks. Keep the suite
and receipt outside the checkout export, provider installation, `/usr`, system
mounts, agent runtime/state and every ancestor/descendant of those mounts.

```sh
# PRIVATE is an operator-created 0700 directory outside all agent mounts.
python3 -B evals/patch-effectiveness-v1/eval.py freeze \
  --plan "$PRIVATE/seed-plan.json" --suite "$PRIVATE/suite.json" \
  --closure "$PRIVATE/rootfs" --author '<independent author>' \
  --reviewer '<independent reviewer>' --receipt '<reviewed immutable receipt>' \
  --allowed-path src/program.py --entrypoint src/program.py \
  --out "$PRIVATE/freeze"
```

Freeze appends the exact format/bounds to both prompts and binds the prompt,
source revision, suite, closure, evaluator and protocol in a timestamped custody
receipt. It writes exact request files and a new plan including every harness
hash, provider/model/settings pins, exact baseline tool binary hashes, bubblewrap
hash and memory/process ceilings. Admission verifies them before provider output.
Review and externally retain this lock **before** launching either arm;
identities/timestamps/hashes detect drift but do not authenticate who authored
truth, guarantee independence or establish temporal custody. Root verifies the
receipt and access controls separately. Existing profiles/captures and their
locks are never regenerated.

Run each scheduled slot serially with the **existing** runner. For slot N,
set `REQUEST=$PRIVATE/freeze/request-N.json`, `HEAD_EXPORT`/`BASE_EXPORT` to its
pinned solution-free source views, and `CAPTURE` to a fresh episode directory.
`SOURCE_REPO`, `PLUGIN_REPO`, `ORBIT`, `GRAPH`, `CODEX`, `MODEL` and the identical
settings are exactly those root froze/qualified:

```sh
python3 -B scripts/agent-eval/eval_runner.py run \
  --request "$REQUEST" --head "$HEAD_EXPORT" --base "$BASE_EXPORT" \
  --source-repo "$SOURCE_REPO" --plugin-repo "$PLUGIN_REPO" \
  --codex "$CODEX" --model "$MODEL" --setting model_reasoning_effort=medium \
  --rg "$(command -v rg)" --orbit "$ORBIT" --orbit-graph "$GRAPH" \
  --containment bwrap --truth-path "$PRIVATE/suite.json" \
  --patch-suite "$PRIVATE/suite.json" --patch-custody "$PRIVATE/freeze/custody.json" \
  --patch-closure "$PRIVATE/rootfs" --out "$CAPTURE"
# Only after that command has returned and sealed episode.json:
python3 -B evals/patch-effectiveness-v1/eval.py evaluate \
  --episode "$CAPTURE" --head "$HEAD_EXPORT" --base "$BASE_EXPORT" \
  --suite "$PRIVATE/suite.json" --custody "$PRIVATE/freeze/custody.json" \
  --closure "$PRIVATE/rootfs" --out "$ASSESSMENT"
```

Run no evaluator after a refusal; retain the runner's `refusal.json` as that
slot. Never replace an attempt or drop unsuccessful slots. Preserve original
capture directories and byte-exact safe `final-message.txt` patch artifacts.
Redaction, truncated capture or changed identities invalidate provenance;
redacted bytes are never silently treated as an original patch. Reports are
private, bounded, sealed and never fed back into the model. Evaluate never
launches a provider. Self-seals are consistency evidence, not independent
attestation of a hostile same-user process; the operator is trusted and must
keep captures/receipts read-only to candidates.

Pass one assessment directory **or refusal capture directory** per slot, in
exact plan order:

```sh
python3 -B evals/patch-effectiveness-v1/eval.py score \
  --plan "$PRIVATE/freeze/plan.json" --slot "$SLOT_0" --slot "$SLOT_1"
```

Every slot remains in the denominator. `success` requires apply plus every
behavior and regression check. Separate outcomes are `setup_refusal`,
`failed_submission`, `invalid_patch`, `rejected_path`, `apply_failure`, `timeout`,
`failed_tests`, `success`, and `integrity_failure`. Admission/runtime/custody
failures invalidate study integrity and make the primary rate null; they are
not evidence the patch was wrong. Reports retain no-op/partial flags, family
counts, diff bytes/lines/files, non-overlapping setup/provider clocks and
separate evaluation time, all per-field usage coverage/nulls and nullable USD.
No identity-score surrogate, total-token inference or inferred price is used.
The scorer reports qualification data only (`effectiveness_evidence: false`);
measured real tasks require independent root admission and a new frozen study.
