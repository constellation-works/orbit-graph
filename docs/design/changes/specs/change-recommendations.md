---
title: "Change recommendation history foundation"
owner: maintainers
last_updated: 2026-11-14
status: Accepted
stage: shipped
next: "—"
feature: changes
doc_role: spec
summary: "History index foundation: verified-delivery import contract, extraction, storage and recommendation stages."
---

# Change recommendation history foundation

Status: approved Stage 1 design and integration contract. This document describes
the implementation in `orbit-graph` 0.9.x; it does not authorize a release.

## Purpose and boundaries

The history index learns from files and symbols that actually changed between
immutable Git revisions. Its primary evidence is a verified delivery envelope.
An incremental Git scan is also available, but its commit/trailer associations
are explicitly weaker. Planned task `context_files` are neither an input nor a
mutation target. The crate reads no Orbit database, configuration, task store,
or private API and remains usable without Orbit.

The SQLite database is a local derived index at
`.orbit-graph/change-history.5.sqlite3` (the file name carries
`HISTORY_INDEX_SCHEMA_VERSION`). It is not task authority. External
delivery/task systems retain authority and should be able to replay the public
envelopes if a producer must replay them. A confirmed `history rebuild`
re-extracts Git evidence and preserves imported verified and caller-attested
deliveries by default.

Stage 1 supplies ingestion, extraction, provenance, current-symbol resolution,
and stable types. Stage 2 will rank candidate destinations from this evidence.
Stage 3 will package the importer as a plugin, connect authoritative Orbit reads
to the public contract, and evaluate recommendation quality. Neither later stage
may turn planned context into training evidence or silently promote Git-only
associations to verified deliveries.

## Public JSON import contract: version 2

The producer supplies one UTF-8 JSON object. Unknown fields are rejected. All
revision fields are full immutable commit object IDs; symbolic refs are rejected.
`repository` is the local repository's `origin` URL when a non-empty origin is
configured, otherwise its canonical worktree path. `landing_branch` accepts a
shorthand such as `main` or `refs/heads/main` and is normalized to shorthand.

```json
{
  "schema_version": 2,
  "repository": "https://example.invalid/acme/widgets.git",
  "landing_branch": "main",
  "before_revision": "1111111111111111111111111111111111111111",
  "after_revision": "2222222222222222222222222222222222222222",
  "delivery_id": "pull-request:418",
  "evidence": "verified_delivery",
  "source": {"system": "delivery-service", "record_id": "418"},
  "delivered_at": {
    "status": "known",
    "timestamp": "2026-09-07T00:00:00Z",
    "source": {"system": "delivery-service", "record_id": "418:landed"}
  },
  "captured_at": "2026-09-07T00:01:00Z",
  "tasks": [{
    "task_id": "TASK-123",
    "title": "Change widget behavior",
    "description": "User-approved task text captured at delivery time.",
    "acceptance_criteria": ["The delivered behavior is covered."],
    "source": {"system": "task-service", "record_id": "TASK-123@revision-7"},
    "created_at": {
      "status": "known",
      "timestamp": "2026-09-06T18:00:00Z",
      "source": {"system": "task-service", "record_id": "TASK-123:created"}
    },
    "snapshot_available_at": {
      "status": "known",
      "timestamp": "2026-09-06T19:00:00Z",
      "source": {"system": "task-service", "record_id": "TASK-123@revision-7"}
    },
    "text_availability": "known_pre_execution",
    "captured_at": "2026-09-07T00:00:30Z"
  }]
}
```

Required semantics:

- `before_revision` and `after_revision` must exist as commits locally;
  `after_revision` must be a strict descendant of `before_revision` and reachable
  from the named local landing branch.
- `delivery_id` is unique within repository and branch. Replaying equivalent
  normalized content is idempotent. Reusing it for different boundaries or
  content fails without changing the index.
- Exact duplicate task memberships are collapsed. Conflicting snapshots for one
  task ID in one delivery are rejected. Multiple distinct task IDs are retained;
  their association does not claim that every task changed every extracted file.
- `verified_delivery` is retained for trusted producers and Orbit run records
  observed through the adapter. A plugin `maintain import` claim of this value
  is stored as `caller_attested`; the response reports the downgrade. Its
  recommendation weight is 0.55, equal to `git_only`, versus 1.0 for verified.
  Git boundaries are validated, but that cannot verify the caller's source.
  `git_only` is always weaker than verified, including task IDs parsed from `Task-Id:` or
  `Orbit-Task:` commit trailers.
- `captured_at` is ingestion/snapshot capture time, not delivery, creation, or
  historical availability time. `delivered_at`, task `created_at`, and
  `snapshot_available_at` are separate `TemporalFact` values with `known`,
  `uncertain`, or `unavailable` status, an optional timestamp, and their own
  provenance. A `known` fact requires a timestamp; `unavailable` forbids one.
- Timestamp strings are validated as RFC 3339 or canonical `unix:<seconds>`.
  Invalid timestamps, missing temporal provenance, and inconsistent temporal
  status fail before a transaction writes anything.
- `text_availability` is `known_pre_execution`, `post_execution`, or
  `uncertain`. Only the first is eligible for chronological pre-execution task
  evidence, and it requires a known `snapshot_available_at`. Capture after the
  fact never upgrades availability. Consumers must exclude the other two values.
- Git sync records its actual ingestion time in `captured_at`. It preserves the
  Git commit timestamp only as an `uncertain` delivery proxy, reports task
  creation as `unavailable`, and reports trailer-derived snapshot timing and
  pre-execution availability as `uncertain`. A Git timestamp is not proof of a
  real landing time, and `git_only` imports may not claim that
  `delivered_at.status` is `known`.

The Rust contract is exposed by `DeliveryImport`, `DeliveryEvidence`,
`Provenance`, `TemporalFact`, `TemporalStatus`, `TaskTextAvailability`, and
`TaskAssociation`. Extracted output is exposed by
`DeliveredChange`, `FileChange`, `SymbolChange`, `SymbolIdentity`, change stats,
fallback reasons, evidence strength, and revision-side types. Schema constants
version the import (`DELIVERY_IMPORT_SCHEMA_VERSION`), extractor
(`CHANGE_EXTRACTOR_VERSION`), and SQLite index
(`HISTORY_INDEX_SCHEMA_VERSION`) independently.

