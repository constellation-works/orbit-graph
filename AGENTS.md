# Agent guidance

`orbit-graph` is a standalone Cargo workspace that builds a local SQLite index of a source tree and answers graph queries through a Rust library and a CLI. `CLAUDE.md` imports this file, so Claude Code loads these rules; edit them here only.

## Rules

- Keep the workspace standalone and independent of Orbit control-plane crates, configuration, and runtime state: [CONTRIBUTING.md](CONTRIBUTING.md) lists the dependencies it forbids.
- Keep graph scratch files under .orbit-graph/, the git-ignored runtime directory that [docs/usage.md](docs/usage.md) describes under "Index lifecycle and location".
- Preserve the behavior and tests of recovered code. Historical code recovery requirements are in [CONTRIBUTING.md](CONTRIBUTING.md) and [PROVENANCE.md](PROVENANCE.md).
- Leave releases, tags, publishing, and agent-review automation to maintainers.

## Gates

- Before handoff, run `make ci`, which runs the full validation sequence in [CONTRIBUTING.md](CONTRIBUTING.md).

## Code

- Prefer focused changes over broad refactors.
- Do not suppress broad lint categories to make validation pass.
- Crate roles and the allowed dependency edges are in [ARCHITECTURE.md](ARCHITECTURE.md), which names the gate that enforces them.

## Tests

- CLI tests: the rule that they run the real built executable, and where they live, is in [CONTRIBUTING.md](CONTRIBUTING.md).

<!-- constellation-standards:begin -->
<!-- Managed by the constellation's operations/scripts/sync-standards.sh; edits inside this block are overwritten. -->
## Constellation standards

This repository adopts these constellation standards, vendored read-only in `docs/standards/` (each a directory: `STD-nn.md` holds the rules, `why.md` and `checks.md` the reasons and gates):

- `STD-01@4` — [docs/standards/STD-01-cli-surface/STD-01.md](docs/standards/STD-01-cli-surface/STD-01.md): CLI surface — command grammar, inputs and effects, output modes, errors, help, and golden-tested public surfaces
- `STD-02@8` — [docs/standards/STD-02-rust-architecture-and-errors/STD-02.md](docs/standards/STD-02-rust-architecture-and-errors/STD-02.md): Rust architecture and errors — layering, visibility, typed errors, failure semantics, idiom and naming, logging, and…
- `STD-03@2` — [docs/standards/STD-03-concurrency-and-process-safety/STD-03.md](docs/standards/STD-03-concurrency-and-process-safety/STD-03.md): Concurrency and process safety — locks, channels, durable writes, state evolution, subprocess lifecycles, destructive…
- `STD-04@2` — [docs/standards/STD-04-testing-and-verification/STD-04.md](docs/standards/STD-04-testing-and-verification/STD-04.md): Testing and verification — test design, hermetic tests, honest gates, docs as contracts, and evidence-based…
- `STD-05@1` — [docs/standards/STD-05-security-boundaries/STD-05.md](docs/standards/STD-05-security-boundaries/STD-05.md): Security boundaries — authority, filesystem containment, child environments, redaction, local network surfaces, supply…
- `STD-11@3` — [docs/standards/STD-11-agent-context/STD-11.md](docs/standards/STD-11-agent-context/STD-11.md): Agent context files — skills and instruction files loaded by an agent harness

Follow them; they are normative. To deviate from a rule, record a decision in `docs/design/<feature>/4_decisions.md` citing `STD-nn@<version> §Rn`; never edit `docs/standards/` (`sh docs/standards/check.sh` enforces this).
Reviewers check every change against the adopted standards and report violations as `STD-nn §Rn` with file:line evidence.
<!-- constellation-standards:end -->
