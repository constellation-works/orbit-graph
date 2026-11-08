---
title: "Scope: graph store, sync bounds and state containment"
owner: maintainers
last_updated: 2026-11-08
status: Accepted
stage: shipped
next: "—"
feature: graph-store
doc_role: scope
summary: "Scope: version-aware SQLite graph index lifecycle, bounded sync, and owner-only state containment."
---

# Scope: graph store, sync bounds and state containment

Bearing: `orbit-graph` builds a local SQLite index (`.orbit-graph/`) of a
source tree. Read commands and `read_only` plugin tools never create,
initialise or delete index state; only `sync` builds and only `sync` and
`clean` remove. Sync finishes or fails within stated bounds, and repository
content can neither redirect nor pre-populate index, history, plugin or
snapshot-cache state.

## Problem

Per-task decision folders (`graph-store/`, `sync-bounds/`,
`state-containment/`) each recorded one task's choices about the same index:
how it is opened, how long sync may take, and where its state may live. Read
together they are one feature — the store — so they are folded here under
one entry doc (STD-06@1 §R1–§R2, scope track).

## Goal

One folder answers how the index is built, opened, bounded and contained.
The decisions in [`4_decisions.md`](4_decisions.md) are unchanged in substance;
each keeps its source task's D-number with a source prefix.

## Shape

- **Lifecycle.** Only `sync` builds; only `sync` and `clean` remove;
  a missing index is reported as `index_missing` naming the building command.
- **Bounds.** Git ignore matching in process, one bounded lock helper,
  chunked extraction, a per-file parse deadline, and a counted watcher
  overflow that rescans.
- **Containment.** One module (`crates/orbit-graph/src/state_dir.rs`)
  decides every state path; state is created owner-only (`0700`/`0600`) and
  every durable replacement goes through `atomic_write`.

## Non-goals

- **Changing the store schema or extractor version.** Identity is validated,
  not migrated; see `4_decisions.md`.
- **User-facing command behaviour.** That is the [terminal interface](../terminal-interface/1_overview.md).

## Task References

- ORB-13156 — made a graph sync finish or fail within stated bounds
- ORB-13158 — made read commands and `read_only` plugin tools never create, initialise or delete index state
- ORB-13162 — contained index, history, plugin and snapshot-cache state against repository redirection

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
