# Change-explorer evaluation (Milestone 5)

Five real change studies across two orbit-graph-supported language ecosystems.

The original run was against `orbit-graph-explorer` at commit
`c1332cf13a4daa7f3695950fa6767622ed61fd28`, `EXTRACTOR_VERSION` 6,
`STORE_SCHEMA_VERSION` 1. **Studies 1, 2, 3 and 5 were re-run on 2026-09-13**
against `orbit-graph-explorer` at commit
`ab6e3e303689742910c5452d6acc700552428b89` (`agent-main`), `EXTRACTOR_VERSION` 8,
`STORE_SCHEMA_VERSION` 1 (unchanged) — after ORB-12416 (same-named-method
self-loops) and ORB-12417 (impl-block cross-file resolution), the two resolver
defects the original run found dominating the Rust studies, both merged. Two
other fixes filed by the original evaluation landed in the same binary and show
up incidentally in the re-run data: ORB-12410 (`command:` selector span error)
and ORB-12411 (`import_relationship` bare-name matching). Study 4 was not
re-run: ORB-12416 also touches Python bare-name method-call resolution, which
study 5 exercises, but nothing in study 4's diff does (`git show` on the
ORB-12416 commit touches `python.rs`'s attribute-call handling only, and study
4's changed lines are in `run.py`/`metrics.py` argument and CSV handling, not
attribute calls with an unresolved receiver). Every number here is reproducible
from [`scripts/`](scripts/); every claim cites a selector and a `file:line@sha`.
Each affected study file below carries the original findings plus a dated
"After ORB-12416 / ORB-12417" section — read both to see what changed and what
did not.

**There is no accuracy percentage anywhere in this directory, by design.** A single
number would average a correct 200-path answer together with a resolver defect that
erases every cross-crate reference. What follows is lists: what was right, what was
wrong, what is unknown.

| # | Repository | Base → head | Subject | Report |
| --- | --- | --- | --- | --- |
| 1 | `orbit-graph` | `8ff25d082567` → `9e5c15986b14` | ORB-12372 reference-resolution rewrite | [1-orbit-graph-9e5c15986b14.md](1-orbit-graph-9e5c15986b14.md) |
| 2 | `orbit` | `1ca6416e0ba2` → `5a5b45fec9a8` | `orbit workspace teardown` CLI command handler | [2-orbit-5a5b45fec9a8.md](2-orbit-5a5b45fec9a8.md) |
| 3 | `orbit` | `ab6135e11aeb` → `156dc93d940e` | `TaskComplexity`, a library type used across crates | [3-orbit-156dc93d940e.md](3-orbit-156dc93d940e.md) |
| 4 | `observatory` | `0d05e34ef446` → `ae145d1c361a` | Python: renamed test, changed `assessment` signature | [4-observatory-ae145d1c361a.md](4-observatory-ae145d1c361a.md) |
| 5 | `orrery` | `148b668391f5` → `82c24be9a49d` | Python + JavaScript: 228-file migration range | [5-orrery-82c24be9a49d.md](5-orrery-82c24be9a49d.md) |

Hardware, corpus sizes, cold/warm indexing, per-endpoint latency, memory and
truncation counts: [performance.md](performance.md).

Corpora were cloned with `git clone --no-hardlinks file://…` into `/tmp/orb-12393`
and every explorer invocation was given an explicit `--cache-dir` under that
directory. No sibling checkout was written to.

[`reports/`](reports/) holds the machine-contract export for each study —
`<n>-<repo>-<short-sha>.json` plus the self-contained static `.html`. They are
bounded on purpose: `orbit-graph-explorer report` over every changed symbol at
depth 3 produces 3–8 MB of JSON per study, so the committed profile is the two
symbols each study's question 4 analyses, at `--depth 1`, with `--excerpts none`
and a pinned `--generated-at` (see [`scripts/export-reports.sh`](scripts/export-reports.sh)).
Even so they total about 3 MB, most of it study 2's 200 node-capped `fuzzy_name`
paths — which is itself the finding. Regenerate at any depth with
`DEPTH=3 scripts/export-reports.sh`.

