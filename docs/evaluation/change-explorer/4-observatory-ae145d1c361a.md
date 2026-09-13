# Study 4 — `observatory`, correcting the LA-1940 Fig. 1 mode labels (Python)

| Field | Value |
| --- | --- |
| Repository | `observatory`, cloned to `/tmp/orb-12393/repos/observatory` with `git clone --no-hardlinks file://~/workspace/constellation/codebases/observatory` |
| Base | `0d05e34ef4467aa597c092ffba2b6e6b34506f09` (ORB-12362) |
| Head | `ae145d1c361a82ba94b501a467678d8a6b62fad9` (ORB-12375, "Correct mode labels in the digitized LA-1940 Fig. 1 reference…") |
| Comparison mode | `direct_base_head` (`effective_base_sha` equals `base_sha`) |
| Working tree | clean |
| `EXTRACTOR_VERSION` / `STORE_SCHEMA_VERSION` | 6 / 1 |
| Explorer commit | `c1332cf13a4daa7f3695950fa6767622ed61fd28`, release profile |
| Snapshots | base 1263 files indexed / 1388 written / 50 excluded; head 1265 / 1393 / 50 |
| Exported report | [`reports/4-observatory-ae145d1c361a.json`](reports/4-observatory-ae145d1c361a.json), [`.html`](reports/4-observatory-ae145d1c361a.html) |

Chosen because it renames a test function in place, changes a function signature,
and touches binary and CSV reference data in the same commit — three things the
contract treats differently.

## Q1 — Changed symbols

44 rows. By status:

| Status | Count |
| --- | --- |
| `modified` | 23 |
| `added` | 19 |
| `signature_changed` | 1 |
| `removed` | 1 |
| out of scope | 7 |

No `uncertain`, `moved` or `renamed` row.

| Status | Selector | Manual verdict |
| --- | --- | --- |
| `signature_changed` | `symbol:experiments/physics/fput-recurrence-reproduction/run.py#assessment:function` | **Right.** `run.py:118@0d05e34ef446` is `def assessment(metrics: dict[str, dict[str, Any]]) -> tuple[str, str]:`; `run.py:121@ae145d1c361a` adds a second parameter `gating_controls: tuple[str, ...] = ("C1", "C2", "C3")`. |
| `removed` | `symbol:…/tests/test_runner.py#test_forced_control_failure_has_distinct_failed_state:function` | **Wrong label, right disclosure.** `tests/test_runner.py:56@0d05e34ef446` was renamed to `test_forced_gating_control_failure_has_distinct_failed_state` at `tests/test_runner.py:76@ae145d1c361a`, and the explorer emits `removed` + a separate `added` row for the new name. The containing file is `modified`, not renamed, so the pairing ladder's `renamed` rung (which requires Git rename or similarity evidence for the file) cannot be reached. The row carries the contract's "This is not a claim that nothing replaced it." |

Out-of-scope entries (all `unsupported_language`, correctly refused rather than
called `removed`): `…/reference/fig1-digitized.csv` (base and head),
`…/reference/fig1-hidpi-full.png`, `…/reference/fig1-hidpi-detail-mode234.png`,
`…/reference/fig1-hidpi-detail-mode2345.png` (head only — these are new),
`knowledgebase/studies/physics/fput-recurrence-reproduction.png` (base and head).
That is exactly the set of non-source files this commit touched, and the CSV is
the file whose *content* carries the corrected mode labels: the substance of the
change is invisible to the explorer, and the explorer says so.

## Q2 — Affected entry points and the rule that classified each

| Rule | Classifications |
| --- | --- |
| `test_function` | 22 |
| `main_function` | 9 |
| `crate_root_public_item` | 0 |
| `cli_command_handler` | 0 |

26 of the 44 queries returned no entry point, each with four disclosed reasons —
no rule matched; syntax-driven index; generated/macro-expanded code; and "50 tree
entr(ies) were excluded from this snapshot and contribute no evidence."

`main_function` fired on `symbol:experiments/physics/fput-recurrence-reproduction/run.py#main:function`,
which is correct: `run.py:346@ae145d1c361a` is `raise SystemExit(main())`.
`crate_root_public_item` correctly did not fire — no changed file is an
`__init__.py`. `cli_command_handler` did not fire, which is also correct here:
`run.py` uses `argparse` sub-parsers, which the Rust-oriented command extractor
does not discover, and the response says so by listing the rule as applied but
unfired.

