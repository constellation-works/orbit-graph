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
# The parser fails closed. It accepts only the conventional shape — one
# `[dependencies]`-style table per section, each dependency a bare
# `name = ...` or `name.workspace = true` line — and reports anything else
# that can declare a dependency as an error rather than skipping it: a
# `[dependencies.<name>]` table header, a dotted key (`clap.version = "4"`,
# `dependencies.clap = ...`), a quoted key, a `package = ...` rename, or a
# line in a dependency table it cannot read.
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

# Parse one Cargo.toml. Prints one record per dependency declaration:
#   dep <normal|dev> <name> <workspace|local>
# and one record per construct it refuses to interpret:
#   unsupported <line number> <reason>
parse_manifest() {
  awk '
    function trim(text) { sub(/^[[:space:]]+/, "", text); sub(/[[:space:]]+$/, "", text); return text }
    function refuse(reason) { print "unsupported", NR, reason }
    # TOML allows whitespace inside header brackets and around the dots of a
    # dotted key or header: `[ dependencies ]`, `[ target . x . dependencies ]`.
    function squeeze(text) {
      gsub(/\[[[:space:]]+/, "[", text); gsub(/[[:space:]]+\]/, "]", text)
      gsub(/[[:space:]]*\.[[:space:]]*/, ".", text)
      return text
    }
    {
      line = trim($0)
      if (line == "" || line ~ /^#/) next
    }
    # Table headers, with any trailing comment removed.
    line ~ /^\[/ {
      header = line
      sub(/\][[:space:]]*#.*$/, "]", header)
      header = squeeze(trim(header))
      in_deps = 0
      if (header ~ /^\[(target\..*\.)?(build-|dev-)?dependencies\]$/) {
        in_deps = 1
        kind = (header ~ /dev-dependencies\]$/) ? "dev" : "normal"
      } else if (header ~ /^\[(target\..*\.)?(build-|dev-)?dependencies\./) {
        refuse("table-form dependency header " header "; declare it as one line in [dependencies]")
      } else if (header ~ /dependencies/ && header != "[workspace.dependencies]") {
        # Fail closed: a header that names dependencies in a form not read above.
        refuse("unrecognized dependency header " header)
      }
      next
    }
    {
      key = line; sub(/=.*/, "", key); key = squeeze(trim(key))
      # A dotted key that reaches into a dependency table from anywhere else.
      if (line ~ /=/ && key ~ /(^|\.)(build-|dev-)?dependencies\./) {
        refuse("dotted-key dependency " key "; declare it as one line in [dependencies]")
        next
      }
    }
    !in_deps { next }
    {
      if (line !~ /=/) { refuse("unreadable line in a dependency table: " line); next }
      if (key ~ /^["\047]/) { refuse("quoted dependency key " key); next }
      if (key !~ /^[A-Za-z0-9_-]+(\.workspace)?$/) {
        refuse("dotted dependency key " key "; use an inline table on one line")
        next
      }
      value = line; sub(/^[^=]*=/, "", value)
      if (value ~ /(^|[{,[:space:]])package[[:space:]]*=/) {
        refuse("renamed dependency " key " (package = ...); the direction check matches crate names")
        next
      }
      name = key; sub(/\.workspace$/, "", name)
      inherited = (key ~ /\.workspace$/ || value ~ /(^|[{,[:space:]])workspace[[:space:]]*=[[:space:]]*true/)
      print "dep", kind, name, (inherited ? "workspace" : "local")
    }
  ' "$1"
}

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

uses=""  # "<dep> <crate> <workspace|local>" lines, third-party deps in any table
for manifest in "${manifests[@]}"; do
  crate=$(awk -F'"' '/^[[:space:]]*name[[:space:]]*=/ { print $2; exit }' "$manifest")
  records=$(parse_manifest "$manifest")

  while read -r _tag lineno reason; do
    err "$manifest:$lineno: $reason"
  done < <(grep '^unsupported ' <<<"$records" || true)

  while read -r _tag _kind dep origin; do
    [[ "$dep" == "$PREFIX" || "$dep" == "$PREFIX"-* ]] && continue
    uses+="$dep $crate $origin"$'\n'
  done < <(grep '^dep ' <<<"$records" || true)

  if ! policy "$crate"; then
    err "crate '$crate' ($manifest) has no policy; add one here and a row in $ARCHITECTURE"
    continue
  fi
  while read -r _tag _kind dep _origin; do
    if [[ "$dep" == "$PREFIX" || "$dep" == "$PREFIX"-* ]]; then
      contains "$dep" "$ALLOWED" || err "$crate must not depend on internal crate $dep"
    elif contains "$dep" "$BANNED"; then
      err "$crate must not depend on $dep (a surface crate in the domain)"
    fi
  done < <(grep '^dep normal ' <<<"$records" || true)

  if [[ -f "$ARCHITECTURE" ]]; then
    row="| \`$crate\` | $KIND | $(cell "$ALLOWED") | $(cell "$BANNED") |"
    grep -qxF "$row" "$ARCHITECTURE" \
      || err "$ARCHITECTURE has no crate row matching this policy; expected: $row"
  fi
done

# STD-02 §R9: a dependency that more than one member uses is declared once in
# [workspace.dependencies], so every member inherits it; a single member
# declaring it locally is the drift this catches.
while read -r dep members locals; do
  err "$dep is used by $members members but declared locally by $locals; declare it once in [workspace.dependencies] and inherit it (STD-02 §R9)"
done < <(printf '%s' "$uses" | sort -u | awk '
  NF {
    key = $1 SUBSEP $2
    if (!(key in seen)) { seen[key] = 1; members[$1]++ }
    if ($3 == "local" && !((key, "local") in marked)) {
      marked[key, "local"] = 1
      locals[$1] = locals[$1] (locals[$1] == "" ? "" : ",") $2
    }
  }
  END { for (d in members) if (members[d] > 1 && locals[d] != "") print d, members[d], locals[d] }' | sort)

if [[ "$fail" -eq 0 ]]; then
  echo "dependency-direction: ok (${#manifests[@]} crates)"
fi
exit "$fail"
