# Study 5 — `orrery`, the research-records migration range (Python + JavaScript)

| Field | Value |
| --- | --- |
| Repository | `orrery`, cloned to `/tmp/orb-12393/repos/orrery` with `git clone --no-hardlinks file://~/workspace/constellation/codebases/orrery` |
| Base | `148b668391f575466282eaa81aab0dc943846a87` (ORB-11364) |
| Head | `82c24be9a49dbf9d775db9dc980c1de712d8bb8d` (ORB-11407) |
| Range | three commits: `a1c430d` (ORB-11374), `60cf431` (ORB-11393), `82c24be` (ORB-11407) |
| Comparison mode | `direct_base_head` (`effective_base_sha` equals `base_sha`) |
| Working tree | clean |
| `EXTRACTOR_VERSION` / `STORE_SCHEMA_VERSION` | 6 / 1 |
| Explorer commit | `c1332cf13a4daa7f3695950fa6767622ed61fd28`, release profile |
| Snapshots | base 162 files indexed / 216 written / 1 excluded; head 354 / 430 / 1 |
| Exported report | [`reports/5-orrery-82c24be9a49d.json`](reports/5-orrery-82c24be9a49d.json), [`.html`](reports/5-orrery-82c24be9a49d.html) |

Chosen as the second Python corpus, deliberately as a **multi-commit range** rather
than a single PR: 228 files change, of which only five are Python, and the range
contains a renamed nested function, a changed test, a rewritten JavaScript module
and 193 added record files. It is the study that stresses out-of-scope reporting.

## Q1 — Changed symbols

99 rows. By status:

| Status | Count |
| --- | --- |
| `added` | 70 |
| `removed` | 23 |
| `modified` | 4 |
| `signature_changed` | 2 |
| out of scope | 32 |

No `uncertain`, `moved` or `renamed` row.

The two `signature_changed` rows:

| Selector | Manual verdict |
| --- | --- |
| `symbol:lab/sims/oscillating-electron-retarded-fields/browser-check.py#summary:function` | **Right.** `browser-check.py:17@148b668391f5` is `def summary(values):`; `browser-check.py:12@82c24be9a49d` is `def summary(v):` — a parameter rename, which is a signature change. |
| `symbol:lab/sims/oscillating-electron-retarded-fields/app.js#motionNow:function` | **Right.** Retained in `app.js@82c24be9a49d` (line 40, now `const motionNow = () => ({…})`) with a changed body and shape. |

The 23 `removed` rows, checked one by one:

- **Seven Markdown headings** in `lab/sims/oscillating-electron-retarded-fields/README.md`
  (`Turning electron — retarded fields`, `Watch the turn`, `Trajectory, units and physics`,
  `Computed geometry versus guides`, `Reproducible validation`, `Measured performance and limits`,
  `Lineage`). **Right**: the README was rewritten and those headings do not exist at head.
- **Sixteen JavaScript functions** in `lab/sims/oscillating-electron-retarded-fields/app.js`:
  `arrow`, `buildSamples`, `compress`, `dispose`, `drawArrow`, `drawPower`, `drawStreams`,
  `dynamicGeometry`, `homeCamera`, `line`, `resize`, `selected`, `sphere`, `updateCamera`,
  `updateFields`, `updateGeometry`. **All sixteen are right**: each has exactly one
  `function`/`const` declaration in `app.js@148b668391f5` and none in `app.js@82c24be9a49d`
  (the simulation was rewritten from a three-dimensional renderer to a two-dimensional
  canvas). Three of the names — `line`, `selected`, `sphere` — do exist at head in
  sibling `.mjs` modules (`sampling.mjs`, `streamlines.mjs:3` as
  `export const selected = …`, `physics.mjs`), but `streamlines.mjs` already carried
  `selected` at the base revision, so this is duplication being deleted, not a move.
  The explorer cannot say either way, because `.mjs` is not indexed — see Q5.

## Q2 — Affected entry points and the rule that classified each

| Rule | Classifications |
| --- | --- |
| `test_function` | 55 |
| `main_function` | 41 |
| `crate_root_public_item` | 0 |
| `cli_command_handler` | 0 |

