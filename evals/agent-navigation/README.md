# Paired agent navigation study (fixture corpus v1)

This is a task-solving harness, separate from recommendation ranker backtests,
SQL/API timings and plugin conformance. **No real agent episodes have been run
here. Agent effectiveness remains unmeasured.** The orchestrator owns the real
paired runs and any evidence-driven production follow-ups.

Python 3.10+ and its standard library are sufficient. From the repository root:

```sh
python3 -B evals/agent-navigation/eval.py validate
python3 -B evals/agent-navigation/eval.py check
```

`check` validates independent truth, frozen corpus/split hashes, complete paired
requests, checked-in scripted artifact parity and 23 behavioral tests including
negative subcases. It runs the same inexpensive gate as CI and `make ci-fast`.
Tests use ephemeral directories under `.orbit/tmp/` and exercise the scorer CLI
in child Python processes; no product/provider process is started.
`validate` and `score` only read files and emit JSON. All commands
emit JSON on stdout; errors emit a structured error on stderr and exit 1.

## Pre-registration and independently checkable truth

`corpus.json` freezes 12 synthetic cases in two committed source trees: Rust
and Python. Both families have symbol discovery, direct callers, direct callees,
transitive impact/test selection, changed function bodies, and a case where the
requested runtime/external evidence is unavailable. `price_preview` is a
misleading name match. No production code, tool ranking or corpus was tuned.
Four cases are development and eight are held-out; each split has both families.
The labels and expected sets were frozen before implementing the scorer tests.
`corpus.lock.json` pins corpus, truth and split SHA-256 digests. There is no
automatic lock-update command. Changes need a reviewed new corpus version,
not a silent regeneration after looking at held-out results.

Source revisions are SHA-256 of canonical JSON maps `{relative_path: UTF-8
source_text}` (sorted keys, compact separators, ASCII-escaped strings); they
are immutable **fixture content revisions**, not invented Git commit IDs.
Both base and HEAD maps are pinned. Base intentionally represents a before-fix
state: its multiplier of two fails the existing invoice assertion; HEAD fixes
that behavior with a multiplier of three. This is a navigation/change exercise,
not a requirement that every base snapshot has green tests. Operators materialize them as two Git
commits and retain the mapping of content digest to actual Git SHA in their
external provenance log. Git metadata does not enter the source digest.

`truth.py` is independent of orbit-graph: Python AST extracts module-level
functions and direct calls; a restricted parser extracts the brace-free Rust
function bodies. Reverse reachability selects `test_*` functions, and independent
base/HEAD body comparisons identify changed functions. The unsupported witnesses
explicitly name absent external source or a callback supplied at runtime.
Review the tiny source trees and oracle by hand. This oracle is intentionally
fixture-specific, not a correctness oracle for arbitrary Rust/Python programs.
Answer citations prove identities against HEAD; the independent oracle establishes
relationships and complete truth. Wrong but source-cited identities become false
positives rather than being accepted as correct.

Pre-registered decision rule for this first real cohort: graph must correctly
solve at least 7/8 held-out cases, gain at least two correct cases over baseline,
introduce no extra false positives and no extra failed/invalid/timeout episodes,
and correctly abstain on both held-out unsupported cases. Report every case and
both split denominators regardless of success. Do not use development results
in that decision or tune tools against held-out truth. Report paired wall-time,
calls and bytes; discuss speed only for pairs correct in both arms, and only if
isolation and setup costs were independently audited. This tiny cohort supports
an exploratory follow-up decision, not a statistical superiority claim. Development
and held-out cases share source trees/symbols, and the language counterparts are
correlated: this is a task holdout, not a repository holdout. Fresh agent sessions
prevent transcript carryover; they do not make cases statistically independent. Further
model/settings/repetition cohorts require separate complete bundles and a new
pre-registration; never cherry-pick the fastest or most correct repetition.

## Scripted smoke, reproducibility and bounded raw records

[`smoke-episodes.json`](smoke-episodes.json) and
[`smoke-result.json`](smoke-result.json) are **synthetic/scripted**, including
invented deterministic timing/call data. Their model/tool versions explicitly
say scripted/not executed. They demonstrate incorrect abstention, a false
positive, failed/timeout/invalid episodes, correct unsupported abstentions and
null usage. These are not agent-effectiveness or graph-tool measurements.

After an intended report change, regenerate only the derived result and review
its diff; leave the frozen raw smoke episodes unchanged:

```sh
python3 -B evals/agent-navigation/eval.py score --input evals/agent-navigation/smoke-episodes.json > evals/agent-navigation/smoke-result.json
python3 -B evals/agent-navigation/eval.py check
```

The raw schema is enforced by `score`, with no ignored fields. The smoke bundle
is a complete example of every required field. Its top-level object is exactly
`schema_version: 1`, `study_kind: agent|scripted-smoke`, and `episodes`. Every
record contains:

