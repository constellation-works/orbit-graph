#!/usr/bin/env bash
# scripts/check-orphan-modules.sh — STD-02 §R19.
#
# Unit tests live in `<module>/tests/<source_file>.rs`, declared by that
# directory's `mod.rs`, which the parent module declares with `mod tests;`.
# A test file may instead be declared by its sibling source file,
# `<module>/<name>.rs`, as `#[path = "tests/<name>.rs"] mod tests;` (the form
# most of `crates/orbit-graph-extract` uses today); that compiles it just the same.
# An undeclared file or directory compiles to nothing, so its tests silently
# never run. This fails on:
#   - a `.rs` file under `crates/*/src/**/tests/` that its `mod.rs` does not
#     declare (`mod <name>;`);
#   - a `tests/` directory under `crates/*/src/` with no `mod.rs`;
#   - a `tests/` directory whose parent module (`lib.rs`/`main.rs` for
#     `src/tests/`, else `<dir>/mod.rs` or `<dir>.rs`) does not declare
#     `mod tests;`.
set -euo pipefail
cd "$(dirname "$0")/.."

fail=0
err() { echo "orphan-modules: $*" >&2; fail=1; }

# declares <file> <module name>: the file has a `mod <name>;` item, with any
# visibility and attributes before it on the same line.
declares() {
  grep -qE "^[[:space:]]*(#\[[^]]*\][[:space:]]*)*(pub(\([^)]*\))?[[:space:]]+)?mod[[:space:]]+$2[[:space:]]*;" "$1"
}

# path_declares <source file> <test file name>: the source file has
# `#[path = "tests/<name>.rs"]` directly on a `mod ...;` item (other
# attributes may sit between them).
path_declares() {
  [[ -f "$1" ]] || return 1
  awk -v want="#[path = \"tests/$2.rs\"]" '
    { line = $0; gsub(/^[[:space:]]+|[[:space:]]+$/, "", line) }
    pending && line ~ /^#\[/ { next }
    pending && line ~ /^(pub(\([^)]*\))?[[:space:]]+)?mod[[:space:]]+[A-Za-z0-9_]+[[:space:]]*;$/ { found = 1; exit }
    { pending = (line == want) }
    END { exit !found }
  ' "$1"
}

dirs=""
for src in crates/*/src; do
  [[ -d "$src" ]] || continue
  dirs+=$(find "$src" -type d -name tests | sort)$'\n'
done
dirs=$(sed '/^$/d' <<<"$dirs")
if [[ -z "$dirs" ]]; then
  echo "orphan-modules: no crates/*/src/**/tests directories found; nothing was checked" >&2
  exit 1
fi

checked=0
while IFS= read -r dir; do
  if [[ ! -f "$dir/mod.rs" ]]; then
    err "$dir has no mod.rs, so nothing in it is compiled"
    continue
  fi

  parent_dir=$(dirname "$dir")
  if [[ "$(basename "$parent_dir")" == "src" ]]; then
    parents=("$parent_dir/lib.rs" "$parent_dir/main.rs")
  else
    parents=("$parent_dir/mod.rs" "$parent_dir.rs")
  fi
  declared=0
  for parent in "${parents[@]}"; do
    [[ -f "$parent" ]] && declares "$parent" tests && declared=1
  done
  [[ "$declared" -eq 1 ]] || err "$dir is not declared by its parent module (${parents[*]}); add \`#[cfg(test)] mod tests;\`"

  while IFS= read -r file; do
    name=$(basename "$file" .rs)
    [[ "$name" == "mod" ]] && continue
    checked=$((checked + 1))
    declares "$dir/mod.rs" "$name" || path_declares "$parent_dir/$name.rs" "$name" \
      || err "$file is not declared in $dir/mod.rs (or by $parent_dir/$name.rs with #[path]); add \`mod $name;\`"
  done < <(find "$dir" -maxdepth 1 -type f -name '*.rs' | sort)

  while IFS= read -r nested; do
    err "$nested is a directory inside a tests/ module; flatten it into $dir/<file>.rs"
  done < <(find "$dir" -mindepth 1 -type d | sort)
done <<<"$dirs"

if [[ "$fail" -eq 0 ]]; then
  echo "orphan-modules: ok ($(wc -l <<<"$dirs" | tr -d ' ') tests/ directories, $checked files)"
fi
exit "$fail"
