# Contributing

Use Rust 1.89 or newer. The repository is a virtual Cargo workspace:
`crates/orbit-graph-extract` holds the tree-sitter language extractors and the
Git history extraction, `crates/orbit-graph` is the library built on it,
`crates/orbit-graph-cli` builds the `orbit-graph` executable, and
`crates/orbit-graph-changes` provides change analysis as a library. Keep the workspace standalone: do not add path dependencies on an
Orbit checkout, Orbit runtime configuration, or private crates.

Before opening a pull request, run:

```sh
sh docs/standards/check.sh
scripts/check-dependency-direction.sh
scripts/check-terminal-guard.sh
scripts/check-orphan-modules.sh
scripts/test-repo-gates.sh
python3 -B docs/evaluation/agent-navigation/eval.py check
cargo deny --locked check
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo nextest run --workspace --locked --no-tests=fail
cargo test --workspace --doc --locked
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
cargo build --workspace --locked
git diff --check
```

`make ci` runs the same sequence. The paired agent-evaluation smoke gate uses
Python 3.10+ and its standard library; it validates frozen fixture truth,
negative cases and scripted artifact parity, not agent effectiveness.
`cargo deny` needs cargo-deny 0.19.9
(`cargo install cargo-deny --version 0.19.9 --locked`); CI installs that
release from a SHA-256-pinned download. Tests run under cargo-nextest 0.9.146
(`cargo install cargo-nextest --version 0.9.146 --locked`), which CI also
installs from a SHA-256-pinned download; `.config/nextest.toml` terminates a
test that hangs, fails one whose child processes outlive it, and configures no
retries, so fix a flaky test instead of rerunning it. Fixture `git` commands go
through the crate's config-isolated `git_command` test helper, never a bare
`Command::new("git")`. The layer model and what each
`scripts/check-*.sh` gate enforces are in [ARCHITECTURE.md](ARCHITECTURE.md):
a new crate or internal edge changes that file and
`scripts/check-dependency-direction.sh` together, and a new stream writer
outside an output layer needs a per-site allow-list entry in
`scripts/check-terminal-guard.sh` (file, line pattern, hit count) with its
reason and the task that removes it; the change that removes the write
removes the entry, or the guard fails.

The committed `direct-call` JSON sample under
`docs/evaluation/change-explorer/samples/` is checked against the change
analysis library. After an intended report output change (including an
`EXTRACTOR_VERSION` or crate version bump), regenerate it with
`UPDATE_GOLDENS=1 cargo test -p orbit-graph-changes --test derived_artifacts --locked`
and review the diff.

The canonical Orbit plugin lives entirely under `.orbit-plugin/`. Its
`plugin.yaml` refers to the launcher, schemas, skill, definitions, and
conformance tests relative to that directory. `plugin_contract` checks that
those paths resolve and that the plugin tree contains no symlinks. The legacy
`orbit tool add` sidecars remain under `scripts/legacy-plugin/` for the
deprecated installer; they are not part of the installed plugin.

Three tests register the plugin with a real Orbit and are `#[ignore]`d in the
normal run; CI's `plugin-conformance` job runs them against the pinned Orbit
release. These cover the legacy sidecars and the canonical v2 plugin's complete
CLI and MCP surfaces. Each creates an isolated Orbit root; the v2 test also
uses a private HOME. Run them locally as

```sh
ORBIT_GRAPH_TEST_ORBIT_BIN=/absolute/path/to/orbit \
  cargo test -p orbit-graph-cli --test plugin_integration --test plugin_v2 --locked -- --ignored
```

With `--ignored` and no `ORBIT_GRAPH_TEST_ORBIT_BIN` they fail rather than
pass vacuously.

Changes must follow the constellation standards vendored in
[`docs/standards/`](docs/standards/README.md); cite a rule as `STD-nn §Rn`.
Those files are read-only: never edit them, re-sync instead.

Changes to extraction or storage compatibility may require incrementing
`EXTRACTOR_VERSION`, which intentionally selects a fresh database. Add focused
fixtures for new syntax, query behavior, and CLI changes. CLI tests must invoke
the real built binary and assert both JSON output and failure exit behavior;
they live in `crates/orbit-graph-cli/tests`, because `orbit-graph-cli` has no
library target. In-crate test modules live in a `tests/` directory with a
`mod.rs`.

When recovering more historical code, update `PROVENANCE.md` with an immutable
commit, original path, license, and the exact adaptation boundary.
