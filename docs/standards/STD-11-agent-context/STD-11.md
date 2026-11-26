---
id: STD-11-agent-context
title: Agent context files — skills and instruction files loaded by an agent harness
summary: Normative rules for the skills and instruction files an agent harness loads — their source, content, structure, size, links and checks; follow them in every repository agents work in.
tags: [standard, docs, agents]
version: 3
created_date: 2026-11-22
updated_date: 2026-11-25
last_validated: 2026-11-22
related: [STD-08-economy, STD-09-project-docs]
---

# Agent context files

These rules cover a context file: a skill or an instruction file that an agent's harness loads. A skill is a directory with a `SKILL.md`; instruction files include `AGENTS.md`, `CLAUDE.md` and `GEMINI.md`. These rules apply to every harness the repositories use (Claude Code, Codex, an Orbit plugin).

An instruction file is loaded as standing context. A skill loads in three steps, and each rule below protects one of them:

1. the **name and description**, in every session, whether or not the skill is used;
2. the **`SKILL.md` body**, each time the skill is used;
3. a **bundled file** (a reference, a script, an asset), only when a step sends the agent to it.

Every rule is portable. A skill's minimum is a directory named as the skill, holding a `SKILL.md` with a name, a description that says what and when, and the steps of the common path.

Orbit is the worked example, not the audience. Orbit paths are relative to `codebases/orbit/`.

Other standards own the neighbouring rules, and this one does not repeat them:
- **Each fact once, and a size budget for context an agent rereads** is [STD-08](../STD-08-economy/STD-08.md) §R3 and §R4. This standard applies them to context files and sets their size limits.
- **The front page for people** is [STD-09](../STD-09-project-docs/STD-09.md). An instruction file is not a README: it does not introduce, sell or teach the project.

The keywords MUST, MUST NOT, SHOULD and MAY are normative. A rule marked *review-only* in [`checks.md`](checks.md) has no automated gate, so a reviewer enforces it.

This standard is a directory. This file holds the binding text: the rules, where they apply, and how to deviate. Beside it, [`why.md`](why.md) gives the reason for each rule and its sources, [`checks.md`](checks.md) the gate that enforces it, [`exemplars.md`](exemplars.md) worked examples in Orbit, and [`CHANGELOG.md`](CHANGELOG.md) what changed in each version.

## Rules

### Layout

- **R1.** A skill MUST be one directory, named as the skill, holding `SKILL.md`. Its other files go in `references/` (documents the agent reads when sent to them), `scripts/` (code the agent runs) and `assets/` (templates, schemas and data used in the output). A directory of another name is allowed only for a kind of file none of the three fits, and is named for that kind (`evals/`).
- **R2.** A skill MUST have one canonical directory. Every other place it must appear (a second provider's skill folder, a plugin package, a binary's embedded copy) MUST be a symlink to it or a mirror that CI compares with it byte for byte. A copy edited by hand is not allowed.

### Name and description

- **R3.** `name` MUST equal the name the skill is installed under: its directory name or, for a skill an Orbit plugin ships in `.orbit-plugin/skills/<directory>/`, `<namespace>-<directory>`, the namespace being `metadata.name` in that plugin's `plugin.yaml`. The name is 1 to 64 characters of lowercase letters, digits and single hyphens, not starting or ending with a hyphen, and without the words `anthropic` or `claude`. It names the work or the tool (`orbit-setup`, `research-loop`), never a vague role (`helper`, `utils`).
- **R4.** `description` MUST say what the skill does and when to use it, in the third person, in at most 1,024 characters. Its first sentence carries the capability; the rest names the requests, file types, commands and phrases that should load the skill. Where another skill covers neighbouring work, the description says what this one is not for and names the other.
- **R5.** Frontmatter MUST hold `name` and `description`. Any other field MUST be one the target harness documents. A skill whose steps deploy, publish, delete or spend SHOULD set the harness's switch that stops the model from starting it unasked.

### SKILL.md

- **R6.** `SKILL.md` MUST hold these parts and no other:
  1. what the skill is for, in at most three sentences;
  2. the steps of the common path, in order, with the exact command for each;
  3. the rules that hold on every use: what never to do, and whose decision a thing is;
  4. the routing: every bundled file, with the condition that sends the agent to it.

  What only some uses need (a second procedure, a format, a lookup table, troubleshooting, the reasons) moves to a reference.
- **R7.** Every line of a context file (a skill or an instruction file) MUST be something the agent would get wrong, or could not find, without it. A context file MUST NOT explain what a capable agent already knows, inventory what `--help` or the tree shows (modules, source files, commands, flags, formats, configuration keys), or restate a rule that a standard, a document or a file above it already states: it links that instead.
- **R8.** Every file under `references/`, `scripts/` and `assets/` MUST be linked from `SKILL.md` by a relative path, with the condition for opening it: "read `references/handoffs.md` before writing a handoff". The links sit together under one heading, and MAY be repeated at the step that needs them. A link to a script says whether to run it or read it.

