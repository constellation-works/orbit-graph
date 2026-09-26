#!/usr/bin/env bash
# scripts/check-terminal-guard.sh — STD-02 §R15 (and STD-01's stream rules).
#
# Only an output layer may name the standard streams, use the print!-family
# macros or ask whether a stream is a terminal. clippy's print lints do not
# see `writeln!(io::stdout(), ...)`, so this grep closes that gap. Test code
# (`tests/` directories) and comment lines are exempt.
#
# Output layers (permanent):
#   - crates/orbit-graph-cli/src/output/       the CLI's single output layer
#   - crates/orbit-graph-explorer/src/main.rs  the explorer binary's surface;
#     its write_out/write_err are the explorer's only stream writers, and the
#     explorer library (everything else under src/) must not touch streams.
#
# Anything else goes in ALLOW below, one entry per line:
#   <path>|<extended regex a hit line must match>|<reason; the task that removes it, or the rule that permits it>
set -euo pipefail
cd "$(dirname "$0")/.."

OUTPUT_LAYERS=(
  crates/orbit-graph-cli/src/output/
  crates/orbit-graph-explorer/src/main.rs
)

ALLOW=$(cat <<'EOF'
crates/orbit-graph-cli/src/main.rs|with_writer\(io::stderr\)|STD-02 §R15 permits the one place that installs the log subscriber to hand it io::stderr. Permanent.
crates/orbit-graph-cli/src/main.rs|.|Temporary: the stdin TTY probe, the bare-envelope deprecation warning and the direct JSON/emit writers live in main.rs; ORB-13164 moves them into src/output/ and removes this entry.
crates/orbit-graph-explorer/src/service.rs|.|Temporary: print_launch_banner writes the launch banner and token to stderr from the library; ORB-13168 moves it to the explorer binary and removes this entry.
EOF
)

PATTERN='io::stdout|io::stderr|\bstd(out|err)\(\)|\bprintln!|\beprintln!|\bprint!|\beprint!|\bdbg!|is_terminal|IsTerminal'

if [[ ! -d crates ]] || [[ -z "$(find crates -path '*/src/*' -name '*.rs' -print -quit)" ]]; then
  echo "terminal-guard: no Rust sources under crates/*/src; nothing was checked" >&2
  exit 1
fi

allowed() { # allowed <path> <line text>
  local path="$1" text="$2" entry_path entry_regex _reason
  while IFS='|' read -r entry_path entry_regex _reason; do
    [[ -z "$entry_path" ]] && continue
    if [[ "$path" == "$entry_path" ]] && grep -qE -- "$entry_regex" <<<"$text"; then
      return 0
    fi
  done <<<"$ALLOW"
  return 1
}

in_output_layer() {
  local layer
  for layer in "${OUTPUT_LAYERS[@]}"; do
    if [[ "$layer" == */ ]]; then
      [[ "$1" == "$layer"* ]] && return 0
    else
      [[ "$1" == "$layer" ]] && return 0
    fi
  done
  return 1
}

fail=0
while IFS= read -r hit; do
  [[ -z "$hit" ]] && continue
  path=${hit%%:*}
  rest=${hit#*:}
  text=${rest#*:}
  # Comment lines, including doc comments, do not touch a stream.
  [[ "$text" =~ ^[[:space:]]*// ]] && continue
  in_output_layer "$path" && continue
  allowed "$path" "$text" && continue
  echo "$hit" >&2
  fail=1
done < <(grep -rnE --include='*.rs' --exclude-dir=tests "$PATTERN" crates || true)

# An allow-list entry whose file is gone is dead weight: fail so it is removed.
while IFS='|' read -r entry_path _regex _reason; do
  [[ -z "$entry_path" ]] && continue
  if [[ ! -f "$entry_path" ]]; then
    echo "terminal-guard: allow-list entry for missing file $entry_path; remove it" >&2
    fail=1
  fi
done <<<"$ALLOW"

if [[ "$fail" -ne 0 ]]; then
  echo "terminal-guard: std streams and TTY checks belong to an output layer (${OUTPUT_LAYERS[*]}); see STD-02 §R15" >&2
  exit 1
fi
echo "terminal-guard: ok"
