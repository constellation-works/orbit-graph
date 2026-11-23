# Exemplars — STD-11 Agent skills

Worked examples of the rules in [STD-11](STD-11.md), as pointers into the Orbit codebase.
Orbit is the example, not the audience: this file is optional reading, and useful only with
an Orbit checkout.

Paths are relative to `codebases/orbit/`, and skill paths to
`crates/orbit-core/assets/skills/`. Line numbers and sizes are at `c994882812`.

- R1, followed — `orbit/`, `orbit-orchestrate/` and `orbit-setup/`: each is `SKILL.md` and
  a `references/` directory, with nothing else.
- R2, followed — `crates/orbit-core/assets/skills/` is the canonical tree and
  `plugin/skills/` its mirror; `scripts/sync-plugin-skills.sh --check` fails CI on drift
  (`scripts/ci-guardrails.sh` line 698).
- R4, followed — `orbit/SKILL.md` line 3: the capability first ("Creates, executes and
  reviews Orbit tasks"), then the triggers, then the two neighbouring skills by name and
  what each is for.
- R6 and R8, followed — `orbit/SKILL.md` lines 64 to 86 (`## Choose the reference`): a
  table from the task in hand to the one reference for it. Lines 8 to 10 say "Read only
  the reference needed for the current action; this is not a reading checklist."
- R9, followed — `orbit/references/task-review.md` (5,749 bytes) and `search.md` (4,390):
  one task each, named for it.
- R11, counter-example — `orbit/SKILL.md` has five bullets or paragraphs over 500
  characters, the longest 1,575.
- R16, followed — `orbit/SKILL.md` is 9,409 bytes in 137 lines.
- R16, counter-example — `orbit-setup/references/configuration.md` is 131,911 bytes in
  2,181 lines, and four more references are over 70,000 bytes
  (`orbit-orchestrate/references/run-debugging.md`, `orchestration.md`,
  `orbit/references/tool-surface.md`, `orbit-setup/references/maintenance.md`).
- R19, counter-example — no Orbit skill has an `evals/` directory.