### References

- **R9.** A reference MUST cover one task or one subject that a reader needs whole, be named for it (`references/handoffs.md`), and stand alone: reading it requires `SKILL.md` and no other reference. A file that covers two tasks is two files. When one subject is split into several files to fit the budget (§R16), every resulting file MUST carry the subject as its prefix: `configuration-<section>.md`.
- **R10.** References MUST be one level deep: a file reachable only through another reference is not allowed. A reference of more than 100 lines MUST open with a contents list of its headings.

### Writing

- **R11.** Instructions in a context file MUST be imperative and addressed to the agent: numbered steps for a sequence, bullets for rules (one rule per bullet), a table for a lookup. One term names one thing throughout. No bullet, step or paragraph outside code fences, tables and tooling-managed blocks exceeds 500 characters; one that needs more becomes sub-steps, a reference (skill) or a document the bullet links (instruction file).
- **R12.** The freedom a step leaves MUST match its risk. A step that is fragile, ordered, destructive or checked by a machine gives the exact command or a script, and says what a refusal means. A step that needs judgment gives criteria and one example, not a script. A sequence of commands that is always the same SHOULD be a script in `scripts/`.
- **R13.** Where the form of the output matters, the skill MUST give a template in `assets/` or one worked example, not a description of the form.
- **R14.** A context file MUST describe the repository or procedure as it is now, in the present tense. It MUST NOT tell what changed, when a feature arrived, give phase, slice or version narratives or dates of features, or keep a superseded way beside the current one. That history lives in git, the changelog and the design docs. A rule MAY end with one reference to the incident that caused it.
- **R15.** A step that writes outside the working tree, deletes, sends, publishes or spends MUST be marked as such, with the confirmation it needs and from whom. A skill grants no authority: it points at the document or person that does.

### Size

- **R16.** Every context file MUST have a size budget, and CI MUST check it. `SKILL.md` is at most 16,000 bytes and 500 lines and each reference at most 16,000 bytes; an instruction file's budget is §R25. Content over the budget is split by task (§R9) or, for an instruction file, moved to a document and left as a pointer, not trimmed of the steps an agent needs.

### Staying true

- **R17.** Every relative link and every path a context file names MUST resolve, and CI MUST check it. Every command, flag and tool name it gives MUST exist in the version the file ships with.
- **R18.** A change MUST edit a context file only when what the agent does changes: when it adds, changes or removes a rule, command or pointer in it, or renames, removes or alters a command, flag, file or rule the file names, in the same change. A change MUST NOT add to a context file only to describe a new feature, and a task MUST NOT name an instruction file as a doc to update unless it names the rule that changes. A feature's description goes in the feature's docs.
- **R19.** A skill MUST keep at least three evaluation cases in `evals/`: a request that should load it, a request that should not (naming the skill that should), and a task with the result expected. They are run when the description or the routing changes, on each model tier the skill is used with.

### Instruction files

- **R20.** `AGENTS.md` MUST be the one source file in a directory that has instructions. Every other provider file name the repository supports (`CLAUDE.md`, `GEMINI.md`) MUST be a symlink to it, or a file whose first line imports it (`@AGENTS.md`) and whose remaining lines are rules for that provider alone. A second copy of the text is not allowed.
- **R21.** A subdirectory MAY have its own instruction file only for rules that do not apply to the rest of the repository (a crate, a package, a site). It MUST NOT repeat a rule of a file above it, and §R20 applies to it.
- **R22.** An instruction file holds four kinds of content and no other: what the repository is (§R23); rules the code does not show (what to do, what never to do, whose decision a thing is); the commands to run and when (the gates before handoff, and a build or test command whose right form is not the obvious one); pointers (a link to the document that owns a subject, with a phrase saying what the reader finds there).
- **R23.** An instruction file MUST open with a title and an introduction of at most three sentences: what the repository is, and anything a reader must know about how the file is loaded. Below the introduction the content is bullets under `##` headings (§R11).
- **R24.** The sections `Rules`, `Branching`, `Gates`, `Code` and `Tests` MUST carry these names and this order where they exist. A section of the project's own follows them. A block that tooling manages (such as the list of adopted standards) comes last.
- **R25.** A root instruction file MUST be at most 8,000 bytes and a subdirectory's at most 4,000 bytes, and CI MUST check it. The count is what one provider loads from that directory: the source file, any import file beside it, and managed blocks. Content over the budget moves to a document and leaves a pointer (§R22).

## Applies to

`universal`: every skill and instruction file of every repository agents work in, for any harness. A vendored third-party skill is outside this standard. A harness's own configuration and a user's skills or instruction files outside the repository are outside it too.

## Deviations

An adopting repo that departs from a rule records a decision in its own design docs, citing `STD-11@<version> §Rn` and the reason. It never edits its vendored copy of this standard.
