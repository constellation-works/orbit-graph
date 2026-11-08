---
title: "Scope: failure classes"
owner: maintainers
last_updated: 2026-11-08
status: Accepted
stage: shipped
next: "—"
feature: failure-classes
doc_role: scope
summary: "Scope: failures keep a typed class from where they happen to the CLI code and the plugin envelope code and retryable flag."
---

# Scope: failure classes

Bearing: `GraphError` is public and `#[non_exhaustive]`, so callers outside
the library could only match it with a wildcard. One exhaustive class beside
it lets the CLI translate every failure once, and the same translation feeds
the plugin envelope's `code` and `retryable`.

## Problem

Failure codes were decided at each surface separately, so a failure could
change meaning between the library, the CLI and the plugin. This folder was
one task's decision folder (ORB-13167); it is kept as a scope-track feature
because the class taxonomy is a standing contract every surface consumes,
not a slice of the store, the plugin surface or the terminal rendering.

## Goal

One folder answers which class a failure carries and which code each
surface reports for it. The decisions in
[`4_decisions.md`](4_decisions.md) are unchanged in substance.

## Shape

- **One translator.** `GraphError::class()` returns the exhaustive
  `GraphErrorClass`; `report_graph_error` in the CLI matches it exhaustively
  and returns CLI code, plugin code and `retryable` together.
- **Stable codes.** `retryable` is true only for `Timeout` and transient
  I/O; existing codes keep their meaning and only new ones are added.
- **Supervision.** The Orbit adapter supervises children as process groups
  and observes exit without reaping prematurely.

## Non-goals

- **Rendering errors for humans.** Streams and exits are the
  [terminal interface](../terminal-interface/1_overview.md).
- **Plugin request validation.** Refusals before dispatch are the
  [plugin surface](../plugin-surface/1_scope.md).

## Task References

- ORB-13167 — kept a typed failure class from origin to CLI and plugin codes

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
