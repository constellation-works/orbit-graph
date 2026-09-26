#!/usr/bin/env bash
# scripts/check-dependency-direction.sh — STD-02 §R1–§R7 and §R9 for this
# workspace, from the Cargo.toml files alone (no build, no network, no jq).
#
# 1. Crate edges. Every workspace crate needs a policy below. A crate may
#    depend only on the internal crates in its ALLOWED list, and never on the
#    external crates in its BANNED list. Dev-dependencies are exempt. A crate
#    without a policy fails the check, so a new member forces a decision.
# 2. The layer table in ARCHITECTURE.md states the same policy, row for row
#    (STD-02 §R6: a new edge changes this script and that doc together).
# 3. A third-party dependency used by more than one member is declared once in
#    [workspace.dependencies] (STD-02 §R9).
#
# Manifests must keep one table per section: a dependency in dotted-key form
# (`dependencies.foo = ...`) is not seen.
set -euo pipefail
cd "$(dirname "$0")/.."

PREFIX="orbit-graph"
ARCHITECTURE="ARCHITECTURE.md"
fail=0
err() { echo "dependency-direction: $*" >&2; fail=1; }

# policy <crate> -> sets KIND, ALLOWED (internal deps) and BANNED (external deps)
policy() {
  case "$1" in
    orbit-graph)
      KIND="library"
      ALLOWED=""
      # The domain library never parses arguments, draws on a terminal,
      # installs a log subscriber or serves HTTP: those belong to surfaces.
      BANNED="clap tracing-subscriber tiny_http unicode-width"
      ;;
    orbit-graph-cli)
      KIND="binary \`orbit-graph\`"
      ALLOWED="orbit-graph"
      BANNED=""
      ;;
    orbit-graph-explorer)
      KIND="library and binary \`orbit-graph-explorer\`"
      ALLOWED="orbit-graph"
      BANNED=""
      ;;
    *) return 1 ;;
  esac
}

# Print the dependency names declared in the tables of a Cargo.toml whose
# header matches the extended regex $2.
deps_in() {
  TABLES="$2" awk '
    /^\[/ { in_deps = ($0 ~ ENVIRON["TABLES"]); next }
    in_deps && /^[A-Za-z0-9_-]+[[:space:]]*(\.workspace)?[[:space:]]*=/ {
      line = $0
      name = $1; sub(/\.workspace$/, "", name); sub(/=.*/, "", name)
      gsub(/[[:space:]]/, "", name)
      inherited = (line ~ /^[A-Za-z0-9_-]+[[:space:]]*\.workspace[[:space:]]*=/ ||
                   line ~ /workspace[[:space:]]*=[[:space:]]*true/)
      print name, (inherited ? "workspace" : "local")
    }
  ' "$1"
}
NORMAL_TABLES='^\[(target\..*\.)?(build-)?dependencies\]$'
ALL_TABLES='^\[(target\..*\.)?(build-|dev-)?dependencies\]$'

contains() { # contains <word> <space-separated list>
  local word="$1" item
  for item in $2; do [[ "$item" == "$word" ]] && return 0; done
  return 1
}

# `a`, `b` for a non-empty list, an em dash for an empty one.
cell() {
  local out="" item
  for item in $1; do out+="${out:+, }\`$item\`"; done
  printf '%s' "${out:-—}"
}

manifests=(crates/*/Cargo.toml)
if [[ ! -e "${manifests[0]}" ]]; then
  echo "dependency-direction: no crates/*/Cargo.toml found; nothing was checked" >&2
  exit 1
fi
[[ -f "$ARCHITECTURE" ]] || err "$ARCHITECTURE is missing; it holds the layer table this script enforces"

local_decls=""  # "<dep> <crate>" lines: third-party deps not inherited from the workspace
for manifest in "${manifests[@]}"; do
  crate=$(awk -F'"' '/^name[[:space:]]*=/ { print $2; exit }' "$manifest")

  while read -r dep origin; do
    [[ "$dep" == "$PREFIX" || "$dep" == "$PREFIX"-* ]] && continue
    [[ "$origin" == "local" ]] && local_decls+="$dep $crate"$'\n'
  done < <(deps_in "$manifest" "$ALL_TABLES")

  if ! policy "$crate"; then
    err "crate '$crate' ($manifest) has no policy; add one here and a row in $ARCHITECTURE"
    continue
  fi
  while read -r dep _origin; do
    if [[ "$dep" == "$PREFIX" || "$dep" == "$PREFIX"-* ]]; then
      contains "$dep" "$ALLOWED" || err "$crate must not depend on internal crate $dep"
    elif contains "$dep" "$BANNED"; then
      err "$crate must not depend on $dep (a surface crate in the domain)"
    fi
  done < <(deps_in "$manifest" "$NORMAL_TABLES")

  if [[ -f "$ARCHITECTURE" ]]; then
    row="| \`$crate\` | $KIND | $(cell "$ALLOWED") | $(cell "$BANNED") |"
    grep -qxF "$row" "$ARCHITECTURE" \
      || err "$ARCHITECTURE has no crate row matching this policy; expected: $row"
  fi
done

while read -r dep users; do
  err "$dep is declared separately by $users; declare it once in [workspace.dependencies] (STD-02 §R9)"
done < <(printf '%s' "$local_decls" | sort -u | awk '
  NF { n[$1]++; users[$1] = users[$1] (users[$1] == "" ? "" : ", ") $2 }
  END { for (d in n) if (n[d] > 1) print d, users[d] }' | sort)

if [[ "$fail" -eq 0 ]]; then
  echo "dependency-direction: ok (${#manifests[@]} crates)"
fi
exit "$fail"
