# Study 1 — `orbit-graph`, the ORB-12372 reference-resolution change

| Field | Value |
| --- | --- |
| Repository | `orbit-graph`, cloned to `/tmp/orb-12393/repos/orbit-graph` with `git clone --no-hardlinks file://~/workspace/constellation/codebases/orbit-graph` |
| Base | `8ff25d082567cf8d22b25e393160565f3490f9f1` (ORB-12366, "Close the public-API gaps G1–G7") |
| Head | `9e5c15986b14c51665145b1df84de7623c70dcd2` (ORB-12372, "Make reference resolution honor qualified paths and explicit imports") |
| Comparison mode | `direct_base_head` (`effective_base_sha` equals `base_sha`) |
| Working tree | clean (`working_tree.dirty = false`, no entries) |
| `EXTRACTOR_VERSION` / `STORE_SCHEMA_VERSION` | 6 / 1 |
| Explorer commit | `c1332cf13a4daa7f3695950fa6767622ed61fd28` (`orbit/ORB-12393-6aa5f191`), release profile |
| Snapshots | base 177 files indexed / 203 written / 0 excluded; head 177 / 203 / 0 |
| Exported report | [`reports/1-orbit-graph-9e5c15986b14.json`](reports/1-orbit-graph-9e5c15986b14.json), [`.html`](reports/1-orbit-graph-9e5c15986b14.html) |

Note on paths: this base/head pair predates the `crates/` move (ORB-12380, `1969ef5`),
so the selectors below are `src/…` and `explorer/…`, which is what those two commits
contain.

## Q1 — Changed symbols

61 rows. By status:

| Status | Count |
| --- | --- |
| `modified` | 27 |
| `added` | 20 |
| `removed` | 7 |
| `uncertain` | 5 |
| `signature_changed` | 2 |
| out of scope | 0 |

No `moved` or `renamed` row was produced. Every `uncertain`, `removed` and
`signature_changed` row, with the manual verdict:

| Status | Selector | Manual verdict |
| --- | --- | --- |
| `signature_changed` | `symbol:src/lib.rs#EXTRACTOR_VERSION:const` | **Right.** `src/lib.rs:67@8ff25d082567` is `pub const EXTRACTOR_VERSION: u32 = 4;`, `src/lib.rs:69@9e5c15986b14` is `pub const EXTRACTOR_VERSION: u32 = 5;`. The row quotes both. |
| `signature_changed` | `symbol:src/query/refs.rs#resolve_target:function` | **Right.** Return type changed `Result<RefTarget, GraphError>` → `Result<QueryTarget, GraphError>` (`src/query/refs.rs:134@8ff25d082567` → `src/query/refs.rs:138@9e5c15986b14`). |
| `uncertain` (2 candidates) | `symbol:src/lib.rs#GraphError:impl` | **Right to refuse, but the symbol did not change.** Three blocks collapse onto one selector — `impl GraphError` (head lines 476–506), `impl Display for GraphError` (508–521), `impl std::error::Error for GraphError` (523) — so no body comparison is possible. `scripts/span-overlap.sh 1 src/lib.rs` reports the file's only changed head lines as 67–69 (`EXTRACTOR_VERSION` and its doc), which overlap none of them. |
| `uncertain` (2 candidates) | `symbol:src/query/refs.rs#from_db:method` | **Right to refuse, but the symbol did not change.** Two `from_db` methods collapse (head lines 367–378 and 395–409); the file's last changed head line is 291. |
| `uncertain` (2 candidates) | `symbol:src/query/refs.rs#line_for:method` | Same shape (head lines 460–484 and 515–519). Unchanged. |
| `uncertain` (2 candidates) | `symbol:src/query/refs.rs#new:method` | Same shape (head lines 453–458 and 502–513). Unchanged. |
| `uncertain` (2 candidates) | `symbol:src/sync/tests/pass2.rs#TestWorktree:impl` | **Right to refuse, and neither block changed.** `impl TestWorktree` (head lines 591–617) and `impl Drop for TestWorktree` (619–623) share the selector; the file's changed head line ranges all end at 324, so neither overlaps. |
| `removed` | `symbol:src/sync/pass2.rs#qualified:method` | **Wrong label, right disclosure.** `ResolvedRef::qualified` (`src/sync/pass2.rs:404@8ff25d082567`) was renamed to `ResolvedRef::candidate` (`src/sync/pass2.rs:523@9e5c15986b14`) with a changed parameter. The explorer reports `removed` + a separate `added` row for `candidate`. |
| `removed` | `symbol:src/sync/pass2.rs#unique_candidate_qualified:function` | **Wrong label, right disclosure.** Renamed to `unique_candidate` (`src/sync/pass2.rs:321@9e5c15986b14`) with a changed return type. Reported as `removed` + `added`. |
| `removed` | `symbol:src/sync/pass2.rs#qualified_matches_import:function` | **Right.** Deleted (`src/sync/pass2.rs:337@8ff25d082567`); its job is now split across `candidate_matches_module` and `candidate_matches_qualified`, which are different functions with different signatures. |
| `removed` | `symbol:src/sync/pass2.rs#unique_distinct_qualified:function` | **Right.** Deleted (`src/sync/pass2.rs:315@8ff25d082567`); the de-duplication moved inline into `resolve_import`'s `BTreeMap`. |
| `removed` | `symbol:src/sync/pass2.rs#unique_symbol_id_for_qualified:function` | **Right.** Deleted (`src/sync/pass2.rs:288@8ff25d082567`); the hint now comes from `SymbolCandidate::id`. |
| `removed` | `symbol:src/sync/tests/pass2.rs#same_module_resolution_uses_unique_cross_file_module_match:test` | **Right.** Replaced by `qualified_cross_file_resolution_is_exact`, which asserts a different thing (exact rather than same-module confidence). |
| `removed` | `symbol:src/sync/tests/pass2.rs#target_symbol_hint_is_null_when_resolved_qualified_is_ambiguous:test` | **Right.** Replaced by `duplicate_import_targets_remain_fuzzy_and_unhinted`, again a different assertion. |

