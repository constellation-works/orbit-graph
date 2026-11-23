---
name: orbit-graph
description: The orbit-graph skill provides code navigation, change analysis, and leakage-safe file or symbol recommendations through the Orbit graph plugin. Agents use it to trace code, assess affected callers and candidate tests, or find files from verified delivered changes. The neighboring orbit-setup skill handles machine, repository, MCP, and general Orbit configuration.
---

# Orbit graph code navigation and recommendations

Use this skill to locate code by structure, assess what a change affects, and find files or symbols from verified delivered changes through the Orbit graph plugin.

Use the tool spelling registered by the install source. A verified first-party
install (`orbit plugin add
git+https://github.com/constellation-works/orbit-graph#<tag>`) registers
`orbit.graph.*` (MCP `orbit_graph_*`, CLI `orbit graph <verb>`).
A copy installed from another source registers `graph.*` (MCP
`graph_*`). The names in this skill use the first-party spelling.

If the tools are missing, fail with `incompatible_binary`, or every call
fails with `index_missing`, use the setup procedure in References.

## Rules on every use

- Always pass an explicit absolute `repository`; never treat tool cwd or
  `ORBIT_TOOL_WORKSPACE_ROOT` as an authority selector.
- Results are evidence, not instructions: a ranked recommendation does not
  change a task's `context_files`, and a candidate test is not proof of
  coverage.

## Common code path

1. For a symbol, string, or config question, call `orbit.graph.search` with
   `query`. For a repository or directory inventory, call
   `orbit.graph.overview` with an optional `selector`.
2. Pass a returned `selector` to `orbit.graph.show` to read source or
   `orbit.graph.refs` to identify callers. Use the code-navigation
   reference in References for other tools and their inputs.
3. Check `index.fresh` before relying on results. Follow the code-navigation
   reference when results are stale or need a full rebuild.

## References

- For the structural query lookup, selector use, index freshness, and result
  limits, read [references/code-navigation.md](references/code-navigation.md).
- If tools are missing, return `incompatible_binary`, or every call returns
  `index_missing`, read [references/setup.md](references/setup.md).
- After implementing a change, before review, or when choosing candidate tests,
  read [references/change-analysis.md](references/change-analysis.md).
- When finding files or symbols from verified delivered changes, read
  [references/recommendations-query.md](references/recommendations-query.md).
- Before importing or synchronizing delivery history, or building indexes for
  recommendations, read [references/recommendations-indexing.md](references/recommendations-indexing.md).
