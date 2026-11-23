# Code navigation

Eight read-only tools answer precise structural questions from the plugin's
code-graph index. Prefer them over grep when you need exact callers, callees,
or blast radius:

| Question | Tool | Key input |
|---|---|---|
| Where is a symbol, string, or config key? | `orbit.graph.search` | `query`, optional `kind`, `lang` |
| What does this symbol's source say? | `orbit.graph.show` | `selector`, `max_bytes` |
| Who calls or uses this symbol? | `orbit.graph.refs` | `selector`, `confidence`, `kind` |
| What does this function call? | `orbit.graph.callees` | `selector`, `include_unresolved` |
| What breaks if I change this? | `orbit.graph.impact` | `selector`, `direction`, `depth` |
| What runs under this CLI command? | `orbit.graph.trace` | `command`, `depth` |
| What does this file or directory import? | `orbit.graph.deps` | `file:` or `dir:` `selector` |
| What is in this repository or directory? | `orbit.graph.overview` | optional `selector`, `format` |

Start with `search` or `overview` to find selectors, then use them in the
other tools. Selectors look like `symbol:src/lib.rs#parse:function`,
`file:src/lib.rs`, or `dir:src`. The default `confidence` is `same_module`.
When `refs` finds nothing at that level, it returns name-only matches under
`fallback`. Pass `confidence: fuzzy_name` to include callers that go
through re-exports.

Each response carries `index`, which names the revision it describes. When
`index.fresh` is false, the results describe older code: run the call in
`index.stale.fix` (`graph_sync`) and retry if the difference matters. A tool
fails with `index_missing` until `graph_sync` has run once, and with
`index_incompatible` when the index needs a full rebuild (`full: true`).

Every array, nested ones included, is capped by `limit` (default 50, search
20) and a 256 KiB ceiling. `truncated` and `truncation` report every cut by
field path (for example `files[].symbols`), so narrow the query instead of
raising limits.

`callees` omits unresolved calls with no indexed definition
(standard-library calls such as `map_err`) and counts them in
`hidden_unresolved`; pass `include_unresolved: true` to see them.

