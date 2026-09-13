# Change explorer: evidence contract and snapshot semantics

Status: approved milestone 1 contract. It describes the `orbit-graph-explorer`
workspace member in this repository and the `orbit-graph` 0.9.x public API it
consumes. It does not authorize a release, a user-facing launch, or any change
to root-crate query semantics.

## Purpose and boundaries

The change explorer explains one Git change — a base revision and a head
revision — through source relationships: what changed, which callers and entry
points may be affected, and which tests have a defensible connection. It does
not judge whether a change is correct, complete, or safe to merge. Every claim
it renders must name the revision and the evidence that supports it.

The explorer consumes only the public `orbit_graph` library API. It reads no
Orbit control-plane state: no `.orbit/` directory, no Orbit configuration, no
task store, and no Orbit plugin adapter. It does not modify the root crate's
query semantics or `EXTRACTOR_VERSION`.

Milestone 1 (this document plus the landed scaffold) fixes the contract and the
snapshot foundation. Milestone 2 adds the changed-symbol slice and the loopback
service. Milestone 3 adds base/head exploration and UI polish. Milestone 4 runs
the five-change evaluation. Later milestones may not widen a claim recorded here
to make the UI simpler.

## Decisions

### D1. Placement: a workspace member crate

The explorer is a workspace member of this repository,
`crates/orbit-graph-explorer` (package `orbit-graph-explorer`, with its own
`[[bin]]`). The library (`crates/orbit-graph`) and the `orbit-graph` executable
(`crates/orbit-graph-cli`, installed with
`cargo install --path crates/orbit-graph-cli`) keep their semantics and their
independence from Orbit state.

