---
id: STD-11-agent-skills
title: Agent skills — a short router and core procedure in SKILL.md, with everything else in task-sized references it links by condition
summary: Normative rules for a skill an agent loads for one kind of work (a directory with SKILL.md) — one canonical directory, a description that says what it does and when to use it, a SKILL.md of at most 16,000 bytes that holds the common path and routes to references by condition, references of one task each and one level deep, exact commands where the work is fragile, and evaluations; follow it for every skill a constellation repository ships or keeps.
tags: [standard, docs, agents]
version: 2
created: 2026-11-22
updated: 2026-11-23
last_validated: 2026-11-22
related: [STD-08-economy, STD-10-agent-instruction-files]
---

# Agent skills

These rules cover a skill: a directory with a `SKILL.md` that an agent's harness lists by
name and description, and loads in full only when the work calls for it. They apply to the
skills of every harness the repositories use (Claude Code, Codex, an Orbit plugin). They
exist so that a skill costs a session little until it is needed, tells the agent exactly
what it could not have worked out, and sends it to the one further file its task needs.

A skill loads in three steps, and each rule below protects one of them:

1. the **name and description**, in every session, whether or not the skill is used;
2. the **`SKILL.md` body**, each time the skill is used;
3. a **bundled file** (a reference, a script, an asset), only when a step sends the agent
   to it.

Every rule is portable. The minimum is a directory named as the skill, holding a `SKILL.md`
with a name, a description that says what and when, and the steps of the common path.

Orbit is the worked example, not the audience. Orbit paths are relative to `codebases/orbit/`.

Other standards own the neighbouring rules, and this one does not repeat them:
- **Each fact once, and a size budget for context an agent rereads** is
  [STD-08](../STD-08-economy/STD-08.md) §R3 and §R4. This standard applies them to a skill
  and sets the numbers.
- **The file a harness loads for all work** (`AGENTS.md`) is
  [STD-10](../STD-10-agent-instruction-files/STD-10.md). A rule that holds for every task
  in a repository belongs there, not in a skill.

The keywords MUST, MUST NOT, SHOULD and MAY are normative. A rule marked *review-only* in
[`checks.md`](checks.md) has no automated gate, so a reviewer enforces it.

This standard is a directory. This file holds the binding text: the rules, where they
apply, and how to deviate. Beside it, [`why.md`](why.md) gives the reason for each rule and
its sources, [`checks.md`](checks.md) the gate that enforces it,
[`exemplars.md`](exemplars.md) worked examples in Orbit, and [`CHANGELOG.md`](CHANGELOG.md)
what changed in each version.

## Rules

### Layout

- **R1.** A skill MUST be one directory, named as the skill, holding `SKILL.md`. Its other
  files go in `references/` (documents the agent reads when sent to them), `scripts/` (code
  the agent runs) and `assets/` (templates, schemas and data used in the output). A
  directory of another name is allowed only for a kind of file none of the three fits, and
  is named for that kind (`evals/`).
- **R2.** A skill MUST have one canonical directory. Every other place it must appear (a
  second provider's skill folder, a plugin package, a binary's embedded copy) MUST be a
  symlink to it or a mirror that CI compares with it byte for byte. A copy edited by hand
  is not allowed.

### Name and description

- **R3.** `name` MUST equal the name the skill is installed under: its directory name or, for a skill an Orbit plugin ships in `.orbit-plugin/skills/<directory>/`, `<namespace>-<directory>`, the namespace being `metadata.name` in that plugin's `plugin.yaml`. The name is 1 to 64 characters of lowercase letters, digits and single hyphens, not starting or ending with a hyphen, and without the words `anthropic` or `claude`. It names the work or the tool (`orbit-setup`, `research-loop`), never a vague role (`helper`, `utils`).
- **R4.** `description` MUST say what the skill does and when to use it, in the third
  person, in at most 1,024 characters. Its first sentence carries the capability; the rest
  names the requests, file types, commands and phrases that should load the skill. Where
  another skill covers neighbouring work, the description says what this one is not for
  and names the other.