## Extraction and identity rules

The extractor diffs the before and after commit trees, enables Git rename/copy
similarity detection, and reads blobs from both revisions. Added and deleted
lines are mapped to byte ranges, then to every smallest non-ancestor named
tree-sitter symbol intersecting the line. Same-line siblings are all retained;
a changed child line implicates the child, not its parent or descendants. A
delivery may span multiple commits; extraction compares its two
declared boundary trees, while Git sync creates one delivery per first-parent
commit. Merge commits therefore compare the merge tree with parent zero.

For each revision side, unsupported languages, binary blobs, unavailable files,
parse/extraction uncertainty, and changed lines outside named symbols remain
explicit file-level fallback evidence. Imports and module-level edits naturally
remain file-level unless a smallest named symbol encloses the changed range.
Whitespace-only changes are retained. Line additions/deletions, paths, change
kind, immutable revision, byte spans, signatures, parents, and changed line
numbers are preserved.

### Line lengths (extracted output, additive)

So the word index can weigh a change by how much of a context it touched
(design §2.5), extraction also records, all as optional fields that are
omitted when absent:

| Field | Where | Meaning |
|---|---|---|
| `line_start`, `line_end` | `SymbolIdentity` | One-based first and last (inclusive) line of the symbol's span on its side; its length is `line_end − line_start + 1` |
| `before_lines`, `after_lines` | `FileChange` | A `FileSideLines` for each side |
| `line_count` | `FileSideLines` | Lines in the file on that side: 0 when the side has no file, 1 for a binary blob, and a last line without a newline counts |
| `named_symbol_covered_lines` | `FileSideLines` | Distinct lines covered by the union of the spans of every named symbol extracted from that side, changed or not; 0 for an unsupported, binary, unparseable or absent side. `line_count − named_symbol_covered_lines` is the file's remainder length |
| `whitespace_only_lines` | `FileSideLines` | Changed lines of that side whose change is whitespace only: a blank line added or removed, or a line paired within its hunk with a line of the other side that differs from it only in whitespace. They stay in `changed_lines` and in the additions/deletions; a consumer that ignores whitespace-only changes subtracts them |

These fields are extracted output, not import-contract input: a
`DeliveryImport` envelope is unchanged (still version 2) and the extractor
derives them from Git for every import, sync and rebuild. The
`CHANGE_EXTRACTOR_VERSION` is unchanged, so existing indexes stay readable; a
payload recorded before these fields decodes with them absent, and the word
index's symbol-first mode leaves such a delivery out of its window
(`no_line_lengths`) until it is backfilled (below).

Cross-revision identity is deliberately conservative:

1. a unique equal qualified name and kind is `exact`;
2. a unique equal normalized signature and kind is `similar` (useful for a
   defensible move or rename);
3. multiple candidates are `uncertain` and are not paired;
4. no candidate is `unmatched`.

Matching searches all extracted symbols on the opposite tree, not only symbols
that received changed lines. Emitted records still carry changed-line
attribution only on sides where the diff changed that symbol. Deletion-only and
insertion-only body edits therefore retain both identities and
`live_after = true`; a truly absent after-tree identity remains nonlive.

An after-less record has `live_after = false`. `HistoryIndex::resolve_current_symbol`
checks the current branch tree and returns `live` only for one same-path,
same-qualified-name, same-kind match. Deleted, ambiguous, unsupported, binary,
and unparseable records never become live recommendation destinations.

## Storage, sync, and failure behavior

The SQLite schema stores scope cursors, delivery payloads, normalized task
memberships, delivery/creation/snapshot temporal status, timestamp and
provenance columns, text availability, file stats/fallbacks, both sides of
symbol evidence, and (since schema v4) per-delivery path lineage. Foreign keys cascade within a delivery. Repository, branch,
schema version, extractor
version, provenance, task ambiguity, and evidence strength remain queryable.
`HistoryIndex::deliveries` returns stable typed records so ranking code does not
depend on SQLite layout.

Path lineage (`history_path_lineage`) is written in the same transaction as its
delivery by import, sync, and rebuild. It records the delivery's own
before-to-after rename steps (`old_path -> new_path`) and deletions
(`old_path -> none`) exactly as that delivery's diff classified them. Copies
and modifications record nothing, because the source path stays live. Lineage
is derived evidence: it is recreated whenever its delivery is re-extracted.

Every import and sync holds a sidecar file lock and uses one SQLite immediate
transaction. Delivery rows and the cursor commit together. A failure after any
intermediate write rolls back both. Concurrent cursor movement is detected
before commit. Sync walks only the named branch's first-parent chain, oldest to
newest, and refuses a missing or off-chain stored cursor as divergent/rebound
history. Traversal is capped (default 1,000); exceeding the cap fails without a
partial cursor or partial evidence, and callers may explicitly raise `--limit`.

`history rebuild` previews the selected repository/branch scope by default.
With `--confirm`, it validates and extracts the bounded first-parent range
before opening its replacement transaction, then recreates that scope atomically.
It re-extracts retained verified deliveries from their stored import envelopes;
only `--discard-verified` removes them. The report counts any verified rows
removed. Rebuild refuses the `agent-main` branch.

**Line-length backfill.** After its cursor work, `history sync` re-extracts up
to `--limit` stored deliveries of the scope that lack line lengths, from their
own recorded boundaries (verified and caller-attested ones included). When the
fresh extraction equals the stored record in everything but the line lengths,
the stored payload is rewritten with them in one immediate transaction; its
signatures are redacted first, as on import. A delivery whose commits are gone
from the local clone, or whose re-extraction differs otherwise, is left as it
is. The sync report counts `line_lengths_backfilled` and
`line_lengths_missing` (still without them); repeated syncs drain the rest. A
re-import of an envelope already stored without line lengths fills them in the
same way instead of reporting a conflict. A rebuild preview reports the scope's
`line_lengths_missing`; a confirmed rebuild re-extracts everything and so
records them for every delivery, on any branch but `agent-main`.

