# Change analysis

`orbit.graph.changes` answers "what else could this break?" and "which tests
should I run?" for a whole change in one call. Call it:

- **after implementing**, before you call the work done: read each changed
  symbol's `callers` and `entry_points` and check the ones your edit could
  break;
- **before review**, so the review starts from the affected surface and its
  evidence rather than from the diff alone;
- **to pick tests**: run the entries of `result.tests`, strongest first, and
  say which you ran. A candidate is what the evidence points at, not proof of
  coverage.

With no `base`/`head` it compares the working tree, untracked files included,
against the merge base with the upstream (else `origin/HEAD`, `main`,
`master`); `base` alone compares the working tree against it; `base` and
`head` compare two revisions. It indexes both sides itself, so it needs no
`graph_sync`, but a cold call can take tens of seconds on a large repository;
repeat calls over the same commits reuse cached snapshots.

`changes` needs writable plugin scratch/cache state, even though it does not
change repository files or Git state. This is the documented cache exception
to read-only execution; it is not a promise that a cold call works with plugin
state made read-only.

Trust labels, not list position. Every caller, entry point and test carries a
`source` (`call_path`, `import_relationship`, `reference_path`,
`changed_symbol`, `naming_heuristic`, `runtime_invocation`) and a
`confidence`.

`fuzzy_name`, `name_only` and `file_import` are name or file
matches, often to an unrelated symbol that shares the name; confirm them in
the source before acting on them. When `complete` is false, the budget ran out:
`incomplete` says where, and `not_analysed` lists what was skipped. Every cut
is in `truncation`: narrow with `symbols` (selectors of changed symbols),
`scope` or `language` before raising `max_*`.