Why the two intra-file renames are not paired: the pairing ladder's `renamed` rung
requires "Git rename or similarity evidence for the containing file"
(`docs/design/change-explorer.md`, *Changed-symbol identity*). `src/sync/pass2.rs`
is `modified`, not renamed, so the ladder cannot reach rung 4 and falls through to
one-sided entries. Both rows carry the contract's disclaimer — "Present in base and
absent from head. This is not a claim that nothing replaced it." — so the output is
honest, but a reader has to notice the paired `added` row themselves.

## Q2 — Affected entry points and the rule that classified each

Across all 61 queries at depth 3, `GET /api/entry-points` fired three of the four
disclosed rules:

| Rule | Classifications |
| --- | --- |
| `test_function` | 178 |
| `crate_root_public_item` | 71 |
| `main_function` | 12 |
| `cli_command_handler` | 0 |

26 of the 61 queries returned no entry point at all, each with the disclosed reason
"No affected symbol matched a disclosed entry-point rule… no score is assigned in
place of a rule."

The two `main_function` entry points are the only distinct ones:
`symbol:src/main.rs#main:function` and `symbol:explorer/src/main.rs#main:function`,
each fired by name (`main.rs:@9e5c15986b14`). Both are correct: this repository has
exactly those two binaries at that revision.

`crate_root_public_item` fired on `symbol:src/lib.rs#with_read_connection:method`
and `symbol:src/lib.rs#refs:method` (correct — both are `pub` in the crate root),
and on four fixture symbols that are not part of this crate at all:
`symbol:tests/fixtures/change-explorer/ambiguous-same-name/{base,head}/src/lib.rs#call_{a,b}:function`.
See Q5.

`cli_command_handler` fired nowhere here, which is correct for this pair: no
`command:` selector resolves into `src/sync/pass2.rs` or `src/query/refs.rs`.

## Q3 — Candidate tests and their source category

165 distinct candidate tests across the 61 queries: 198 `call_path`, 244
`import_relationship` and 133 `naming_heuristic` associations.

For `symbol:src/sync/pass2.rs#resolve_ref:function` (19 candidates):