## CLI contract

The default terminal form is human output (a table on a TTY and lossless,
tab-separated plain rows when redirected). Programs consuming this contract
must request `--format json`, which emits one stable JSON document on stdout.
`--format ndjson` emits the documented record units one per line. Failures keep
stdout empty, exit nonzero, and emit the JSON error object on stderr in either
explicit machine mode.

```text
orbit-graph --format json history import --input delivery.json
orbit-graph --format json history import --input -
orbit-graph --format json history sync --branch main [--limit 1000]
orbit-graph --format json history status --branch main
orbit-graph --format json history rebuild --branch main [--limit 1000] [--confirm] [--discard-verified]
```

`status` reports repository/branch, database path, versions, cursor, total
deliveries, verified and Git-only counts, and task-membership count.
Caller-attested deliveries are included in the total and in neither subtype
count. It is safe for operational inspection but is not a task status API.

## Version compatibility and later evaluation

Import schema v2 intentionally does not accept v1 envelopes: its required
temporal fields change the meaning needed for leakage-safe evaluation, and
unknown fields were rejected by v1. Producers must emit `schema_version: 2`;
v1 envelopes fail closed rather than receiving guessed timestamps. The history
index and change extractor also advance independently to version 2, selecting a fresh
`change-history.5.sqlite3` database. Existing v1 databases are left untouched;
verified producers replay v2 envelopes and Git-only evidence can be rebuilt.
The crate/package version remains unchanged because this task is not a release.

History schema v4 (`change-history.4.sqlite3`) adds only derived path lineage.
The first open of a v4 index that holds no scope or delivery copies every scope
cursor and delivery payload from a sibling `change-history.3.sqlite3` whose
extractor and import versions match, deriving lineage on insert. Verified
imports therefore survive the upgrade without replay, and no Git extraction is
repeated. The v3 file is read, never modified, and the copy runs at most once
(the outcome is recorded under `legacy_copy` in `history_meta`); an
incompatible or absent v3 index is recorded as skipped. `history rebuild` and
`history sync` populate lineage for Git-only evidence at any time.

History schema v5 (`change-history.5.sqlite3`) adds separate
`history_supplied_task_snapshots` rows. A supplied `orbit_sync.task_snapshots`
entry retains its original payload under `caller_supplied` provenance; the
verified delivery's task membership always contains the text observed from
`orbit.task.show`. Recommendations use that observed membership. Compatible v4
deliveries and cursors are copied once into v5, and rebuild preserves a
separate snapshot with its verified delivery.

Tree-sitter extraction is syntactic; macros, generated code, dynamic dispatch,
and malformed files can reduce evidence. Rename detection follows libgit2
similarity heuristics, applied once per delivery at ingest. File moves plus semantic rewrites may remain unmatched,
which is preferable to invented identity. Root commits lack a commit-valued
before boundary and are used only as the starting cursor; their initial tree is
not emitted as a delivery in v2.

Stage 2 ranking must weight verified delivery evidence above Git-only and
caller-attested evidence,
retain multi-task uncertainty, exclude task text unless `text_availability` is
`known_pre_execution`, fail closed on unavailable or uncertain cutoffs, filter
current-resolution results to `live`, and measure file versus symbol precision
separately. Stage 3 evaluation should use
held-out delivered changes, report unsupported/uncertain coverage, test duplicate
delivery replay and history rebinds, and compare recommendations with actual
after-tree destinations without consulting planned context files.

## Stage 2 recommendation interface

The library exposes `RecommendationEngine`, `RecommendationRequest`, and the
serializable result types re-exported from the crate root. A request uses the
`RecommendationInput` enum, so query text and a task ID are structurally
mutually exclusive. Task-ID requests may carry a `task_snapshot` using the
public `TaskAssociation` contract. That snapshot is supplied independently of
delivery history, must match the requested ID, and must be proven available
strictly before the cutoff. This supports recommendations for new/pending tasks
and chronological backtests whose held-out delivery must remain excluded.
`HybridTaskHit` is the narrow adapter seam for ranked hits
from another task search system; scores are normalized per request and do not
give the engine access to that system's database. With no supplied hits, the
engine uses deterministic token-overlap retrieval over eligible historical
task title, description, and acceptance-criteria snapshots.

The standalone CLI's machine-mode invocations mirror the contract:

```text
orbit-graph --format json recommend --query "repair parser cache" --level file --limit 10
orbit-graph --format json recommend --task-id TASK-123 --level symbol --revision HEAD~1
orbit-graph --format json recommend --task-id TASK-NEW --task-snapshot task.json --level file
orbit-graph --format json recommend --query "repair parser cache" --hybrid-hits hits.json
```

Exactly one of `--query` and `--task-id` is required. `--branch` selects the
history scope (default `main`), `--revision` is resolved to a full commit
(default: current checkout), and `--cutoff` accepts RFC 3339 or
`unix:<seconds>`. Without `--cutoff`, a live request observes evidence as of
the request time and reports an RFC 3339 `effective_cutoff` with subsecond
precision; this avoids rejecting a newly captured task snapshot merely because
it is later than the target commit. Explicit cutoffs remain reproducible
historical replays with strictly-before evidence semantics. The hybrid-hit file
is a JSON array of objects with `task_id` and a finite non-negative `score`.
`--task-snapshot` accepts one
`TaskAssociation` JSON object and requires `--task-id`; it is never inserted
into the history index.

Ranking counts each delivery once and each file once per delivery. It combines
the strongest task relevance in a delivery with verified-versus-Git-only
evidence, commit-distance recency, delivery breadth, multi-task ambiguity,
location prevalence, generated/lockfile discounts, directional co-change, and
bounded current structure; in live mode a Git-only delivery without task
relevance may instead draw down-weighted relevance from its commit message
(see "Commit text for Git-only history" below). Scores are additive relevance scores, not
probabilities. `reasons` exposes every contribution; `association` exposes the
directional support, source/destination counts, confidence, and lift.
Association support and prevalence use the complete eligible corpus, including
deliveries whose tasks do not match the query; relevance chooses seed
destinations. At most the 256 seeds with the strongest direct evidence are
considered (ties by selector); each seed counts co-occurring destinations over
only the deliveries it appears in, so the pass scales with seed occurrences
rather than with every destination, seed and delivery. Imports with the same repository/branch before-and-after boundary
count as one delivered change even when distinct source IDs or Git-only and
verified evidence coexist. The deterministic representative prefers verified
evidence, while explanation text retains equivalent source IDs without adding
votes or task memberships.

