# Installed graph plugin + shipped skill: profile 2

This profile measures a different treatment from the historical CLI proxies in
`evals/agent-navigation` and `evals/real-agent-navigation`. It makes no claim about
plugin effectiveness and does not change those studies, captures, truth or locks.
A later task must freeze an independent corpus, rubric, splits and run plan before
an operator authorizes model measurements. This worker ran only a fake provider.

The implementation extends `scripts/agent-eval/eval_runner.py` and its existing
broker, supervision, tool approvals, feature readback and transcript audit. It
requires Linux. Schema-1 requests, raw captures and the historical adapter remain
supported. Schema-2 raw captures have kind
`installed-plugin-agent-eval-raw-episode`, profile `installed-plugin-skill-v2`, and
cannot pass the historical `adapt` command. This directory's evaluator performs
provenance/cost replay; it deliberately has no accuracy scoring or corpus yet.

## Treatment and authority

Both arms receive the same question, source views, model, settings and limits.
Both retain the exact baseline `read`, `rg`, `git` tools. The baseline receives no
plugin, skill, host executable or graph cache. Each graph episode:

1. Verifies each selected source file against the original pinned base/head Git
   commit's blob, then creates the existing deterministic two-commit source-only
   snapshot. Original history, truth and task stores are never exported. Selection
   of the source view must be fixed by the future corpus, not by the treatment.
   Profile 2 admits code extensions under `src/`, `crates/`, `tests/`, Cargo
   manifests and root Cargo/Python project manifests. Hidden paths, evaluation,
   truth, answers, docs, skill, vendor and credential directories are refused;
   arbitrary text/JSON exports and root README files are not source inputs.
   The allowed extension list is fixed in `source_path_allowed`. Git replacement
   objects are disabled while verifying these original identities.
2. Exports `.orbit-plugin` from a full immutable local Git commit, copies the
   hash-pinned Orbit and graph executables, and creates a new private Orbit HOME.
   No download, live install, provider configuration or host upgrade occurs.
3. Installs and enables the plugin through the real Orbit CLI in that fixture.
   The local export removes `metadata.origin: orbit`, as the repository's real
   plugin integration fixture does, and replaces the unbound backend override
   with `--backend-sha256`. Both complete manifests and the original file hashes
   are captured. The skill and all its reference files remain byte-for-byte
   unchanged. No first-party origin is impersonated.
4. Discovers actual `graph_*` MCP names, descriptions, schemas and annotations
   through `orbit mcp serve --workspace agent-eval-episode`. The exact plugin
   inventory digest must match the preregistered pin. A changed inventory refuses
   before a provider episode. Other host tools are never advertised or forwarded.
5. Supplies all shipped skill/reference bytes in explicit developer context,
   with a separately recorded binding paragraph naming the private repository,
   workspace, discovered namespace and gateway envelope. The user question stays
   unchanged. Ambient Codex plugins and skill auto-install remain disabled: this
   is an Orbit-installed plugin served through a gateway, not a Codex plugin.
6. Proves through the real host that an ordinary MCP caller cannot maintain the
   index. The gateway uses a fresh **private-host operator session only for
   `graph_maintain` with `operation: graph_sync`**. Other maintenance operations
   are refused before dispatch. Read tools use ordinary sessions. This is an
   explicit evaluation-only authority policy, recorded in every treatment; it
   does not grant the provider general Orbit or operator access. There are no
   seeded task records or live grants in this private host.

The agent chooses whether and when to use graph tools or build an index. Setup
never builds one. `index_missing` is a failed, costed call with a recoverable
product reply; a subsequent maintenance call remains an agent choice. Complete
`invalid_request` replies also remain recoverable. Other product failures,
timeouts, malformed/truncated replies and incomplete cleanup fail the episode.

The gateway forwards arguments without translating to CLI aliases. Repository
and workspace overrides must equal the episode binding. Path selectors refuse
escapes, symlinks, `.git`, `.orbit` and `.orbit-graph`. `read`/`rg` hide private
Orbit state, including state created by workspace initialization. The initializer's
`.gitignore` edit is restored, and private state is excluded through Git's local
exclude file. Product validation handles unknown fields. Replies preserve the
complete MCP result, including `content`, `structuredContent`, and `isError`, in
`product_reply`, alongside bounded transport stderr, exit and cleanup evidence.
The outer broker text envelope keeps the existing exact provider/broker
multiplicity audit. It is part of the declared treatment.

