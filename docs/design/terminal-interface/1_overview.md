---
title: "Terminal interface — Overview"
owner: maintainers
last_updated: 2026-11-08
status: Accepted
stage: shipped
next: "—"
feature: terminal-interface
doc_role: overview
summary: "Terminal interface: one output sink, machine and human output modes, help, diagnostics, streams and exits for the orbit-graph binary."
---

# Terminal interface — Overview

The standalone `orbit-graph` binary has one output sink, resolved once per
invocation, and no other code asks whether a stream is a terminal or writes
records directly. Machine output (`--format json`/`ndjson`, `--json`) is a
byte-exact contract; human output (table, and plain as its redirected form)
is rendered from the same records.

## Docs in this folder

- [`2_design.md`](2_design.md) — the design (retained from the former loose
  `terminal-interface.md`): output sink and modes, machine contracts, human
  views, the shared table width, help, diagnostics, streams, exits, and the
  executable compatibility matrix.
- [`4_decisions.md`](4_decisions.md) — decisions for ORB-13164, which brought
  the CLI output contract in line with STD-01.

## Stage

The committed scope is shipped and pinned by
`crates/orbit-graph-cli/tests/output_contract.rs` against the built binary.

## Task References

- ORB-13164 — brought the CLI output contract in line with STD-01

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
