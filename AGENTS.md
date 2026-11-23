# Agent guidance

`orbit-graph` is a standalone Cargo workspace that builds a local SQLite index of a source tree and answers graph queries through a Rust library and a CLI. `CLAUDE.md` imports this file, so Claude Code loads these rules; edit them here only.

## Rules

- Dependency policy: see the opening paragraph of [CONTRIBUTING.md](CONTRIBUTING.md).
- Scratch-state location: see [Index lifecycle and location](docs/usage.md#index-lifecycle-and-location).
- Preserve the behavior and tests of recovered code. Historical-code record requirements are in the final paragraph of [CONTRIBUTING.md](CONTRIBUTING.md) and [PROVENANCE.md](PROVENANCE.md).
- Leave releases, tags, publishing, and agent-review automation to maintainers.

## Gates

- Handoff validation sequence: see [CONTRIBUTING.md](CONTRIBUTING.md); `make ci` runs it.

## Code

- Prefer focused changes over broad refactors.
- Do not suppress broad lint categories to make validation pass.
- Crate roles and allowed dependency edges: see [Crates](ARCHITECTURE.md#crates), which names the enforcing gate.

## Tests

- CLI test invocation and location: see the CLI testing paragraph of [CONTRIBUTING.md](CONTRIBUTING.md).

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
