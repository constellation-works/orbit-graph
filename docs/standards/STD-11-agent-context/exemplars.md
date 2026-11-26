# Exemplars — STD-11 Agent context files

Worked examples of the rules in [STD-11](STD-11.md), as pointers into the Orbit codebase. Orbit is the example, not the audience: this file is optional reading, and useful only with an Orbit checkout.

Paths are relative to `codebases/orbit/`, and skill paths to `crates/orbit-core/assets/skills/`. Skill line numbers and sizes are at `c994882812`; instruction-file examples are at `9c6c390d3e`.

- R1, followed — `orbit/`, `orbit-orchestrate/` and `orbit-setup/`: each is `SKILL.md` and a `references/` directory, with nothing else.
- R2, followed — `crates/orbit-core/assets/skills/` is the canonical tree and `plugin/skills/` its mirror; `scripts/sync-plugin-skills.sh --check` fails CI on drift (`scripts/ci-guardrails.sh` line 698).
- R4, followed — `orbit/SKILL.md` line 3: the capability first ("Creates, executes and reviews Orbit tasks"), then the triggers, then the two neighbouring skills by name and what each is for.
- R6 and R8, followed — `orbit/SKILL.md` lines 64 to 86 (`## Choose the reference`): a table from the task in hand to the one reference for it. Lines 8 to 10 say "Read only the reference needed for the current action; this is not a reading checklist."
- R9, followed — `orbit/references/task-review.md` (5,749 bytes) and `search.md` (4,390): one task each, named for it.
- R11, counter-example — `orbit/SKILL.md` has five bullets or paragraphs over 500 characters, the longest 1,575.
- R14, followed — `CLAUDE.md` lines 28 onward (`## Gates`) states what to run now; a rule that came from an incident ends with its id (`ORB-16683`, `F2026-10-118`).
- R16, followed — `orbit/SKILL.md` is 9,409 bytes in 137 lines.
- R16, counter-example — `orbit-setup/references/configuration.md` is 131,911 bytes in 2,181 lines, and four more references are over 70,000 bytes (`orbit-orchestrate/references/run-debugging.md`, `orchestration.md`, `orbit/references/tool-surface.md`, `orbit-setup/references/maintenance.md`).
- R19, counter-example — no Orbit skill has an `evals/` directory.
- R20, counter-example — at `9c6c390d3e`, root and crate `AGENTS.md` files were symlinks to `CLAUDE.md`: one source, under the wrong name.
- R21, followed — `crates/orbit-store/CLAUDE.md` holds the store's layering and migration rules, which do not apply to the rest of the repository.
- R22, followed — `CLAUDE.md` lines 11–22 (`## Rules`) gives rules the code does not show; other instruction-file content links to the document that owns it.
- R23, followed — `CLAUDE.md` line 9 opens with a two-sentence introduction.
- R24, followed in part — `CLAUDE.md` orders `Rules`, `Branching`, `Gates` and `Code`, then its own `Tests and QA` section, and puts the managed standards block last (line 70).
- R25, counter-example — at `9c6c390d3e`, root `CLAUDE.md` was 13,383 bytes and `crates/orbit-engine/CLAUDE.md` was 6,363 bytes, over the respective budgets.
- R17, followed in part — `scripts/check-doc-links.py`, run by `scripts/ci-guardrails.sh`, checks Markdown links.
