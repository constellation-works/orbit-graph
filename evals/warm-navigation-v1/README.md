# Warm navigation v1

Proposed qualification capability only. No measured provider results or study
admission are included. Read [PROTOCOL.md](PROTOCOL.md) before freezing anything.
The independently authored source fixtures and fixtures.lock.json are test-only;
they are not a candidate-selected study corpus or effectiveness evidence.

The runner uses request schema **5**, runner **8** and profile
`installed-plugin-warm-batch-v1`. Schema 4 belongs to patch evaluation on the
current base. Historical cold exports, captures and replay remain cold.

Run the deterministic qualification with the real repository executable and a
private installation of the repository's CI-pinned Orbit binary:

```sh
cargo build --workspace --locked
AGENT_EVAL_ORBIT=/absolute/path/to/pinned/orbit \
AGENT_EVAL_ORBIT_GRAPH="$PWD/target/debug/orbit-graph" \
  python3 -B -m unittest discover -s evals/warm-navigation-v1 -p 'test_*.py' -v
```

Missing binaries fail this qualification. The strict test reports its exact
missing capability when skipped. Diagnostic tests never contact a provider and
never grant or configure a production installation. The suite covers paired
baseline controls, real installed skill/inventory, unchanged-source reuse,
independent source edit, stale index observation, sync and fresh/incremental
semantic parity, unknown product cache status, missing usage, failed provider,
partial refusal, cancellation, wrong backend/source pins, changed installed
skill, undeclared state, unsafe links and modified original provenance/rubric.

Root must run strict qualification on Linux with working bubblewrap namespaces
and observable finite cgroup-v2 memory.max and pids.max:

```sh
WARM_REQUIRE_STRICT=1 \
AGENT_EVAL_ORBIT=/absolute/path/to/pinned/orbit \
AGENT_EVAL_ORBIT_GRAPH="$PWD/target/debug/orbit-graph" \
  python3 -B -m unittest discover -s evals/warm-navigation-v1 -p 'test_*.py' -v
```

A strict refusal is not a pass or a sanctioned test deferral. The worker host
may be unable to create nested namespaces; root owns this host rehearsal,
independent review and the later actual-client/cohort decision.

Before actual-client rehearsal, independently author the source-only initial
and edited commits, exports, prompts and answer rubric. Freeze all controls
before output. Use plugin-inspect to discover the pinned real installation's
inventory and skill binding, without starting a provider:

```sh
python3 -B scripts/agent-eval/eval_runner.py plugin-inspect \
  --plugin-repo "$PWD" --plugin-commit "$PLUGIN_COMMIT" \
  --orbit "$ORBIT_BIN" --orbit-graph "$GRAPH_BIN" \
  --out "$SCRATCH/inspect" --containment bwrap
```

`PLUGIN_COMMIT` is a full commit; all binary paths and `SCRATCH` are absolute.
`SCRATCH` is a new private directory under .orbit/tmp. Use the returned pin
unchanged. Orbit is installed only in the private fixture; no ambient grants,
authentication or host configuration are imported.

Author `study-lock.json` with exactly schema_version (1), purpose,
source_fixture_lock_sha256, and expected_items (one nonempty string array per
scheduled slot). Truth stays outside provider mounts. Author `spec.json` with:

- schema_version (1), profile and the state_policy from warm_batch.POLICY;
- model with provider `codex-cli`, exact name, version and settings, and
  provider_binary_sha256;
- runtime: baseline_binaries hashes for git/rg/python, bwrap_sha256,
  resource_limits with observed memory.max/pids.max, and containment `bwrap`;
- product_identity: the complete `orbit-graph version --json` document;
- batch_order: baseline/graph/graph/baseline or its reverse;
- schedule: ordered objects with view (a single safe directory name) and
  requests.baseline/requests.graph. Each request has schema_version 5, profile,
  cache_policy `declared-private-state-per-batch-v1`, source/base content
  revisions and immutable source_commits, prompt, limits, setup_limits, plugin
  pin, case_id, split, order equal to its slot index, and exact arm tools.

All paired request fields except arm/tools/request_sha256 must match. Use the
existing runner `revision --tree` command for content revisions. Every `view`
exists under both HEAD_EXPORTS and BASE_EXPORTS, including repeated views; the
latter can export the same pinned base for every slot. The source verifier
requires every permitted source file from each pinned Git commit. Never export
truth, docs, prompts, skills or ambient state into these views. Protocol,
evaluator, helper and study-lock hashes are calculated by export:

```sh
python3 -B evals/warm-navigation-v1/eval.py export \
  --spec "$SCRATCH/spec.json" --lock "$SCRATCH/study-lock.json" \
  --output "$SCRATCH/freeze.json"
```

Root's actual-client rehearsal uses a new frozen contract with the actual Codex
binary/version/model/settings. This command starts providers and therefore is
for root's later authorized rehearsal, not the worker qualification:

```sh
python3 -B scripts/agent-eval/eval_runner.py run-batch \
  --request "$SCRATCH/freeze.json" --lock "$SCRATCH/study-lock.json" --batch-index 0 \
  --head "$HEAD_EXPORTS" --base "$BASE_EXPORTS" --source-repo "$SOURCE_REPO" \
  --plugin-repo "$PWD" --orbit "$ORBIT_BIN" --orbit-graph "$GRAPH_BIN" \
  --codex "$CODEX_BIN" --model "$MODEL" --rg /usr/bin/rg \
  --containment bwrap --truth-path "$SCRATCH/study-lock.json" \
  --out "$SCRATCH/batch-0"
```

Pass the exact frozen settings using repeated `--setting KEY=VALUE`; explicitly
allowlisted provider environment names and any code-mode selection must stay
equal across batches. Credentials follow the existing runner's read-only auth
mount policy. Do not copy or publish auth stores. Runtime drift refuses before
provider output. Each batch output must be new: no resume/retry/replacement.
After root authorizes the cohort, execute indexes 0 through 3 sequentially in
the frozen order, with distinct batch directories and identical controls.

Replay and scoring never start providers:

```sh
python3 -B evals/warm-navigation-v1/eval.py replay --batch "$SCRATCH/batch-0"
python3 -B evals/warm-navigation-v1/eval.py score \
  --batch "$SCRATCH/batch-0" --truth "$SCRATCH/truth.json"
python3 -B evals/warm-navigation-v1/eval.py cohort \
  --batch "$SCRATCH/batch-0" --batch "$SCRATCH/batch-1" \
  --batch "$SCRATCH/batch-2" --batch "$SCRATCH/batch-3"
```

`truth.json` contains exactly freeze_sha256 and expected_items, which must match
the original pre-output lock. Scoring additionally checks each returned citation
against the retained source bytes and their pinned content revision. For test
captures append `--diagnostic`; that flag cannot produce strict evidence.

Keep the adjacent freeze-file batch admission receipts, freeze.json, lock.json, schedule.json, all checkpoints, original capture
files and batch.json. SIGKILL/host-loss batches without batch.json are incomplete
and refuse completed-batch replay. Keep them as all-attempt evidence; never
replace them with another attempt. Producer seals are drift checks, not proof
against same-user forgery. Model usage scope/cache semantics and OS page-cache
state remain unqualified. No prices, total tokens or effectiveness conclusions
are inferred by this tool.
