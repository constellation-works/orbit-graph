# Machine checks — STD-11 Agent context files

The gate that enforces each rule in [STD-11](STD-11.md), or `review-only`. Ship a change with the gate its rule names, or say it is review-only.

One script can carry the gates for R1, R3, R4, R8, R10, R11, R16, R17, R20 and R25: it walks each skill directory and each repository instruction file. "Orbit: none yet" means Orbit has no such gate today.

| Rule | Gate |
|---|---|
| R1 | A CI step that fails when a skill directory holds a file outside `SKILL.md`, `references/`, `scripts/`, `assets/` and the directories the repository lists as allowed (`evals/`). Orbit: none yet. Every adopter: `sh docs/standards/check.sh`, vendored by `.orbit-plugin/lib/sync-standards.sh`. |
| R2 | A CI step that fails when a second location of a skill is neither a symlink to the canonical directory nor byte-identical to it. Orbit: `scripts/sync-plugin-skills.sh --check`, run by `scripts/ci-guardrails.sh`. |
| R3 | A CI step that fails unless `name` equals the installed name (the directory name, or `<namespace>-<directory>` for a skill under `.orbit-plugin/skills/`, the namespace read from that plugin's `plugin.yaml`) and matches the allowed characters and length. Orbit: none yet. Every adopter: `sh docs/standards/check.sh`, vendored by `.orbit-plugin/lib/sync-standards.sh` (the plugin case: ORB-27272). |
| R4 | A CI step that fails when `description` is empty or over 1,024 characters. What it says is review-only. Orbit: none yet. Every adopter: `sh docs/standards/check.sh`, vendored by `.orbit-plugin/lib/sync-standards.sh`. |
| R5 | review-only. |
| R6 | review-only. |
| R7 | review-only for skills and instruction files. |
| R8 | A CI step that fails when a file under `references/`, `scripts/` or `assets/` is not linked from `SKILL.md`. The condition beside the link is review-only. Orbit: none yet. Every adopter: `sh docs/standards/check.sh`, vendored by `.orbit-plugin/lib/sync-standards.sh`. |
| R9 | review-only. The shared prefix of a split subject is checked by the reviewer of the split. |
| R10 | The R8 gate covers "one level deep". A CI step that fails when a reference of more than 100 lines has no list of links to its own headings in its first 30 lines. Orbit: none yet. Every adopter: `sh docs/standards/check.sh`, vendored by `.orbit-plugin/lib/sync-standards.sh`. |
| R11 | A CI step that fails when a bullet, step or paragraph in a skill or instruction file outside code fences, tables and tooling-managed blocks exceeds 500 characters. The rest is review-only. Orbit: none yet. Every adopter: `sh docs/standards/check.sh`, vendored by `.orbit-plugin/lib/sync-standards.sh`. |
| R12 | review-only. |
| R13 | review-only. |
| R14 | review-only for skills and instruction files. |
| R15 | review-only. |
| R16 | A CI step that fails when a skill's `SKILL.md` exceeds 16,000 bytes or 500 lines, a reference exceeds 16,000 bytes, or an instruction file exceeds its §R25 budget. Orbit: none yet. Every adopter: `sh docs/standards/check.sh`, vendored by `.orbit-plugin/lib/sync-standards.sh`. |
| R17 | A CI link check over each skill and instruction file that resolves relative links and backticked repository paths. Orbit: `scripts/check-doc-links.py` in `scripts/ci-guardrails.sh` covers Markdown links. That a command or flag exists is review-only. Every adopter: `sh docs/standards/check.sh`, vendored by `.orbit-plugin/lib/sync-standards.sh`. |
| R18 | review-only, at two points: the task author when deciding whether a context file needs an update, and the reviewer of a diff that changes a rule, command or pointer it names. |
| R19 | A CI step that fails when a skill has no `evals/` file with at least three cases. Running them is review-only. The umbrella: `.claude/skills/constellation-standards/evals/`. Orbit: none yet. Every adopter: `sh docs/standards/check.sh`, vendored by `.orbit-plugin/lib/sync-standards.sh`. |
| R20 | A CI step that, for every directory holding `AGENTS.md`, `CLAUDE.md` or `GEMINI.md`, fails unless `AGENTS.md` is a regular file and each other name is a symlink to it or starts with the line `@AGENTS.md`. Orbit: none yet. Every adopter: `sh docs/standards/check.sh`, vendored by `.orbit-plugin/lib/sync-standards.sh`. |
| R21 | review-only. |
| R22 | review-only. |
| R23 | review-only. |
| R24 | review-only. |
| R25 | A CI step that fails when the bytes of `AGENTS.md` plus any import file beside it exceed 8,000 at the root or 4,000 in a subdirectory. Orbit: none yet. The umbrella: `operations/scripts/check-instruction-contract.sh` (`make instruction-check`), with its own larger budget. Every adopter: `sh docs/standards/check.sh`, vendored by `.orbit-plugin/lib/sync-standards.sh`. |