- `call_path` — `symbol:src/sync/tests/pass2.rs#duplicate_import_targets_remain_fuzzy_and_unhinted:test`
  and `symbol:src/sync/tests/pass2.rs#pass2_failure_rolls_back_ref_rewrites_and_meta_update:test`.
  **Both really exercise the change**: `src/sync/tests/pass2.rs@9e5c15986b14` drives
  a real sync through `pass2::run`, which calls `resolve_ref` at
  `src/sync/pass2.rs:37@9e5c15986b14`.
- `call_path` — `symbol:src/sync/tests/pass1.rs#pass1_returns_extracted_refs_for_pass2_handoff:test`,
  `…#pass1_writes_relations_but_not_refs:test`. **Do not exercise it**: they assert
  pass-1 output and stop before pass 2.
- `call_path` — `symbol:src/cli/tests/mod.rs#invalid_selector_errors_are_selector_parse_errors:test`,
  `…#run_json:function`. **Exercise it incidentally**: they run the CLI end to end,
  which syncs, which reaches `resolve_ref`.
- `call_path` — the four `tests/fixtures/change-explorer/ambiguous-same-name/**` fixture
  functions. **False**, see Q5.
- `import_relationship` — eight `src/**/tests/*.rs` files that import `crate::sync`
  or a sibling module. **File-level only, and correctly labelled as such**; of these,
  only `src/sync/tests/mod.rs` really drives pass 2.
- `naming_heuristic` — `file:src/sync/tests/pass2.rs`. Correct and correctly labelled.

For `symbol:src/query/refs.rs#resolve_target:function` (14 candidates), the ten
`call_path` entries in `src/query/tests/refs.rs@9e5c15986b14` all genuinely exercise
the change: each calls `Graph::refs`, which is `src/lib.rs:250@9e5c15986b14`
`query::refs::run(self, sel, opts)`, which calls `resolve_target` at
`src/query/refs.rs:23@9e5c15986b14`. The two `explorer/tests/snapshot_integration.rs`
entries also genuinely reach it through `Snapshot::refs`. The two `naming_heuristic`
entries (`file:src/query/tests/refs.rs`, `file:src/query/tests/refs.golden.json`)
are name matches; the `.golden.json` one is a data file, not a test.

## Q4 — Strongest evidence path for the two most consequential changed symbols

**`symbol:src/sync/pass2.rs#resolve_ref:function`** — the resolution ladder itself.
112 paths at depth 3, `truncated_by: "depth"`. Strongest path (`resolved_call`,
distance 1, confidence `exact`):

- `symbol:src/sync/pass2.rs#run:function` → `symbol:src/sync/pass2.rs#resolve_ref:function`
  at `src/sync/pass2.rs:37@9e5c15986b14`.
  Verified: line 37 reads `let resolved = resolve_ref(&tx, &file_refs.file_path, raw_ref)?;`.

**`symbol:src/query/refs.rs#resolve_target:function`** — the signature-changed
public query entry. 20 paths at depth 3, `truncated_by: "depth"`. Strongest paths:

- distance 1, `resolved_call`, `exact`:
  `symbol:src/query/refs.rs#run:function` → `symbol:src/query/refs.rs#resolve_target:function`
  at `src/query/refs.rs:23@9e5c15986b14`.
  Verified: line 23 reads `let target = resolve_target(conn, sel)?;`.
- distance 2, `resolved_call`, `exact`:
  `symbol:src/lib.rs#refs:method` → `symbol:src/query/refs.rs#run:function`
  at `src/lib.rs:250@9e5c15986b14`, then the hop above.
  Verified: line 250 reads `query::refs::run(self, sel, opts)`.

**The ORB-12372 known omission is closed.** ORB-12372 recorded, as a known
omission, calls nested in closures. ORB-12379 (`4c1a4db`) fixed the Rust extractor
by recursing into method-chain receivers and `?`/`.await`-wrapped receivers and by
no longer emitting a turbofish chain receiver's source text as a callee name; it
bumped `EXTRACTOR_VERSION` to 6.

Two pieces of evidence that the omission is closed at version 6:

1. In this study, `src/query/refs.rs:23@9e5c15986b14` — `let target = resolve_target(conn, sel)?;` —
   sits inside the closure passed to `graph.with_read_connection(|conn| { … })` at
   `src/query/refs.rs:22@9e5c15986b14`, and the edge is present, resolved and
   `exact`.
