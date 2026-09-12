# Contributing

Use Rust 1.89 or newer. The repository is a virtual Cargo workspace:
`crates/orbit-graph` is the library, `crates/orbit-graph-cli` builds the
`orbit-graph` executable, and `crates/orbit-graph-explorer` builds the change
explorer. Keep the workspace standalone: do not add path dependencies on an
Orbit checkout, Orbit runtime configuration, or private crates.

Before opening a pull request, run:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
cargo build --workspace --locked
git diff --check
```

Changes to extraction or storage compatibility may require incrementing
`EXTRACTOR_VERSION`, which intentionally selects a fresh database. Add focused
fixtures for new syntax, query behavior, and CLI changes. CLI tests must invoke
the real built binary and assert both JSON output and failure exit behavior;
they live in `crates/orbit-graph-cli/tests`, because `orbit-graph-cli` has no
library target. In-crate test modules live in a `tests/` directory with a
`mod.rs`.

When recovering more historical code, update `PROVENANCE.md` with an immutable
commit, original path, license, and the exact adaptation boundary.