## Q3 — Candidate tests and their source category

10 distinct candidate tests: 13 `call_path`, 6 `import_relationship`, 1
`naming_heuristic`.

Correct, and genuinely exercising the change:

- `symbol:…/fput/metrics.py#evaluate_metrics:function` ← `call_path` from
  `symbol:…/tests/test_numerics.py#test_metric_functions_find_known_synthetic_features:function`
  and `…#test_v1_protocol_keeps_first_local_maximum_and_summed_m4:function`.
  Verified at `tests/test_numerics.py:104@ae145d1c361a` and `:158@ae145d1c361a`,
  both `metrics = evaluate_metrics(`.
- The same two tests for `…/fput/metrics.py#_reported_feature` and
  `#_m3_additional_reporting` (both new helpers called from `evaluate_metrics`),
  and for `…/fput/compare.py#load_reference`.
- `symbol:…/tests/test_runner.py#run_short:function` ← `call_path` from the five
  `test_*` functions in that file. Correct: `run_short` is their shared helper.

**Missed:** `symbol:…/run.py#assessment:function` — the signature-changed symbol —
has **zero** candidate tests. Five tests in `tests/test_runner.py@ae145d1c361a`
genuinely exercise it, but they drive `run.py` through
`subprocess.run(["uv", "run", str(RUNNER), "baseline", …])`
(`tests/test_runner.py:17-34@ae145d1c361a`). A subprocess invocation produces no
call edge; the response returns `unsupported_scope` and the "syntax-driven index"
reason instead of a candidate. See Q5.

## Q4 — Strongest evidence path for the two most consequential changed symbols

**`symbol:experiments/physics/fput-recurrence-reproduction/run.py#assessment:function`**
— the signature change that adds protocol-driven gating controls. 2 paths at depth 3,
not truncated:

- distance 1, `resolved_call`, confidence `exact`:
  `symbol:…/run.py#main:function` → `symbol:…/run.py#assessment:function`
  at `run.py:270@ae145d1c361a`.
  Verified: line 270 reads `status, scientific_assessment = assessment(metrics, gating_controls)`.
- distance 2, `resolved_call`, confidence `exact`:
  `…/run.py` (module level) → `symbol:…/run.py#main:function` at `run.py:346@ae145d1c361a`,
  then the hop above.
  Verified: line 346 reads `    raise SystemExit(main())`.

Those are the only two call sites of `assessment` in the repository at that SHA
(`git grep -n "assessment(" ae145d1c361a -- …` returns the definition at `run.py:121`
and the call at `run.py:270`), so the evidence is complete for this symbol.

**`symbol:experiments/physics/fput-recurrence-reproduction/fput/metrics.py#evaluate_metrics:function`**
— the metric evaluator the mode-label correction actually changes. 6 paths at depth 3,
not truncated. The three distance-1 `resolved_call` paths, all confidence
`import_resolved`:

- `symbol:…/run.py#main:function` → `evaluate_metrics` at `run.py:253@ae145d1c361a`
  (`        metrics = evaluate_metrics(`).
- `symbol:…/tests/test_numerics.py#test_metric_functions_find_known_synthetic_features:function`
  → `evaluate_metrics` at `tests/test_numerics.py:104@ae145d1c361a`.
- `symbol:…/tests/test_numerics.py#test_v1_protocol_keeps_first_local_maximum_and_summed_m4:function`
  → `evaluate_metrics` at `tests/test_numerics.py:158@ae145d1c361a`.

Every hop verified against the source at `ae145d1c361a`.

## Q5 — Unresolved impact: false associations and missed real callers

**Every false association found by manual inspection.**

1. `symbol:…/run.py#main:function` ← `file:experiments/economics/_parallax/tests/test_cli.py`,
   category `import_relationship`, note "imports `parallax.cli`". That file
   (`experiments/economics/_parallax/tests/test_cli.py:10@ae145d1c361a`) is
   `from parallax.cli import _research_database_path, build_parser, main`. It has
   nothing to do with the FPUT runner; the only thing shared is the bare name
   `main`. This one violates the design's own categorisation — an
   `import_relationship` should be a module/import edge, not a name match — and is
   filed as **ORB-12411**.
2. `symbol:…/run.py#main:function` ←
   `symbol:experiments/physics/physics-field-guide/orbits-numerical-error/tests/browser_check.py#main:function`,
   category `naming_heuristic`, note "test name `main` resembles `main`; name
   similarity only, no call or import edge is asserted". Substantively wrong,
   correctly labelled.

