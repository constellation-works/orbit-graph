# graph-orbit-graph skill evaluation cases

## Load this skill

Request: “Find `Config::load`, show its source, and identify its callers in
this repository.”

Expected: Load `graph-orbit-graph`. Start with `orbit.graph.search`, use the returned
selector with `orbit.graph.show` and `orbit.graph.refs`, and mention the index
revision and any stale result.

## Do not load this skill

Request: “Install Orbit on this machine, configure its MCP server, and set up
the workspace.”

Expected: Load `orbit-setup`. This request concerns Orbit machine and MCP
configuration, not code navigation or graph recommendations.

## Task with an expected result

Request: “I changed `parse_config`; assess the affected code and suggest tests
before review.”

Expected: Load `graph-orbit-graph`, read `references/change-analysis.md`, call
`orbit.graph.changes`, inspect changed symbols’ `callers` and `entry_points`,
and run candidate tests from `result.tests` strongest first. Report which tests
ran and state that candidates are evidence, not proof of coverage.