49 of the 99 queries returned no entry point, all with the disclosed
"no rule matched" reason; 19 of those also carried the syntax-driven-index,
macro-expansion and "1 tree entr(ies) were excluded" reasons.

The 41 `main_function` classifications resolve to six distinct symbols:

| Entry point | Times classified | Verdict |
| --- | --- | --- |
| `symbol:scripts/research_records.py#main:function` | 26 | Correct — a real `if __name__ == "__main__":` script entry |
| `symbol:scripts/native_registration_fixture.py#main:function` | 7 | Correct — likewise |
| `symbol:lab/sims/lattice-two-source-superposition/main.py#main:function` | 3 | **False**, see Q5 |
| `symbol:lab/sims/scarcity-rotation-curve-fit/main.py#main:function` | 3 | **False**, see Q5 |
| `symbol:lab/sims/flowing-lattice-photon-propagation/main.py#main:function` | 1 | **False**, see Q5 |
| `symbol:lab/sims/lattice-photon-propagation/main.py#main:function` | 1 | **False**, see Q5 |

`crate_root_public_item` correctly did not fire: no `__init__.py` changed.

## Q3 — Candidate tests and their source category

14 distinct candidate tests: 38 `call_path`, 31 `naming_heuristic`, and — notably —
**zero** `import_relationship`. The Python files here are scripts run from the
command line rather than importable packages, so the only import edges in play are
into third-party modules.

`symbol:scripts/research_records.py#supporting_paths:function` — the symbol whose
signature changed — gets exactly three candidates:

- `call_path` from `symbol:tests/test_research_records.py#test_landed_migration_survives_head_advance_and_refuses_source_drift:method`,
  note "call reference at tests/test_research_records.py:113; 1 hop(s) to the changed
  symbol". Verified: `tests/test_research_records.py:113@82c24be9a49d` is
  `baseline_supporting = checker.supporting_paths(checkout, baseline_catalogs)`.
  **Really exercises the change** — and with the new two-argument form.
- `call_path` from `…#test_source_addition_and_deletion_fail_closed:method`, at
  `tests/test_research_records.py:131@82c24be9a49d`, the same call. **Really exercises
  the change.**
- `naming_heuristic` `file:tests/test_research_records.py`, note "file name matches
  scripts/research_records.py". Correct signal, correctly labelled.

Notably `checker` there is a module object produced by
`importlib.util.spec_from_file_location` (`tests/test_research_records.py:36-37@82c24be9a49d`),
so this is a method-style call on a runtime-loaded module that the extractor still
resolved to the right symbol.

The 31 `naming_heuristic` candidates are name matches only, and each carries the
"name similarity only, no call or import edge is asserted" note.

## Q4 — Strongest evidence path for the two most consequential changed symbols

**`symbol:scripts/research_records.py#supporting_paths:function`** — the function
whose arity changed from `(catalogs)` to `(root, catalogs)`, the substance of the
head commit. 12 paths at depth 3 (7 `resolved_call`, 5 `observed_reference`), **not
truncated**. The three distance-1 `resolved_call` paths, all confidence `exact`:

- `symbol:scripts/research_records.py#baseline_source_report:function` → `supporting_paths`
  at `scripts/research_records.py:250@82c24be9a49d`
  (`        return report, supporting_paths(checkout, catalogs)`).
- `symbol:scripts/research_records.py#require_live_source_matches_baseline:function` → `supporting_paths`
  at `scripts/research_records.py:267@82c24be9a49d`
  (`    require(supporting_paths(ROOT, live_catalogs) == baseline_supporting,`).
- `symbol:scripts/research_records.py#build:function` → `supporting_paths`
  at `scripts/research_records.py:334@82c24be9a49d`
  (`    for path, scientific_role in supporting_paths(ROOT, catalogs):`).

Those are exactly the three call sites in the repository at that SHA, and each is a
site whose argument list had to change. Every hop verified.

**`symbol:lab/sims/oscillating-electron-retarded-fields/browser-check.py#summary:function`**
— the signature-changed Python helper in the rewritten simulation harness. 2 paths at
depth 3, not truncated, both distance 1, `resolved_call`, confidence `exact`:

