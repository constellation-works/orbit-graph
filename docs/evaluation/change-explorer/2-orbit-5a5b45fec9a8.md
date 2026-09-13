# Study 2 — `orbit`, the `orbit workspace teardown` CLI command handler

| Field | Value |
| --- | --- |
| Repository | `orbit`, cloned to `/tmp/orb-12393/repos/orbit` with `git clone --no-hardlinks file://~/workspace/constellation/codebases/orbit` |
| Base | `1ca6416e0ba2c6a713e42561b7b35a72b045aa25` |
| Head | `5a5b45fec9a83b765b485043398d3abadaa468de` (ORB-12347 / PR #2085, "workspace teardown: require an explicit `<workspace_id>` argument") |
| Comparison mode | `direct_base_head` (`effective_base_sha` equals `base_sha`) |
| Working tree | clean |
| `EXTRACTOR_VERSION` / `STORE_SCHEMA_VERSION` | 6 / 1 |
| Explorer commit | `c1332cf13a4daa7f3695950fa6767622ed61fd28`, release profile |
| Snapshots | base 2228 files indexed / 2462 written / 10 excluded; head 2228 / 2462 / 10 |
| Exported report | [`reports/2-orbit-5a5b45fec9a8.json`](reports/2-orbit-5a5b45fec9a8.json), [`.html`](reports/2-orbit-5a5b45fec9a8.html) |

This is the large-corpus study and the one that targets the `cli_command_handler`
entry-point rule directly.

## Q1 — Changed symbols

25 rows. By status:

| Status | Count |
| --- | --- |
| `added` | 15 |
| `modified` | 9 |
| `signature_changed` | 1 |
| `removed` / `uncertain` / `moved` / `renamed` | 0 |
| out of scope | 0 |

The one `signature_changed` row:

| Selector | Manual verdict |
| --- | --- |
| `symbol:crates/orbit-cli/src/command/workspace/tests/teardown.rs#register:function` | **Right.** The test helper went from `fn register(global_root: &Path, workspace_id: &str, repo_root: &Path, orbit_dir: &Path)` to a multi-line signature with an extra parameter. |

**This slice is exactly right.** `git diff -U0 1ca6416e0ba2 5a5b45fec9a8 -- crates/orbit-cli/src/command/workspace/`
yields eight new functions in `teardown.rs` (`resolve_teardown_target`,
`unknown_teardown_selector`, `workspace_id_for_selector`,
`refuse_if_cwd_belongs_to_another_checkout`, `planned_task_store_partitions`,
`format_teardown_plan`, `format_deleted_partition`, `partition_bundle_count`), one
changed test helper (`register`), one new test helper (`teardown`) and six new
tests. All sixteen appear with the right status. The three `modified` rows in
`teardown.rs`/`command.rs` (`WorkspaceTeardownArgs:struct`, `:impl`,
`execute:method`, `WorkspaceSubcommand:enum`) and the three `docs/runbooks/health-checks.md`
headings match the diff. Nothing was removed in this range and nothing was reported
removed.

## Q2 — Affected entry points and the rule that classified each

| Rule | Classifications |
| --- | --- |
| `test_function` | 512 |
| `main_function` | 0 |
| `crate_root_public_item` | 0 |
| `cli_command_handler` | **0** |

**The rule written for this exact case did not fire, and three queries failed
outright.** `GET /api/entry-points` returned HTTP 400 `entry_points_failed` for
`symbol:…/teardown.rs#WorkspaceTeardownArgs:impl`,
`symbol:…/teardown.rs#WorkspaceTeardownArgs:struct` and
`symbol:…/tests/teardown.rs#teardown:function`, all with

```
resolve command selector for the head snapshot: read source span for graph show:
invalid span 790..5305 for crates/orbit-cli/src/command/workspace/command.rs with 2585 bytes
```

Root cause: the graph's `command:` selector resolution pairs the *command's* file
(`command.rs`, 2585 bytes) with the *handler symbol's* span (`teardown.rs#execute`,
`790..5305`). Because `show` reports the command file rather than the handler file,
`command_handler_for`'s `view.metadata.file != address.path` check can never pass
for a repository whose handlers live in sibling files, so `cli_command_handler` is
unreachable here. Filed as **ORB-12410** with a two-file synthetic reproduction.

