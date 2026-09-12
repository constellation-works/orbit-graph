# Change-explorer fixture corpus

Each subdirectory of `tests/fixtures/change-explorer/` is one **case**: a small
source tree at a `base` state and a `head` state, plus an `expected.json`
manifest that states exactly what the `orbit-graph` queries (`refs`, `impact`,
`deps`, `show`, `search`) must and must not report once each state is indexed.

`tests/change_explorer_fixtures.rs` builds each case into a real, temporary Git
repository, indexes each named snapshot with the built `orbit-graph` binary,
and checks every claim in `expected.json` against the real query output. No
`.git` directory is committed under `tests/fixtures/`; the harness creates one
per test run with fixed author/committer identity and timestamps so that base
and head commit SHAs are reproducible across runs (verified by building each
case twice and comparing SHAs).

## Directory layout

```
tests/fixtures/change-explorer/<case-id>/
  base/            # source tree at the base snapshot
  head/             # source tree at the head snapshot
  root/            # (branch-divergence only) source tree at the common ancestor
  expected.json    # manifest described below
```

## `expected.json` schema

Top-level fields:

| Field | Type | Meaning |
|---|---|---|
| `case_id` | string | Matches the directory name. |
| `languages` | array of string | Ecosystems exercised (`"rust"`, `"python"`, ...). |
| `description` | string | What the case demonstrates and why. |
| `topology` | `"linear"` \| `"branch_divergence"` | How the harness builds the Git history (see below). |
| `branch_topology` | object (branch_divergence only) | `{root_branch, base_branch, head_branch}` branch names used to build the divergent history. |
| `changed_symbols` | array | Symbols whose presence changes across snapshots (see below). |
| `unchanged_symbols` | array | Symbols asserted stable across every listed snapshot; used to prove a change did *not* leak into unrelated evidence. |
| `expected_references` | array | Reference/relation evidence the `refs` query must return. |
| `expected_impact` | array | Blast-radius evidence the `impact` query must return. |
| `candidate_tests` | array | Tests that would plausibly cover a changed symbol, with the evidence category behind that judgement. |
| `known_gaps` | array | Extractor/query limitations this case deliberately exposes, with real captured output. Always present, may be empty. |
| `direct_diff_note` / `merge_base_diff_note` | string (branch_divergence only) | Free-text explanation of how a naive base→head diff and a merge-base-relative diff disagree for this case. |

### Topology

* `linear` — the harness commits the `base/` tree, then rewrites the working
  tree to match `head/` and commits again on the same branch. `changed_symbols`
  and friends use the snapshot names `"base"` and `"head"`.
* `branch_divergence` — the harness commits `root/` on `branch_topology.root_branch`,
  then branches twice from that tip: one branch commits `base/` (snapshot
  `"base"`), the other commits `head/` (snapshot `"head"`). The test asserts
  `git merge-base base head` equals the root commit, i.e. the merge-base
  genuinely differs from `base`. Snapshot names are `"root"`, `"base"`, `"head"`.

### `changed_symbols[]`

```json
{
  "selector": "symbol:<path>#<name>:<kind>",
  "change": "added" | "removed" | "modified" | "renamed" | "uncertain",
  "present_in": ["<snapshot>", ...],
  "absent_in": ["<snapshot>", ...],
  "note": "optional free text"
}
```

For every snapshot in `present_in`, the harness asserts `orbit-graph show
<selector>` resolves to a non-null document in that snapshot's index. For
every snapshot in `absent_in`, it asserts `show` resolves to `null`. This is
also how `removed` symbols are proven absent from `head` and `added` symbols
are proven absent from `base`.

### `unchanged_symbols[]`

```json
{ "selector": "symbol:...", "present_in": ["base", "head"], "note": "..." }
```

Same presence check as `changed_symbols`, but the point of the entry is that
the symbol is stable everywhere listed — used to prove a change did not
spuriously affect unrelated evidence (e.g. a same-file symbol that must not
move when a neighboring test file changes).

### `expected_references[]`