## Per study: what was right, what was wrong, what is unknown

The lists below describe the original run at `c1332cf`, `EXTRACTOR_VERSION` 6,
kept intact as the historical record. For studies 1, 2, 3 and 5, the linked
study file's dated "After ORB-12416 / ORB-12417" section is the current
picture at `ab6e3e3`, `EXTRACTOR_VERSION` 8.

### Study 1 — `orbit-graph`, ORB-12372

**Right**

- All 61 changed-symbol rows correspond to real symbols in the two snapshots; the
  20 `added`, 27 `modified` and 2 `signature_changed` rows match the diff.
- Both `signature_changed` rows quote the actual old and new signatures.
- Five of the seven `removed` rows are true deletions with no successor.
- The strongest evidence path for both question-4 symbols is exact and verified
  hop by hop against the source at `9e5c15986b14`.
- The ORB-12372 known omission — calls nested in closures — is **closed**: the
  `resolve_target` call inside `graph.with_read_connection(|conn| …)` is recorded
  as a `resolved_call` edge at confidence `exact`, and
  `scripts/closure-gap-probe.sh` shows all four shapes ORB-12379 names extracting
  at extractor version 6.
- The `main_function` entry points are exactly the repository's two binaries.
- Every fuzzy fallback carries the "name-only matches … may include unrelated
  symbols" note and a `fuzzy_name` confidence.

**Wrong**

- Two intra-file renames are reported as `removed` + `added`
  (`ResolvedRef::qualified` → `::candidate`, `unique_candidate_qualified` →
  `unique_candidate`). Contract-conformant — rung 4 needs Git file-rename evidence —
  but no row states the relationship.
- All five `uncertain` rows are symbols the source proves unchanged (`from_db`,
  `line_for`, `new` in `src/query/refs.rs`; `GraphError:impl`; `TestWorktree:impl`).
- Four fixture symbols under `tests/fixtures/change-explorer/**/src/lib.rs` are
  classified as `crate_root_public_item` entry points of this crate, reached through
  a single `fuzzy_name` hop (ORB-12413).
- Nine `import_relationship` candidate tests attach the Python extractor's
  `collect_call_ref` to `tests/{c,csharp,java,…}.rs`, which never exercise it.
- A golden data file, `file:src/query/tests/refs.golden.json`, is offered as a
  candidate test.

**Unknown**

- 18 of 61 queries returned no path at any depth, with the syntax-driven-index and
  macro-expansion reasons. Manual inspection found no real caller hidden there, but
  the answer is "no path in the indexed evidence", not "no caller".
- Raising the bounds to depth 10 / node cap 5000 changed the answer for 36 of 54
  symbols. The shipped answer is a bounded view and says so.

### Study 2 — `orbit`, `orbit workspace teardown`

**Right**

- The changed-symbol slice is exactly right: eight new functions, one changed test
  helper, one new test helper, six new tests, three modified types and three
  modified runbook headings, matching `git diff` declaration for declaration. No
  spurious row, no missing row, nothing wrongly called removed.
- The `signature_changed` row on the `register` test helper is correct.
- The strongest evidence path for `resolve_teardown_target` is exact and verified.
- The 16 fully resolved entry points are all real.

**Wrong**

- `cli_command_handler` — the rule written for this exact case — fired zero times,
  and `GET /api/entry-points` returned HTTP 400 for three changed symbols
  (ORB-12410).
- The dispatcher `WorkspaceCommand::execute` is recorded as calling **itself** seven
  times at confidence `exact` (ORB-12416).
- 496 of 512 entry points and 496 of 508 `call_path` candidate tests ride one
  `fuzzy_name` hop through the name `execute`; every `.execute(&runtime)` call site
  in the CLI test tree is attached to this change. The six tests that genuinely
  drive the changed handler are in that list, indistinguishable from the rest.