Of the 512 `test_function` classifications, **496 have a `heuristic_match` edge
somewhere on their path** and only 16 are resolved end to end. The 16 are all in
`crates/orbit-cli/src/command/workspace/tests/teardown.rs`, and every one of them is
correct — the ten distance-0 self-classifications of the changed tests themselves,
five distance-1 hops from those tests into `register`, and
`teardown_plan_names_workspace_checkout_and_partition` reaching
`format_teardown_plan` (distance 1) and `partition_bundle_count` (distance 2). The
other 496 are the fuzzy-`execute` fan-out described in Q5.

## Q3 — Candidate tests and their source category

152 distinct candidate tests: 508 `call_path`, 459 `import_relationship`, 44
`naming_heuristic` associations.

The `source` and `category` fields disagree for most of them, and the payload says
so on every row:

| `source` / `category` | Count |
| --- | --- |
| `call_path` / `heuristic_match` | 496 |
| `call_path` / `resolved_call` | 12 |
| `import_relationship` / `import_relationship` | 459 |
| `naming_heuristic` / `heuristic_match` | 44 |

A `call_path` candidate whose category is `heuristic_match` is a chain of
call-relationship edges in which at least one edge resolved only by name. The
`source` label says "call-path", the `category` says "heuristic match", and the UI
renders both badges (`crates/orbit-graph-explorer/ui/app.js`,
`renderCandidateTests`). It is disclosed, but the stronger label is the one that
reads first.

The 12 candidates whose `category` is `resolved_call` are all inside
`crates/orbit-cli/src/command/workspace/tests/teardown.rs@5a5b45fe`, and manual
inspection confirms every one really exercises the change:

- `teardown_plan_names_workspace_checkout_and_partition` for `format_teardown_plan`
  and, one hop further, `partition_bundle_count`;
- five tests for the changed `register` helper
  (`teardown_deletes_the_bound_task_store_partition_and_retires_its_bindings`,
  `teardown_rejects_a_selector_that_does_not_match_the_cwd_checkout`,
  `teardown_rejects_a_selector_that_is_not_registered`,
  `teardown_without_confirm_leaves_the_task_store_and_registration_untouched`,
  `teardown_without_confirm_prints_the_resolved_plan_and_does_not_delete`);
- the same five for the new `teardown` helper.

Only two of the eight new production functions in `teardown.rs` —
`format_teardown_plan` and `partition_bundle_count` — pick up a resolved candidate
test. The other six (`resolve_teardown_target`, `unknown_teardown_selector`,
`workspace_id_for_selector`, `refuse_if_cwd_belongs_to_another_checkout`,
`planned_task_store_partitions`, and the `execute` method itself) are reached only
through the fuzzy fan-out below.

The six new tests do reach them — each ends in
`teardown("…", true).execute(&runtime)`, for example
`crates/orbit-cli/src/command/workspace/tests/teardown.rs:115-116@5a5b45fe` —
but that is a `.execute()` method call with an unknown receiver, so the edge lands
at `fuzzy_name` and is **indistinguishable from the ~100 unrelated
`.execute(&runtime)` call sites elsewhere in the CLI test tree**. The right answer
is in the list; nothing in the list marks it as the right answer.

## Q4 — Strongest evidence path for the two most consequential changed symbols

**`symbol:crates/orbit-cli/src/command/workspace/teardown.rs#resolve_teardown_target:function`**
— the new function that implements the mandatory-selector rule. 200 paths at depth 3,
`truncated_by: "impact_node_cap"` (200). Strongest path (`resolved_call`, distance 1,
confidence `exact`):

