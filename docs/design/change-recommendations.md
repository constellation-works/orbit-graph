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
`.orbit-graph/change-history.2.sqlite3`. It is not task authority. External
delivery/task systems retain authority and should be able to replay the public
envelopes after `history rebuild`, which intentionally recreates only evidence
available from Git.

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
- `verified_delivery` means the producer supplied verified landing evidence.
  The importer validates Git boundaries but does not invent this strength.
  `git_only` is always weaker, including task IDs parsed from `Task-Id:` or
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

The v2 SQLite schema stores scope cursors, delivery payloads, normalized task
memberships, delivery/creation/snapshot temporal status, timestamp and
provenance columns, text availability, file stats/fallbacks, and both sides of
symbol evidence. Foreign keys cascade within a delivery. Repository, branch,
schema version, extractor
version, provenance, task ambiguity, and evidence strength remain queryable.
`HistoryIndex::deliveries` returns stable typed records so ranking code does not
depend on SQLite layout.

Every import and sync holds a sidecar file lock and uses one SQLite immediate
transaction. Delivery rows and the cursor commit together. A failure after any
intermediate write rolls back both. Concurrent cursor movement is detected
before commit. Sync walks only the named branch's first-parent chain, oldest to
newest, and refuses a missing or off-chain stored cursor as divergent/rebound
history. Traversal is capped (default 1,000); exceeding the cap fails without a
partial cursor or partial evidence, and callers may explicitly raise `--limit`.

`history rebuild` validates and extracts the entire bounded first-parent range
before opening its replacement transaction, then deletes and recreates only the
selected repository/branch scope atomically. Verified envelopes are not hidden
inside a second source of truth and must be replayed by their producer afterward.

## CLI contract

All successful commands emit one JSON value on stdout. Failures emit the normal
JSON error object on stderr and exit nonzero.

```text
orbit-graph history import --input delivery.json
orbit-graph history import --input -
orbit-graph history sync --branch main [--limit 1000]
orbit-graph history status --branch main
orbit-graph history rebuild --branch main [--limit 1000]
```

`status` reports repository/branch, database path, versions, cursor, total
deliveries, verified versus Git-only counts, and task-membership count. It is
safe for operational inspection but is not a task status API.

## Version compatibility and later evaluation

Import schema v2 intentionally does not accept v1 envelopes: its required
temporal fields change the meaning needed for leakage-safe evaluation, and
unknown fields were rejected by v1. Producers must emit `schema_version: 2`;
v1 envelopes fail closed rather than receiving guessed timestamps. The history
index and change extractor also advance independently to version 2, selecting a fresh
`change-history.2.sqlite3` database. Existing v1 databases are left untouched;
verified producers replay v2 envelopes and Git-only evidence can be rebuilt.
The crate/package version remains unchanged because this task is not a release.

Tree-sitter extraction is syntactic; macros, generated code, dynamic dispatch,
and malformed files can reduce evidence. Rename detection follows libgit2
similarity heuristics. File moves plus semantic rewrites may remain unmatched,
which is preferable to invented identity. Root commits lack a commit-valued
before boundary and are used only as the starting cursor; their initial tree is
not emitted as a delivery in v2.

Stage 2 ranking must weight verified delivery evidence above Git-only evidence,
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

The standalone JSON CLI mirrors the contract:

```text
orbit-graph recommend --query "repair parser cache" --level file --limit 10
orbit-graph recommend --task-id TASK-123 --level symbol --revision HEAD~1
orbit-graph recommend --task-id TASK-NEW --task-snapshot task.json --level file
orbit-graph recommend --query "repair parser cache" --hybrid-hits hits.json
```

Exactly one of `--query` and `--task-id` is required. `--branch` selects the
history scope (default `main`), `--revision` is resolved to a full commit
(default: current checkout), and `--cutoff` accepts RFC 3339 or
`unix:<seconds>`. The hybrid-hit file is a JSON array of objects with `task_id`
and a finite non-negative `score`. `--task-snapshot` accepts one
`TaskAssociation` JSON object and requires `--task-id`; it is never inserted
into the history index.

Ranking counts each delivery once and each file once per delivery. It combines
the strongest task relevance in a delivery with verified-versus-Git-only
evidence, commit-distance recency, delivery breadth, multi-task ambiguity,
location prevalence, generated/lockfile discounts, directional co-change, and
bounded current structure. Scores are additive relevance scores, not
probabilities. `reasons` exposes every contribution; `association` exposes the
directional support, source/destination counts, confidence, and lift.
Association support and prevalence use the complete eligible corpus, including
deliveries whose tasks do not match the query; relevance chooses seed
destinations. Imports with the same repository/branch before-and-after boundary
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
cutoff; equality is excluded. Task-ID mode reads only snapshots proven available
before execution and excludes every equivalent delivery boundary associated
with the target task. Historical paths are followed through Git rename detection;
symbols must resolve uniquely by current qualified identity or conservative
signature identity. Deleted and ambiguous symbols are omitted. Symbol mode
uses a `file:` selector only when history has file-only or unresolvable symbol
evidence, and marks it with `file_fallback` and `fallback_reason`.

`source_freshness`, `resolved_target_revision`, `effective_cutoff`, and
`fallbacks` make stale/cold-start limitations explicit. Current-tree lexical
matching remains useful with empty history. Structural expansion is used only
when the requested revision is the current checkout; a non-HEAD request never
silently reuses HEAD graph structure. Every cached structural destination is
also resolved against the requested Git tree, so a graph synced before a later
committed deletion cannot return the removed file or symbol; skipped stale rows
are explicit in `fallbacks`. These seams let Stage 3 run chronological
backtests by setting both `target_revision` and `cutoff`, then comparing the
ranked selectors to held-out delivered destinations.
