# Agent guidance

- This repository is a standalone Cargo workspace: `crates/orbit-graph-extract`
  (the language extractors and Git history extraction), `crates/orbit-graph`
  (the library), `crates/orbit-graph-cli` (the `orbit-graph` JSON CLI), and
  `crates/orbit-graph-explorer` (the change explorer).
- Keep it independent of Orbit control-plane crates, configuration, and runtime
  state. Graph scratch files belong under `.orbit-graph/`.
- Preserve recovered behavior and tests. Record any additional historical code
  in `PROVENANCE.md` with an immutable commit and original path.
- Prefer focused changes over broad refactors. Do not suppress broad lint
  categories to make validation pass.
- Run the complete validation sequence in `CONTRIBUTING.md`; CLI behavior must
  be exercised through the real executable.
- Leave releases, tags, publishing, and agent-review automation to maintainers.

<!-- constellation-standards:begin -->
<!-- Managed by the constellation's operations/scripts/sync-standards.sh; edits inside this block are overwritten. -->
## Constellation standards

This repository adopts these constellation standards, vendored read-only in `docs/standards/`:

- `STD-01@2` — [docs/standards/STD-01-cli-surface.md](docs/standards/STD-01-cli-surface.md)
- `STD-02@2` — [docs/standards/STD-02-rust-architecture-and-errors.md](docs/standards/STD-02-rust-architecture-and-errors.md)
- `STD-03@2` — [docs/standards/STD-03-concurrency-and-process-safety.md](docs/standards/STD-03-concurrency-and-process-safety.md)
- `STD-04@1` — [docs/standards/STD-04-testing-and-verification.md](docs/standards/STD-04-testing-and-verification.md)
- `STD-05@1` — [docs/standards/STD-05-security-boundaries.md](docs/standards/STD-05-security-boundaries.md)

Follow them; they are normative. To deviate from a rule, record a decision in `docs/design/<feature>/4_decisions.md` citing `STD-nn@<version> §Rn`; never edit `docs/standards/` (`sh docs/standards/check.sh` enforces this).
Reviewers check every change against the adopted standards and report violations as `STD-nn §Rn` with file:line evidence.
<!-- constellation-standards:end -->