- `symbol:…/teardown.rs#execute:method` → `symbol:…/teardown.rs#resolve_teardown_target:function`
  at `crates/orbit-cli/src/command/workspace/teardown.rs:29@5a5b45fe`.
  Verified: line 29 reads
  `let (workspace, checkout) = resolve_teardown_target(&registry, &self.workspace)?;`.

**`symbol:crates/orbit-cli/src/command/workspace/teardown.rs#execute:method`** — the
command handler itself. 200 paths at depth 3, `truncated_by: "impact_node_cap"`, and
**every single one is `heuristic_match`**: there is no `resolved_call`,
`observed_reference` or `import_relationship` path to this symbol at any depth. Its
one real caller —
`crates/orbit-cli/src/command/workspace/command.rs:61@5a5b45fe`,
`WorkspaceSubcommand::Teardown(args) => args.execute(runtime),` — is absent from the
answer. See Q5; this is not a truncation artefact.

## Q5 — Unresolved impact: false associations and missed real callers

**Every false association found by manual inspection.**

1. **The dispatcher calls itself, at `exact` confidence.** All seven
   `args.execute(runtime)` calls in
   `crates/orbit-cli/src/command/workspace/command.rs:53-62@5a5b45fe` are stored as
   `('crates/orbit-cli/src/command/workspace/command.rs', 'execute',
   '<WorkspaceCommand as Execute>::execute', 3577, 'call', 'exact')`. The resolver's
   same-file exact rung matched the bare method name `execute` against the only
   `execute` symbol in that file — the dispatcher's own method. Filed as
   **ORB-12416** with a five-file synthetic reproduction.
2. **496 of the 512 entry points, and 496 of the 508 `call_path` candidate tests,
   ride a single fuzzy hop.** Example, verified end to end:
   `symbol:crates/orbit-cli/src/command/config/tests/get.rs#get_rejects_misspelled_crew_field:test`
   is reported as a distance-2 `call_path` candidate for `resolve_teardown_target`,
   note "call reference at crates/orbit-cli/src/command/config/tests/get.rs:55;
   2 hop(s) to the changed symbol". Line 55 at that SHA is `.execute(&runtime)` —
   a *config* command's `execute`, matched by name to `teardown.rs#execute`
   (`heuristic_match` / `fuzzy_name`), then a real `resolved_call` hop into
   `resolve_teardown_target`. Every test in `crates/orbit-cli/src/command/**/tests/`
   that calls `.execute(&runtime)` is attached this way. The rows carry
   `category: heuristic_match` and `truncated: true`.
3. `symbol:crates/orbit-cli/src/command/workspace/command.rs#WorkspaceSubcommand:enum`
   ← 20+ `import_relationship` candidate tests
   (`config/tests/get.rs`, `config/tests/set.rs`, `executor/tests/show.rs`,
   `init/tests/command.rs`, `job/tests/command.rs`, `log/tests/tail.rs`,
   `mcp/setup/tests/{args,workspace}.rs`, `run/tests/{job,mod,ship,sweep}.rs`,
   `task/tests/{add,artifact,command,list,publication,show,update}.rs`,
   `tests/auto_task.rs`, …). These import `crate::command::…`, a real module edge,
   correctly labelled file-level — and none of them tests workspace teardown.

**Every missed real caller found by manual inspection.**

1. `crates/orbit-cli/src/command/workspace/command.rs:61@5a5b45fe` →
   `symbol:…/teardown.rs#execute:method`. The single dispatch site of the changed
   handler. Absent at depth 3 with the shipped bounds, and still absent after
   re-querying at `--depth 10 --node-cap 5000 --time-budget-ms 60000` (5000 paths,
   all `heuristic_match`, zero mentioning `workspace/command.rs`). The edge does not
   exist in the graph — see finding 1 above.
2. The same root cause hides the other six dispatch edges in that file
   (`List`, `Show`, `SourceRemote`, `Role`, `Publication`, `Remove`), which are
   outside this change's scope but confirm the pattern.