Only delivery revisions on the requested revision's ancestry are eligible.
An explicit chronological cutoff additionally excludes evidence whose landing
time is uncertain or unavailable. All comparisons use the same strict RFC
3339/`unix:<seconds>` parser, preserve arbitrary fractional-second ordering,
normalize offsets, and fail on invalid dates, trailing input, and arithmetic
overflow. Delivery and task-snapshot evidence must be strictly before the
cutoff; equality is excluded. Strict replay reads only snapshots proven
available before execution. Live mode may use current started/completed task
observations that predate the request while preserving their honest
`post_execution` label. Both modes exclude every equivalent delivery boundary
associated with the target task. Historical paths are followed forward through
persisted path lineage (see "Path lineage at query time" below);
symbols must resolve uniquely by current qualified identity or conservative
signature identity. Deleted and ambiguous symbols are omitted. Symbol mode
uses a `file:` selector only when history has file-only or unresolvable symbol
evidence, and marks it with `file_fallback` and `fallback_reason`.

`source_freshness`, `resolved_target_revision`, `effective_cutoff`, and
`fallbacks` make stale/cold-start limitations explicit. When `cutoff` is
omitted, `effective_cutoff` is the request observation time captured with
subsecond precision for ordinary live recommendations; explicit cutoffs retain
strictly-before historical replay semantics. Current-tree lexical matching
remains useful with empty history. Structural expansion is used only when the
requested revision is the engine checkout; a non-HEAD operational request never
silently reuses HEAD graph structure. Evaluation materializes the exact target
tree in an isolated clone, so historical graph-only rows use a real frozen
graph. Every cached structural destination is
also resolved against the requested Git tree, so a graph synced before a later
committed deletion cannot return the removed file or symbol; skipped stale rows
are explicit in `fallbacks`. These seams let Stage 3 run chronological
backtests by setting both `target_revision` and `cutoff`, then comparing the
ranked selectors to held-out delivered destinations.

### Path lineage at query time

Recommendation performs no whole-tree rename or copy detection per delivery.
Each request builds one in-memory lineage from the persisted steps of every
*visible* delivery, ordered by first-parent commit distance from the target
(memoized per revision). A changed path observed at a delivery's landed
revision resolves as follows:

1. a path present in the target tree is used unchanged;
2. otherwise the path is chained through every later visible delivery's
   rename map, oldest first; a recorded deletion ends it and the file
   contributes nothing;
3. otherwise, when the newest visible delivery is behind the target (the
   history cursor lags), the path is looked up in one bounded renames-only
   diff from that delivery to the target. The diff is computed at most once per
   request and only when some path needs it. It never detects copies, and when
   more than 500 files were added or deleted in the gap it follows only
   content-identical renames. Its use is reported as a `path_lineage_gap` or
   `path_lineage_gap_exact_only` fallback.

A resolved path must exist in the target tree. Paths that still cannot be
followed are omitted; when indexed deliveries leave unindexed ranges (a
delivery's base is not another visible delivery's landing), the omission is
reported as `path_lineage_incomplete`.

Visibility follows the same rule as ranking evidence, except that the target
task's own boundary is kept: live requests use every delivery on the target's
ancestry, and strict replay uses only deliveries that pass the explicit cutoff
(known landing time strictly before it). Lineage recorded by a post-cutoff
delivery therefore never resolves a path in strict replay. A rename performed
by an excluded delivery can be followed only by the gap diff, which compares
Git trees up to the target revision and so reveals nothing the target
revision does not already contain; in every mode the target tree must contain
the resolved path.

### Commit text for Git-only history

Git-only deliveries carry no task text, so a query over a history that holds
only `history sync` evidence would otherwise rank from the current tree alone.
In live mode the combined variant therefore reads each Git-only delivery's
commit message from its landed commit object at query time, and uses it as
direct relevance when the delivery has no task relevance of its own. Only
free-text `query` requests use it: a task-ID request's own Git-only delivery
carries no task association, so its message could describe the very change
being predicted. Nothing new is persisted, so the history schema is unchanged.
The rules:

- The text is labelled `post_execution` / `git_commit_message`: it was written
  with or after the change, so it is never pre-execution task text. It is used
  only in live requests; an explicit `cutoff` (strict replay) never reads it,
  whatever the landing time.
- It is never task evidence. Bracketed IDs such as `[ORB-13007]` in the
  message are shown as non-authoritative hints in the explanation and are not
  added to `supporting_task_ids`, and a delivery with matching task text
  always uses that instead.
- Known attribution and bookkeeping trailers are removed from the final
  paragraph: `Co-authored-by`, `Signed-off-by`, `Reviewed-by`, `Acked-by`,
  `Tested-by`, `Reported-by`, `Suggested-by`, `Helped-by`, `Cc`, `Change-Id`,
  `Planned-by`, `Implemented-by`, `Claude-Session` and any `Orbit-*` key, case
  insensitively. Other `Key: value` lines, such as `Scope: …` or the squashed
  `fix: …` subjects Orbit's history contains, are message content and are
  kept. The message is then truncated to 4 KiB.
- Relevance is `0.5 × similarity²`, where similarity is the same query-coverage
  token overlap used for task text. The weight and the exponent are the
  measured production setting from the live Git evaluation in
  [`evals/commit-text-live.md`](../../../../evals/commit-text-live.md)
  (ORB-13229). That run holds out a first-parent commit, queries its subject
  at `target = commit^`, and checks that the held-out commit and later
  deliveries contribute no evidence. It compares no commit text, linear
  `0.5 × s`, squared `0.5 × s²`, squared `0.25 × s²`, and linear `1.0 × s` on
  Orbit and orbit-graph. Squared weight 0.5 stays: under the cohort rule in
  that record, no other point improved MRR@10 by at least 0.01 on both
  repositories. Squaring keeps a message that describes the whole query at the
  full 0.5 weight and cuts a half-covering message to a quarter of that. The
  value then passes through the usual Git-only evidence weight, recency,
  breadth, ubiquity and artifact discounts, and seeds directional co-change
  like any direct change.