```json
{
  "target": "symbol:<path>#<name>:<kind>",
  "snapshot": "<snapshot>",
  "confidence": "exact" | "import_resolved" | "same_module" | "fuzzy_name",
  "kind": "call" | "type" | "use" | "trait_bound" | "impl" | "extends" | "implements",
  "file": "<path of the referencing site>",
  "line": <1-based line of the referencing site>
}
```

Verified by running `orbit-graph refs <target> --confidence <confidence>
--kind <kind>` against the named snapshot and requiring a `refs[]` (or
`relations[]`) entry at exactly that `file`/`line` whose own `confidence`
field equals the manifest value. `confidence` is deliberately the *exact*
value the query reports, not just a floor, because the floor argument passed
to `--confidence` is set to the same value: the strictest floor that still
admits the expected entry.

### `expected_impact[]`

```json
{
  "origin": "symbol:<path>#<name>:<kind>",
  "snapshot": "<snapshot>",
  "confidence": "exact" | "import_resolved" | "same_module" | "fuzzy_name",
  "qualified_name": "<qualified name reached by traversal>",
  "edge_kind": "call" | "type" | "use" | "trait_bound" | "impl" | "extends" | "implements",
  "distance": <breadth-first distance from origin>
}
```

Verified by running `orbit-graph impact <origin> --confidence <confidence>`
against the named snapshot and requiring a `touched[]` entry with that exact
`qualified_name`, `edge_kind`, and `distance`.

### `candidate_tests[]`

A candidate test is a test symbol that plausibly exercises a changed symbol.
`category` says what kind of evidence backs that judgement:

* `"call-path"` — the test statically calls the changed symbol. Verified
  identically to `expected_references` (same `confidence`/`kind`/`file`/`line`
  fields), using `test_selector` as the file/line source of the call and
  `target` as the query subject.
* `"import"` — the test imports the changed symbol's module but the call
  itself is not being asserted. Verified with `orbit-graph deps
  file:<test file>` requiring an `imports[]` entry whose `target_path` equals
  `import_target_path`.
* `"naming-heuristic"` — the test's name or location suggests coverage, but no
  static call or import edge is extractable (dynamic dispatch, reflection,
  macro-wrapped calls, ...). No query is run for these; `rationale` explains
  the naming signal, and the corresponding blind spot is always cross-referenced
  from a `known_gaps` entry with real observed output.

### `known_gaps[]`

```json
{
  "description": "what the extractor/query layer cannot currently see, in plain language",
  "snapshot": "<snapshot the observation was taken from>",
  "check": { "type": "...", ... },
  "observed_command": ["<argv...>"],
  "observed_output": <verbatim JSON captured from a real run of observed_command>
}
```

`check` is optional documentation-only metadata unless it names one of the
following types, in which case the harness re-verifies the gap still holds
(so a gap can never silently stop being true without the test noticing):

* `"absent_reference"` — `{target, file}`: asserts the loosest-floor `refs`
  query for `target` in `snapshot` contains no entry at `file`.
* `"identical_ambiguous_refs"` — `{target_a, target_b, confidence}`: asserts
  `refs target_a --confidence <confidence>` and `refs target_b --confidence
  <confidence>` return the same `refs[]` array in `snapshot`, i.e. the
  resolver cannot distinguish the two same-named targets.
* `"unindexed_file"` — `{path, search_query}`: asserts `show file:<path>`
  resolves to `null` in `snapshot`, and that `search <search_query>` returns
  no match whose `path` equals `path`.

A known gap is never used to weaken an assertion elsewhere in the same
manifest: the case still asserts every edge the tooling *does* produce, and
separately proves (with real command output) which edge it cannot produce.

## Boundaries

These fixtures exercise only the existing `sync`/`refs`/`impact`/`deps`/`show`/
`search` queries through the real `orbit-graph` binary. They do not implement
or assume a future "diff" or "candidate test recommendation" command — the
`branch-divergence` case's two diff interpretations and every case's
`candidate_tests` category are proven with today's per-snapshot queries plus
a small amount of test-harness bookkeeping (line numbers, branch topology),
not with new product code.