| Field | Contract |
| --- | --- |
| `request` | Exact exported request, including prompt, content revisions, corpus/request hashes, tools, limits, order and cache policy |
| `run_id`, `model` | Unique supervisor run ID; provider/name/version and full non-secret settings; identical model/settings across the complete cohort |
| `tool_versions` | Nonempty exact `read`, `rg`, `git` versions; graph arm additionally `orbit-graph` version and binary SHA-256 in its version string |
| `isolation` | Unique repository/cache identifiers and operator attestations `truth_inaccessible: true`, `cold_start: true` |
| `status`, `error` | `ok` with null error, or `failed|timeout|invalid` with nonempty code/message |
| `answer`, `final_output` | Parsed answer and exact bounded raw final text for ok; null answer on failure, retain available raw final text |
| `calls` | Sequential tool records: allowed tool, captured input/output strings, elapsed_ms, `ok|failed|timeout|truncated` status |
| `wall_ms`, `output_bytes` | Whole episode wall time including setup; exact UTF-8 call-output bytes plus raw final text bytes |
| `usage` | Null when unavailable; otherwise input_tokens/output_tokens/cost_usd nullable and nonempty provider telemetry source; no estimates |
| `record_sha256` | SHA-256 of canonical JSON of the record excluding this field |

Agent answers have exactly `items`, `abstain`, `reason`, `evidence`. Every item
is a unique `symbol:path#name:function` or Rust `symbol:path#name:test` selector with a citation containing
`item`, `file`, positive `line`, and exact definition-line `quote`. Abstentions
require empty items/evidence and a specific reason. A valid failure is always
incorrect and stays in the denominator. A captured, parsed answer that violates
the answer contract (including missing, duplicate or unsupported citations)
also stays in the report as incorrect, with a nonempty `answer_error`.
The scorer preserves raw `status`, `error`, captures and telemetry; it never
deduplicates or repairs answers. `answer_error` is null when validation passed
or was not attempted for a raw failed/timeout/invalid episode. Invalid answers
receive no credit for identities or abstention and miss every expected identity.
Missing pairs, invalid truth, malformed records, unknown collection fields,
changed requests/hashes, answer/capture mismatches and inconsistent accounting
refuse the entire score (exit 1, no success report). Unparseable agent replies
must be captured as `status: invalid`, `answer: null`, `final_output: <raw reply>`,
and error details; they are not missing records. Do not relabel or reseal an
existing capture merely because scoring finds an invalid answer.

Limits: 2 MiB input JSON, 8..32 cases, 32 answer items/citations, 40 sequential
calls per episode, 16 KiB each call input/output or raw final text, 128 KiB total
captured output, 120 seconds wall time (timeout records permit 10 seconds of
termination grace). Duplicate JSON keys and nonfinite numbers are refused.
Capture a bounded prefix on overflow, mark the call `truncated`, and terminate
with a failed/timeout episode; preserve the full supervisor log privately with
a hash in external provenance. Never label a truncated final reply `ok`.

The scorer reports per-case correctness, false positives, missed identities,
abstentions/correct abstentions, failures/errors, wall time, calls including
failed calls, bytes and nullable usage. Split/arm aggregates and paired deltas
retain every episode; `failures` counts raw failed/timeout/invalid episodes and
captured answers with validation errors. It does not impute missing tokens/costs
or authenticate operator assertions; raw transcript/provenance review is part
of any real result.

## Operator runner: real paired agents without solutions

The managed worker only exports/ingests; it must not call providers or dispatch
agents. An authorized supervising operator follows this protocol:

1. Run the validation gate, then export to a **new** directory. Check disk use
   before creating source/build roots; at 80% or more stop. Choose an approved,
   fixed crew (replace `sol` below if needed); keep sampling/reasoning settings
   fixed across the cohort. Existing destinations are refused rather than overwritten.

   ```sh
   df --output=pcent .
   python3 -B evals/agent-navigation/eval.py export --output .orbit/tmp/navigation-v1 --crew sol
   ```

2. Export contains `plan.json` and 24 episode directories, each with only a
   `repository/` HEAD tree, a `before/` base tree, `prompt.txt` and `request.json`.
   There is **no truth, corpus, oracle, smoke answer, evaluator source or score**
   in these bundles. Do not give agents this documentation or evaluator checkout.
   Transfer one episode bundle at a time to a disposable evaluation host/container
   or OS identity with **no read access to the evaluator checkout or prior episodes**.
   Cwd-only restrictions, withheld tool names, and Codex `read-only` are accident
   guards, not a boundary: `orbit.agent.invoke` runs outside Orbit's filesystem
   sandbox as the host user. Use a separate filesystem/identity boundary, keep
   credentials in the provider's own store, and independently audit it. If that
   boundary is unavailable, do not run or label the data as isolated agent evidence.