- Contributions carry the reason kind `historical_change_commit_text`, and a
  response that used any is reported with the `git_commit_text_used`
  fallback. Task-search-only, graph-only and frequency variants do not use
  commit text.

One earlier manual query on a 200-commit Git-only Orbit index ("auto-task
delete durable opt-out for shipped defaults") moved
`crates/orbit-core/src/application/auto_tasks/delete.rs` from a lexical-only
tie at rank 6 to rank 2, behind its own test module. The live evaluation above
is the measurement that fixes the weight and exponent; that single query is
only an illustration of the rank change.

### Target-tree symbols and their cache

Destinations are resolved against the immutable target tree, never the
worktree. Walking the tree's blob paths is cheap; parsing every blob is the
dominant fixed cost of a request, so symbols are loaded only when the request
reads them (combined and graph-only lexical and structural evidence, or symbol
level). File-level task-search-only and frequency requests, and file-selector
liveness checks, never parse. The extracted symbol table is cached in the
history index directory as
`recommend-target.<EXTRACTOR_VERSION>.<target-tree-oid>.json`. The key is the
tree, not the commit, so a commit that doesn't change the tree reuses the
entry. The entry records its format, extractor version and tree, and a
mismatch, parse failure or unreadable file is treated as a miss and
re-extracted. It is published atomically (owner-only temp file, then rename),
and a warm hit refreshes the entry's modification time to record its last use.
Free-text signatures in that table pass through the shared redactor before
the entry is written or used, so a credential-shaped default argument is not
stored. Names and qualified names are left unchanged. Cached and uncached
requests therefore return identical results. The graph
index is deliberately not used as a symbol source: it reflects the worktree,
including uncommitted and ignored files, rather than the target tree.

Each write then prunes the directory, which it shares with the history index
and graph state, so deletion is scoped by ownership (STD-03 §R29): only names
that parse exactly as `recommend-target.<extractor u32>.<40-hex tree>.json`, or
that name plus `.tmp-<pid>`, are ever considered, and anything else is left
alone. The grammar, rather than an owned-entries record, is the ownership
proof: the name is fully determined by the cache key, so no other writer
produces it by accident. Entries of *older* extractor versions are removed; a
newer version's entries belong to a newer binary sharing the directory and are
kept, so an old and a new binary never evict each other (STD-03 §R10). Temp
files left by a crashed writer are removed once their pid is gone (after a 60 s
grace; liveness is read from `/proc`, and where that is unavailable the file
only ages out) or once they are an hour old. Of the current version's entries,
the one just written and the three most recently used others are kept.

### Word-index variant

`variant: word_index` ranks destinations with an inverted index from past
task wording to what those tasks changed. It is the fifth variant beside
`combined`, `task_search_only`, `graph_only` and `frequency`, follows sections
2 to 7 of the context-index design (`docs/design/context-index/2_design.md` in
the Orbit repository, ORB-24844) and departs from it in the text it reads
(below). The index is built in memory for every request; there are no
persisted generations, routine or pilot hook yet.

**Evidence.** Only the history store. A delivery's words come from its task's
text, and its labels are the live files and symbols its recorded diff changed,
followed through path lineage to the target tree exactly as the other variants
do. No plan, execution summary, comment, Git commit message, tag or task type
is read on either side, and no current-tree lexical or structural evidence is
added (`structure_applied` is always `false`). Hybrid task hits are ignored.

**Text source.** `text_fields` selects the text on the index side and the
query side together. The default, `title`, indexes each delivery's task title
and queries with the target task's title (a `--query` string is used as given).
`title_description_criteria` switches both sides to the title, description and
acceptance criteria, so `evaluate` can measure what the wider text adds. The
design indexes the wider text; the title-only default is Daniel's decision of
2026-11-14 (ORB-24852).

**Window.** The window is the newest `window_size` (`n`) eligible deliveries
on the target's ancestry, nearest the target first (commit distance, then
delivery ID). Eligibility is the request's own rule: with a `cutoff`, only
deliveries landed strictly before it, whose task text was available before it,
and never the target task's own delivery; under `evaluate` every case is such a
replay. A delivery is left out, without taking a window slot, when it names
several tasks (its diff cannot be divided between them), has no eligible task
text, or has no live file or more than 20 live files after path exclusions
(lockfiles and `CHANGELOG.md`). The window is rebuilt from the stored labels on
every request; no count is ever decremented. The response's `word_index`
object reports the window size `N` and the exclusions by reason.

**Tokeniser** (version `1`). Text is split on whitespace and on punctuation
other than the identifier joiners `_ - . / :`. URLs, task IDs (`ORB-123`),
hexadecimal runs of seven or more characters containing a digit, and bare
numbers are dropped. An identifier is split at joiners and case changes and
keeps its parts and its lowercased whole (`context_files` gives `context`,
`file`, `context_file`; `widenContextFiles` gives `widen`, `context`, `file`,
`widencontextfile`); a path keeps its parts and its directory-free stem, and a
file extension is dropped. Terms are lowercased and a trailing plural `s` is
folded (not after `ss`, `us` or `is`). Terms shorter than 2 or longer than 40
characters and a fixed English stopword list are dropped. A term counts once
per task. Query terms present in more than `max_term_share` of the window's
tasks are window stopwords and are dropped.

**Scoring.** For a query term `w` and a context `c` (a file, or at symbol
level a symbol or file-fallback leaf), with `N` window tasks, `df(w)` tasks
containing `w`, `T(c)` tasks touching `c` and `t(w,c)` tasks with both:

```text
E(w,c)        = df(w) · T(c) / N
assoc(w,c)    = max(0, ln((t(w,c) + κ) / (E(w,c) + κ)))      only when t(w,c) >= min_support
score(c)      = Σ over query terms w of assoc(w,c)
file_score(F) = score(F) + (b − 1) · max over symbols S in F of score(S)
```

Rare words count more, a context every task touches scores nothing beyond its
base rate, and one co-occurrence scores at most `ln 2` with `κ = 1`. At file
level each file is ranked by `file_score`; at symbol level symbols are ranked
by their own `score`, with the same `file:` fallback leaves as the other
variants for changed files without a live symbol. Each result's `reasons`
name every contributing term (`word_index_term`: posting, `df`, `T` and the
expected count) and the boost (`word_index_symbol_boost`). Results are the
same live `file:`/`symbol:` selectors as the other variants.

**Empty answers.** The variant never pads. When nothing scores the response
has no recommendations, a `word_index_empty` fallback, and
`word_index.empty_reason`: `no_window_tasks`, `no_query_terms`,
`no_known_terms` (no query term in the window outside window stopwords) or
`below_min_support` (no posting reached `min_support` with a positive
association).

**Parameters.** The request's optional `word_index` object (CLI flags
`--text-fields`, `--window-size`, `--symbol-boost`, `--min-support`,
`--smoothing`, `--max-term-share` and the four below; the evaluation corpus's optional
`word_index` object) is refused with any other variant:

| Field | Default | Range |
|---|---|---|
| `text_fields` | `title` | `title`, `title_description_criteria` |
| `window_size` (`n`) | 4000 | at least 1 |
| `symbol_boost` (`b`) | 1.5 | at least 1; 1 is no boost |
| `min_support` | 2 | at least 1 |
| `smoothing` (`κ`) | 1 | greater than 0 |
| `max_term_share` | 0.3 | greater than 0, at most 1 |

| `symbol_first` | `false` | `true` or `false` |
| `posting_weight` | `presence` | `presence`, `symbol_fraction`, `diff_share`; other than `presence` needs `symbol_first` |
| `smoothing_unit` | `tasks` | `tasks`, `relative`; identical under `presence`, where it is reported as `tasks` |
| `added_weight` (`α`) | 1 | from 0 to 1; other than 1 needs `symbol_first` |

The CLI flags for the last four are `--symbol-first`, `--posting-weight`,
`--smoothing-unit` and `--added-weight`. An option at its default is omitted
from the echoed options, so a default request's output and an evaluation
corpus's digest are unchanged.

The response echoes the options with the tokeniser version, `N`, the query
terms, the known terms and the window stopwords. Title weighting, configurable
path exclusions and the file emission rule of design §5.2 are not implemented
in this version. Nothing is indexed, stored or reported by task type or tag.

**Symbol-first mode** (`symbol_first`, design §2.4). Each changed line belongs
to its smallest enclosing named symbol, or else to its file's remainder;
whitespace-only changes are not changed lines. Each task then holds a record
per context — a live symbol, a file's remainder and the live file itself —
with its added and deleted lines, its length on each side (a symbol's span; a
remainder's `line_count − named_symbol_covered_lines`; a file's
`line_count`; 0 on a side where it does not exist) and its status: `added`
(only after), `modified` (both sides) or `deleted` (only before). A file's
status is its Git change kind (added or copied, deleted, otherwise modified)
and its record is its rolled-up lines. A file the delivery changed no line of
(a pure rename or mode change) has a record with no changed lines and no
leaves; a binary side is one changed line of a one-line file. The leaf level
holds the symbols and the remainders; a remainder (internally
`remainder:<path>`) is scored but is never returned as a selector and never
earns the symbol boost: at file level, a positive remainder score is shown as a
`word_index_remainder` reason with contribution 0. The file level scores each
file from its own rolled-up record; the boost uses its best named symbol.
There is no `file:` fallback leaf in this mode. A delivery whose payload has
no line lengths is left out of the window (`no_line_lengths`) under every
weight, so all symbol-first configurations share one window.

**Posting weights** (design §5.4). A task `d` gives context `c` a weight
`x(d,c)`; the counts of the scoring formulas become masses:
`t(w,c) = Σ x(d,c)` over window tasks with `w`, `T(c) = Σ x(d,c)` over all
window tasks, and `E(w,c) = df(w) · T(c) / N`. The number of tasks with both
stays an integer for `min_support` and for the evidence.

```text
presence         x = 1
symbol_fraction  x = min(1, changed / (len_before + len_after))   (1 when the length is 0 and changed > 0)
diff_share       x = changed / Σ over the task's records at the same level of α·changed if added else changed
```

`changed` is added plus deleted lines. Under `diff_share` the leaf level and
the file level each have their own total, which counts every record of the
task at that level, live or not, and is taken after `added_weight` is applied.
With `smoothing_unit: relative` each weight is divided by the context's mean
weight over the window (its mass divided by the number of tasks touching it), so `κ` keeps the meaning of one
typical task; with `tasks` it is one task of weight 1.

**Added versus modified** (design §5.5). A record whose status is `added` is
multiplied by `added_weight` after the relative normalization, so `α = 0`
learns only from modifications and deletions. Each `word_index_term` reason
then gives the posting's and the context's weight and how many tasks modified
and created the context.

## Stage 3 Orbit adapter and plugin

The shipped integration is an Orbit external tool, not a private control-plane
dependency. Three schema-version-1 `*.orbit-tool.yaml` manifests register the
same executable as `orbit.graph.recommend`, `orbit.graph.status`, and
`orbit.graph.maintain`. Orbit invokes the binary with no argv, JSON stdin/stdout,
and `ORBIT_TOOL_NAME`; the ordinary clap interface remains unchanged otherwise.

Requests route the repository by an explicit absolute path. Authoritative task
and hybrid reads additionally require an explicit Orbit workspace selector.
Cwd and `ORBIT_TOOL_WORKSPACE_ROOT` are never interpreted as authority. The
adapter resolves the installed `orbit` executable from `PATH` and makes every
Orbit read through `orbit tool run`: `orbit.workspace.list`,
`orbit.task.show`, `orbit.search`, and `orbit.workflow.run.show`. It never reads
SQLite, task bundles, or another private store.