3. 11 of the 25 queries returned zero paths at every depth, always with the three
   disclosed reasons (syntax-driven index; generated/macro-expanded code; 10 excluded
   tree entries). Sampling: the three `docs/runbooks/health-checks.md` headings (no
   callers by nature) and the six newly added `#[test]` functions (no inbound callers
   by construction). None of the 11 turned out to have a real caller the explorer
   missed.

**Out of scope.** Zero out-of-scope changed files. Ten tree entries were excluded
from each snapshot at materialization and are named in every `no_path_reasons` list.

## Q6 — Bounds hit, and whether raising them changed the answers

| Depth | Queries truncated | `truncated_by` | Paths returned |
| --- | --- | --- | --- |
| 1 | 14 / 25 | `depth` 13, `impact_node_cap` 1 | 233 |
| 2 | 12 / 25 | `depth` 6, `impact_node_cap` 6 | 1244 |
| 3 | 12 / 25 | `depth` 3, `impact_node_cap` 9 | 1848 |

The time budget was never reached. Unlike the small corpora, the node cap becomes
the dominant bound as depth rises: at depth 3, nine of the twelve truncated queries
are cut by `impact_node_cap`, not by depth.

`scripts/bounds-probe.sh 2` re-ran all 25 head-side changed symbols at
`--depth 10 --node-cap 5000 --time-budget-ms 60000`. **12 of 25 symbols returned a
different answer**, and every one of the nine `teardown.rs` functions went from 200
paths to the new cap of 5000:

| Selector | paths | nodes | entry points |
| --- | --- | --- | --- |
| `symbol:…/teardown.rs#execute:method` | 200 → 5000 | 102 → 254 | 57 → 123 |
| `symbol:…/teardown.rs#resolve_teardown_target:function` | 200 → 5000 | 102 → 255 | 56 → 123 |
| `symbol:…/command.rs#WorkspaceSubcommand:enum` | 8 → 71 | 6 → 49 | 0 → 13 |
| `symbol:…/teardown.rs#WorkspaceTeardownArgs:struct` | 14 → 82 | 13 → 58 | 0 → 0 |

Raising the bounds **did not** change any answer that matters: the additional 4800
paths per handler are more of the same fuzzy `execute` fan-out, and the one real
missing caller stays missing. On a corpus this size the node cap is doing the work
of hiding a resolver defect.

## Scripted baseline versus the service — **agent-only, not a human usability study**

| Arm | Command | Wall clock | What it answered |
| --- | --- | --- | --- |
| Baseline | `scripts/baseline.sh 2` | **227.71 s** | 16 declaration names from the diff, `git grep -w` counts over the whole tree at head, `orbit-graph sync --full` on a detached head worktree, then `refs`/`impact` per resolvable selector |
| Service, cold | `scripts/service-study.sh 2` | **573.30 s** (519.74 s to index both sides, 53.55 s question pass) | 25 changed symbols, 25 × 3 evidence depths, 25 entry-point reports, 25 candidate-test reports, 24 searches |
| Service, warm | `scripts/service-study.sh 2 --warm` | **54.11 s** (0.12 s to ready, 53.99 s question pass) | same |

On the large corpus the cold service pass costs ~9.5 minutes, of which 91 % is
indexing two full snapshots; the warm pass beats the baseline by a factor of four
while answering far more. Cold indexing is the number to watch, and it is broken out
in [`performance.md`](performance.md). Agent commands, not human effort.

## Reproduce

```sh
cargo build --workspace --locked --release
docs/evaluation/change-explorer/scripts/clone-corpora.sh
docs/evaluation/change-explorer/scripts/baseline.sh        2
docs/evaluation/change-explorer/scripts/service-study.sh   2
docs/evaluation/change-explorer/scripts/service-study.sh   2 --warm
docs/evaluation/change-explorer/scripts/summarize-study.sh 2
docs/evaluation/change-explorer/scripts/bounds-probe.sh    2
docs/evaluation/change-explorer/scripts/export-reports.sh  2
```
