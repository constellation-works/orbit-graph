# Why — STD-11 Agent context files

Why each rule in [STD-11](STD-11.md) exists, tied to what was observed or to a published source. Read the entry for a rule when its intent is unclear or before deviating. The rules themselves are in `STD-11.md`; this file explains them and adds none.

The standard starts from the owner's words in the chief's chat on 2026-11-22: "we need a proper, top-quality, SOA standard for skills. bad example: codebases/scribe/skills/scribe/SKILL.md", and "chunk it up into logical groups - and link them in the SKILL.md."

The instruction-file rules came from agent-written scribe entry `standard-8`, which quotes Daniel: "I think some of these can be consolidated, e.g. agent-skills + agent-instruction-files."

## Sources

Read on 2026-11-22. A limit below that carries a number is quoted from them.

- The Agent Skills specification (agentskills.io/specification): the directory layout, the limits on `name` and `description`, "Keep your main `SKILL.md` under 500 lines", instructions "< 5000 tokens recommended", "Keep file references one level deep".
- Anthropic's skill authoring best practices and Agent Skills overview (platform.claude.com): the three loading levels and their cost, "Always write in third person", "Only add context Claude doesn't already have", degrees of freedom, "For reference files longer than 100 lines, include a table of contents at the top", "Avoid time-sensitive information", "At least three evaluations created" and "Tested with Haiku, Sonnet, and Opus".

## What was measured

Across the constellation umbrella and its repositories that day: 25 skill directories of our own, 23 distinct skills among them (two are installed copies), and 71 reference files, copies included.

- scribe's `SKILL.md` was 31,478 bytes in 526 lines, with no bundled file and no link. It was 3,890 bytes on 2026-11-11 and was edited by 43 commits. Eight subjects sat in one file, the largest of them 6,048 characters; an agent asked for a handoff loaded the rules for settling claims with it.
- The formation skill's `SKILL.md` was 33,900 bytes in 492 lines, although it has references: incidents, unverified items and mechanics had gathered in the main file.
- The other 21 skills were under 16,000 bytes. Eleven had no bundled file, at 2,925 to 8,569 bytes.
- 27 of the 71 reference files were over 16,000 bytes. The largest, Orbit's `configuration.md`, was 131,911 bytes: about 33,000 tokens for one lookup.
- Bullets or paragraphs over 500 characters: 18 in scribe's skill, 13 in formation's, 8 in nebula's, 5 in Orbit's.
- One description, `constellation-standards`, was 1,366 characters, over the published limit of 1,024.
- scribe's skill existed as three identical files in its repository and Orbit's three skills as two, each set kept identical by a script with a `--check` mode.

The instruction-file evidence came from the same constellation repositories:

- scribe's `AGENTS.md` at `bf2042c` was 19,292 bytes; its 13,431-character introduction used about 3,400 tokens in every session and run. That paragraph had grown from 1,185 characters through 39 commits as feature details accumulated.
- Of twelve root instruction files, ten were under 7,300 bytes. Orbit's was 13,383 bytes and scribe's 19,226. Four files had bullets over 500 characters.
- The umbrella root had already been reduced to a 2,087-byte router, with its size checked by `operations/scripts/check-instruction-contract.sh`.

## Per rule