2. A four-shape probe against the built binary
   (`scripts/closure-gap-probe.sh`, a single-file crate reproducing each shape
   ORB-12379's own extractor tests name) extracts every one at version 6:

   | Shape | Call site | Extracted callee | Confidence |
   | --- | --- | --- | --- |
   | closure passed to one method call | `graph.with_read_connection(\|conn\| resolve_target(conn))` | `resolve_target` | `exact` |
   | closure inside a method chain | `.filter(\|c\| qualified_matches_import(c))` | `qualified_matches_import` | `exact` |
   | `?`-wrapped call as a chain receiver | `fetch()?.checked_add(1)` | `fetch` | `exact` |
   | turbofish on a chain | `values.iter().copied().collect::<Vec<_>>()` | `iter`, `copied`, `collect` (method names) | `fuzzy_name` |

   No bogus callee name derived from chain source text appears in any of them.

The fixture corpus's four remaining `known_gaps` are unrelated to this shape and all
still hold — see *Fixture corpus at this commit* in [`README.md`](README.md).

## Q5 — Unresolved impact: false associations and missed real callers

**Every false association found by manual inspection.**

1. `symbol:tests/fixtures/change-explorer/ambiguous-same-name/base/src/lib.rs#call_a:function`
   → `symbol:src/sync/pass2.rs#run:function`, and the three sibling rows
   (`call_b`, and both under `head/`). Reported as a `crate_root_public_item` entry
   point for `resolve_ref` at distance 2, and as `call_path` candidate tests. The
   first hop is at `tests/fixtures/change-explorer/ambiguous-same-name/base/src/lib.rs:5@9e5c15986b14`,
   whose source is `a::run()` — a call to `run` in the fixture's own `src/a.rs`, not
   to `src/sync/pass2.rs#run`. The edge itself is labelled `heuristic_match` /
   `fuzzy_name`, so the evidence view discloses it; the entry-point row does not.
   Filed as ORB-12413.
2. `symbol:src/extract/languages/python.rs#collect_call_ref:function` ←
   `file:src/extract/languages/tests/{c,csharp,java,javascript,kotlin,markdown,rust,typescript,config}.rs`,
   nine `import_relationship` candidate tests. Each of those files does import
   `crate::extract::languages` (for example `src/extract/languages/tests/c.rs:6@9e5c15986b14`
   is `use crate::extract::languages::CExtractor;`), so the edge is real and
   correctly labelled file-level. None of them exercises the Python extractor.
   Contract-conformant, substantively wrong.
3. `symbol:README.md#Commands:heading` ← `file:tests/fixtures/change-explorer/README.md`,
   `naming_heuristic` (also for the `Install` and `orbit-graph` headings). Two
   unrelated Markdown files.
4. `symbol:src/query/refs.rs#resolve_target:function` ←
   `file:src/query/tests/refs.golden.json`, `naming_heuristic`. A golden data file
   is not a test.
5. `symbol:src/query/tests/refs.rs#…` and 1195 further `heuristic_match` edges at
   depth 3, concentrated on `symbol:src/lib.rs#GraphError:impl` (200 edges),
   `symbol:src/query/refs.rs#new:method` (199) and `symbol:src/sync/pass2.rs#resolve_ref:function`
   (97). Every one carries `confidence: "fuzzy_name"` and the fallback note
   "These are name-only matches and may include unrelated symbols sharing the
   name". Spot checks confirm the note: querying the `impl` selector
   `symbol:src/lib.rs#GraphError:impl` returns references to the *type* `GraphError`
   from `explorer/src/snapshot.rs`, `src/cli/*.rs` and elsewhere, none of which
   reference the impl block.
6. **All five `uncertain` rows are symbols `scripts/span-overlap.sh` proves unchanged**
   (`from_db`, `line_for`, `new` in `src/query/refs.rs`; `GraphError:impl` in
   `src/lib.rs`; `TestWorktree:impl` in `src/sync/tests/pass2.rs` — see Q1). They are
   in the changed-symbol list only because their selector collapses several indexed
   rows, so the byte comparison that would have dropped them cannot run.

**Every missed real caller found by manual inspection.**

1. The two intra-file renames in Q1 (`ResolvedRef::qualified` → `ResolvedRef::candidate`,
   `unique_candidate_qualified` → `unique_candidate`) are not reported as renames,
   so the callers of the *new* name are not attached to the *old* symbol's row.
   Reading the two rows together recovers the relationship; no single row states it.
2. `symbol:src/lib.rs#EXTRACTOR_VERSION:const` reports no call path (constants are
   not called). Its real consumer is `resolve_db_path_for_commit`
   (`src/lib.rs:618@9e5c15986b14`) and every DB filename derived from it; the
   evidence view reports `no path` with the syntax-driven-index reason rather than
   naming that consumer.
3. 18 of the 61 queries returned zero paths at every depth, always with the two
   disclosed reasons (syntax-driven index; generated/macro-expanded code indexed as
   written). Manual inspection of `symbol:src/sync/pass2.rs#file_module_parts:function`
   shows it *is* called, from `module_prefixes_for_file` (`src/sync/pass2.rs:282@9e5c15986b14`)
   and `resolve_import_path` (`src/sync/pass2.rs:410@9e5c15986b14`) — those calls are
   found at depth 1 (11 paths), so this symbol is not among the 18. The 18 are
   dominated by `added`/`removed` test functions, which have no inbound callers by
   construction; none of them turned out to have a real caller the explorer missed.

**Out of scope and truncated.** Zero out-of-scope entries: every changed file in this
range is Rust, Markdown or JSON, all of which the extractor indexes. Truncation is
reported in Q6.

## Q6 — Bounds hit, and whether raising them changed the answers

At the shipped bounds (`depth` 3, `IMPACT_NODE_CAP` 200, 5000 ms budget), across all
61 evidence queries:

| Depth | Queries truncated | `truncated_by` | Paths returned |
| --- | --- | --- | --- |
| 1 | 42 / 61 | `depth` 40, `impact_node_cap` 2 | 519 |
| 2 | 42 / 61 | `depth` 40, `impact_node_cap` 2 | 703 |
| 3 | 41 / 61 | `depth` 39, `impact_node_cap` 2 | 1264 |

No query hit the time budget. `skipped_low_confidence` was 0 throughout.

`scripts/bounds-probe.sh 1` re-ran the 54 head-side changed symbols at
`--depth 10 --node-cap 5000 --time-budget-ms 60000`. **36 of 54 symbols returned a
different answer.** Examples (paths, distinct nodes, entry points; shipped → raised):

| Selector | paths | nodes | entry points |
| --- | --- | --- | --- |
| `symbol:src/sync/pass2.rs#resolve_ref:function` | 112 → 1448 | 30 → 356 | 12 → 196 |
| `symbol:src/query/refs.rs#resolve_target:function` | 20 → 1327 | 16 → 340 | 13 → 195 |
| `symbol:src/query/refs.rs#new:method` | 200 → 2449 | 144 → 544 | 65 → 276 |
| `symbol:src/sync/pass2.rs#ImportResolution:enum` | 3 → 1365 | 3 → 344 | 0 → 195 |

Raising the bounds does not find a *different* strongest path — the distance-1 and
distance-2 hops in Q4 are unchanged — but it enlarges the affected set by an order of
magnitude and turns "no entry point" into ~195 entry points for symbols that have
none at depth 3. The additional material is overwhelmingly transitive and
fuzzy-name; the shipped bounds are what make the answer readable, and the payload
says so every time (`truncated: true`, `truncated_by`, `bounds_hit`).

## After ORB-12416 / ORB-12417 (re-run 2026-09-13)

Re-run against `orbit-graph-explorer` at `ab6e3e303689742910c5452d6acc700552428b89`
(`agent-main`), `EXTRACTOR_VERSION` 8, `STORE_SCHEMA_VERSION` 1 (unchanged), same
base/head pair, same scripts. This corpus is a single small crate, and neither
defect's trigger condition (a Rust method call whose receiver's real type differs
from a same-named method in the *calling* file, ORB-12416; a type with any `impl`
block being consulted from a different file, ORB-12417) occurs at scale here, so
the effect on this study is small.