## Isolation and accounting

Strict runs require bubblewrap namespaces **and observable finite cgroup-v2
`memory.max` and `pids.max` ceilings** imposed by the operator/runner. An example
operator scope is `systemd-run --user --scope -p MemoryMax=4G -p TasksMax=256 ...`;
choose and preregister appropriate bounds. The runner only observes limits; it
never changes host cgroups. Unsupported capabilities refuse explicitly. The
source mount is writable for private workspace installation only, then read-only
throughout provider execution. Only the episode's `.orbit` and `.orbit-graph`
subtrees are writable over that mount; the real host needs private `.orbit/tasks`
directories during dispatch. The broker refuses access to both subtrees. Existing
host-path/truth probes, disabled ambient
features, fixed child environments, timeouts, bounded logs and process-group
cleanup remain in force. PID namespaces provide the external descendant boundary.
Provider credentials follow the existing runner's read-only credential mount;
installation and Orbit children receive no provider secrets or host authority.

Every output directory, HOME, workspace and index is new. Disk usage at or above
80% refuses a new profile fixture. Scratch/examples belong under ignored
`.orbit/tmp/`; leave the captured outputs there for review.

Installation/export/discovery and the negative authority probe have explicit
`setup_limits.wall_ms` and `setup_limits.output_bytes`; installation time is also
inside the total episode wall limit. Provider preflight costs are recorded.
`timing.plugin_install_ms`, `setup_output_bytes`, the full setup log,
`timing.graph_sync_ms`, every agent call/error, usage and output bytes distinguish
setup from choices. Tool and answer output uses the unchanged `limits` contract.
The inherited unadvertised-name refusal budget bounds hostile unknown calls.
The artifact seals setup, actual inventory, original/bound manifests, skill files,
binding context, source blob identities, executable pins and all three harness
file hashes. Hashes establish local integrity, not a third-party attestation.

`--containment none` is an explicitly uncontained fixture mode. It still uses a
private real install, gateway checks and supervision. It is never evidence of
truth isolation or effectiveness. On this worker, bubblewrap reports that it
cannot create namespaces. Strict installed-plugin tests must run on the operator
host before admitting measurements; no permissive sandbox fallback is provided.

## Preregister and run (operator only)

First capture installation provenance and the actual inventory without a model:

```sh
TOOL=scripts/agent-eval/eval_runner.py
mkdir -p .orbit/tmp/plugin-profile
python3 -B "$TOOL" plugin-inspect \
  --plugin-repo /absolute/local/orbit-graph \
  --plugin-commit FULL_40_HEX_COMMIT \
  --orbit /absolute/orbit --orbit-graph /absolute/orbit-graph \
  --out .orbit/tmp/plugin-profile/inspection > .orbit/tmp/plugin-profile/inventory.json
```

The inspect command defaults to strict containment. For installation-only
fixtures on a restricted developer host, pass `--containment none` and keep its
`contained: false` label. Inspect performs no model call and no index build.
It emits `pin` and `inventory` plus a sealed `plugin-treatment.json` and setup log.
Copy the `pin` unchanged into **both** arms' requests. Inspection is separate
preregistration work; every graph episode repeats and measures its own cold install.

A schema-2 request has all schema-1 request fields (same answer contract) plus:

```json
{
  "schema_version": 2,
  "profile": "installed-plugin-skill-v2",
  "plugin": {
    "commit": "FULL_40_HEX_PLUGIN_COMMIT",
    "backend_sha256": "64_HEX",
    "orbit_sha256": "64_HEX",
    "inventory_sha256": "64_HEX",
    "skill_sha256": "64_HEX"
  },
  "source_commits": {"base": "FULL_40_HEX", "head": "FULL_40_HEX"},
  "setup_limits": {"wall_ms": 60000, "output_bytes": 8388608}
}
```

