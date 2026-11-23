# Agent guidance

orbit-graph is a standalone Cargo workspace that builds a local SQLite index of a source tree, answers graph queries from a library and the `orbit-graph` CLI, and provides change analysis. `CLAUDE.md` imports this file, so edit only `AGENTS.md`.

## Rules

- Keep the workspace independent of Orbit control-plane crates, configuration and runtime state; [CONTRIBUTING.md](CONTRIBUTING.md) names the forbidden dependencies.
- Graph scratch files belong under `.orbit-graph/`, which git ignores; [docs/usage.md](docs/usage.md) describes its layout.
- Preserve the behavior and tests of recovered code.
- Record newly recovered code in [PROVENANCE.md](PROVENANCE.md), as [CONTRIBUTING.md](CONTRIBUTING.md) describes.
- Leave releases, tags, publishing, and agent-review automation to maintainers.

## Gates

- Before opening a pull request, run `make ci`, which runs the full validation sequence that [CONTRIBUTING.md](CONTRIBUTING.md) lists.

## Code

- Prefer focused changes over broad refactors.
- Do not suppress broad lint categories to make validation pass.
- Layers and crate dependency edges are in [ARCHITECTURE.md](ARCHITECTURE.md), which also names the gates that enforce them.

## Tests

- Exercise CLI behavior through the real `orbit-graph` executable; [CONTRIBUTING.md](CONTRIBUTING.md) says where those tests live.

<!-- constellation-standards:begin -->
<!-- Managed by the constellation's operations/scripts/sync-standards.sh; edits inside this block are overwritten. -->
## Constellation standards

This repository adopts these constellation standards, vendored read-only in `docs/standards/` (each a directory: `STD-nn.md` holds the rules, `why.md` and `checks.md` the reasons and gates):

- `STD-01@2` — [docs/standards/STD-01-cli-surface/STD-01.md](docs/standards/STD-01-cli-surface/STD-01.md)
- `STD-02@4` — [docs/standards/STD-02-rust-architecture-and-errors/STD-02.md](docs/standards/STD-02-rust-architecture-and-errors/STD-02.md)
- `STD-03@2` — [docs/standards/STD-03-concurrency-and-process-safety/STD-03.md](docs/standards/STD-03-concurrency-and-process-safety/STD-03.md)
- `STD-04@2` — [docs/standards/STD-04-testing-and-verification/STD-04.md](docs/standards/STD-04-testing-and-verification/STD-04.md)
- `STD-05@1` — [docs/standards/STD-05-security-boundaries/STD-05.md](docs/standards/STD-05-security-boundaries/STD-05.md)

Follow them; they are normative. To deviate from a rule, record a decision in `docs/design/<feature>/4_decisions.md` citing `STD-nn@<version> §Rn`; never edit `docs/standards/` (`sh docs/standards/check.sh` enforces this).
Reviewers check every change against the adopted standards and report violations as `STD-nn §Rn` with file:line evidence.
<!-- constellation-standards:end -->
