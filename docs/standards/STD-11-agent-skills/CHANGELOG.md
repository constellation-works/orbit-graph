# Changelog — STD-11 Agent skills

Version history of [STD-11](STD-11.md). Adopting repos pin the `version:` in its frontmatter
as `STD-11@<version>`; a version changes only when a rule is added, removed or changed in
meaning.

- **v1 (2026-11-22):** initial publication as a draft, R1–R19. From the owner's direction in
  the chief's chat on 2026-11-22, with the published skill specification and authoring
  guidance read that day, and the skills of the constellation repositories measured that
  day, as the base. The owner approved the draft the same day with one addition, now in
  §R9: "sounds good but one thing: when we break up large reference doc, e.g. configuration
  reference, I would like all of its resulting files to have the same prefix, e.g.
  configuration-<section>.md."
- **Editorial (2026-11-22):** §R8 names the three directories whose files must be linked,
  so that evaluation fixtures under `evals/` are not counted. No version change.
- **Editorial (2026-11-23, version unchanged):** STD-11 checks.md R1, R3, R4, R8, R10, R11, R16, R17 and R19 name the vendored gate (`sh docs/standards/check.sh`, ORB-27134). No version change (ORB-27155).
- **v2 (2026-11-23):** changes R3: a skill an Orbit plugin ships is named `<namespace>-<directory>`, the name Orbit installs it under; any other skill keeps its directory name. From the owner's decision dr-896fcf20bb45f09b (option 1, "whatever captain recommends", 2026-11-23) (ORB-27142, ORB-27272).
- **Editorial (2026-11-23, version unchanged):** the frontmatter drops `status`, which nothing read (ORB-27289).
