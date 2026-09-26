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
# Anything else is one ALLOW entry per write site, one line each:
#   <path>|<hits>|<extended regex over the hit's source line>|<reason; the task that removes it, or the rule that permits it>
# An entry must match exactly <hits> hits in <path>. A new stream write that
# happens to match an existing entry's regex changes the count and fails, and
# an entry whose site is gone (0 hits) fails too, so a stale entry is removed
# in the change that removes its write.
set -euo pipefail
cd "$(dirname "$0")/.."

OUTPUT_LAYERS=(
  crates/orbit-graph-cli/src/output/
  crates/orbit-graph-explorer/src/main.rs
)

ALLOW=$(cat <<'EOF'
crates/orbit-graph-cli/src/main.rs|1|\.with_writer\(io::stderr\)$|The log subscriber's writer; STD-02 §R15 permits the one place that installs it to hand it io::stderr. Permanent.
crates/orbit-graph-cli/src/main.rs|1|^use std::io::\{self, IsTerminal, Read, Write\};$|Import for the stdin TTY probe below. Temporary: ORB-13164 moves the probe to src/output/ and removes this entry.
crates/orbit-graph-cli/src/main.rs|1|!io::stdin\(\)\.is_terminal\(\)|The bare-invocation stdin TTY probe. Temporary: ORB-13164 moves it to src/output/ and removes this entry.
crates/orbit-graph-cli/src/main.rs|1|^[[:space:]]*io::stderr\(\)\.lock\(\),$|The bare-envelope deprecation warning. Temporary: ORB-13164 moves it to src/output/ and removes this entry.
crates/orbit-graph-cli/src/main.rs|2|^[[:space:]]*let mut stdout = io::stdout\(\)\.lock\(\);$|emit_to_process and write_json_to_stdout. Temporary: ORB-13164 moves them to src/output/ and removes this entry.
crates/orbit-graph-cli/src/main.rs|1|^[[:space:]]*let mut stderr = io::stderr\(\)\.lock\(\);$|emit_to_process. Temporary: ORB-13164 moves it to src/output/ and removes this entry.
crates/orbit-graph-explorer/src/service.rs|1|^[[:space:]]*let mut stderr = io::stderr\(\)\.lock\(\);$|print_launch_banner writes the launch banner and token from the library. Temporary: ORB-13168 moves it to the explorer binary and removes this entry.
EOF
)

PATTERN='io::stdout|io::stderr|\bstd(out|err)\(\)|\bprintln!|\beprintln!|\bprint!|\beprint!|\bdbg!|is_terminal|IsTerminal'

if [[ ! -d crates ]] || [[ -z "$(find crates -path '*/src/*' -name '*.rs' -print -quit)" ]]; then
  echo "terminal-guard: no Rust sources under crates/*/src; nothing was checked" >&2
  exit 1
fi

# Entries as parallel indexed arrays (bash 3 compatible).
entry_paths=()
entry_hits=()
entry_regexes=()
entry_counts=()
while IFS='|' read -r entry_path hits regex _reason; do
  [[ -z "$entry_path" ]] && continue
  entry_paths+=("$entry_path")
  entry_hits+=("$hits")
  entry_regexes+=("$regex")
  entry_counts+=(0)
done <<<"$ALLOW"

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

# A grep exit status of 1 means no match; 2 or more means grep itself failed,
# which must not read as "no stream writes" (STD-02 §R29).
set +e
hits=$(grep -rnE --include='*.rs' --exclude-dir=tests "$PATTERN" crates)
grep_status=$?
set -e
if [[ "$grep_status" -gt 1 ]]; then
  echo "terminal-guard: grep failed (exit $grep_status); nothing was checked" >&2
  exit 1
fi

fail=0
while IFS= read -r hit; do
  [[ -z "$hit" ]] && continue
  path=${hit%%:*}
  rest=${hit#*:}
  text=${rest#*:}
  # Comment lines, including doc comments, do not touch a stream.
  [[ "$text" =~ ^[[:space:]]*// ]] && continue
  in_output_layer "$path" && continue
  matched=0
  for i in "${!entry_paths[@]}"; do
    if [[ "$path" == "${entry_paths[$i]}" ]] && grep -qE -- "${entry_regexes[$i]}" <<<"$text"; then
      entry_counts[$i]=$((entry_counts[$i] + 1))
      matched=1
      break
    fi
  done
  [[ "$matched" -eq 1 ]] && continue
  echo "$hit" >&2
  fail=1
done <<<"$hits"

for i in "${!entry_paths[@]}"; do
  if [[ "${entry_counts[$i]}" -ne "${entry_hits[$i]}" ]]; then
    echo "terminal-guard: allow-list entry ${entry_paths[$i]}|${entry_regexes[$i]} expects ${entry_hits[$i]} hit(s) and matched ${entry_counts[$i]}; update or remove it" >&2
    fail=1
  fi
done

if [[ "$fail" -ne 0 ]]; then
  echo "terminal-guard: std streams and TTY checks belong to an output layer (${OUTPUT_LAYERS[*]}); see STD-02 §R15" >&2
  exit 1
fi
echo "terminal-guard: ok (${#entry_paths[@]} allow-list entries)"