- module level of `browser-check.py` → `#summary` at
  `browser-check.py:62@82c24be9a49d`. Verified: line 62 calls `summary(...)` twice in
  one expression (`'frameIntervalMs':summary(metrics['frameIntervalsMs']),
  'cpuComputeAndCanvasSubmitMs':summary(metrics['cpuRenderMs'])`), which is why two
  paths are reported with identical endpoints and the same `file:line`.

## Q5 — Unresolved impact: false associations and missed real callers

**Every false association found by manual inspection.**

1. 31 `naming_heuristic` candidate tests, none of which asserts a real relationship.
   They are correctly labelled and carry the disclaiming note.
2. **`list.append(...)` resolves to a changed function named `append`.**
   `scripts/native_registration_fixture.py#append:function` is a changed symbol in
   this range. Four unrelated simulation scripts are reported as its entry points
   because each contains an ordinary list `.append(` call that the resolver matched
   by bare method name, at confidence `same_module` and category
   `observed_reference` — not `heuristic_match`, so nothing marks it weak:
   - `lab/sims/lattice-two-source-superposition/main.py:277@82c24be9a49d` —
     `        resolution.append(`
   - `lab/sims/scarcity-rotation-curve-fit/main.py:774@82c24be9a49d` —
     `        rows.append(`
   - and the same shape in `lab/sims/flowing-lattice-photon-propagation/main.py`
     and `lab/sims/lattice-photon-propagation/main.py`.

   The chain then continues with genuine `resolved_call` edges
   (`native_registration_fixture.py:60@82c24be9a49d` → `cli`,
   `:27@82c24be9a49d` → `run`), so two further changed symbols pick up the same four
   false entry points at distance 2 and 3. Eight of the 41 `main_function`
   classifications in Q2 come from this one defect — the Python side of ORB-12416.
3. No `heuristic_match` evidence edge appeared in this study at all: the depth-3
   census is 2080 `resolved_call`, 605 `observed_reference`, 3
   `import_relationship`, 0 `heuristic_match`, and `skipped_low_confidence` was 0.
   That is exactly why finding 2 matters: the false association arrived at
   `same_module`, above the disclosure floor, rather than as a labelled heuristic.

**Every missed real caller / missed change found by manual inspection.**

1. **The renamed nested function is invisible.** `a1c430d` (inside this range)
   renames `snapshot` to `snap` inside `browser-check.py`'s runner:
   `browser-check.py:30@148b668391f5` is `    def snapshot(): return page.evaluate('window.orrery.snapshot()')`,
   `browser-check.py:23@82c24be9a49d` is ` def snap():return page.evaluate('window.orrery.snapshot()')`.
   The Python extractor indexes exactly one symbol in that file on each side —
   `summary:function` — so neither `snapshot` nor `snap` exists in the graph, and the
   rename is neither reported nor disclosed as a gap. Nested `def`s are not symbols.
2. **The `.mjs` rewrite is out of scope, so nothing can be said about where the 16
   removed `app.js` functions went.** Four `.mjs` files in the changed set are
   reported `unsupported_language`
   (`physics.mjs` base and head, `sampling.mjs` head, `dense-checks.mjs` head), and
   `streamlines.mjs` and `checks.mjs` are not even in the changed set. The
   `removed` rows carry the contract's "This is not a claim that nothing replaced
   it", which is the correct statement — but the answer to "did this move?" is
   unavailable.
3. **The subprocess-driven tests are not attached.** Six of the ten test methods in
   `tests/test_research_records.py@82c24be9a49d` drive the script through
   `subprocess.run([sys.executable, str(root / "scripts/research_records.py"), …])`
   (`tests/test_research_records.py:21-22@82c24be9a49d`, the `run_at` helper):
   `test_authority_is_exact_and_complete`, `test_isolated_migration_is_byte_identical`,
   `test_rollback_restores_every_legacy_json_byte`, `test_tampered_authority_fails_closed`,
   `test_wide_binary_failures_missingness_and_diagnosis_survive` and
   `test_cancelled_fixture_registers_without_science`. Every one of them runs `build`
   whatever subcommand it passes, because `main` calls it unconditionally before
   dispatching (`scripts/research_records.py:600@82c24be9a49d`,
   `    outputs = build(principia, astrolabe)`), and `build` calls
   `supporting_paths` at `scripts/research_records.py:334@82c24be9a49d`.
   None appears as a candidate test for `supporting_paths`; only the two that call the
   function directly on a loaded module object do.