- Only two of the eight new production functions get a candidate test backed by a
  resolved call; the other six are reached only through the fuzzy fan-out.
- The real caller of the changed handler —
  `crates/orbit-cli/src/command/workspace/command.rs:61@5a5b45fe` — is absent at the
  shipped bounds **and** at depth 10 / node cap 5000.

**Unknown**

- 11 of 25 queries returned no path at any depth. Manual inspection found no real
  caller hidden there.
- Whether `cli_command_handler` classifies correctly at all: it has not fired on any
  corpus in this evaluation.

### Study 3 — `orbit`, `TaskComplexity`

**Right**

- The four `added` and nine `modified` rows match the diff.
- `parse_task_complexity`'s signature change is correct, and its evidence is
  **complete**: one direct caller and both of that caller's callers, all verified.
- `TaskComplexity:impl` is correctly the one collapsed impl selector whose span
  overlaps a changed line.
- The five fully resolved entry points are the changed tests themselves, correctly
  classified.

**Wrong**

- Ten of the twelve `uncertain` rows are symbols the source proves unchanged; they
  appear only because their canonical selector collapses several indexed rows.
- No resolved cross-crate reference to `TaskComplexity` exists at all: 4 refs at
  `exact` inside the defining file, 56 at `fuzzy_name` with a null target across 32
  files (ORB-12417). The type's consumers in `orbit-cli`, `orbit-config`,
  `orbit-core`, `orbit-store` and `orbit-web` are missing from the evidence.
- Not one of the 288 `call_path` candidate tests is backed by a resolved call.
- 133 of the 136 `import_relationship` candidate tests for `TaskComplexity` are test
  files that import the crate and do not test complexity handling.
- At depth 10 the entry-point endpoint fails outright for ten symbols with the
  ORB-12410 span error; the bounds probe records those as "0 entry points", which is
  a failure, not a finding.

**Unknown**

- 12 of 26 queries returned no path at any depth.
- What the cross-crate impact of this change really is, from the explorer alone. On
  this question `git grep -w TaskComplexity` outperforms the tool.

### Study 4 — `observatory`, FPUT mode labels

**Right**

- 23 `modified`, 19 `added`, 1 `signature_changed` — all matching the diff.
- `assessment`'s added `gating_controls` parameter is correctly a signature change.
- The seven out-of-scope entries are exactly the changed binary and CSV files, each
  named with `unsupported_language` and the snapshot it came from — including the
  CSV that carries the substance of the change.
- The strongest evidence path for both question-4 symbols is exact and verified;
  for `assessment` the two reported call sites are the only two in the repository.
- All 13 `call_path` candidate tests are real call paths; the two `test_numerics.py`
  tests genuinely exercise `evaluate_metrics`.
- `main_function` fired only on `run.py#main`, which is correct.

**Wrong**

- The renamed test `test_forced_control_failure_has_distinct_failed_state` →
  `test_forced_gating_control_failure_has_distinct_failed_state` is reported as
  `removed` + `added`.
- `file:experiments/economics/_parallax/tests/test_cli.py` is offered as an
  `import_relationship` candidate for `run.py#main` purely because it imports a
  different `main` from `parallax.cli` (ORB-12411).
- `browser_check.py#main` is offered as a `naming_heuristic` candidate for
  `run.py#main` — correctly labelled, substantively unrelated.

**Unknown**

- `assessment` has zero candidate tests, although five tests exercise it through
  `subprocess.run(["uv", "run", str(RUNNER), …])`. Subprocess invocation is invisible
  to the index and the response says so.
- 34 of 44 queries returned no path at any depth.

### Study 5 — `orrery`, research-records migration range

**Right**

- Both `signature_changed` rows are correct (`summary(values)` → `summary(v)`;
  `motionNow` reshaped).
- All 23 `removed` rows are true: seven README headings and sixteen `app.js`
  functions that exist at base and not at head.
