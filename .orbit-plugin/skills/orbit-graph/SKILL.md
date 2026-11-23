---
name: graph-orbit-graph
description: The graph-orbit-graph skill provides code navigation (search, show, refs, callees, impact, trace, deps, overview), change analysis (changes), and leakage-safe file or symbol recommendations from verified delivered changes through the Orbit graph plugin. Agents use it to trace code, assess affected callers and candidate tests, or find files from delivered changes. The neighboring orbit-setup skill handles machine, MCP, and general Orbit configuration.
---

# Orbit graph code navigation and recommendations

Use this skill to locate code by structure, assess what a change affects, and find files or symbols from verified delivered changes through the Orbit graph plugin.

Tool names: a verified first-party install (`orbit plugin add
git+https://github.com/constellation-works/orbit-graph#<tag>`) registers
`orbit.graph.*` (MCP `orbit_graph_*`, CLI `orbit graph <verb>`).
A copy installed from any other source registers bare `graph.*` (MCP
`graph_*`) instead; use whichever spelling your tool list shows. The names in
this skill use the first-party spelling.

If the tools are missing, fail with `incompatible_binary`, or every call
fails with `index_missing`, use the setup procedure in References first.

## Rules on every use

- Always pass an explicit absolute `repository`; never treat tool cwd or
  `ORBIT_TOOL_WORKSPACE_ROOT` as an authority selector.
- Results are ranked evidence, not an instruction to mutate a task's
  `context_files`; a candidate test is what the evidence points at, not proof
  of coverage.

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
  `index_missing`, read [references/setup.md](references/setup.md) first.
- After implementing a change, before review, or when choosing candidate tests,
  read [references/change-analysis.md](references/change-analysis.md).
- When finding files or symbols from verified delivered changes, read
  [references/recommendations-query.md](references/recommendations-query.md).
- Before importing or synchronizing delivery history, or building indexes for
  recommendations, read [references/recommendations-indexing.md](references/recommendations-indexing.md).