4. 30 of the 99 queries returned zero paths at every depth, each with the three
   disclosed reasons. Sampling: `scripts/research_records.py#main` (a `__main__`
   entry, genuinely uncalled), the added record-file Markdown headings (no callers by
   nature), and the removed `app.js` functions on the base side. None turned out to
   have a real caller the explorer missed.

**Out of scope.** 32 entries, every one `unsupported_language`: 21 `.png` assets, 3
`.html` (`lab/gallery/index.html`, `.../index.html`, `.../assets/comparison.html`),
3 `.mjs`, `style.css`, and `requirements-research.txt`. That is the complete set of
changed non-source files, correctly refused rather than mislabelled.

## Q6 — Bounds hit, and whether raising them changed the answers

| Depth | Queries truncated | `truncated_by` | Paths returned |
| --- | --- | --- | --- |
| 1 | 58 / 99 | `depth` 58 | 441 |
| 2 | 46 / 99 | `depth` 45, `impact_node_cap` 1 | 891 |
| 3 | 31 / 99 | `depth` 29, `impact_node_cap` 2 | 1340 |

The time budget was never reached.

`scripts/bounds-probe.sh 5` re-ran the 76 head-side changed symbols at
`--depth 10 --node-cap 5000 --time-budget-ms 60000`. **17 of 76 symbols returned a
different answer.** The largest movers, all in `scripts/native_registration_fixture.py`
(a script whose helpers are called from many places):

| Selector | paths | nodes | entry points |
| --- | --- | --- | --- |
| `symbol:scripts/native_registration_fixture.py#git:function` | 21 → 365 | 7 → 159 | 1 → 27 |
| `symbol:scripts/native_registration_fixture.py#cli:function` | 200 → 363 | 88 → 157 | 3 → 27 |
| `symbol:scripts/native_registration_fixture.py#commit:function` | 9 → 356 | 4 → 158 | 1 → 27 |
| `symbol:scripts/research_records.py#external_index:function` | 4 → 5 | 3 → 4 | 1 → 1 |
| `symbol:lab/sims/…/app.js#worldScale:function` | 21 → 46 | 9 → 13 | 0 → 0 |

Neither of the two Q4 symbols changed: `supporting_paths` and `summary` are already
complete at the shipped bounds, which is consistent with their `truncated: false`.
Raising the bounds neither found a new caller for them nor changed their strongest
path.

## After ORB-12416 / ORB-12417 (re-run 2026-09-13)

Re-run against `orbit-graph-explorer` at `ab6e3e303689742910c5452d6acc700552428b89`
(`agent-main`), `EXTRACTOR_VERSION` 8, `STORE_SCHEMA_VERSION` 1 (unchanged), same
base/head pair, same scripts. Study 5 is re-run (not just 1–3) because
ORB-12416's fix touches Python bare-name attribute-call resolution
(`crates/orbit-graph/src/extract/languages/python.rs`), and this study is where
that side of the defect was filed.

**The Python false-positive (Q5 finding 2) is fixed.** The four unrelated
simulation scripts previously attached to
`scripts/native_registration_fixture.py#append:function` because each contains
an ordinary list `.append(` call are gone. `main_function` entry-point
classifications: **33 (was 41)** — exactly the eight false ones removed. The
two real `main_function` symbols now account for all 33:
`scripts/research_records.py#main` (26) and
`scripts/native_registration_fixture.py#main` (7); zero classifications remain
for `lattice-two-source-superposition`, `scarcity-rotation-curve-fit`,
`flowing-lattice-photon-propagation` or `lattice-photon-propagation`'s
`main.py#main`. Directly querying
`GET /api/entry-points?selector=symbol:scripts/native_registration_fixture.py%23append:function&side=head&depth=3`
now returns exactly the one real entry point
(`scripts/native_registration_fixture.py#main`), not five.

