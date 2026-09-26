#!/usr/bin/env bash
# scripts/test-repo-gates.sh — STD-04 §R12: a gate that cannot fail proves
# nothing. Each repository gate script runs against a scratch copy of this
# tree, first unchanged (it must pass), then with one seeded violation at a
# time (it must fail and name the violation), then with nothing to check (it
# must fail rather than pass on zero inputs).
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$PWD

scratch=$(mktemp -d "${TMPDIR:-/tmp}/orbit-graph-gates.XXXXXX")
trap 'rm -rf "$scratch"' EXIT

passed=0
failed=0
case_dir=""

# fresh_copy: a scratch copy of the files the gates read.
fresh_copy() {
  case_dir="$scratch/case$((passed + failed))"
  mkdir -p "$case_dir/scripts" "$case_dir/crates"
  cp "$ROOT"/scripts/check-*.sh "$case_dir/scripts/"
  cp "$ROOT/ARCHITECTURE.md" "$case_dir/"
  local crate
  for crate in "$ROOT"/crates/*/; do
    crate=$(basename "$crate")
    mkdir -p "$case_dir/crates/$crate"
    cp "$ROOT/crates/$crate/Cargo.toml" "$case_dir/crates/$crate/"
    cp -R "$ROOT/crates/$crate/src" "$case_dir/crates/$crate/"
  done
}

# expect <pass|fail> <gate script> <description> [<regex the output must match>]
expect() {
  local want="$1" gate="$2" what="$3" pattern="${4:-}" out status
  set +e
  out=$(bash "$case_dir/scripts/$gate" 2>&1)
  status=$?
  set -e
  local ok=1
  if [[ "$want" == "pass" && "$status" -ne 0 ]]; then ok=0; fi
  if [[ "$want" == "fail" && "$status" -eq 0 ]]; then ok=0; fi
  if [[ "$ok" -eq 1 && -n "$pattern" ]] && ! grep -qE -- "$pattern" <<<"$out"; then ok=0; fi
  if [[ "$ok" -eq 1 ]]; then
    passed=$((passed + 1))
    echo "ok   $gate: $what"
  else
    failed=$((failed + 1))
    echo "FAIL $gate: $what (expected $want${pattern:+ matching /$pattern/}, exit $status)" >&2
    sed 's/^/     | /' <<<"$out" >&2
  fi
}

# append <file> <text>: add a line to a copied file.
append() { printf '%s\n' "$2" >>"$case_dir/$1"; }

# add_dep <crate dir> <dependency line>: put a line at the top of [dependencies].
add_dep() {
  local manifest="$case_dir/crates/$1/Cargo.toml"
  awk -v line="$2" '{ print } $0 == "[dependencies]" { print line }' "$manifest" >"$manifest.new"
  mv "$manifest.new" "$manifest"
}

# --- dependency direction (STD-02 §R1–§R7, §R9) ------------------------------
G=check-dependency-direction.sh
fresh_copy; expect pass $G "the current tree"
fresh_copy; add_dep orbit-graph 'clap = "4.5"'
expect fail $G "clap in the domain library" 'orbit-graph must not depend on clap'
fresh_copy; add_dep orbit-graph 'tracing-subscriber = "0.3"'
expect fail $G "tracing-subscriber in the domain library" 'orbit-graph must not depend on tracing-subscriber'
fresh_copy; add_dep orbit-graph 'tiny_http = "0.12"'
expect fail $G "tiny_http in the domain library" 'orbit-graph must not depend on tiny_http'
fresh_copy; add_dep orbit-graph 'orbit-graph-cli = { path = "../orbit-graph-cli" }'
expect fail $G "the library depending on the CLI" 'orbit-graph must not depend on internal crate orbit-graph-cli'
fresh_copy; add_dep orbit-graph-cli 'orbit-graph-explorer = { path = "../orbit-graph-explorer" }'
expect fail $G "an unlisted internal edge between surfaces" 'orbit-graph-cli must not depend on internal crate orbit-graph-explorer'
fresh_copy; mkdir -p "$case_dir/crates/orbit-graph-new"
printf '[package]\nname = "orbit-graph-new"\n\n[dependencies]\n' >"$case_dir/crates/orbit-graph-new/Cargo.toml"
expect fail $G "a crate with no policy" "crate 'orbit-graph-new' .* has no policy"
fresh_copy; sed -i.bak 's/^| `orbit-graph` | library | — | .*$/| `orbit-graph` | library | — | `clap` |/' "$case_dir/ARCHITECTURE.md"
expect fail $G "ARCHITECTURE.md disagreeing with the policy" 'ARCHITECTURE.md has no crate row matching'
fresh_copy; rm "$case_dir/ARCHITECTURE.md"
expect fail $G "a missing ARCHITECTURE.md" 'ARCHITECTURE.md is missing'
fresh_copy; add_dep orbit-graph 'itoa = "1"'; add_dep orbit-graph-explorer 'itoa = "1"'
expect fail $G "a third-party dependency declared by two members" 'itoa is declared separately by'
fresh_copy; printf '\n[dev-dependencies]\nclap = { workspace = true }\n' >>"$case_dir/crates/orbit-graph/Cargo.toml"
expect pass $G "a banned crate as a dev-dependency only"
fresh_copy; rm -rf "$case_dir"/crates/*
expect fail $G "an empty tree" 'nothing was checked'

# --- terminal guard (STD-02 §R15) --------------------------------------------
G=check-terminal-guard.sh
fresh_copy; expect pass $G "the current tree"
fresh_copy; append crates/orbit-graph/src/evaluation.rs 'fn seeded() { println!("x"); }'
expect fail $G "println! in the domain library" 'crates/orbit-graph/src/evaluation.rs:[0-9]+:'
fresh_copy; append crates/orbit-graph-explorer/src/cache.rs 'fn seeded() { let _ = std::io::stderr(); }'
expect fail $G "io::stderr in the explorer library" 'crates/orbit-graph-explorer/src/cache.rs:[0-9]+:'
fresh_copy; append crates/orbit-graph-cli/src/command/search.rs 'fn seeded() -> bool { std::io::IsTerminal::is_terminal(&std::io::stdout()) }'
expect fail $G "a TTY check in a CLI command module" 'crates/orbit-graph-cli/src/command/search.rs:[0-9]+:'
fresh_copy; append crates/orbit-graph-cli/src/command/search.rs 'fn seeded() { dbg!(1); }'
expect fail $G "dbg! outside the output layer" 'crates/orbit-graph-cli/src/command/search.rs:[0-9]+:'
fresh_copy; append crates/orbit-graph/src/evaluation.rs '// println!("only a comment")'
expect pass $G "a comment that mentions println!"
fresh_copy; append crates/orbit-graph-cli/src/output/render.rs 'fn seeded() { let _ = std::io::stdout(); }'
expect pass $G "io::stdout inside the CLI output layer"
fresh_copy; rm "$case_dir/crates/orbit-graph-explorer/src/service.rs"
expect fail $G "an allow-list entry whose file is gone" 'allow-list entry for missing file crates/orbit-graph-explorer/src/service.rs'
fresh_copy; rm -rf "$case_dir"/crates/*
expect fail $G "an empty tree" 'nothing was checked'

# --- orphan modules (STD-02 §R19) --------------------------------------------
G=check-orphan-modules.sh
fresh_copy; expect pass $G "the current tree"
fresh_copy; printf '#[test]\nfn seeded() {}\n' >"$case_dir/crates/orbit-graph-cli/src/tests/seeded.rs"
expect fail $G "an undeclared test file" 'src/tests/seeded.rs is not declared'
fresh_copy; sed -i.bak '/^mod rust;$/d' "$case_dir/crates/orbit-graph/src/extract/languages/tests/mod.rs"
expect fail $G "a test file whose mod line was removed" 'languages/tests/rust.rs is not declared'
fresh_copy; sed -i.bak '/^#\[path = "tests\/java.rs"\]$/d' "$case_dir/crates/orbit-graph/src/extract/languages/java.rs"
expect fail $G "a #[path] test file whose attribute was removed" 'languages/tests/java.rs is not declared'
fresh_copy; sed -i.bak '/^mod tests;$/d' "$case_dir/crates/orbit-graph-cli/src/main.rs"
expect fail $G "a tests/ directory its crate root does not declare" 'orbit-graph-cli/src/tests is not declared by its parent'
fresh_copy; mkdir -p "$case_dir/crates/orbit-graph/src/store/history/tests/nested"
expect fail $G "a nested directory inside tests/" 'store/history/tests/nested is a directory inside'
fresh_copy; rm "$case_dir/crates/orbit-graph-cli/src/output/tests/mod.rs"
expect fail $G "a tests/ directory without mod.rs" 'output/tests has no mod.rs'
fresh_copy; rm -rf "$case_dir"/crates/*
expect fail $G "an empty tree" 'nothing was checked'

total=$((passed + failed))
if [[ "$failed" -ne 0 ]]; then
  echo "repo-gates self-test: $failed of $total cases failed" >&2
  exit 1
fi
echo "repo-gates self-test: ok ($total cases)"