These are field examples, not a complete runnable request. Graph `tools` are
`read`, `rg`, `git`, followed by the inspection's inventory names in order;
baseline tools are exactly `read`, `rg`, `git`. Preserve each question and its
limits between arms, recalculate `request_sha256` if present, and preregister
orders, source selection, repetitions, timeout handling and all model settings.
Pin the harness file hashes as well as executable hashes. No retry may overwrite
an episode or silently replace a failed observation. Freeze the plan externally
before any model run; the evaluator cannot prove when a plan was written.

Use the existing `run` flags documented in `scripts/agent-eval/README.md`, adding
`--source-repo /original/git/repo` for both arms and `--plugin-repo`, `--orbit`,
`--orbit-graph` for the graph arm. The source-only `--head` and `--base` exports
must match the requested original commits and content digests. Strict `run`
performs preflight itself, including the final treatment context configuration.
Standalone schema-1 `preflight` does not pretend to validate this new profile.

## Offline replay and tests

No provider is contacted by this command:

```sh
python3 -B evals/plugin-agent-navigation/eval.py replay \
  --preregistration /sealed/plan.json \
  --episode /captures/00-baseline --episode /captures/01-graph
```

The preregistration JSON contains exactly `schema_version: 2`, `profile`, `model`
(the runner's provider/name/version/settings object), `provider_binary_sha256`,
`harness` (SHA-256 per `eval_runner.py`, `eval_broker.py`, `plugin_profile.py`), and
`requests` (complete requests in order). Replay checks file seals, actual host
inventory and authority evidence, manifest transformation, exact skill bytes and
provider context, source provenance, pairing, isolation, telemetry and outcomes.
It recomputes outcomes from sealed captures and retains genuine failed, invalid and
timeout episodes in the complete paired cohort, including telemetry and approval
failures. A forged successful label is refused. Failed episodes keep their costs
and error codes and cannot establish successful call telemetry or cohort
containment. It reports costs and behavior outcomes, always with
`effectiveness_evidence:false`.
A future independent rubric is needed to score accuracy. `--diagnostic` permits
uncontained fake-provider artifacts, reports `containment_verified:false`, and
never changes their evidence status.

The positive test creates both captures and a preregistration, then runs this
same replay path offline. Reproduce it without any model calls:

```sh
AGENT_EVAL_ORBIT=/absolute/orbit \
AGENT_EVAL_ORBIT_GRAPH="$PWD/target/debug/orbit-graph" \
  python3 -B -m unittest discover -s scripts/agent-eval/tests -p test_plugin_profile.py -v

# Operator admission check: skips become failures.
AGENT_EVAL_REQUIRE_BWRAP=1 \
AGENT_EVAL_ORBIT=/absolute/orbit \
AGENT_EVAL_ORBIT_GRAPH="$PWD/target/debug/orbit-graph" \
  python3 -B -m unittest discover -s scripts/agent-eval/tests -v
```

Use a fresh checkout/build that matches the chosen plugin commit and check disk
usage before creating a build directory. Tests never alter the real HOME or
install a global plugin. Host variables do not enter the private fixture.

### Lifecycle version boundary and operator handoff

New captures use runner version 4 and
`lifecycle_contract: eof-idle-at-term-observation-v2`, retaining schema 2 and
this profile. Replay selects that contract explicitly. Runner version 2 without
a contract retains historical cancellation failures; runner version 3 with the
original `eof-exited-at-term-observation-v1` retains its narrower rules, including
the first failed actual-Codex rehearsal. Unsupported combinations are refused.
Harness hashes in the capture and preregistration bind exact implementation
bytes. New semantics cannot be mixed into a frozen cohort or old rehearsal.

See the [prospective teardown contract](../../scripts/agent-eval/README.md#prospective-orderly-teardown-contract)
for EOF, idle checkpoint, final count fence and bounded drain requirements. The
first actual-Codex rehearsal proved that an already-exited-worker rule was too
narrow. The revised offline fixture covers EOF while the worker is still alive.
The operator must repeat strict zero-skip admission on the exact revised hashes,
then run a new small synthetic actual-Codex rehearsal in both arms before new
measurements. A fake-provider pass alone is insufficient actual-client evidence.
Keep all 12 current outcomes and the original scoring attempt under the frozen
contract, including redacted-treatment refusals. Source-output redaction repair
is a separate change; no accepted old score is required for prospective review.