**But the same fix silently drops a genuinely correct call this study's own Q3
verified.** `tests/test_research_records.py:113` and `:131` call
`checker.supporting_paths(checkout, baseline_catalogs)`, where `checker` is a
module object returned by `importlib.util.spec_from_file_location`
(`load_checker`, `tests/test_research_records.py:36-37`) — a real call, in the
new two-argument form, that the original Q3 explicitly verified as "really
exercises the change." After the fix, `GET /api/candidate-tests` for
`symbol:scripts/research_records.py#supporting_paths:function` returns exactly
**one** candidate (`naming_heuristic`, `file:tests/test_research_records.py`);
the two `call_path` candidates from the original run are gone, and
`GET /api/evidence?...&depth=3` for the same selector returns only the 7 same-file
`exact` calls inside `research_records.py` — the `checker.supporting_paths(...)`
edge is absent at **every** confidence, including `fuzzy_name`. The receiver
`checker` has no statically-known type, so it now falls into the same
"receiver unknown, refuse to match" bucket as the `list.append` false positive
— correctly for `.append`, incorrectly here. **Filed as ORB-12425**: the fix
traded a disclosed false positive for a silent false negative, and nothing in
the response distinguishes "genuinely uncalled" from "receiver-typed call
refused."

**Everything else in Q1/Q5/Q6 unaffected.** `scripts/span-overlap.sh`-style
manual checks confirm the nested-function rename gap is unchanged: searching
the head snapshot for `snap` returns zero matches (`GET /api/search?q=snap`), so
`snapshot` → `snap` inside `browser-check.py` is still invisible, as before —
this is the "nested `def`s are not symbols" limitation, untouched by either fix.
The nine-test subprocess-invisibility finding (Q5, "missed real caller" #3) is
unaffected: none of the six `subprocess.run`-driven tests in
`tests/test_research_records.py` were ever attached to `supporting_paths`
before or after.

**Q6 bounds-probe: 12 of 76 symbols change when the bounds are raised** (was 17
of 76) — a real drop, consistent with the eight false `main_function`
associations no longer existing to be probed. `symbols_probed	76` /
`symbols_changed_by_raising	12`.

**Net verdict.** The Python side of ORB-12416 is a genuine, verified fix for the
false positive it targeted, with a genuine, verified regression alongside it —
this study is the one place in the re-run where the fix's incompleteness (no
receiver-type inference, just a stricter refusal rule) produces a *new* wrong
answer rather than only fixing an old one.

## Scripted baseline versus the service — **agent-only, not a human usability study**

| Arm | Command | Wall clock | What it answered |
| --- | --- | --- | --- |
| Baseline | `scripts/baseline.sh 5` | **10.03 s** | 52 declaration names from the diff, `git grep -w` counts, `orbit-graph sync --full`, then `refs`/`impact` per resolvable selector |
| Service, cold | `scripts/service-study.sh 5` | **30.39 s** (2.14 s index, 28.25 s question pass) | 99 changed symbols, 99 × 3 evidence depths, 99 entry-point reports, 99 candidate-test reports, 95 searches |
| Service, warm | `scripts/service-study.sh 5 --warm` | **33.15 s** (0.02 s to ready, 33.13 s question pass) | same |

The baseline recovered 52 names against the explorer's 99 rows, and it silently
mixed the two sides: a `-def` line and a `+def` line look the same to it, so it
cannot say which of the 52 exist at head. It also said nothing about the 32
out-of-scope files, which for this range is most of the change. Agent commands, not
human effort.

## Reproduce

```sh
cargo build --workspace --locked --release
docs/evaluation/change-explorer/scripts/clone-corpora.sh
docs/evaluation/change-explorer/scripts/baseline.sh        5
docs/evaluation/change-explorer/scripts/service-study.sh   5
docs/evaluation/change-explorer/scripts/service-study.sh   5 --warm
docs/evaluation/change-explorer/scripts/summarize-study.sh 5
docs/evaluation/change-explorer/scripts/bounds-probe.sh    5
docs/evaluation/change-explorer/scripts/export-reports.sh  5
```