- **R1 (layout).** The published layout. The three names tell the agent how to treat a file before it opens it: read it, run it, or copy it.
- **R2 (one source).** Each provider and each package wants the skill at its own path. Copies kept by hand drift on the first edit; a symlink or a checked mirror cannot.
- **R3 (name).** The published limits, plus the reserved words of the Claude platform. The name is the command a person types and the label in every listing. v2: Orbit links a plugin skill under `<namespace>-<directory>`, so a plugin skill could not have its name equal both its directory and the id agents call it by. Renaming the directory to the full name made Orbit install `research-research-loop` (ORB-27142, review F1); keeping the old rule meant renames across every plugin skill. The owner chose the installed name as the rule (decision dr-896fcf20bb45f09b option 1, 2026-11-23): what an agent sees in `name` is what it invokes (ORB-27272).
- **R4 (description).** It is the only part of a skill in context when the agent decides whether to load it, so it must carry both the capability and the triggers. A harness may cut a long entry or drop it under a listing budget, so the capability comes first. Third person because it is injected into a system prompt. Naming the neighbour skill is what our own skills already do to stop the wrong one loading (`orbit`, `orbit-orchestrate`, `orbit-setup`).
- **R5 (frontmatter).** An unknown field is ignored by one harness and an error in another. The invocation switch exists because a skill with side effects should not start on a guess about the user's intent.
- **R6 (what SKILL.md holds).** The body is paid for on every use. scribe's file made every use pay for all eight subjects. With the common path and the routing only, a use costs the main file plus the one reference its task needs.
- **R7 (only what the agent lacks).** The published advice says to add only the context a reader needs; command listings drift and `--help` does not. In v3 the line test and no-restating rule also apply to instruction files, combining their former STD-10 v1 R3, R5 and R6 (scribe standard-8).
- **R8 (routing by condition).** A file the agent is not told when to open is either never read or always read. Orbit's skill states it in its opening lines: "Read only the reference needed for the current action; this is not a reading checklist."
- **R9 (one task per reference).** This is the owner's "chunk it up into logical groups". The unit is a task because that is what an agent arrives with. A reference that needs a second one doubles the cost and hides the dependency. The shared prefix is the owner's addition when he approved the draft: the parts of one split subject sort together in the directory and in the routing table, and a reader sees that they were one document.
- **R10 (one level deep, contents list).** The published rules. An agent that follows a link from a reference may read only part of the target; a file linked from `SKILL.md` is read whole. The contents list lets it see the scope of a long file from its first lines.
- **R11 (form).** A step that grows by one sentence per change is how both large skills got large; a cap on each bullet stops it at the first change. In v3 the 500-character limit and one-rule-per-bullet form also apply to instruction files, as in former STD-10 v1 R8 (scribe standard-8).
- **R12 (freedom matches risk).** The published "degrees of freedom". A captain's self-clear and a `scribe entry add` with fixed headings fail on a wrong flag, so they get the exact command. A review gets criteria.
- **R13 (templates and examples).** A described format is reproduced approximately; a template is reproduced exactly. The formation briefs are files for this reason.
- **R14 (no history).** The formation skill carried dated accounts of how its clear procedure used to work, beside the procedure. In v3 the former STD-10 v1 R4 rule also applies to instruction files (scribe standard-8).
- **R15 (side effects and authority).** A skill is loaded on a description match, which is not consent. The formation skill's authority table lives in the skill, but the grants in it quote the owner; a skill that invented authority would be followed as if he had.
- **R16 (budget).** 16,000 bytes is about 4,000 tokens, under the published 5,000 for the body, and 21 of 23 skills already fit. The same number for a reference keeps it cheaper than the file it was split from. In v3 instruction files use the limits in R25, merged from former STD-10 v1 R10 (scribe standard-8).
- **R17 (links and commands resolve).** A context file that follows R7 is mostly rules and pointers, so a dead link or path is a common way to fail. In v3 the link check also covers instruction files, as in former STD-10 v1 R13 (scribe standard-8).
- **R18 (same change).** A skill is the only documentation an agent reads before it runs the command. Scribe's skill grew because each feature task listed it as a doc to update. In v3 the same-change and removal rules also cover instruction files, combining former STD-10 v1 R11 and R12 (scribe standard-8).
- **R19 (evaluations).** The published minimum. The description decides whether a skill loads, and nothing else tests it. The tiers matter here because formation ranks run the same skills on three models.
- **R20 (one source).** Providers load different instruction-file names. The repositories had four arrangements: linked copies drifted or gave providers different rules. The import form permits provider-specific rules (former STD-10 v1 R1; scribe standard-8).
- **R21 (subdirectories).** Orbit keeps one instruction file per crate for its own layering rules. An agent loads the root plus its crate, not all crates; a nested file that repeats the root rule loads it twice (former STD-10 v1 R2; scribe standard-8).
- **R22 (content).** A content test keeps the file to rules, commands, repository facts and pointers. Orbit's healthy instruction files already held these kinds; the code and docs show the rest (former STD-10 v1 R3; scribe standard-8).
- **R23 (opening).** Scribe's long introduction grew because it was the only prose in its file. A short introduction gives readers the repository and loading context without becoming a feature history (former STD-10 v1 R7 and R8; scribe standard-8).
- **R24 (section order).** Five repositories already used `Rules`, `Branching`, `Gates` and `Code` in the same order. Adding `Tests` to that sequence makes the common questions land under familiar headings; the managed block remains last (former STD-10 v1 R9; scribe standard-8).
- **R25 (instruction-file budget).** "Short" needs a number or it erodes one paragraph at a time. Ten of twelve root files were under 7,300 bytes, and nine of Orbit's ten crate files fit 4,000 bytes; source and imports count together (former STD-10 v1 R10; scribe standard-8).
