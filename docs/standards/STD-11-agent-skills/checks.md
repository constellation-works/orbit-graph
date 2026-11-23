# Machine checks — STD-11 Agent skills

The gate that enforces each rule in [STD-11](STD-11.md), or `review-only`. Ship a change
with the gate its rule names, or say it is review-only.

One script can carry the gates for R1, R3, R4, R8, R10, R11, R16 and R17: it walks each
skill directory and reads `SKILL.md` and the files beside it. "Orbit: none yet" means Orbit
has no such gate today.

| Rule | Gate |
|---|---|
| R1 | A CI step that fails when a skill directory holds a file outside `SKILL.md`, `references/`, `scripts/`, `assets/` and the directories the repository lists as allowed (`evals/`). Orbit: none yet. Every adopter: `sh docs/standards/check.sh`, vendored by `.orbit-plugin/lib/sync-standards.sh`. |
| R2 | A CI step that fails when a second location of a skill is neither a symlink to the canonical directory nor byte-identical to it. Orbit: `scripts/sync-plugin-skills.sh --check`, run by `scripts/ci-guardrails.sh`. |
| R3 | A CI step that fails unless `name` equals the installed name (the directory name, or `<namespace>-<directory>` for a skill under `.orbit-plugin/skills/`, the namespace read from that plugin's `plugin.yaml`) and matches the allowed characters and length. Orbit: none yet. Every adopter: `sh docs/standards/check.sh`, vendored by `.orbit-plugin/lib/sync-standards.sh` (the plugin case: ORB-27272). |
| R4 | A CI step that fails when `description` is empty or over 1,024 characters. What it says is review-only. Orbit: none yet. Every adopter: `sh docs/standards/check.sh`, vendored by `.orbit-plugin/lib/sync-standards.sh`. |
| R5 | review-only. |
| R6 | review-only. |
| R7 | review-only. |
| R8 | A CI step that fails when a file under `references/`, `scripts/` or `assets/` is not linked from `SKILL.md`. The condition beside the link is review-only. Orbit: none yet. Every adopter: `sh docs/standards/check.sh`, vendored by `.orbit-plugin/lib/sync-standards.sh`. |
| R9 | review-only. The shared prefix of a split subject is checked by the reviewer of the split. |
| R10 | The R8 gate covers "one level deep". A CI step that fails when a reference of more than 100 lines has no list of links to its own headings in its first 30 lines. Orbit: none yet. Every adopter: `sh docs/standards/check.sh`, vendored by `.orbit-plugin/lib/sync-standards.sh`. |
| R11 | A CI step that fails when a bullet, step or paragraph outside code fences and tables exceeds 500 characters. The rest is review-only. Orbit: none yet. Every adopter: `sh docs/standards/check.sh`, vendored by `.orbit-plugin/lib/sync-standards.sh`. |
| R12 | review-only. |
| R13 | review-only. |
| R14 | review-only. |
| R15 | review-only. |
| R16 | A CI step that fails when `SKILL.md` exceeds 16,000 bytes or 500 lines, or a file in `references/` exceeds 16,000 bytes. Orbit: none yet. Every adopter: `sh docs/standards/check.sh`, vendored by `.orbit-plugin/lib/sync-standards.sh`. |
| R17 | A CI link check over each skill that resolves relative links and backticked repository paths. Orbit: `scripts/check-doc-links.py` in `scripts/ci-guardrails.sh` covers Markdown links. That a command or flag exists is review-only. Every adopter: `sh docs/standards/check.sh`, vendored by `.orbit-plugin/lib/sync-standards.sh`. |
| R18 | review-only, at two points: the task author when listing docs to update, and the reviewer of a diff that changes a command a skill names. |
| R19 | A CI step that fails when a skill has no `evals/` file with at least three cases. Running them is review-only. The umbrella: `.claude/skills/constellation-standards/evals/`. Orbit: none yet. Every adopter: `sh docs/standards/check.sh`, vendored by `.orbit-plugin/lib/sync-standards.sh`. |