- 32 out-of-scope entries — 21 PNGs, three HTML files, three `.mjs` modules, a CSS
  file and a requirements file — are the complete set of changed non-source files.
- The strongest evidence paths for both question-4 symbols are exact, complete
  (all three `supporting_paths` call sites) and verified.
- Both `call_path` candidate tests for `supporting_paths` really exercise the new
  two-argument form, including through a module object loaded by `importlib`.
- No `heuristic_match` evidence edge appeared in this study at all.

**Wrong**

- Eight of the 41 `main_function` entry points are false: four unrelated simulation
  scripts are attached to the changed `native_registration_fixture.py#append`
  because each contains an ordinary list `.append(` call, matched by bare method
  name at confidence `same_module` and category `observed_reference` — above the
  heuristic floor, so nothing marks them weak. The Python side of ORB-12416.
- The 31 `naming_heuristic` candidates are weak but correctly labelled.

**Unknown**

- The rename `snapshot` → `snap` inside `browser-check.py` is invisible: nested
  `def`s are not indexed as symbols, so neither name exists in the graph and no gap
  is disclosed for them.
- Where the 16 removed `app.js` functions went. Three of the names exist at head in
  `.mjs` modules the extractor does not index; the explorer can only say they are
  gone from `app.js`.
- Six of the ten tests in `tests/test_research_records.py` drive the script through
  `subprocess.run` and are not attached to any changed symbol.
- 30 of 99 queries returned no path at any depth.

## Fixture corpus at this commit

`scripts/fixture-corpus.sh` re-runs
`cargo test -p orbit-graph-cli --locked --test change_explorer_fixtures`. This
evaluation changes no code, so the harness runs against the tree at
`c1332cf13a4daa7f3695950fa6767622ed61fd28`: **10 passed, 0 failed**.

| Case | Languages | Changed symbols | Expected refs | Expected impact | Candidate tests | Known gaps |
| --- | --- | --- | --- | --- | --- | --- |
| `ambiguous-same-name` | rust | 1 | 4 | 1 | 0 | 0 |
| `branch-divergence` | rust | 2 | 0 | 0 | 0 | 0 |
| `changed-signature` | python | 1 | 6 | 2 | 3 | 1 |
| `changed-test` | rust | 1 | 0 | 0 | 1 | 1 |
| `cycle` | rust | 1 | 4 | 2 | 0 | 0 |
| `direct-call` | rust | 1 | 4 | 2 | 1 | 0 |
| `generated-unsupported` | rust | 1 | 0 | 0 | 0 | 1 |
| `removed-symbol` | rust | 1 | 1 | 1 | 0 | 0 |
| `renamed-file` | python | 4 | 8 | 0 | 0 | 1 |

**No `expected.json` was edited.** The harness re-verifies every `known_gap` whose
`check` names a supported type, and all four gaps still hold at extractor version 6:
macro-argument calls (`changed-test`), `getattr` dynamic dispatch
(`changed-signature`), unsupported languages (`generated-unsupported`) and
cross-path rename identity (`renamed-file`). None of them is the closure /
method-chain gap that ORB-12379 (`4c1a4db`) fixed, so ORB-12379 closed no fixture
expectation. The closure gap is instead verified directly in study 1, question 4,
on the real ORB-12372 change.

## Defects filed

Each was found during this evaluation, reproduced, and filed as a separate bug task
rather than fixed here.

| Task | Summary |
| --- | --- |
| ORB-12410 | `command:` selector resolution pairs the command's file with the handler symbol's span: `show` returns wrong source, or fails with "invalid span"; `/api/entry-points` returns 400 and `cli_command_handler` can never fire |
| ORB-12411 | Candidate-test `import_relationship` matches an imported symbol by bare name, attaching unrelated test files |
| ORB-12412 | Per-side indexing progress: both sides share one `started_at`, and `languages` is emptied once a side reaches `ready` |
| ORB-12413 | Entry points reached only through a `fuzzy_name` hop are listed beside resolved ones; `crate_root_public_item` fires inside fixture trees |
| ORB-12416 | Method calls resolve to a same-named method in the calling file at `exact` confidence: false self-loops, real callee hidden with no fallback |
| ORB-12417 | A Rust type with any `impl` block cannot resolve cross-file references — the impl symbols share the type's name and defeat every uniqueness test |

