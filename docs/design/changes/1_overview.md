---
title: "Changes — Overview"
owner: maintainers
last_updated: 2026-11-08
status: Accepted
stage: shipped
next: "—"
feature: changes
doc_role: overview
summary: "Change analysis: snapshot comparison, changed symbols, evidence paths, candidate tests, recommendations, and the changes command surface."
---

# Changes — Overview

Change analysis explains one Git change — a base and a head revision — through
source relationships: what changed, which callers and entry points may be
affected, and which tests have a defensible connection. It spans the
`orbit-graph-changes` library, the `orbit-graph changes` command, the
`orbit.graph.changes` plugin tool, and the history/recommendation foundation
that learns from delivered changes.

## Docs in this folder

- [`2_design.md`](2_design.md) — the evidence contract and snapshot semantics
  (retained from the former `change-explorer.md`): snapshot isolation,
  changed-symbol identity, evidence categories, bounds and payload contracts.
- [`specs/change-recommendations.md`](specs/change-recommendations.md) — the
  change-recommendation history foundation: the version-2 import contract,
  extraction and identity rules, storage, and the later recommendation stages.
- [`4_decisions.md`](4_decisions.md) — decisions for the `changes` command
  and plugin tool (ORB-13256), folded from the former `changes-command/`
  per-task folder.

## Stage

The committed scope is shipped: the snapshot/evidence contracts are retained
in `orbit-graph-changes` (ORB-13255), and the CLI/plugin surface shipped in
ORB-13256. What remains is vision and follow-up work recorded in
`4_decisions.md` (D11), not committed scope.

## Task References

- ORB-13255 — retained the snapshot, changed-symbol, evidence and JSON report contracts in `orbit-graph-changes`
- ORB-13256 — exposed the analysis as `orbit-graph changes` and the `orbit.graph.changes` plugin tool

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