Reasoning: the explorer is the first consumer that exercises the public library
API end to end, so keeping it in-tree makes an API gap visible in the same
change that needs it, and one `cargo test --workspace` run covers both crates. A
spin-out to a companion repository remains possible precisely because the
dependency is one-directional and version-pinned: the explorer depends on
`orbit-graph` through its public API only, so the split cost is a manifest edit
plus a published version bump, not a code untangling. Nothing in the explorer may
be merged into the root crate to "make it work"; a genuine gap is recorded in
[Capability gaps](#public-api-capability-gaps) and resolved by a separate,
focused core change.

### D2. Delivery: a loopback HTTP service with an embedded UI

The UI is served by a small HTTP service bound to `127.0.0.1` only, with an
explicit repository scope, an `Origin`/`Host` check, and a per-launch bearer
token. The static UI assets are embedded in the binary. There is no desktop
shell, no installer, and no packaging work in v1.

Reasoning: a browser gives the three-pane layout and source rendering for free,
and a loopback service keeps the graph-querying process local, short-lived, and
inspectable. Embedding the assets avoids a second deployable artifact and
removes the possibility of serving files from the inspected repository.

### D3. Comparison semantics: direct base → head

Direct comparison of the two revisions the user names is the primary and default
mode (`direct_base_head`). A PR-style merge-base mode (`merge_base`) may be
added later, but it must be labelled distinctly in the UI and in the exported
report, and it must show the effective base SHA it computed. The two modes are
never silently swapped, and the mode is a required field of every comparison
payload.

Reasoning: a merge-base rewrite of the user's stated base changes which symbols
count as "changed". Users diagnose confusing output by comparing SHAs, so the
effective base must always be visible.

### D4. Untrusted repository content is never executed

Opening a repository must never execute its hooks, build scripts, test
commands, package scripts, or macros. Git is driven through `git2` or through
argument-vector process invocation; no shell string is ever interpolated from
repository or user input. Snapshots are materialized into task-owned temporary
trees — never by checking out over the user's working tree — and only
task-owned artifacts are ever removed. Materialized files are written without
the executable bit, so no snapshot path is invocable even by accident.

## Verified API inventory

Every row below was verified against the current implementation
(`crates/orbit-graph/src/lib.rs`, `crates/orbit-graph/src/query/*`,
`crates/orbit-graph-cli/src/command/*`), not against the README. Confidence
labels are `exact`, `import_resolved`, `same_module`, `fuzzy_name` (CLI
spellings `exact`,
`import`, `same_module`, `fuzzy`); the floor admits its own level and every
stricter level. Reference kinds are textual (`call`, `type`, `use`,
`trait_bound`) or structural (`impl`, `extends`, `implements`).

| Operation | Input selector forms | Confidence | Kinds | Bounds | What the result does not claim |
| --- | --- | --- | --- | --- | --- |
| `sync(SyncMode::Auto\|Full)` | none; whole worktree | n/a | n/a | none; `Auto` compares mtime then content hash, `Full` re-extracts | Not a compilation. Skips dot-files and dot-directories, `.orbitignore` and default ignore patterns, Git-ignored paths, binary/archive/lock extensions, and files in unsupported languages. `files_indexed` is indexable files, not repository files. |
| `search(SearchQuery{query,kind,lang,limit})` | free text; optional `kind` (`symbol`\|`string`\|`config`) and `lang` | n/a | n/a | `limit` default 20 (`DEFAULT_SEARCH_LIMIT`); empty query or `limit == 0` returns no matches | Lexical FTS5 only: whitespace-split terms are quoted as phrases and ANDed, so operator syntax, prefixes, and semantic similarity are unavailable. Ranking is internal `bm25` and is not returned. A match is a definition, string literal, or config key — never a relationship. |
| `show(Selector, max_bytes)` | `symbol:`, `file:`, `module:`, `command:`; `dir:` resolves to `None` | n/a | n/a | `max_bytes` default 65536 (`DEFAULT_SHOW_MAX_BYTES`); `metadata.truncated` marks a clipped span | Source bytes are read from the worktree at query time, not from the index, so a stale index can disagree with the returned text. Non-UTF-8 source is returned as `{encoding:"bytes",bytes:[…]}`, not as text. |
| `refs(Selector, RefOpts{confidence,kind})` | only `symbol:` resolves a target; other forms return an unresolved target with empty lists | floor, default `same_module`; each entry carries its own level | a textual `kind` filters `refs` only; a structural `kind` filters `relations` only | no node cap; `skipped_low_confidence` counts rows excluded by the floor | Inbound syntactic references and structural relations, not a caller proof. When the precise floor yields no textual refs, a `fallback` block of `fuzzy_name` (name-only) matches is attached; those may be unrelated symbols that share a short name. Dynamic dispatch, macro-generated, and runtime-resolved call sites are missing or name-only. |
| `callees(Selector)` / `callees_with_options(Selector, CalleeOpts)` | `symbol:` only (`kind` may be empty); other forms return an empty list | every edge carries typed `RefConfidence`; options add a confidence floor | call edges only; options accept a kind filter | none | Outbound call sites written inside the symbol's span. `target_qualified` is `None` for unresolved or name-only targets, so an edge can name a callee the index cannot locate. The no-options method preserves the unfiltered result. Not a runtime call trace. |
| `impact(Selector, depth, min_confidence)` / `impact_with_direction(…, ImpactDirection)` | `symbol:`, `module:`, `command:`; `file:`/`dir:` return an empty set | floor, default `same_module` | `inbound` follows callers/reverse relations, `outbound` follows callees/forward relations, and `both` preserves the historical neighbourhood | `depth` default 3 (`DEFAULT_IMPACT_DEPTH`, `0` means default), `IMPACT_NODE_CAP` 200 nodes, `truncated` flag | A bounded static traversal, not a reachability proof. `ImpactEntry::origin` distinguishes symbols from file-attributed call sites while `qualified_name` retains its compatible value. A `fallback` block, when present, is name-only. |
| `trace(command, depth, min_confidence)` | a command name, with or without the `command:` prefix | floor, default `same_module` | call edges only | `depth` default 5 (`DEFAULT_TRACE_DEPTH`), `TRACE_NODE_CAP` 200 nodes, `truncated` flag | Resolves only commands the extractor discovered into the `commands` table with a handler symbol; an unknown command yields `root: None`, which is not evidence that the command does not exist. Nodes whose `qualified_name` is `None` were not resolved and are expanded no further. |
| `deps(Selector)` | `file:` or `dir:` only; other forms fail with `InvalidData` | n/a | import/module edges | none | Direct source-level import edges declared by in-scope files. Not a transitive closure and not the Cargo/package dependency graph. `target_path` is a language-specific opaque specifier, not a resolved file. |
| `overview(Option<Selector>, format)` | `None`, `file:`, or `dir:`; other forms fail with `InvalidData` | n/a | n/a | `summary` returns top files with empty `symbols`; `full` returns every in-scope file | Counts of indexed rows, so unsupported languages and skipped files are invisible in the totals rather than reported as gaps. |
| `implementors(Selector)` | `symbol:` or `module:` (trailing `::` segment is the trait name); `file:`/`dir:`/`command:` return an empty list | recorded per relation | `impl`, `implements` | none | Matches a trait by **short name**, so same-named traits from other modules or crates can appear. Absence is not proof that no implementation exists, particularly across languages without an explicit implements relation. |

Repository-wide caveats that apply to every row: the index is a static,
syntax-driven tree-sitter graph, not a compiler or language server. Dynamic
dispatch, reflection, generated code, macro expansion, runtime imports, and
ambiguous same-name symbols produce missing or lower-confidence edges.
Cross-file and cross-language resolution is deliberately conservative. The
supported languages are Rust, C, C#, Go, Java, JavaScript/JSX, TypeScript/TSX,
Kotlin, Python, and Ruby, plus Markdown headings and JSON/YAML/TOML/dotenv
config keys; anything else is unindexed and must be reported as out of scope
rather than as "no relationship found".

## Revision and snapshot semantics

**Refs resolve once, to immutable SHAs.** A comparison resolves the
user-supplied base and head refs through `git2` revision parsing and peels each
to a commit. Both full SHAs are recorded and are echoed in every payload and
export. A ref that cannot be resolved is an error naming the ref; no snapshot is
materialized.

**Each revision is a separate, isolated index.**
`crates/orbit-graph-explorer/src/snapshot.rs`
materializes the committed tree of each revision into its own temporary
directory (`orbit-graph-explorer-base-*`, `orbit-graph-explorer-head-*` under
the system temporary directory), then opens a `Graph` on that directory with
`SyncPolicy::Manual` and runs one `SyncMode::Full` sync. Queries name a side, so
no result can blend revisions.

Materialization walks the commit tree with `git2` and writes blobs directly. It
never checks out, never touches the user's working tree, index, or object store,
and deliberately excludes, with a reason recorded per entry:

| Excluded | Reason label | Why |
| --- | --- | --- |
| Symbolic links | `symlink` | A recreated link could resolve outside the snapshot tree. |
| Submodules (gitlinks) | `submodule` | Submodule content is not fetched; there is nothing to index. |
| Blobs over 4 MiB | `oversize_blob` | `DEFAULT_MAX_BLOB_BYTES`; large blobs are generated, vendored, or binary. |
| Unsafe entry names | `unsafe_name` | Empty, `.`, `..`, containing a separator or NUL, or named `.git` in any case. |
| Other object types | `unsupported_kind` | Nothing else is materializable. |

**Where snapshot indexes live.** `orbit_graph` derives the database path from
the worktree root it is given, so a snapshot index is written inside its own
snapshot tree at `<snapshot>/.orbit-graph/HEAD.<EXTRACTOR_VERSION>.db` (today
`HEAD.4.db`). Nothing is written under the user repository's `.orbit-graph/`,
and an integration test asserts both facts. An empty Git repository is
initialized at the snapshot root — no remote, no commit, no templates, and
therefore no hook — solely to anchor Git discovery and `git check-ignore` inside
the snapshot so neither can escape to a repository that happens to contain the
system temporary directory.

**Cleanup and staleness in v1.** Snapshot trees are owned by the comparison:
dropping it removes the tree and the index inside it, and only those task-owned
paths are removed. Because each snapshot tree is fresh and fully synced before
any query runs, a v1 snapshot index is never stale — and never reused, which
means a session pays full indexing cost per revision.

**Snapshot lifetime is a hard constraint.** `refs`, `callees`, `search`, and
`show` read source text from the worktree at query time to resolve lines and
return source. A snapshot tree must therefore outlive every query against it.
The tree is not a scratch artifact that may be deleted after indexing.

**Snapshot caching (landed after Milestone 2).** Unless `--no-cache` is given,
`serve`, `snapshot`, and `report` persist each revision's materialized tree
and index under a cache directory (default
`<repo>/.orbit-graph/explorer/snapshots`, override with `--cache-dir`),
keyed by `(commit SHA, EXTRACTOR_VERSION, STORE_SCHEMA_VERSION)`
(`crates/orbit-graph-explorer/src/cache.rs`). A key that does not match the
running binary is rebuilt, never reused. An entry is published atomically —
built into a staging directory, then renamed into place — so a crashed or
concurrent build never leaves a half-indexed tree behind under a trusted SHA.
`orbit-graph-explorer clean` removes entries whose key no longer matches this
binary and entries for commits the repository no longer has; it never removes
anything outside the cache directory, and it never touches the inspected
repository's own `.orbit-graph/*.db` files.

**Dirty working trees are detected and reported, never indexed.** A comparison
inspects the repository status (untracked files included, ignored files
excluded) without refreshing or rewriting the Git index, and records a dirty
verdict plus up to 200 classified entries (`modified`, `untracked`, `added`,
`deleted`, `renamed`, `type_changed`, `conflicted`) with a truncation flag. The
verdict itself is never truncated. Uncommitted work is excluded from snapshots
by construction — a snapshot contains exactly a committed tree — and the UI and
the exported report must carry the explicit notice that indexed evidence is not
what is currently on disk.

**Every path is labelled with its supporting snapshot.** Symbols removed in head
and callers that only exist in base are base-snapshot evidence. Symbols added in
head are head-snapshot evidence. A symbol present in both may have evidence from
both, and the two sets are presented separately rather than merged.

## Changed-symbol identity

Symbols are paired across base and head by **canonical selector**
(`symbol:<path>#<name>:<kind>`). The pairing ladder, strongest first:

1. **Same selector.** Identical path, symbol name, and kind in both snapshots:
   one symbol, possibly with a changed body or signature.
2. **Same path and kind, changed signature.** The symbol is paired and labelled
   `signature_changed`; its evidence is recomputed on both sides because callers
   may differ.
3. **Moved.** Same name and kind at a different path, where the Git change also
   shows the file being renamed or the content moving. Labelled `moved` with
   both selectors retained.
4. **Renamed.** Same path and kind, different name, with Git rename or
   similarity evidence for the containing file. Labelled `renamed` with both
   selectors retained.
5. **Uncertain correspondence.** Anything weaker — several plausible partners, a
   name that occurs multiple times in a file, or a move plus rename together —
   is recorded as `uncertain` with every candidate listed and none chosen.

Rules that later milestones may not relax:

- Ambiguity is preserved, never resolved by guessing. `uncertain` is a
  first-class state in the data contract and must be visible in the UI.
- A pairing label is evidence about identity, not about behavior: a paired
  symbol is not a claim that behavior is unchanged, and an unpaired symbol is
  not a claim that nothing replaced it.
- `added` and `removed` are stated relative to the two named SHAs only. A
  symbol that never existed in indexed form (unsupported language, skipped file)
  is reported as out of scope, not as removed.

## Evidence categories

Every edge and every candidate test carries exactly one category, and the
category survives into the UI and the export:

| Category | Meaning | Strength |
| --- | --- | --- |
| `observed_reference` | A syntactic reference at a known file and line in the named snapshot. | Directly observed in source. |
| `resolved_call` | A call edge whose target resolved to a qualified symbol (`exact` or `import_resolved`). | Observed plus resolved. |
| `import_relationship` | A module/import edge between files (`deps`). | Observed, file-level only. |
| `heuristic_match` | A name-only or same-module association, including every `fuzzy_name` result and fallback block. | Weak; may be a different symbol with the same name. |
| `user_selected` | An association a person asserted in the UI. | Asserted, not derived. |

Consequences that must be stated wherever impact is shown:

- Static reachability is **potential** impact. It is not proof of execution, not
  proof of coverage, and not a severity judgment.
- Candidate tests come from three disclosed sources, and the source is always
  shown: a call path from a test symbol to a changed symbol; an import
  relationship from a test file to a changed file; or a naming/file heuristic
  (for example `foo.rs` ↔ `foo_test.rs`, `tests/` siblings). A heuristic
  candidate is never promoted to a call-path candidate.
- "No path found" means **no path in the indexed evidence**, and it is always
  reported together with the reasons a path could be missing: unsupported
  languages in the relevant files, unresolved or name-only symbols, excluded
  files, and any bound that truncated the traversal.
- A `fallback` block returned by `refs` or `impact` is rendered as
  `heuristic_match` with its note attached, never silently merged into the
  primary result.
- `refs`'s own `fallback` only fires when its floor-filtered `refs` list is
  **empty**: a symbol with even one same-file `exact` reference gets no
  `fallback` at all, so every `fuzzy_name` reference to it — for example a
  method call on a receiver of unresolvable type — would otherwise be
  invisible, not merely downgraded (ORB-12425). At the `import_resolved` and
  `same_module` floors (the default), the explorer's evidence and outbound
  traversal disclose these unconditionally: inbound evidence issues a second,
  `fuzzy_name`-floor `refs` query per node and adds whatever the
  floor-filtered query missed as `heuristic_match` edges; outbound evidence
  already receives every confidence from `callees_with_options` and keeps a
  `fuzzy_name`-only edge instead of dropping it. Each such edge carries a note
  explaining it resolved only at `fuzzy_name`. This is expansion-local: only
  edges incident to the node currently being expanded are checked, so the
  extra query is one per node, not a second traversal, and the node cap and
  depth bound apply exactly as they do to every other edge. The strict
  `exact` floor never receives these — a caller asking for guaranteed matches
  only never receives a name-only guess in their place — and the `fuzzy_name`
  floor needs no special handling, since every match already comes back
  through the ordinary query at that floor. A `call_path` candidate test whose
  weakest hop is one of these edges is a `heuristic_match` candidate through
  the same category-propagation rule as any other path, so a test reaching the
  symbol only that way is disclosed as weak rather than absent.

## Bounds and truncation

The explorer sets bounds explicitly and surfaces every one it hits:

| Bound | v1 value | Surfaced as |
| --- | --- | --- |
| Impact depth | `DEFAULT_IMPACT_DEPTH` (3), caller-overridable | `query_options.depth` in the payload |
| Impact nodes | `IMPACT_NODE_CAP` (200) | `truncated: true` plus a visible UI marker |
| Trace depth / nodes | `DEFAULT_TRACE_DEPTH` (5) / `TRACE_NODE_CAP` (200) | `truncated: true` |
| Search results | `DEFAULT_SEARCH_LIMIT` (20), caller-overridable | `query_options.limit` |
| Source excerpt | `DEFAULT_SHOW_MAX_BYTES` (64 KiB) | `metadata.truncated` |
| Dirty-entry listing | 200 entries (`DIRTY_ENTRY_CAP`) | `working_tree.truncated` |
| Materialized blob size | 4 MiB (`DEFAULT_MAX_BLOB_BYTES`) | `excluded[].reason = oversize_blob` |
| Per-request wall clock | set by the service (milestone 2) | `truncated_by: "time_budget"` |

A truncated result is never presented as complete. Concretely: any view
rendering a truncated set must say so next to the set, the exported report
repeats every truncation flag and the bound that caused it, and no summary
sentence ("N callers affected") may be generated from a truncated set without
the "at least" qualifier and the bound that stopped it.

## Minimal data contract

These are the payloads the service and the export use. Field names are stable;
unknown fields must be ignored by readers, and a removed field is a breaking
change. `schema_version` is independent of `orbit-graph`'s crate version and of
`EXTRACTOR_VERSION`.

### Resolved comparison

```json
{
  "schema_version": 1,
  "repository": "/work/widgets",
  "mode": "direct_base_head",
  "base": {"requested_ref": "main", "commit_sha": "1111111111111111111111111111111111111111"},
  "head": {"requested_ref": "feature/x", "commit_sha": "2222222222222222222222222222222222222222"},
  "effective_base_sha": "1111111111111111111111111111111111111111",
  "working_tree": {
    "dirty": true,
    "truncated": false,
    "notice": "Working tree has 2 uncommitted change(s). Snapshots index committed revisions only; uncommitted files are excluded from all evidence.",
    "entries": [{"path": "src/lib.rs", "change": "modified"}]
  },
  "snapshots": [
    {
      "side": "base",
      "commit_sha": "1111111111111111111111111111111111111111",
      "files_indexed": 412,
      "extractor_version": 4,
      "excluded": [{"path": "vendor/blob.bin", "reason": "oversize_blob", "bytes": 9437184}]
    }
  ]
}
```

`mode` is required and is `direct_base_head` or `merge_base`. In `merge_base`
mode `effective_base_sha` is the computed merge base and differs from
`base.commit_sha`; in direct mode the two are equal. A reader must display the
mode and both SHAs.

### Changed-symbol list

```json
{
  "schema_version": 1,
  "symbols": [
    {
      "status": "removed",
      "pairing": "same_selector",
      "base": {"selector": "symbol:src/lib.rs#removed_helper:function", "snapshot": "base"},
      "head": null,
      "uncertain_candidates": [],
      "note": null
    },
    {
      "status": "modified",
      "pairing": "signature_changed",
      "base": {"selector": "symbol:src/lib.rs#entry:function", "snapshot": "base"},
      "head": {"selector": "symbol:src/lib.rs#entry:function", "snapshot": "head"},
      "uncertain_candidates": [],
      "note": "return type changed"
    }
  ],
  "out_of_scope": [{"path": "web/app.svelte", "reason": "unsupported_language"}]
}
```

`status` is `added`, `removed`, `modified`, or `uncertain`. `pairing` is
`same_selector`, `signature_changed`, `moved`, `renamed`, or `uncertain`. When
`pairing` is `uncertain`, `uncertain_candidates` lists every candidate selector
with its snapshot and no candidate is elevated.

### Evidence path

```json
{
  "schema_version": 1,
  "from": {"selector": "symbol:tests/api.rs#covers_entry:function", "snapshot": "head"},
  "to": {"selector": "symbol:src/lib.rs#entry:function", "snapshot": "head"},
  "truncated": false,
  "truncated_by": null,
  "edges": [
    {
      "from": "tests/api.rs#covers_entry",
      "to": "src/lib.rs#entry",
      "relationship": "call",
      "category": "resolved_call",
      "confidence": "import_resolved",
      "snapshot": "head",
      "source": {"file": "tests/api.rs", "line": 42},
      "note": null
    }
  ]
}
```

`relationship` is a `RefKind` value (`call`, `type`, `use`, `trait_bound`,
`impl`, `extends`, `implements`). `category` is an evidence category from the
table above. `confidence` is the `orbit_graph` label for a derived edge and is
`null` only for `user_selected`. `snapshot` is required on every edge, so a path
can never silently cross revisions. `source.line` may be `null` when a call site
could not be attributed to a line; `source.file` is always present.

`GET /api/evidence` accepts a `direction` parameter, `inbound` (the default) or
`outbound`, echoed in `query_options.direction` and `impact.direction`. Both
directions share every bound, category, and side-separation rule above; only
the traversal edge and the hop ordering differ. `inbound` walks callers and
reverse relations back to the queried symbol: `edges[0].from` is the affected
(farthest) symbol and the last edge's `to` is the queried symbol, so a reader
walks the chain in the direction the change propagates. `outbound` walks
callees forward from the queried symbol, using `orbit_graph::Graph::callees`
in place of `refs` for each hop: the ordering is the same contract with the
arrow reversed, so `edges[0].from` is the queried symbol and the last edge's
`to` is the reached callee. A call the resolver could not bind to an indexed
symbol — an external crate function, a dynamically dispatched call, or a name
the extractor could not resolve — is never dropped or fabricated as a node;
it is reported instead in a report-level `unresolved_callees` list:

```json
{"unresolved_callees": [{"name": "log_event", "line": 42, "reason": "…"}]}
```

`GET /api/entry-points` and `GET /api/candidate-tests` are inbound only, by
definition: an entry point and a candidate test are both about what could
reach the queried symbol, not what it reaches.

### Candidate tests

```json
{
  "schema_version": 1,
  "candidates": [
    {
      "test": {"selector": "symbol:tests/api.rs#covers_entry:function", "snapshot": "head"},
      "source": "call_path",
      "category": "resolved_call",
      "path_id": "p-1",
      "changed_symbols": ["symbol:src/lib.rs#entry:function"],
      "truncated": false
    },
    {
      "test": {"selector": "file:tests/lib_test.rs", "snapshot": "head"},
      "source": "naming_heuristic",
      "category": "heuristic_match",
      "path_id": null,
      "changed_symbols": ["symbol:src/lib.rs#entry:function"],
      "truncated": false,
      "note": "file name matches src/lib.rs"
    }
  ],
  "unsupported_scope": [{"path": "web/app.svelte", "reason": "unsupported_language"}]
}
```

`source` is `call_path`, `import_relationship`, or `naming_heuristic`. A
candidate is never presented as coverage, and the list is never described as
"the tests for this change".

### Exported change report

```json
{
  "schema_version": 1,
  "generated_at": "2026-09-12T00:00:00Z",
  "comparison": {"…": "the resolved comparison payload, verbatim"},
  "index_identity": {
    "extractor_version": 8,
    "store_schema_version": 1,
    "orbit_graph_version": "0.9.2",
    "explorer_version": "0.9.2",
    "base_index_identity": "blake3:…",
    "head_index_identity": "blake3:…"
  },
  "query_options": {
    "depth": 3, "direction": "inbound", "min_confidence": "same_module", "kind": null,
    "node_cap": 200, "time_budget_ms": 5000, "source_max_bytes": 65536, "language": null,
    "change_kind": [], "scope": null, "excerpts": "controlled", "excerpt_radius_lines": 5,
    "selection": []
  },
  "changed_symbols": {"…": "the changed-symbol payload"},
  "evidence_paths": [{"…": "inbound evidence path payloads"}],
  "outbound_paths": [{"…": "outbound (callee) evidence path payloads, same shape, arrow reversed"}],
  "candidate_tests": {"…": "the candidate-test payload"},
  "scope": {
    "truncated": [{"what": "impact", "bound": "impact_node_cap", "value": 200}],
    "unsupported": [{"path": "web/app.svelte", "reason": "unsupported_language"}],
    "excluded": [{"path": "vendor/blob.bin", "reason": "oversize_blob"}]
  },
  "source_rendering": "reference"
}
```

`store_schema_version` is the graph store's public `STORE_SCHEMA_VERSION`
(currently 1). Together with `extractor_version`, it identifies the index
format used for a snapshot.

`source_rendering` is `reference` (file, line span, and SHA only) or `excerpt`
(bounded source text included). An export containing excerpts carries the
`excerpt` value and the byte bound that produced it, so a reader can tell
whether the report embeds repository content. Reports are self-describing: both
SHAs, the extractor and schema versions, the index identity, the query options,
and the complete truncated/unsupported/excluded scope are always present, even
when empty.

`outbound_paths` carries the same per-location `embedded`/`reference` marking
as `evidence_paths`, for every queried changed symbol's outbound (callee)
evidence; it is always present, even when empty (a leaf changed symbol calls
nothing). Two exports of the same inputs remain byte-identical with
`outbound_paths` populated, the same as every other field.

## Service surface

All endpoints are served on `127.0.0.1` only, under one repository scope fixed
at launch. Every request must carry the per-launch token
(`Authorization: Bearer <token>`, printed once at launch and never written to a
world-readable file), and every request with an `Origin` or `Referer` header must
match the service's own origin; a mismatch is rejected before any graph work.
Requests whose repository parameter is not the launch-scoped repository are
rejected — the scope is never inferred from the request.

| Method and path | Purpose | Returns |
| --- | --- | --- |
| `GET /` | Embedded UI shell | HTML from the binary; never from the repository |
| `GET /api/comparison` | The resolved comparison for the launch scope | Resolved comparison payload |
| `GET /api/changed-symbols` | Changed-symbol slice (milestone 2) | Changed-symbol payload |
| `GET /api/evidence?selector=…&side=…&depth=…&confidence=…&direction=inbound\|outbound` | Relationship evidence for one symbol in one snapshot, inbound (default) or outbound | Evidence-path payloads |
| `GET /api/entry-points?selector=…&side=…&depth=…&confidence=…` | Inbound-only entry points that can reach the selector, grouped by evidence category | Entry-point payload (each row carries a top-level `category`) |
| `GET /api/candidate-tests?selector=…&side=…&confidence=…` | Candidate tests for one changed symbol in one snapshot | Candidate-test payload |
| `GET /api/source?selector=…&side=…` | Bounded source excerpt for an evidence location | `{"file","span","bytes_or_text","truncated","snapshot"}` |
| `GET /api/search?q=…&side=…&kind=…&lang=…&limit=…` | Full-text search over one snapshot's symbols, strings, and config keys (milestone 4) | Selector-addressed matches, each labelled with its changed-symbol status |
| `GET /api/status` | Per-side cold-build progress for the launch scope (milestone 4) | `{"indexing":{"base":{...},"head":{...}}}` |
| `POST /api/cancel` | Abort an in-progress index build for the launch scope (milestone 4) | `{"cancelling":true,"indexing":{...}}` |
| `POST /api/report` | Export the current comparison as a complete report from an optional JSON body (`selection`, `excerpts`, `confidence`, `depth`, `include_absolute_paths`, `language`, `change_kind`, `scope`); a body may lower the launch scope's bounds, never raise them | Exported change-report payload |
| `GET /api/health` | Liveness and scope echo | `{"status","repository","base_sha","head_sha","mode"}` |

Rules:

- **Source is data, never markup.** Source text is transported as JSON string or
  byte values and inserted into the DOM as text content. The service never
  renders repository content into HTML, and the UI never assigns it to
  `innerHTML`. Syntax highlighting, if added, tokenizes text that is already in
  the DOM as text.
- A `side` parameter is required wherever a snapshot matters, and the response
  echoes the snapshot and its SHA. It defaults to `head` for the evidence,
  candidate-tests, and source routes; accepted values are `base` and `head`.
- `confidence` defaults to `same_module` and accepts `exact`,
  `import_resolved`, `same_module`, and `fuzzy_name` (the CLI aliases `import`
  and `fuzzy` are also accepted). Evidence and candidate-test payloads carry
  `schema_version`, `target`, `commit_sha`, `query_options`, and their result
  arrays. Evidence additionally carries `resolved`, `resolved_qualified`,
  `skipped_low_confidence`, `truncated`, `truncated_by`, and
  `no_path_reasons`; each path carries `path_id`, `from`, `to`, `truncated`,
  `truncated_by`, and `edges`, and each edge carries `from`, `from_selector`,
  `to`, `relationship`, `category`, `confidence`, `snapshot`, `commit_sha`,
  `source.file`, `source.line`, and optional `note`. Candidate tests additionally
  carry `unsupported_scope`; each candidate carries `test`, `source`, `label`,
  `category`, optional `path_id`, `changed_symbols`, `truncated`, and optional
  `note`.
- The changed-symbol payload has `schema_version`, `symbols`, and
  `out_of_scope`. Each symbol carries `status`, `pairing`, `pairing_evidence`,
  optional `base` and `head` symbol references, `supporting_snapshots`,
  `file_change`, optional `base_path` and `head_path`,
  `uncertain_candidates`, and optional `note`. A symbol reference carries
  `selector`, `snapshot`, and `commit_sha`; an uncertain candidate carries
  `symbol` and `reason`. Statuses are `added`, `removed`, `modified`,
  `signature_changed`, `moved`, `renamed`, and `uncertain`. File-change values
  are `added`, `deleted`, `modified`, `renamed`, `copied`, `type_changed`, and
  `other`. Out-of-scope entries carry `path`, `reason`, and `snapshot`.
- The comparison payload's scope envelope carries `schema_version`,
  `repository`, `mode`, `base` and `head` objects (`requested_ref` and
  `commit_sha`), `base_sha`, `head_sha`, `effective_base_sha`, `working_tree`,
  `indexing_status`, `indexing_error`, and `extractor_version`; snapshots add
  `side`, `requested_ref`, `commit_sha`, `files_indexed`, `files_written`,
  `extractor_version`, and `excluded` entries (`path` and `reason`). Health
  returns the launch identifiers plus `status: "ok"` and indexing fields.
  Coarse indexing statuses (`indexing_status`, on health, the scope envelope,
  and `GET /api/status`) are `indexing`, `ready`, `failed`, and `cancelled`
  (milestone 4).
- (Milestone 4) `/api/comparison` and `GET /api/status` both carry an
  `indexing` object with per-side progress:
  `{"base": {...}, "head": {...}}`, each side
  `{"state","files_seen","files_indexed","files_ignored",
  "unsupported_constructs","languages","started_at","elapsed_ms","error",
  "phase","phase_progress"}`.
  `state` is `pending`, `materializing`, `indexing`, `ready`, `failed`, or
  `cancelled`. `files_seen` and `files_indexed` cover extraction (`orbit_graph`
  pass 1) only: they are reported live while `phase` is `extracting` and
  increase monotonically, but they reach `files_seen` well before the `state`
  leaves `indexing`, because reference resolution (pass 2) and any later work
  run after extraction with no file-count progress of their own. `phase` is
  `null` while `state` is `pending`, `ready`, `failed`, or `cancelled`;
  otherwise it is `materializing`, `extracting`, or `resolving`, naming the
  sub-phase within `materializing`/`indexing`. `phase_progress` is
  `{"done","total"}` counting units of the current `phase` (files while
  `extracting`, resolved refs while `resolving`) and is `null` while `phase`
  has no counter yet, such as during `materializing`. `files_ignored` and
  `unsupported_constructs` come from materialization and are fixed once
  indexing starts; `languages` is a best-effort, extension-derived list built
  up as indexing progresses. `started_at` is milliseconds since the Unix
  epoch; `elapsed_ms` is live while a side is in progress and frozen at its
  value once the side reaches `ready`, `failed`, or `cancelled`. A query
  endpoint that needs a side that is not `ready` returns 409
  `side_not_ready` with the current `indexing` object in `error.details`
  rather than blocking.
- (Milestone 4) `POST /api/cancel` requests cancellation of an in-progress
  build; it is non-blocking; it returns 200 `{"cancelling":true,...}` when a
  build was in progress, or 409 `not_indexing` when neither side had a build
  running. `GET /api/status` reports the eventual transition to `cancelled`,
  and no cache entry is published for a side that ends `cancelled`
  regardless of the phase cancellation was requested in. The next `/api/*`
  request restarts the build for the launch scope.
- (Milestone 4) `GET /api/search` runs the core `Graph::search` FTS5 query
  against the requested side. Each match carries `kind` (`symbol`, `string`,
  or `config`), the canonical `selector` the other selector-addressed routes
  accept, `label`, `file`, `line`, and `changed` — the match's status in the
  current comparison's changed-symbol list (`added`, `removed`, `modified`,
  `signature_changed`, `moved`, `renamed`, `uncertain`, or `unchanged` when
  the selector is not in that list; a string or config match, which has no
  symbol identity, is addressed by `file:<path>` and is always `unchanged`).
  The response also carries `q` (the query, echoed as received), `snapshot`,
  `limit` (defaults to 20, the core default), `truncated`, and
  `truncated_by` (`"limit"` when the match count reaches the limit). A
  `limit` above 200 is refused as `unsupported_limit` rather than silently
  clamped — the same rule `depth` follows. Query text is always bound to
  FTS5 as data, never interpolated into SQL or a shell; a query that still
  fails at the FTS5 layer — an embedded NUL byte defeats SQLite's C-string
  binding, for example — is `invalid_query` (400), never a 500.
