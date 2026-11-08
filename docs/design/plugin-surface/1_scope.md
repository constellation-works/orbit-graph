---
title: "Scope: plugin surface"
owner: maintainers
last_updated: 2026-11-08
status: Accepted
stage: shipped
next: "—"
feature: plugin-surface
doc_role: scope
summary: "Scope: honest Orbit plugin surface — config keys take effect, inapplicable fields refused, schemas match code, backend bound to consent, and plugin input byte-bounded."
---

# Scope: plugin surface

Bearing: the same `orbit-graph` executable serves as an Orbit plugin backend.
Orbit sends tool calls as a JSON envelope on stdin; the manifest, schemas and
launcher under `.orbit-plugin/` (and the generated compatibility tree) are
the contract an operator consents to.

## Problem

Per-task folders (`plugin-surface/`, `plugin-input/`) recorded one task's
choices each about that contract: whether config keys take effect, whether
schemas match the code, how the backend is bound to consent, and how large a
request may be. Together they are one feature — the plugin surface — so
they are folded here under one entry doc (STD-06@1 §R1–§R2, scope track).

## Goal

One folder answers what the plugin promises and where each promise is
checked. The decisions in [`4_decisions.md`](4_decisions.md) are unchanged
in substance; the plugin-input entry gains the `##` title STD-06@1 §R11
requires.

## Shape

- **Honesty.** Every config key is consumed, fields an operation does not
  read are refused, and schema parity is proven through the real binary.
- **Consent.** The backend executable is bound by SHA-256 digest or the
  named `--allow-unbound-backend` override, reported on every call.
- **Bounds.** Plugin stdin is read at most 1 MiB plus one sentinel byte;
  an oversized request is refused before JSON decoding.

## Non-goals

- **The query and changes tool contracts themselves.** Those live in
  [changes](../changes/1_overview.md) and `docs/plugin.md`.
- **Installing or consenting the plugin.** That is operator procedure in
  `docs/plugin.md`.

## Task References

- ORB-13166 — made the Orbit plugin surface honest: config, schemas, consent binding and installed-Orbit CI tests

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