No `heuristic_match` edges appeared in this study at all: the depth-3 edge census
is 57 `resolved_call` and 9 `import_relationship`, and `skipped_low_confidence`
was 0. The Python extractor's import resolution carried the whole study.

**Every missed real caller found by manual inspection.**

1. The five subprocess-driven tests for `run.py#assessment` (Q3). By name:
   `test_short_cycle_override_is_auditable_and_fast`,
   `test_forced_gating_control_failure_has_distinct_failed_state`,
   `test_forced_c3_failure_is_reported_but_does_not_gate_status`,
   `test_protocol_v1_flag_still_runs_the_original`,
   `test_protocol_v1_forced_c3_failure_still_gates_status`, all in
   `experiments/physics/fput-recurrence-reproduction/tests/test_runner.py@ae145d1c361a`.
   Each asserts on `run.json`'s `status` / `scientific_assessment`, which are exactly
   `assessment`'s two return values.
2. The rename in Q1 is not stated as a rename, so the callers of the new test name
   are not attached to the old row.
3. 34 of the 44 queries returned zero paths at every depth. Sampling them: the
   Markdown headings (`docs/design/paper-reproduction-workbench.md#…`,
   `protocol/v2.md#…`, `reference/README.md#…`, `knowledgebase/studies/…#…`) have no
   callers by nature, and the newly added test functions have none either. No query
   in this set turned out to have a real caller the explorer missed, beyond the
   subprocess case above.

**Out of scope and truncated.** The seven out-of-scope entries above. Truncation is
reported in Q6.

## Q6 — Bounds hit, and whether raising them changed the answers

| Depth | Queries truncated | `truncated_by` | Paths returned |
| --- | --- | --- | --- |
| 1 | 9 / 44 | `depth` 9 | 24 |
| 2 | 2 / 44 | `depth` 2 | 36 |
| 3 | 1 / 44 | `depth` 1 | 42 |

The node cap and the time budget were never reached. Truncation falls as depth
rises, which is the expected shape for a small, well-resolved Python neighbourhood:
by depth 3 only one query is still cut off.

`scripts/bounds-probe.sh 4` re-ran the 43 head-side changed symbols at
`--depth 10 --node-cap 5000 --time-budget-ms 60000`. **One of 43 symbols returned a
different answer:**

| Selector | paths | nodes | entry points |
| --- | --- | --- | --- |
| `symbol:…/fput/metrics.py#_reported_feature:function` | 8 → 9 | 7 → 7 | 3 → 3 |

Nothing else changed — no new node, no new entry point, no new candidate test. On
this corpus the shipped bounds are not the limiting factor; the extractor's reach is.

## Scripted baseline versus the service — **agent-only, not a human usability study**

| Arm | Command | Wall clock | What it answered |
| --- | --- | --- | --- |
| Baseline | `scripts/baseline.sh 4` | **4.03 s** | 9 declaration names from the diff, `git grep -w` counts, `orbit-graph sync --full`, then `refs`/`impact` per resolvable selector |
| Service, cold | `scripts/service-study.sh 4` | **18.78 s** (6.08 s index, 12.70 s question pass) | 44 changed symbols, 44 × 3 evidence depths, 44 entry-point reports, 44 candidate-test reports, 44 searches |
| Service, warm | `scripts/service-study.sh 4 --warm` | **16.66 s** (0.13 s to ready, 16.53 s question pass) | same |

The baseline recovered only 9 of the 44 changed symbols, because it reads
declaration headers out of the unified diff and this commit is mostly *body*
changes plus Markdown headings, which have no `def`/`class` line in the diff. It
also said nothing about the seven unsupported-language files. The service is
slower and answers a strictly larger question set. Agent commands, not human effort.

## Reproduce

```sh
cargo build --workspace --locked --release
docs/evaluation/change-explorer/scripts/clone-corpora.sh
docs/evaluation/change-explorer/scripts/baseline.sh        4
docs/evaluation/change-explorer/scripts/service-study.sh   4
docs/evaluation/change-explorer/scripts/service-study.sh   4 --warm
docs/evaluation/change-explorer/scripts/summarize-study.sh 4
docs/evaluation/change-explorer/scripts/bounds-probe.sh    4
docs/evaluation/change-explorer/scripts/export-reports.sh  4
```