Matching the checkout's `origin` URL to a registered workspace's `git_remote`
is an accident guard against selecting the wrong workspace. The checkout owner
can rewrite `origin`, so this match does not authorize access. Orbit's per-call
tool authorization is the security boundary (STD-05 §R5).

An Orbit delivery is marked verified only when a successful public run exposes
a committed step with exact base, commit, and task ID, and Git verifies both
strict ancestry and landing-branch reachability. Public `orbit.workspace.list`
must match the requested workspace and Git common repository; each
run's public workspace path and task are checked even when a snapshot is
supplied. Nested CLI calls have a hard timeout and combined output cap, capture
through pipes without filesystem scratch, and kill/reap the direct child. The
run's finish time is an uncertain landing-time proxy unless a future public feed
attests exact delivery time. Current task text is `known_pre_execution` only
when observed while the public lifecycle has no start and is still pending;
otherwise it is explicitly post-execution or uncertain. Earlier versioned
`TaskAssociation` observations can be supplied; they are stored in separate
`caller_supplied` rows and never replace run-observed task text or its
provenance. They do not become a second task authority.

Orbit resolves its own global root when the adapter invokes each registered
tool; the plugin never supplies `--root`. After validating the selected
workspace through `orbit.workspace.list`, the adapter invokes
`orbit.workflow.run.show` from the explicitly routed repository and rejects the
response unless its prepare workspace shares the selected workspace's Git
common directory. Thus cwd participates only in locating the workspace-local
run record; it never selects or substitutes task authority.

Bounded `orbit_sync` accepts explicit run IDs and task IDs (resolved only to
their current public `job_run_id`). It is incrementally replayable through
stable delivery IDs and preserves the first immutable observation on replay;
genuinely changed Git boundaries still conflict. Its response always says
coverage is partial. Bounded Git bootstrap records a frozen tip/frontier and
advances over multiple requests without moving the complete cursor until the
entire first-parent snapshot has been visited. The
missing Orbit-side seam is precisely a cursor-paginated public delivery feed
containing workspace, task ID, immutable base/landed commit, exact landing time
with provenance, and an immutable pre-execution task snapshot or snapshot
revision. Until that exists, older retries and unlisted tasks require explicit
IDs and historical task text without an attested capture remains excluded.

## Stage 3 chronological evaluation

The versioned evaluation corpus separates the query-time task snapshot and
target revision from a held-out delivery envelope. The evaluator validates the
snapshot strictly predates the cutoff, requires verified held-out evidence with
a proven post-cutoff time/lower bound, rejects contradictory chronology where a
known delivery predates the cutoff despite a later prospective lower bound, and
rejects held-out boundaries declared in training. Every case uses a disposable clone, corpus-only history index, and
graph synced at the exact target revision; it never opens the operational
history/graph. It previews the held-out Git diff without writing it, and maps
truth only to destinations live at the target revision while reporting added,
unsupported, and otherwise unresolved truth omitted from each denominator. All declared future or
uncertain-time deliveries pass through the same cutoff filter as production
ranking.

Five `recommend` variants are evaluated: combined evidence; direct
similar-task change evidence; current-tree lexical plus bounded graph
structure; query-independent eligible delivery frequency; and the word index,
built for each case only from the deliveries before that case's cutoff, with
the corpus's optional `word_index` options (echoed as `word_index_options`).
Beside them the evaluator scores the baselines and combinations of section
10.5 of the context-index design (`docs/design/context-index/2_design.md` in
the Orbit repository), so every case is ranked nine ways. Reports contain
separate file and symbol recall/precision@K, stale-result rate, mean/maximum
latency, coverage/exclusions, exact revisions, source provenance, and a
normalized input digest. The recorded real cohort in `evals/` is
intentionally small and has no eligible historical training text, so it
supports no superiority claim.

### Folds

The corpus may declare `folds`, each `{id, cutoff, revision}`: a rolling-origin
fold whose cases were first dispatched after `cutoff`, and the full commit
that was the landing branch's tip then. Every case then names its `fold`. A
case naming no declared fold is excluded (`fold_missing_or_undeclared`), as
is one cut before its fold (`fold_cutoff_after_case_cutoff`) or whose target
revision is neither the fold's revision nor a descendant of it
(`fold_revision_not_ancestor_of_target`): BM25 may never read a tree later
than the case's own. A fold with a duplicate or empty id, or a revision that
is not a full commit ID in the repository, refuses the corpus. Without
`folds`, each case is its own fold, cut at its own cutoff and target
revision, and a case that names a fold is excluded. Folds are read in cutoff
order (then id); the report's `folds` lists each with its role, its evaluated
cases and the BM25 documents it indexed.

### Baselines and combinations

| Row | What it ranks |
|---|---|
| `bm25_code` | The task text as a query against the tree of the case's fold revision, ranked by SQLite FTS5 `bm25` (best first, ties by selector). At file level the documents are the files (path and content columns); at symbol level they are the symbols the language extractors find (name and qualified name, signature, body). No history is read |
| `explicit_mentions` | The paths and symbol names the task text writes that resolve at the case's target revision, in the order written |
| `word_index_plus_mentions` | Explicit mentions first, then the word index's ranking without repeats |
| `word_index_bm25_rrf` | The word index and `bm25_code` fused by reciprocal rank: each selector scores `Σ 1 / (60 + rank)` over the two rankings, each read 50 deep (or K when deeper) |

