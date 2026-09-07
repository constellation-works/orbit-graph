# Agent guidance

- This repository is the standalone `orbit-graph` Rust crate and JSON CLI.
- Keep it independent of Orbit control-plane crates, configuration, and runtime
  state. Graph scratch files belong under `.orbit-graph/`.
- Preserve recovered behavior and tests. Record any additional historical code
  in `PROVENANCE.md` with an immutable commit and original path.
- Prefer focused changes over broad refactors. Do not suppress broad lint
  categories to make validation pass.
- Run the complete validation sequence in `CONTRIBUTING.md`; CLI behavior must
  be exercised through the real executable.
- Leave releases, tags, publishing, and agent-review automation to maintainers.
