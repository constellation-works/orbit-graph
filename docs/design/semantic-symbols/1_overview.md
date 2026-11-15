---
title: "Semantic symbols — Overview"
owner: maintainers
last_updated: 2026-11-14
status: Draft
stage: proposed
next: "Daniel answers the open questions in 2_design.md §10; build step 1 is then filed"
feature: semantic-symbols
doc_role: overview
summary: "Proposal: model-written function descriptions, embeddings and similar-symbol queries in the graph plugin, measured offline first; nested cluster ids are deferred until a consumer needs them."
---

# Semantic symbols — Overview

A proposal to give the graph plugin search by meaning: a short model-written
description for each function, an embedding of it, and two queries ("similar
to this symbol" and "symbols matching this text"). It answers Daniel's request
of 2026-11-14 (ORB-24871) for semantic ids. Nothing here is built, and no
build task is filed.

## Recommendation

1. **Measure before building.** The first step embeds function text with a
   local model and checks, in a split by time, whether functions that were
   later changed together sit close. It needs no model-written descriptions,
   no plugin change and no network. Its result decides whether anything else
   is built ([`2_design.md`](2_design.md) §7).
2. **Descriptions have to earn their cost.** The orbit repository changes
   about 5,200 non-test function bodies a week, 19% of the 27,683 it has, so
   upkeep is a fifth of a full pass by count, and a third by bytes, every
   week. The measurement compares
   descriptions with embedding the source directly, which costs no model runs.
3. **The query is nearest neighbours, not an id prefix.** At this size an
   exact scan of every vector takes milliseconds. Ids `10004` and `10006` are
   no closer to `10005` than any other member of cluster `1000`, and a true
   neighbour can sit in a different cluster.
4. **Ids are static, and they wait.** An id is a handle and a label for a
   region of the code, not a search key. Following Daniel's follow-up, the
   clusters are frozen after an initial batch, an id is assigned once and
   never changes, new symbols are appended to their nearest cluster, and a
   new embedding model or a re-clustering starts a new generation with a
   map from the old ids. The design recommends building them only when one
   of three named consumers is ready.
5. **The plugin stays `network: none`.** Descriptions are written by a
   low-cost crew through a plugin activity; embeddings are computed inside the
   plugin backend with a local model.

## Measured for this design

From a graph index of the orbit repository at `cf8989b2f` (2026-11-14), built
with this repository's `orbit-graph sync`:

| Quantity | Value |
|---|---|
| Indexed symbols of every kind | 70,538 |
| Functions and methods | 38,909 |
| Functions and methods outside test paths | 27,683 (23.2 MB of source) |
| Of those, under 200 bytes | 7,164 (26% of the count, 4% of the bytes) |
| Distinct function bodies, all paths | 38,274 |
| Non-test bodies new or changed in the last 7 days | 5,223 (8.0 MB) |
| Non-test bodies new or changed in the last 28 days | 15,123 |

## Docs in this folder

- [`2_design.md`](2_design.md) — the design: symbol scope and the type field,
  descriptions, embeddings, the two query tools, how results join `recommend`,
  nested cluster ids, the offline evaluation and its stop rule, what leaves
  the machine, the build order, and open questions with recommendations.

## Stage

Proposed. The design changes no code or configuration. The first build step
is a measurement; every later step waits on its result.

## Task References

- ORB-24871 — this design
- ORB-24852 — the `word_index` variant the semantic variant is compared with
- ORB-24853 — the baselines, intervals and gate readout the evaluation reuses

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