Both baselines read the task text the corpus's `baseline_text_fields` names
(`title` or `title_description_criteria`, the default: the text as filed),
through the word index's tokeniser so a term means the same to every ranking.
BM25 indexes pre-tokenised text (each term in order, repeats kept), so its
term frequencies are the tokeniser's; a query matches any of its terms. A
lockfile or changelog (the word index's path exclusions), a binary blob or a
blob over 1 MiB is not a document, and the fold reports how many were left
out by reason. BM25 indexes the tree at the fold's cutoff commit, not at each
case's own target; with implicit folds the two are the same.

Explicit mentions read a token as a path when it has a `/` or a file
extension: it resolves to the file with that exact path, else to the one
file whose path ends with it at a `/` (an ambiguous name resolves to none).
`file:` and `symbol:` selectors, `./` prefixes and `:line` suffixes are
accepted. Any other token is read as a symbol name only when it is shaped
like code (it has a `_`, a `::` or `.` qualifier, or an uppercase letter
after its first character), so ordinary words never match. It resolves to
every symbol with that short name (and, when qualified, a qualified name
ending with it), unless more than five symbols match. At file level the row
lists the mentioned files and the files of mentioned symbols; at symbol level
the mentioned symbols.

A combination's latency is the sum of its parts'. Stale results are counted
against the case's target tree for every row; BM25 can return a file the
fold's tree has and the target does not.

### Readout

Every metric row adds `cases_with_truth`, `mean_recall_at_k` (recall at K
per case, averaged over the cases with truth at that level), `cases_answered`
and `coverage` (the share of evaluated cases with a non-empty answer), beside
the pooled `recall_at_k`. `split_metrics` repeats the rows for
`mostly_added_files` (more than half of the held-out delivery's changed files
were added, so most of its work is outside any truth) and
`mostly_existing_files`; each case reports `mostly_added_files`.

`paired_differences` holds, for every other row, level and per-case metric
(`recall_at_k` over the cases with truth, `precision_at_k` over every case),
the mean of `word_index − baseline` over the same cases and its 95%
percentile-bootstrap interval (2,000 resamples, fixed seed, so a rerun gives
the same interval). The verdict is `word_index_higher` or `baseline_higher`
when the interval excludes zero, `no_difference_shown` when it includes it,
and `unevaluable` with fewer than two paired cases.

### Gates

`gates` states G1 and G2 of design section 10.5 as `passed`, `failed` or
`unevaluable`, with the numbers behind each. Both are read on file
`mean_recall_at_k` paired differences at K = 10; a corpus with another K, or
fewer than two cases with file truth, leaves both `unevaluable`, and an
unevaluable gate counts as not passed.

- **G1, keep the index:** passed when `word_index − frequency` and
  `word_index − bm25_code` both have intervals above zero and the word
  index's file coverage is at least 0.5. Otherwise failed, and `alternative`
  names whichever of `bm25_code`, `word_index_plus_mentions` and
  `word_index_bm25_rrf` has the highest mean file recall, the candidate for
  the slot instead.
- **G2, which ranking the plugin keeps:** passed, `keep: word_index`, when
  the word index beats both `combined` and `task_search_only` with intervals
  above zero. Failed otherwise: `keep` names an existing ranking that beats
  the word index (the larger difference when both do), or is `null` when no
  difference is shown. `combinations_beating_both` lists the combinations
  that beat the word index and each existing ranking, in which case both
  parts would stay. No combination of the word index with `recommend`'s own
  rankings is scored, so this reads the two scored combinations.

- **G4, which weighting options stay** (one result per option, `option`
  `posting_weight` — with its `smoothing_unit` — or `added_weight`): read on
  the headline folds as the cell chosen on the selection folds minus the grid
  cell equal to it with that option at its baseline (`presence`, or `α = 1`),
  per case, for file and symbol mean recall@10. Passed when one level's
  interval is above zero and the other's is not below zero; the selected
  setting is then kept. Failed when neither holds, or when the chosen cell
  already holds the baseline. Unevaluable without a grid, when the grid does
  not sweep the option, when it has no matching baseline cell, without a
  chosen cell or headline cases, off K = 10, or with fewer than two paired
  cases at either level. Each result gives `selected_setting` and
  `kept_setting`; an option that does not pass is set back to its baseline.

G1 and G2 are read on the configuration G4 keeps (`word_index_options`; the
chosen one is `selected_word_index_options`), so an option that fails G4
never reaches them. G3 needs pilot data from Orbit and is not reported.
Without a grid the gates read every evaluated case (`scope: all_cases`).

### Parameter grid

`word_index_grid` sweeps the word index's options:
`{selection_folds, text_fields, window_size, symbol_boost, min_support,
smoothing, max_term_share, symbol_first, posting_weight, smoothing_unit,
added_weight}`. Each axis is a list; an empty or omitted axis
holds the corpus's `word_index` value, and the cells are every combination
(at most 256 before deduplication; each must be valid options). A cell under
`presence` is normalized to `smoothing_unit: tasks` and duplicates are kept
once; a cell that sets `posting_weight` or `added_weight` away from its
baseline without `symbol_first` is left out, and a grid with no cell left is
refused. `selection_folds` must be at least
1 and fewer than the folds: the first that many folds in cutoff order choose
the cell and the rest report the headline numbers, as design section 10.4
chooses on the first five weekly cutoffs and reports on the last three.

Every case is ranked once more per cell, at both levels. The report's
`sweep` lists each cell's options and its `word_index` file and symbol rows
over the selection folds and over the headline folds, and `selected_cell`:
the highest selection-fold mean file recall@K, then mean symbol recall@K,
then the earlier cell (`null` when no selection case has file truth).
`kept_cell` is the cell G4 keeps: the selected cell with every option that
did not pass G4 set back to its baseline (`null` when that is not a cell).
`sweep.headline` reads every row, split and paired difference over the
headline folds with the word index and its combinations on the kept cell,
and the gates are read there (`scope: headline_folds`). The top-level
`metrics`, `split_metrics` and `paired_differences` stay over every case
with the corpus's own `word_index` options.

### Report version

The report is schema version 4: version 3 plus gate G4 (each gate result's
`option`, `selected_setting` and `kept_setting`, `null` for G1 and G2),
`gates.selected_word_index_options` and `sweep.kept_cell`. Version 3 was
version 2 plus the four rows above, the per-row coverage and mean-recall
fields, `baseline_text_fields`, `folds`, `split_metrics`,
`paired_differences`, `gates`, `sweep`, and each case's `fold` and
`mostly_added_files`. The corpus stays version 1: `folds`, a
case's `fold`, `baseline_text_fields` and `word_index_grid` are optional, and
a corpus without them has the same digest as before.