3. Before invocation, register/admit the copied fixture repository as an isolated
   evaluation workspace on that host (never a live customer workspace). Initialize
   its Git history from the exported base files, then replace them with the HEAD
   files and commit HEAD. Use explicit local identity, no hooks/global/system Git
   configuration, and private HOME/product roots. Verify both content hashes
   against the request and log their actual Git SHAs. For the exported fixture
   layout, run this preparation in the episode directory **before transferring**
   the repository to its isolated execution context (only repository goes there):

   ```sh
   NAV_EPISODE_DIR="$(pwd -P)"
   mkdir private-home
   cp -R repository head-copy
   cp -R before/. repository/
   nav_git() {
     env -i PATH="$PATH" HOME="$NAV_EPISODE_DIR/private-home" \
       GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null \
       GIT_AUTHOR_DATE=2026-10-03T00:00:00Z GIT_COMMITTER_DATE=2026-10-03T00:00:00Z \
       git -C "$NAV_EPISODE_DIR/repository" \
       -c user.name=fixture -c user.email=fixture@example.invalid \
       -c core.hooksPath=/dev/null -c commit.gpgSign=false "$@"
   }
   nav_git init --initial-branch=main
   nav_git add --all
   nav_git commit --message=base
   cp -R head-copy/. repository/
   nav_git add --all
   nav_git commit --message=head
   nav_git log --format='%H %s'
   ```

   Each prepared repository contains exactly two commits; `HEAD^` is the base.
   This is read-only navigation,
   so no builds are needed. Do not put `before/` inside the indexed repository;
   agents inspect the base through Git. All agents see the same two-commit history.

4. `plan.json` gives exact machine-readable `invoke_input` objects for
   `orbit.agent.invoke`: `prompt`, absolute `cwd`, `crew`, `timeout_seconds: 120`,
   `provider_sandbox: read-only`, `idempotency_key`. After transferring, change only
   cwd to the admitted physical repository path; keep the request unchanged.
   Use a unique export/run name for repetitions so idempotency keys do not reuse
   earlier episodes. Submit each object from an **operator** session using the
   advertised `orbit.agent.invoke` tool; the managed executor is not granted it.
   The call returns a run ID; poll through the operator's granted run interface
   until its terminal outcome and capture the actual provider transcript/usage.
   No nested agents or continuation sessions; start a fresh provider session for
   each episode, in the exported order (baseline-first then graph-first alternating,
   balanced within each split). Never send sibling answers or scorer feedback.

5. Supervisor tool routing must enforce `request.tools`: baseline permits reads,
   `rg` and read-only `git`; graph adds graph_sync/search/show/refs/callees/impact/
   changes through the real orbit-graph executable or admitted plugin. Do not
   grant an unrestricted shell that defeats this routing. Stage a fixed binary
   on the isolated host; never install/update a live plugin for this study.
   Initialize a fresh `.orbit-graph/` index per graph episode **after starting
   the episode clock**, and count graph_sync as a call. Keep graph DBs/snapshot
   caches and repositories distinct; never reuse an index between cases/arms.
   Restore the same cold setup policy each time. OS page caches and provider
   warmup still vary; counterbalancing mitigates but does not remove that skew.

6. Host supervision enforces wall/call/output limits and process-group cleanup,
   captures tool I/O and versions, failure states, raw final replies and telemetry.
   Normalize tool names to the exported names. All calls are sequential. Record
   read implementation version (e.g. adapter Git SHA), `rg --version`, `git --version`,
   binary version/hash, exact model revision/settings and supervisor run ID. The scorer
   rejects tool-version changes within a cohort and successful episodes with
   no captured calls or truncated calls.
   Provider-unavailable usage is null; keep partially available fields null.
   Prefix truncation on failure must be marked; never invent an empty transcript
   to conceal missing capture. Redact credential values before any durable write;
   keep no credentials in episode JSON. Exported prompts/sources contain no secrets.
   Record externally the host identity, isolation audit, setup command log and
   hashes of full bounded/private supervisor evidence. The scorer checks shape,
   source evidence and internal consistency; it cannot certify these host facts.

7. On the evaluator side (never inside an agent context), assemble all 24 records
   in exported order using the raw format above, calculate canonical record hashes,
   and keep the original supervisor logs privately. Score a complete bundle:

   ```sh
   python3 -B evals/agent-navigation/eval.py score --input .orbit/tmp/real-episodes.json > .orbit/tmp/real-result.json
   ```

   If admission/start fails, include a failed episode with an explicit cause,
   observed time/calls/bytes and null usage. Unparseable answer => raw invalid episode;
   parsed but contract-invalid answer => scorer `answer_error`, with raw status
   preserved; timeout => timeout episode. Do not drop or replace unsuccessful runs. Publish
   sanitized bounded raw episodes, result, source/plan/binary hashes and independent
   host attestation together. Real episodes and interpretation remain follow-on
   work; these smoke artifacts cannot satisfy the pre-registered success criterion.