- **R5.** Frontmatter MUST hold `name` and `description`. Any other field MUST be one the
  target harness documents. A skill whose steps deploy, publish, delete or spend SHOULD
  set the harness's switch that stops the model from starting it unasked.

### SKILL.md

- **R6.** `SKILL.md` MUST hold these parts and no other:
  1. what the skill is for, in at most three sentences;
  2. the steps of the common path, in order, with the exact command for each;
  3. the rules that hold on every use: what never to do, and whose decision a thing is;
  4. the routing: every bundled file, with the condition that sends the agent to it.

  What only some uses need (a second procedure, a format, a lookup table, troubleshooting,
  the reasons) moves to a reference.
- **R7.** Every line MUST be something the agent would get wrong, or could not find,
  without it. A skill MUST NOT explain what a capable agent already knows, list what
  `--help` or the tree shows, or carry a rule that a standard, a document or the
  instruction file already states: it links that instead.
- **R8.** Every file under `references/`, `scripts/` and `assets/` MUST be linked from
  `SKILL.md` by a relative path, with the condition for opening it: "read `references/handoffs.md` before writing a handoff". The
  links sit together under one heading, and MAY be repeated at the step that needs them. A
  link to a script says whether to run it or read it.

### References

- **R9.** A reference MUST cover one task or one subject that a reader needs whole, be
  named for it (`references/handoffs.md`), and stand alone: reading it requires `SKILL.md`
  and no other reference. A file that covers two tasks is two files. When one subject is
  split into several files to fit the budget (§R16), every resulting file MUST carry the
  subject as its prefix: `configuration-<section>.md`.
- **R10.** References MUST be one level deep: a file reachable only through another
  reference is not allowed. A reference of more than 100 lines MUST open with a contents
  list of its headings.

### Writing

- **R11.** Instructions MUST be imperative and addressed to the agent: numbered steps for a
  sequence, bullets for rules, a table for a lookup. One term names one thing throughout.
  No bullet, step or paragraph exceeds 500 characters; one that needs more becomes
  sub-steps or a reference.
- **R12.** The freedom a step leaves MUST match its risk. A step that is fragile, ordered,
  destructive or checked by a machine gives the exact command or a script, and says what
  a refusal means. A step that needs judgment gives criteria and one example, not a script.
  A sequence of commands that is always the same SHOULD be a script in `scripts/`.
- **R13.** Where the form of the output matters, the skill MUST give a template in
  `assets/` or one worked example, not a description of the form.
- **R14.** A skill MUST describe the procedure as it is now, in the present tense. It MUST
  NOT tell what changed, when a feature arrived, or keep a superseded way beside the
  current one. A rule MAY end with one reference to the incident that caused it; a longer
  account goes to the design docs.
- **R15.** A step that writes outside the working tree, deletes, sends, publishes or spends
  MUST be marked as such, with the confirmation it needs and from whom. A skill grants no
  authority: it points at the document or person that does.

### Size

- **R16.** `SKILL.md` MUST be at most 16,000 bytes and 500 lines, and each reference at
  most 16,000 bytes, and CI MUST check both. Content over the budget is split by task
  (§R9), not trimmed of the steps an agent needs.

### Staying true

- **R17.** Every relative link and every path the skill names MUST resolve, and CI MUST
  check it. Every command, flag and tool name it gives MUST exist in the version the skill
  ships with.
- **R18.** A change that renames, removes or alters a command, flag, file or rule a skill
  names MUST update the skill in the same change. A change MUST NOT add to a skill only to
  describe a new feature: it edits the skill when what the agent does changes.
- **R19.** A skill MUST keep at least three evaluation cases in `evals/`: a request that
  should load it, a request that should not (naming the skill that should), and a task
  with the result expected. They are run when the description or the routing changes, on
  each model tier the skill is used with.

## Applies to

`universal`: every skill a constellation repository ships, embeds or keeps for its own
agents, for any harness. A vendored third-party skill is outside this standard. A harness's
own configuration and a user's skills outside the repository are outside it too.

## Deviations

An adopting repo that departs from a rule records a decision in its own design docs, citing
`STD-11@<version> §Rn` and the reason. It never edits its vendored copy of this standard.