## Limitations (handoff)

Ordered by how much they distort an answer.

1. **Receiver-type-unaware call resolution is now the binding constraint, not
   the two defects that used to mask it.** ORB-12416 and ORB-12417 are both
   fixed and verified in the 2026-09-13 re-run (see each study's dated After
   section): `WorkspaceCommand::execute`'s false self-loop is gone and its real
   caller is now found (study 2), and `TaskComplexity`'s 56 cross-crate
   references now resolve to a real target instead of `NULL` (study 3). What
   remains is the defect underneath both: the resolver has no receiver-type
   inference, so any method call on a value of unknown static type still falls
   back to a name-only match against every same-named method in the corpus.
   Studies 2 and 3 are still dominated by that fallback — 442/470 (94.0%,
   was 496/512) and 288/288 `call_path` candidates still 100% `heuristic_match`
   — because a same-name method call was never what either fix targeted. Until
   the resolver can distinguish call sites by receiver type, "affected callers"
   on a Rust workspace this size is still mostly a name search with extra
   steps, and the payload's `confidence` field is the only thing keeping that
   honest. A related, narrower gap surfaced by the fix: `TaskComplexity`'s
   references now resolve at `same_module` confidence rather than `NULL`, but
   the candidate-test/entry-point *category* classifier still buckets
   `same_module` under `heuristic_match`, same as before the fix — the
   underlying data improved without the surfaced category changing (study 3's
   After section).
2. **A weak hop is disclosed per edge but not per row.** Unaffected by either
   fix and confirmed unchanged in the re-run: study 2's `call_path` candidates
   still split 442 `heuristic_match` / 12 `resolved_call` behind one `source`
   label, same shape as the original 496/12. The evidence pane groups
   `heuristic_match` separately; the entry-point list does not, and a
   `call_path` candidate can carry `category: heuristic_match`. Both fields are
   in the payload and both badges render, but the strong label reads first
   (ORB-12413).
3. **Ambiguous selectors inflate the changed-symbol list.** Unaffected by
   either fix. Re-confirmed in the 2026-09-13 re-run: `scripts/span-overlap.sh 3
   crates/orbit-types/src/task/model.rs` still finds ten of study 3's twelve
   `uncertain` rows, and `scripts/summarize-study.sh 1` still finds all five of
   study 1's, provably unchanged. The contract's refusal to guess is right; the
   cost is that a reader cannot tell an ambiguous-and-changed row from an
   ambiguous-and-unchanged one.
4. **Intra-file renames are never paired.** Unaffected by either fix.
   Re-confirmed in the re-run: study 1's `changed-symbols.json` still reports
   `ResolvedRef::qualified` → `::candidate` and
   `unique_candidate_qualified` → `unique_candidate` as `removed` + `added`
   pairs, not renames. Rung 4 of the ladder needs Git file-rename evidence, so
   a rename inside an otherwise-modified file always surfaces this way. Seen in
   studies 1 and 4.
5. **Runtime invocation is invisible, and it is how Python test suites drive
   CLI entry points.** Unaffected by either fix, and the re-run adds a second,
   sharper example: study 5's `checker.supporting_paths(...)` call (a
   receiver-typed call on an `importlib`-loaded module) is no longer even
   fuzzy-matched after ORB-12416's Python-side change, so this is now also a
   *resolution* gap, not only a runtime-invisibility one — filed as
   **ORB-12425**. Study 4's `assessment` and study 5's six subprocess-driven
   tests remain real coverage the tool cannot see; the disclosure is still
   generic ("the index is syntax-driven"), not specific to the subprocess
   call.
