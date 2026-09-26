# Contributing

Use Rust 1.89 or newer. The repository is a virtual Cargo workspace:
`crates/orbit-graph-extract` holds the tree-sitter language extractors and the
Git history extraction, `crates/orbit-graph` is the library built on it,
`crates/orbit-graph-cli` builds the `orbit-graph` executable, and
`crates/orbit-graph-explorer` builds the change explorer. Keep the workspace standalone: do not add path dependencies on an
Orbit checkout, Orbit runtime configuration, or private crates.

Before opening a pull request, run:

```sh
sh docs/standards/check.sh
scripts/check-dependency-direction.sh
scripts/check-terminal-guard.sh
scripts/check-orphan-modules.sh
scripts/test-repo-gates.sh
cargo deny --locked check
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
cargo build --workspace --locked
git diff --check
```

`make ci` runs the same sequence. `cargo deny` needs cargo-deny 0.19.9
(`cargo install cargo-deny --version 0.19.9 --locked`); CI installs that
release from a SHA-256-pinned download. The layer model and what each
`scripts/check-*.sh` gate enforces are in [ARCHITECTURE.md](ARCHITECTURE.md):
a new crate or internal edge changes that file and
`scripts/check-dependency-direction.sh` together, and a new stream writer
outside an output layer needs a per-site allow-list entry in
`scripts/check-terminal-guard.sh` (file, line pattern, hit count) with its
reason and the task that removes it; the change that removes the write
removes the entry, or the guard fails.

Some committed files are derived from the binaries and checked by tests: the
explorer README's "Every flag" block and the `direct-call` sample export under
`docs/evaluation/change-explorer/samples/`. After an intended change to the
explorer's help text or report output (including an `EXTRACTOR_VERSION` or
crate version bump), regenerate them in the same change with
`UPDATE_GOLDENS=1 cargo test -p orbit-graph-explorer --test derived_artifacts --locked`
and review the diff.

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