- `GET /api/source` returns `schema_version`, `scope`, `selector`, `snapshot`,
  `commit_sha`, `file`, `span.start`, `span.end`, `kind`, `name`, `qualified`,
  `encoding`, `bytes_or_text`, `truncated`, `truncated_by`, and
  `source_max_bytes`. `encoding` is `text` or `bytes`; a missing selector on
  the requested side returns 404 with error code `not_in_snapshot`.
- Errors use `{"schema_version":1,"error":{"code","message","details"}}`
  (milestone 4 adds `details`, a JSON object with whatever structured context
  is useful — empty when there is none), with a `scope` envelope on
  comparison-dependent errors. The service emits `unauthorized` (401),
  `host_mismatch`, `origin_mismatch`, or `repository_out_of_scope` (403),
  `invalid_side`, `invalid_confidence`, `invalid_direction`, `invalid_excerpts`,
  `missing_selector`, `unsupported_depth`, `evidence_failed`,
  `entry_points_failed`, `candidate_tests_failed`, `invalid_selector`,
  `invalid_request_body`, `invalid_query`, `invalid_kind`, `invalid_limit`, or
  `unsupported_limit` (400), `not_found` or `not_in_snapshot` (404),
  `method_not_allowed` (405), `index_unavailable`, `indexing_failed`,
  `changed_symbols_failed`, `entry_points_failed`, `candidate_tests_failed`,
  `evidence_failed`, `report_failed`, or `source_failed` (500),
  `side_not_ready` or `not_indexing` (409). The pre-milestone-4 `indexing`
  (503) code is retired: a side that is not ready now answers 409
  `side_not_ready` for every query endpoint, matching the `POST /api/cancel`
  refusal. See [API reference](#api-reference) for the full, cross-checked
  endpoint-by-endpoint listing.
- Responses are read-only with respect to the user's repository. No endpoint
  writes to the working tree, the Git index, or the object store.
- The service exits when its session ends, taking both snapshot trees with it.

## Amendments

These Milestone 2 amendments reconcile the original Milestone 1 contract with
the shipped service and serializers; they are recorded rather than silently
rewriting the original examples.

- **2026-09-12 — ORB-12373:** The service uses `tiny_http` 0.12 with default
  features disabled. It is a small blocking dependency suited to this
  short-lived loopback service: seven JSON routes need no async runtime,
  streaming, or middleware, and no TLS backend is needed.
- **2026-09-12 — ORB-12373:** Changed-symbol statuses expand the original
  example's `added|removed|modified|uncertain` to
  `added|removed|modified|signature_changed|moved|renamed|uncertain` so the
  pairing ladder is represented directly. One-sided entries use
  `pairing: same_selector`; Git rename or copy evidence is required for
  `moved` and `renamed`, otherwise candidates remain `uncertain`.
- **2026-09-12 — ORB-12373:** Milestone 2 adds the explicit service envelope,
  depth-1 query options, default `side=head`, confidence aliases, source
  encoding, host checks, and the error codes listed in Service surface.
  `POST /api/report` remains a 501 placeholder and `/` remains an embedded
  placeholder shell.
- **2026-09-12 — ORB-12373:** Snapshot caching is deferred. The service creates
  fresh per-comparison trees and indexes under each snapshot tree; the
  previously described repository cache is not shipped.
- **2026-09-13 — ORB-12413:** Each `entry_points[]` row carries a top-level
  `category`, the weakest evidence category on its `path` (mirroring
  `EvidencePath::category`), so the UI can tell a `resolved_call` entry point
  from one reached only through a `heuristic_match` hop without reaching into
  the path. The entry-points pane groups `heuristic_match` rows under their
  own "Heuristic / fallback matches" heading, matching the evidence and
  callees panes. `crate_root_public_item` no longer fires for a root-named
  file (`lib.rs`, `main.rs`, `__init__.py`, `index.js`/`.ts`) under a
  test-classified path: a fixture tree checked into the repository under
  analysis is not that repository's own crate root.
- **2026-09-13 — ORB-12394:** `POST /api/report` is implemented and no longer
  the Milestone 2 501 placeholder: it accepts an optional JSON body
  (`selection`, `excerpts`, `confidence`, `depth`, `include_absolute_paths`,
  `language`, `change_kind`, `scope`) that may only lower the launch scope's
  bounds, and returns the same exported change-report payload as the `report`
  CLI subcommand. `GET /api/entry-points` is added to the Service surface
  table (it shipped with ORB-12373 but was missing from the table). New error
  codes: `invalid_direction` (`/api/evidence`'s `direction` parameter),
  `invalid_excerpts` and `invalid_request_body` (`POST /api/report`), and
  `entry_points_failed` (`/api/entry-points`). Snapshot caching, described as
  deferred future work by the Milestone 2 amendment below, has since landed
  (`crates/orbit-graph-explorer/src/cache.rs`, the `--cache-dir`/`--no-cache`
  flags, and the `clean` subcommand); see Revision and snapshot semantics
  above for the corrected description. This handoff task adds the
  [API reference](#api-reference) appendix below, generated from and
  cross-checked against the service's real serializers and route table by
  `crates/orbit-graph-explorer/tests/api_reference_doc.rs`.

## UI sketch

Three panes, with a readable fallback that is mandatory rather than optional:

```text
┌──────────────────────────────┬───────────────────────────────────────────────┐
│ 1. CHANGE LIST               │ 2. FOCUSED RELATIONSHIP VIEW                  │
│ base 1111111 → head 2222222  │ symbol:src/lib.rs#entry:function  [head]      │
│ mode: direct base→head       │                                               │
│ ⚠ working tree dirty (2)     │ callers (potential impact, depth 3, ≤200)     │
│                              │  • api::handle_request   call  exact   head   │
│ ▸ removed  removed_helper    │  • cli::main             call  import  head   │
│     (base evidence)          │  ⚠ truncated at impact_node_cap = 200         │
│ ▸ modified entry             │                                               │
│ ▸ added    validate          │ candidate tests                               │
│ ▸ uncertain moved_helper(2)  │  • covers_entry   call_path     resolved_call │
│                              │  • lib_test.rs    naming_heur.  heuristic     │
│ out of scope: 1 file         │                                               │
│                              ├───────────────────────────────────────────────┤
│                              │ 3. SOURCE / DIFF EVIDENCE                     │
│                              │ tests/api.rs:42  [head 2222222]               │
│                              │   41 | let graph = open();                    │
│                              │   42 | entry();        ← call site            │
│                              │   43 | }                                      │
│                              │ excerpt bounded to 64 KiB                     │
└──────────────────────────────┴───────────────────────────────────────────────┘
```

Pane 1 — change list. One row per changed symbol, grouped by status (`removed`,
`modified`, `added`, `uncertain`). Each row shows the symbol name, its kind, the
file, and which snapshot supports it. An `uncertain` row shows the candidate
count and expands to the candidates. A fixed header always shows both short
SHAs, the comparison mode, and the dirty-working-tree notice when present. An
"out of scope" footer links to the unsupported/excluded list.

Pane 2 — focused relationship view. For the selected symbol: potential impact
(callers and neighbours) and candidate tests, each row carrying relationship
type, evidence category, confidence, and snapshot. Bounds in force are printed
above the list, and any truncation is marked in the list, not only in a tooltip.
Heuristic and fallback results are grouped under their own labelled heading.

Pane 3 — source/diff evidence. The source location for the selected edge, with
the snapshot SHA in the header and the line highlighted. For a `modified`
symbol, base and head excerpts are shown side by side, each labelled with its
SHA. Text only, with the byte bound stated.

Mandatory fallback. Every pane renders as a plain table or list of text rows
with the same columns and the same labels, usable without a graph rendering, and
that fallback is the source of truth for wording. A graph/diagram view may be
added, but it may never be the only way to read a path, a confidence, or a
truncation notice. Requirements a later UI task inherits: keyboard reachability
for every row, no colour-only encoding of confidence or category, complete
selectors available as copyable text, and no claim in a summary line that is not
present in a row.

## Public-API capability gaps

These gaps were found while building the milestone-1 scaffold and are now closed:

- **G1 — closed:** `Graph::open_with_db_path(source_root, db_path, policy)`
  separates the indexed source root from the caller-owned database path.
- **G2 — closed:** `Graph::open_with_revision(source_root, revision, policy)`
  applies the `detached-<short-sha>` database naming contract to synthetic trees.
- **G3 — closed:** `Graph::worktree_root()` and `Graph::db_path()` expose both
  paths represented by a graph handle.
- **G4 — closed:** `Graph::impact_with_direction` accepts `ImpactDirection`, and
  the CLI exposes `--direction inbound|outbound|both` with `both` as the default.
- **G5 — closed:** `ImpactEntry::origin` is a typed `ImpactOrigin::Symbol` or
  `ImpactOrigin::File`; `qualified_name` retains its prior compatible value.
- **G6 — closed:** `Graph::callees_with_options` accepts `CalleeOpts`, and
  `CalleeEdge::confidence` is a typed `RefConfidence`. `Graph::callees` keeps
  the historical unfiltered behavior.
- **G7 — closed:** `STORE_SCHEMA_VERSION` and `GraphDbPath::schema_version()`
  expose the store version, and the `version` command reports it.

## Milestone 1 scope

Landed with this document: the Cargo workspace conversion (the library plus the
explorer crate), the `crates/orbit-graph-explorer/src/snapshot.rs` module
described above, the
milestone-1 diagnostic binary (a human report explicitly labelled as not a
machine contract), and tests covering the removed-symbol case across both
snapshots, working-tree immutability, snapshot-index location, dirty-tree
detection, and the real executable.

At the Milestone 1 boundary, the HTTP service, UI, changed-symbol diff, report
export, and snapshot caching were deliberately absent. The service and
changed-symbol diff now belong to Milestone 2; the UI, report export, and
snapshot caching remain later work and must conform to the contract above.

## Milestone 2 scope

Landed after Milestone 1: the changed-symbol slice
(`crates/orbit-graph-explorer/src/changes.rs`), depth-1 inbound evidence and
candidate-test classification
(`crates/orbit-graph-explorer/src/evidence.rs`), and the authenticated loopback
JSON service (`crates/orbit-graph-explorer/src/service.rs`). The service
resolves direct base and head refs, materializes isolated snapshots, reports
dirty working-tree state,
and serves the routes and bounded payloads in Service surface. It does not add
a UI, multi-hop evidence, report export, or snapshot caching; those remain
later milestone work.

## Limitations

Status: milestone 5 handoff. This section is the shipped explorer's
product-level boundary — bound defaults, cache behavior, and stated
non-goals. It does not restate the resolver and language-coverage findings
from real-repository evaluation: those nine findings, ordered by how much
they distort an answer, live at
[`docs/evaluation/change-explorer/README.md#limitations-handoff`](../evaluation/change-explorer/README.md#limitations-handoff)
and are being actively revised by ORB-12422 against the resolver fixes in
ORB-12416/ORB-12417; read that document directly rather than a copy of it.

- **Extractor blind spots and language coverage.** See
  [the evaluation's limitations](../evaluation/change-explorer/README.md#limitations-handoff)
  for the current, evolving list — Rust cross-file resolution degrading to
  name-only matching, intra-file renames never paired, nested functions
  invisible to the graph, runtime/subprocess invocation unseen, and the
  languages actually exercised across the five studies (Rust, Python,
  JavaScript).
- **Bounds default to the values in [Bounds and truncation](#bounds-and-truncation)
  above**, every one surfaced in its response payload rather than silently
  applied: impact/entry-point/evidence depth 3 (`DEFAULT_IMPACT_DEPTH`,
  `EVIDENCE_DEPTH`, capped at `MAX_EVIDENCE_DEPTH` 10), traversal node cap 200
  (`IMPACT_NODE_CAP`/`TRACE_NODE_CAP`), trace depth 5 (`DEFAULT_TRACE_DEPTH`),
  search result limit 20 (`DEFAULT_SEARCH_LIMIT`, refused rather than clamped
  above `MAX_SEARCH_LIMIT` 200), source excerpt 64 KiB
  (`DEFAULT_SHOW_MAX_BYTES`), dirty-entry listing 200 entries
  (`DIRTY_ENTRY_CAP`), and materialized blob size 4 MiB
  (`DEFAULT_MAX_BLOB_BYTES`). Raising bounds changed the answer for a large
  fraction of queries in every evaluation study; a bounded view is a
  presentation choice, never a completeness claim.
- **Cache behavior and its edges.** The snapshot cache (see Revision and
  snapshot semantics above) is per-repository, per-launch-scope disk state
  under `<repo>/.orbit-graph/explorer/snapshots`: it is never shared across
  separate clones or worktrees of the same repository, it accumulates one
  entry per distinct base/head SHA ever compared until `clean` is run
  manually, and it has no automatic size cap or eviction policy. Correctness
  relies entirely on its `(commit SHA, EXTRACTOR_VERSION,
  STORE_SCHEMA_VERSION)` key, which cannot mismatch a commit's real content
  because the key includes the content-addressed SHA itself; it can, and by
  design does, go stale across an `orbit-graph-explorer` upgrade that bumps
  either version, at which point the entry is rebuilt rather than reused.
- **No merge-base mode.** Only `direct_base_head` comparison (Decision D3) is
  shipped. A `merge_base` mode that rewrites the user's stated base to the
  computed merge base remains unimplemented; the two modes would never be
  silently swapped if it landed, and `mode` stays a required field of every
  comparison payload today for exactly that reason.
- **No risk scoring.** The explorer reports potential impact and a typed
  evidence category (see Evidence categories above) for every edge and
  candidate test. It never assigns a severity, confidence score, or
  merge/no-merge judgment to a change; static reachability is potential
  impact, not proof of execution, coverage, or safety.
- **No PR or GitHub integration.** `--base`/`--head` name Git refs the caller
  resolves and supplies directly. The explorer never fetches pull-request
  metadata, review state, CI status, or anything else from GitHub or another
  forge, and it never authenticates to one.
- **Single-repository scope.** One `--repo` per launch, fixed at launch and
  never inferred from a request. Comparing a change across two repositories
  (for example, a vendored copy against its upstream) is out of scope, and
  Git submodules are excluded from every snapshot outright (`submodule` in
  the materialization exclusion table above) rather than fetched and indexed.
- **Loopback-only service.** `serve` binds `127.0.0.1` only, by design
  (Decision D2). There is no remote or team-shared deployment mode, no
  multi-user access, no authentication beyond the per-launch bearer token,
  and no persistence of a comparison across launches beyond the snapshot
  cache above. The evaluation recorded no browser-based UI timing because no
  headless browser was available on its host; that gap is tracked in the
  evaluation document, not here.

## API reference

Status: milestone 5 handoff appendix. Every row is verified against the
current service (`crates/orbit-graph-explorer/src/service.rs`) rather than
against the narrative sections above; `crates/orbit-graph-explorer/tests/api_reference_doc.rs`
launches the real built service, exercises each route, and fails the build if
a route path or an `error.code` string the service actually returns is
missing from this document's text. Query parameters are optional unless
marked **required**. `side` and `confidence` defaults, and every bound, are
shared with Service surface above; this table exists to enumerate them per
endpoint in one place.

### Endpoints

| Method and path | Parameters | Response (top-level fields) | Error codes it can return | Bounds |
| --- | --- | --- | --- | --- |
| `GET /` | none | Embedded UI shell (HTML) | `method_not_allowed`, `unauthorized`\* | none |
| `GET /api/health` | none | `status`, `repository`, `base_sha`, `head_sha`, `mode`, plus scope/indexing fields | `unauthorized`, `host_mismatch`, `origin_mismatch`, `repository_out_of_scope` | none |
| `GET /api/comparison` | none | Resolved comparison payload (scope envelope, `snapshots[]`, `indexing`) | `unauthorized`, `host_mismatch`, `origin_mismatch`, `repository_out_of_scope`, `index_unavailable`, `indexing_failed` | none |
| `GET /api/changed-symbols` | none | `schema_version`, `symbols[]`, `out_of_scope[]` | above, plus `changed_symbols_failed`, `side_not_ready` | none |
| `GET /api/evidence` | `selector` **required**, `side` (default `head`), `depth` (default `EVIDENCE_DEPTH`, ≤ `MAX_EVIDENCE_DEPTH`), `confidence` (default `same_module`), `direction` (`inbound` default \| `outbound`), `language`, `change_kind`, `scope` | `schema_version`, `target`, `commit_sha`, `query_options`, `resolved`, `resolved_qualified`, `skipped_low_confidence`, `truncated`, `truncated_by`, `no_path_reasons`, evidence-path array | `missing_selector`, `invalid_side`, `unsupported_depth`, `invalid_confidence`, `invalid_direction`, `not_in_snapshot`, `evidence_failed`, `side_not_ready` | `depth` ≤ `MAX_EVIDENCE_DEPTH`; node cap and time budget from launch scope |
| `GET /api/entry-points` | `selector` **required**, `side` (default `head`), `depth`, `confidence`, `language`, `change_kind`, `scope` (inbound only; no `direction`) | Entry-point payload; each row carries a top-level `category` (the weakest evidence category on its path) | `missing_selector`, `invalid_side`, `unsupported_depth`, `invalid_confidence`, `not_in_snapshot`, `entry_points_failed`, `side_not_ready` | same as `/api/evidence` |
| `GET /api/candidate-tests` | `selector` **required**, `side` (default `head`), `confidence`, `depth`, `language`, `change_kind`, `scope` | `schema_version`, `candidates[]`, `unsupported_scope[]` | `missing_selector`, `invalid_side`, `invalid_confidence`, `candidate_tests_failed`, `side_not_ready` | same as `/api/evidence` |
| `GET /api/source` | `selector` **required**, `side` (default `head`) | `schema_version`, `scope`, `selector`, `snapshot`, `commit_sha`, `file`, `span.start`, `span.end`, `kind`, `name`, `qualified`, `encoding`, `bytes_or_text`, `truncated`, `truncated_by`, `source_max_bytes` | `missing_selector`, `invalid_side`, `invalid_selector`, `not_in_snapshot`, `source_failed` | `source_max_bytes` = `DEFAULT_SHOW_MAX_BYTES` |
| `GET /api/search` | `q`, `side` (default `head`), `kind` (`symbol`\|`string`\|`config`), `lang`, `limit` (default `DEFAULT_SEARCH_LIMIT`, ≤ `MAX_SEARCH_LIMIT`) | `schema_version`, `scope`, `snapshot`, `q`, `limit`, `truncated`, `truncated_by`, `matches[]` | `invalid_kind`, `invalid_limit`, `unsupported_limit`, `invalid_query` | `limit` ≤ `MAX_SEARCH_LIMIT` (200), refused above rather than clamped |
| `GET /api/status` | none | `{"indexing":{"base":{...},"head":{...}}}` | `unauthorized`, `host_mismatch`, `origin_mismatch`, `repository_out_of_scope` | none |
| `POST /api/cancel` | none (no body) | `{"cancelling":true,"indexing":{...}}` | `not_indexing` | none |
| `POST /api/report` | optional JSON body: `selection[]`, `excerpts` (`none`\|`controlled`\|`full-span`), `confidence`, `depth`, `include_absolute_paths`, `language`, `change_kind`, `scope` | Exported change-report payload (see Exported change report) | `invalid_request_body`, `invalid_excerpts`, `invalid_confidence`, `unsupported_depth`, `report_failed`, `side_not_ready` | a body may only lower the launch scope's `depth`/node-cap/time-budget bounds, never raise them |

\* `/` and the embedded `/ui/app.css` and `/ui/app.js` assets are the only
routes served without the per-launch bearer token; every other route requires
it and answers `unauthorized` without one.

### Error codes

| Code | Status | Returned by |
| --- | --- | --- |
| `unauthorized` | 401 | every route except the embedded UI assets |
| `host_mismatch` | 403 | every route, when `Host` does not match the service's own origin |
| `origin_mismatch` | 403 | every route, when `Origin`/`Referer` does not match the service's own origin |
| `repository_out_of_scope` | 403 | any route whose repository parameter is not the launch-scoped repository |
| `not_found` | 404 | any unrecognized path |
| `not_in_snapshot` | 404 | `/api/evidence`, `/api/entry-points`, `/api/source` |
| `method_not_allowed` | 405 | any recognized path called with the wrong HTTP method |
| `missing_selector` | 400 | `/api/evidence`, `/api/entry-points`, `/api/candidate-tests`, `/api/source` |
| `invalid_side` | 400 | every route with a `side` parameter |
| `invalid_confidence` | 400 | `/api/evidence`, `/api/entry-points`, `/api/candidate-tests`, `POST /api/report` |
| `invalid_direction` | 400 | `/api/evidence` |
| `invalid_excerpts` | 400 | `POST /api/report` |
| `invalid_request_body` | 400 | `POST /api/report` (malformed JSON) |
| `unsupported_depth` | 400 | `/api/evidence`, `/api/entry-points`, `POST /api/report` |
| `invalid_selector` | 400 | `/api/source` |
| `invalid_query` | 400 | `/api/search` |
| `invalid_kind` | 400 | `/api/search` |
| `invalid_limit` | 400 | `/api/search` |
| `unsupported_limit` | 400 | `/api/search` (limit above `MAX_SEARCH_LIMIT`) |
| `evidence_failed` | 400/500 | `/api/evidence` |
| `entry_points_failed` | 400/500 | `/api/entry-points` |
| `candidate_tests_failed` | 400/500 | `/api/candidate-tests` |
| `changed_symbols_failed` | 500 | `/api/changed-symbols` |
| `source_failed` | 500 | `/api/source` |
| `report_failed` | 500 | `POST /api/report` |
| `index_unavailable` | 500 | any comparison-dependent route, before indexing has started |
| `indexing_failed` | 500 | any comparison-dependent route, once a side's cold build has failed |
| `side_not_ready` | 409 | any comparison-dependent route, while the requested side is still indexing |
| `not_indexing` | 409 | `POST /api/cancel`, when neither side has a build in progress |

### Core library API used by the explorer

The explorer's public dependency on `orbit_graph` is the surface named in
[Verified API inventory](#verified-api-inventory) plus the constructors and
sync-progress types the service and CLI use directly:
`Graph::open_with_db_path`, `Graph::open_with_revision`,
`Graph::impact_with_direction`, `CalleeOpts`, `Graph::sync_with_observer` (with
`SyncObserver`, `SyncProgress`, `SyncPhase`, `SyncOutcome`), and
`STORE_SCHEMA_VERSION`. Each carries a compiling rustdoc example exercised by
`cargo test --doc -p orbit-graph`.