6. **Nested functions are not symbols.** Unaffected by either fix. Re-confirmed
   in the re-run: `GET /api/search?q=snap` against the re-built study-5 index
   returns zero matches, so study 5's `snapshot` → `snap` rename is still
   absent from the graph with no gap recorded.
7. **The one-second interaction target is met on the small corpora and missed
   on `orbit`, by a smaller margin than before the fixes.** Re-measured
   2026-09-13 (warm cache, no cold build, same subjects, `EXTRACTOR_VERSION`
   8): on study 3's subject, `/api/candidate-tests` is now 1 298 ms p50 / 1 687
   ms p95 (was 1 474 / 2 017), `/api/evidence` at depth 3 is 854 ms p50 / 882 ms
   p95 (was 1 141 / 1 340) and `/api/entry-points` at depth 3 is 883 ms p50 /
   924 ms p95 (was 1 065 / 1 187) — all improved, none under target. Study 2's
   subject moved the other way on some endpoints (`/api/candidate-tests` 800 ms
   p50, up from 741, but still below the study-3 figure). The cause is
   unchanged from item 1: the same name-only fan-out, now with fewer false
   entries in it. Full tables in [performance.md](performance.md). Cold
   indexing was not re-measured (unaffected by either fix; observed
   incidentally at 3.5–470 s across the four re-run studies, consistent with
   the original 1.5–441 s range) — still 6.6–8.7 minutes for `orbit`, with the
   two sides built sequentially; warm launch 0.02–1.03 s.
8. **Depth 3 plus a 200-node cap is a presentation choice, not a completeness
   claim.** Re-measured 2026-09-13: raising both changed the answer for 36/54
   (study 1, unchanged), 11/25 (study 2, was 12/25), 11/26 (study 3, was
   13/26), 1/43 (study 4, not re-run) and 12/76 (study 5, was 17/76 — the drop
   tracks the eight false `main_function` associations ORB-12416 removed) of
   the symbols in studies 1–5. On `orbit` the extra material is still more
   fuzzy fan-out, and study 2's one previously-missing real caller is no longer
   missing at any bound, including the shipped one (study 2's After section).
9. **This evaluation is agent-only.** Every baseline-versus-service comparison
   measures commands an agent issues. No human used the UI, and no headless
   browser was available on this host, so no browser timing was recorded — true
   of the original run and, unchanged, of the 2026-09-13 re-run.

## Reproduce everything

```sh
cargo build --workspace --locked --release
docs/evaluation/change-explorer/scripts/clone-corpora.sh
docs/evaluation/change-explorer/scripts/corpus-sizes.sh
docs/evaluation/change-explorer/scripts/fixture-corpus.sh
for s in 1 2 3 4 5; do
  docs/evaluation/change-explorer/scripts/baseline.sh        "$s"
  docs/evaluation/change-explorer/scripts/service-study.sh   "$s"
  docs/evaluation/change-explorer/scripts/service-study.sh   "$s" --warm
  docs/evaluation/change-explorer/scripts/summarize-study.sh "$s"
  docs/evaluation/change-explorer/scripts/bounds-probe.sh    "$s"
  docs/evaluation/change-explorer/scripts/performance.sh     "$s" 25
done
docs/evaluation/change-explorer/scripts/progress-cancel.sh 2
docs/evaluation/change-explorer/scripts/closure-gap-probe.sh
docs/evaluation/change-explorer/scripts/span-overlap.sh 3 crates/orbit-types/src/task/model.rs
docs/evaluation/change-explorer/scripts/export-reports.sh
```

Budget roughly 45 minutes of wall clock on a machine like the one in
[performance.md](performance.md): the two `orbit` studies each cold-index twice
(once for the study pass, once for the performance pass) and `progress-cancel.sh`
cold-indexes twice more, at 6.6–8.7 minutes per build. Everything else is seconds.
`SCRATCH` defaults to `/tmp/orb-12393` and needs about 1.5 GiB.