**Q1 unchanged.** All 61 rows keep the same status counts (27 `modified`, 20
`added`, 7 `removed`, 5 `uncertain`, 2 `signature_changed`); the same five
`uncertain` selectors (`GraphError:impl`, `from_db`, `line_for`, `new`,
`TestWorktree:impl`) are still symbols the source proves unchanged, and the same
two intra-file renames are still reported as `removed` + `added`.

**Q2 close but not identical: 73/12/169 vs 71/12/178.** `crate_root_public_item`
73 (was 71), `main_function` 12 (unchanged), `test_function` 169 (was 178).
`scripts/summarize-study.sh 1` at the new SHA gives these directly; the shift is
small enough, and orthogonal enough to what ORB-12416/ORB-12417 touch, that it is
attributed to the two other already-merged commits carried in this binary
(ORB-12406's outbound-evidence feature and ORB-12411's `import_relationship`
bare-name fix — see `README.md`'s header) rather than to the two resolver fixes
this task is re-running for.

**The closure gap (ORB-12379) is still closed at version 8.**
`scripts/closure-gap-probe.sh` re-run against the new binary extracts all four
shapes the same way as before: `resolve_target`, `qualified_matches_import` and
`fetch` all still `exact`, turbofish-chain method names still `fuzzy_name`. No
regression from either fix.

**Q4 unchanged.** Both question-4 evidence chains
(`src/sync/pass2.rs#resolve_ref` ← `run` at `src/sync/pass2.rs:37@9e5c15986b14`;
`src/query/refs.rs#resolve_target` ← `run` ← `refs`) are still exact end to end
at the new extractor version — neither defect's fix had anything to correct here,
since this crate's own `impl` blocks and same-named methods do not hit either
bug's trigger.

**Q6 bounds-probe unchanged: 36 of 54 symbols still change when the bounds are
raised** (`scripts/bounds-probe.sh 1` at the new SHA reports
`symbols_changed_by_raising	36` against `symbols_probed	54`, identical to the
original count). Raising the bounds still does not change either Q4 strongest
path.

**Net effect on this study: none of the "Right"/"Wrong"/"Unknown" verdicts above
change.** Study 1 is the control case — a single crate too small to exhibit the
cross-file `impl`-block or same-file same-named-method collisions the two fixes
address — which is itself informative: the two defects are corpus-size- and
cross-crate-structure-dependent, not universal, and studies 2 and 3 (below and in
their own files) are where the fixes show up.

## Scripted baseline versus the service — **agent-only, not a human usability study**

| Arm | Command | Wall clock | What it answered |
| --- | --- | --- | --- |
| Baseline | `scripts/baseline.sh 1` | **3.85 s** | 28 declaration names recovered from the diff; `git grep -w` counts per name; `orbit-graph sync --full` (2.3 s, 177 files) then `refs` + `impact` for the 17 names whose selector could be guessed |
| Service, cold | `scripts/service-study.sh 1` | **21.80 s** (5.16 s to index both sides, 16.64 s for the question pass) | 61 changed symbols with pairing status, 61 × 3 evidence depths, 61 entry-point reports, 61 candidate-test reports, 60 searches |
| Service, warm | `scripts/service-study.sh 1 --warm` | **18.03 s** (0.12 s to ready, 17.91 s for the question pass) | same |

What the baseline could not answer: which side a symbol exists on, whether a removal
had a replacement, which references are `exact` versus `fuzzy_name`, which tests
have a call path, and which bound truncated an answer. It also could not construct a
selector for 11 of the 28 names (Rust methods and `impl` blocks need a `kind` the
diff does not state), so those names produced no `refs`/`impact` output at all. The
baseline is faster because it answers less; the comparison measures commands issued
by an agent, not effort spent by a person.

## Reproduce

```sh
cargo build --workspace --locked --release
docs/evaluation/change-explorer/scripts/clone-corpora.sh
docs/evaluation/change-explorer/scripts/baseline.sh       1
docs/evaluation/change-explorer/scripts/service-study.sh  1
docs/evaluation/change-explorer/scripts/service-study.sh  1 --warm
docs/evaluation/change-explorer/scripts/summarize-study.sh 1
docs/evaluation/change-explorer/scripts/bounds-probe.sh   1
docs/evaluation/change-explorer/scripts/closure-gap-probe.sh
docs/evaluation/change-explorer/scripts/span-overlap.sh   1 src/query/refs.rs
docs/evaluation/change-explorer/scripts/export-reports.sh 1
```
