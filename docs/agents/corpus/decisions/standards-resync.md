---
type: decision
applies_to: [agent_implement]
status: active
links: []
---

# Standards Resync

## Scope

Curated rules for this topic. Only a current rule may be cited. A rule answers a new request only when the operator has set auto on it and a human has merged that edit.

## Entries

Raw decision history is not a rule. Human answers land here only as citations on a rule under Rules.

## Rules

```yaml
id: "older-superseded"
question: "ORB-19667 re-syncs vendored standards at STD-02@4. ORB-13244 is an older task for the same files at STD-02@3. Does the newer re-sync supersede the older one?"
answer: "1"
why: "A newer re-sync to a later standard version covers the same files, so the older task is redundant."
citations:
  - task: "ORB-21966"
    date: "2026-11-04"
    option: "1"
depends_on:
  []
status: "current"
review_by: "2027-02-02"
auto: false
```
