# Deterministic shipped-tool correctness, v1

This evaluator measures source-grounded graph-tool contracts and diagnostic cost.
It makes no model calls and makes no effectiveness, superiority, or population
generalization claim. The frozen corpus contains 95 distinct semantic scenarios.
It is independent of the agent-question studies; their inputs and results are
unchanged. Product quality failures remain findings for separately scoped repairs.

`corpus.json` contains independently specified identities, edges, uncertainty,
denominators, surface applicability and assertions. `manifest.json` pins its bytes
and every literal fixture file. [source-review.md](source-review.md) explains the
source review and its limits. Do not generate truth from candidate output or
replace a failed attempt with a retry. A new run requires a new output directory.

Run the offline gates with Python 3.10 or newer:

```sh
python3 -B evals/tool-correctness-v1/eval.py check
python3 -B -m unittest discover -s evals/tool-correctness-v1 -p 'test_*.py' -v
```

Build and run an explicit candidate after the disk gate:

```sh
df --output=pcent . | tail -1
cargo build --workspace --locked
python3 -B evals/tool-correctness-v1/eval.py run \
  --candidate "$PWD/target/debug/orbit-graph" \
  --candidate-sha256 "$(sha256sum target/debug/orbit-graph | cut -d ' ' -f 1)" \
  --orbit /absolute/path/to/orbit \
  --orbit-sha256 <sha256-of-that-orbit> \
  --source-repo "$PWD" --source-commit <full-commit> \
  --plugin-commit <full-commit> --plugin-manifest-sha256 <original-manifest-sha256> \
  --output "$PWD/.orbit/tmp/tool-correctness-v1/attempt-001"
```

`run` exports immutable plugin Git objects, reuses the shipped bundler and the
`plugin_v2.rs` local namespace/private HOME installation pattern, and invokes
the real installed `graph.*` CLI and `graph_*` MCP tools. Bounded MCP transport
and child-process cleanup come from `scripts/agent-eval/plugin_profile.py` and
`eval_broker.py`; no graph implementation or mock product oracle is duplicated.
All tool inputs, errors, raw streams, exits, supervision and timings are retained
in `raw/`, `attempts.jsonl` and `report.json`. Production HOME, grants, task/run
history and checkout indexes are never used. Only synthetic fixture repositories
are indexed. Installing the exported plugin explicitly grants its requested
permissions in the disposable private HOME; maintenance uses the private host's
documented operator path. Other calls use ordinary authority.

The caller must provide an externally bounded runtime with a timeout, memory/PID
ceilings and descendant reclamation (STD-03 R21/R22). The runner records visible
enclosing cgroup ceilings; visibility may be unavailable in a container. Its
own child waits and streams are bounded and cleanup owns each process group.
Visibility limits are reported and do not establish an external containment
claim. No host service-manager configuration is performed by the evaluator.

Each applicable case/surface gets a separate synthetic repository and HOME.
The cold sample starts after explicitly logged prerequisite index/history setup,
with no query/snapshot-cache warmup. Three identical warm samples reuse that
same source and state. Setup process costs are reported separately from measured
query costs, without counting a process twice. File copies/export staging are
reported through identities and are not attributed to query latency. These are
diagnostics, not timing inference. The named volatile paths in the protocol
remove only stated paths/timestamps/timing values and operational cache/generation
identifiers from semantic comparison;
other differences fail qualification. Maintenance's first publication changes
state and operational counts, so equality compares the three subsequent samples;
each publication is independently checked against fresh full overview, search,
imports, callees and references for the reviewed selectors.

Per-tool pass/fail is the conjunction of every applicable attempt, including
cold failures. Empty returned/eligible denominators yield null precision/recall.
Complete source-truth sets use multiplicity, so duplicate edges cannot hide a
missing edge or inflate precision. Uncertainty/unsupported challenges never
contribute fabricated true positives. Coverage is unit-specific and is not pooled
across unrelated files, calls and scenarios. The candidate may correctly report
budget exhaustion; budgets test disclosure rather than exact milliseconds.

The manifest ships exactly 13 tools. `implementors` is a CLI-only surface gap;
`impact` and `trace` expose no plugin time-budget argument, which is explicitly
tested as a refused input. Ranking variants are evaluated through the real
standalone `evaluate --input` command with synthetic public envelopes, not an
unshipped plugin argument. That supplemental strict task-text measurement is
labelled separately from plugin `recommend` and from `evaluate --live` commit-text
studies. No live-history sync is used.

A completed evaluation may exit 1 and report `qualified: false`: this is the
intended outcome for a failed invariant, not permission to fix the product in
this task. Exit 2 denotes preflight/input failure. A root reviewer should review
the frozen bytes/truth and raw report independently before accepting findings.

Re-score saved evidence after a scorer correction without executing the candidate:

```sh
python3 -B evals/tool-correctness-v1/replay.py \
  .orbit/tmp/tool-correctness-v1/attempt-003 \
  --output .orbit/tmp/tool-correctness-v1/replayed-003.json
```

Replay retains the original score alongside the new score and cannot promote a
failed capture's qualification to passing. [capture-review.md](capture-review.md)
records the protocol corrections and current candidate findings; the committed
sample dashboard is observed output, never fixture truth.

`--case <id>` can restrict a run for diagnostic repair of the evaluator itself;
its omitted coverage stays visible and the incomplete run cannot qualify.
Generate the current dashboard from preserved captures with:

```sh
python3 -B evals/tool-correctness-v1/dashboard.py .orbit/tmp/tool-correctness-v1 \
  --current .orbit/tmp/tool-correctness-v1/attempt-007/report.json \
  --output .orbit/tmp/tool-correctness-v1/dashboard.json
```

The dashboard reads one complete current capture whose corpus/scorer hashes match
the final protocol and verifies source/binary/runtime pins across all attempts.
It retains earlier failed qualification alongside current diagnostics. Root's
independent source-only review is recorded; corrected final approval is not
self-attested by the executor.
