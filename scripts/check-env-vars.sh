#!/usr/bin/env bash
# scripts/check-env-vars.sh — STD-01@4 §R25.
#
# Every process environment variable read in production code is declared once
# as a documented constant in crates/orbit-graph-cli/src/environment.rs (with
# meaning, Default:, and Overridden flag:). This script generates the
# `## Environment` table in docs/usage.md from that list and fails when the
# constants, the doc table, or production env reads disagree.
#
# Usage:
#   scripts/check-env-vars.sh           Verify constants, docs/usage.md, and reads
#   scripts/check-env-vars.sh --update  Regenerate the table in docs/usage.md
set -euo pipefail
cd "$(dirname "$0")/.."

ENV_RS="crates/orbit-graph-cli/src/environment.rs"
USAGE_MD="docs/usage.md"
BEGIN_MARKER="<!-- env-table:begin -->"
END_MARKER="<!-- env-table:end -->"

mode="check"
if [[ "$#" -gt 1 ]]; then
  echo "usage: $0 [--update]" >&2
  exit 2
elif [[ "$#" -eq 1 ]]; then
  if [[ "$1" == "--update" ]]; then
    mode="update"
  else
    echo "usage: $0 [--update]" >&2
    exit 2
  fi
fi

if [[ ! -d crates ]] || [[ -z "$(find crates -path '*/src/*' -name '*.rs' -print -quit)" ]]; then
  echo "env-vars: no Rust sources under crates/*/src; nothing was checked" >&2
  exit 1
fi

if [[ ! -f "$ENV_RS" ]]; then
  echo "env-vars: missing $ENV_RS" >&2
  exit 1
fi

if [[ ! -f "$USAGE_MD" ]]; then
  echo "env-vars: missing $USAGE_MD" >&2
  exit 1
fi

# Parse constants and doc comments from environment.rs. Emits:
#   IDENT\t<list of constant identifiers>
#   TABLE\t<each line of the generated markdown table block>
parsed_file=$(mktemp "${ORBIT_SCRATCH_DIR:-${TMPDIR:-/tmp}}/orbit-graph-env-parsed.XXXXXX")
table_file=$(mktemp "${ORBIT_SCRATCH_DIR:-${TMPDIR:-/tmp}}/orbit-graph-env-table.XXXXXX")
trap 'rm -f "$parsed_file" "$table_file"' EXIT

if ! awk -v begin="$BEGIN_MARKER" -v end="$END_MARKER" '
  function trim(s) {
    sub(/^[[:space:]]+/, "", s)
    sub(/[[:space:]]+$/, "", s)
    return s
  }
  function reset_doc() {
    meaning = ""
    def_val = ""
    flag_val = ""
  }
  function escape_cell(s) {
    gsub(/\|/, "\\|", s)
    return s
  }
  BEGIN {
    reset_doc()
    in_list = 0
    n = 0
    m = 0
    err = 0
  }
  {
    line = $0
    sub(/\r$/, "", line)
  }
  in_list {
    if (line ~ /\];[[:space:]]*$/) {
      in_list = 0
      next
    }
    item = trim(line)
    sub(/,$/, "", item)
    if (item != "" && item !~ /^\/\//) {
      listed[++m] = item
    }
    next
  }
  line ~ /^pub const ENV_VARS:[[:space:]]*&\[&str\][[:space:]]*=[[:space:]]*&\[/ {
    reset_doc()
    in_list = 1
    next
  }
  line ~ /^[[:space:]]*\/\/\// {
    text = line
    sub(/^[[:space:]]*\/\/\/[[:space:]]?/, "", text)
    text = trim(text)
    if (text == "") next
    if (text ~ /^Default:[[:space:]]*/) {
      def_val = text
      sub(/^Default:[[:space:]]*/, "", def_val)
      def_val = trim(def_val)
    } else if (text ~ /^Overridden flag:[[:space:]]*/) {
      flag_val = text
      sub(/^Overridden flag:[[:space:]]*/, "", flag_val)
      flag_val = trim(flag_val)
    } else {
      meaning = (meaning == "" ? text : meaning " " text)
    }
    next
  }
  line ~ /^pub const [A-Z0-9_]+:[[:space:]]*&str[[:space:]]*=[[:space:]]*"[^"]+";[[:space:]]*$/ {
    ident = line
    sub(/^pub const /, "", ident)
    sub(/:.*/, "", ident)
    val = line
    sub(/^[^"]*"/, "", val)
    sub(/".*$/, "", val)
    if (meaning == "") {
      printf "env-vars: %s:%d: %s is missing a meaning doc comment\n", FILENAME, NR, ident > "/dev/stderr"
      err = 1
    }
    if (def_val == "") {
      printf "env-vars: %s:%d: %s is missing a \"Default:\" doc line\n", FILENAME, NR, ident > "/dev/stderr"
      err = 1
    }
    if (flag_val == "") {
      printf "env-vars: %s:%d: %s is missing an \"Overridden flag:\" doc line\n", FILENAME, NR, ident > "/dev/stderr"
      err = 1
    }
    if (ident in seen_ident) {
      printf "env-vars: %s:%d: duplicate constant %s\n", FILENAME, NR, ident > "/dev/stderr"
      err = 1
    }
    if (val in seen_val) {
      printf "env-vars: %s:%d: duplicate variable name %s\n", FILENAME, NR, val > "/dev/stderr"
      err = 1
    }
    seen_ident[ident] = 1
    seen_val[val] = 1
    n++
    idents[n] = ident
    vals[n] = val
    defs[n] = def_val
    flags[n] = flag_val
    meanings[n] = meaning
    reset_doc()
    next
  }
  line ~ /^[[:space:]]*$/ || line ~ /^[[:space:]]*#\[/ {
    next
  }
  {
    reset_doc()
  }
  END {
    if (n == 0) {
      printf "env-vars: %s declares no environment variable constants\n", FILENAME > "/dev/stderr"
      err = 1
    }
    if (m != n) {
      printf "env-vars: %s: ENV_VARS lists %d constant(s), expected %d\n", FILENAME, m, n > "/dev/stderr"
      err = 1
    } else {
      for (i = 1; i <= n; i++) {
        if (listed[i] != idents[i]) {
          printf "env-vars: %s: ENV_VARS[%d] is %s, expected %s\n", FILENAME, i, listed[i], idents[i] > "/dev/stderr"
          err = 1
        }
      }
    }
    if (err) exit 1
    ident_line = ""
    for (i = 1; i <= n; i++) {
      ident_line = (ident_line == "" ? idents[i] : ident_line " " idents[i])
    }
    print "IDENT\t" ident_line
    print "TABLE\t" begin
    print "TABLE\t| Variable | Default | Overridden flag | Meaning |"
    print "TABLE\t| --- | --- | --- | --- |"
    for (i = 1; i <= n; i++) {
      printf "TABLE\t| `%s` | %s | %s | %s |\n",
        vals[i], escape_cell(defs[i]), escape_cell(flags[i]), escape_cell(meanings[i])
    }
    print "TABLE\t" end
  }
' "$ENV_RS" >"$parsed_file"; then
  exit 1
fi

known_idents=$(awk -F'\t' '$1 == "IDENT" { print $2; exit }' "$parsed_file")
awk -F'\t' '$1 == "TABLE" { print substr($0, 7) }' "$parsed_file" >"$table_file"

# Check every production Rust source under crates/*/src (excluding tests/).
prod_files=()
while IFS= read -r file; do
  case "$file" in
    */tests/*) continue ;;
  esac
  prod_files+=("$file")
done < <(find crates -path '*/src/*' -name '*.rs' | sort)

if [[ "${#prod_files[@]}" -eq 0 ]]; then
  echo "env-vars: no production Rust sources under crates/*/src; nothing was checked" >&2
  exit 1
fi

if ! awk -v known="$known_idents" '
  function trim(s) {
    sub(/^[[:space:]]+/, "", s)
    sub(/[[:space:]]+$/, "", s)
    return s
  }
  BEGIN {
    split(known, arr, " ")
    for (i in arr) {
      if (arr[i] != "") valid[arr[i]] = 1
    }
    err = 0
    pending_kind = ""
    pending_line = 0
  }
  FNR == 1 {
    pending_kind = ""
  }
  {
    line = $0
    sub(/\r$/, "", line)
    if (line ~ /^[[:space:]]*\/\//) next
    code = line
    sub(/[[:space:]]\/\/.*$/, "", code)

    if (pending_kind != "") {
      arg = code
      if (index(arg, ")") > 0) {
        sub(/\).*/, "", arg)
        arg = trim(arg)
        if (!(arg in valid)) {
          printf "%s:%d: %s(%s) must pass a documented constant from %s\n",
            FILENAME, pending_line, pending_kind, arg, ENVIRON["ENV_RS_PATH"] > "/dev/stderr"
          err = 1
        }
        pending_kind = ""
      } else {
        arg = trim(arg)
        if (arg != "" && !(arg in valid)) {
          printf "%s:%d: %s(%s) must pass a documented constant from %s\n",
            FILENAME, pending_line, pending_kind, arg, ENVIRON["ENV_RS_PATH"] > "/dev/stderr"
          err = 1
          pending_kind = ""
        }
        next
      }
    }

    if (code ~ /try_from_default_env[[:space:]]*\(/) {
      printf "%s:%d: try_from_default_env() reads RUST_LOG implicitly; use EnvFilter::try_from_env(RUST_LOG_ENV)\n",
        FILENAME, FNR > "/dev/stderr"
      err = 1
    }

    rest = code
    while (match(rest, /(env::var_os|env::var|read_var|try_from_env)[[:space:]]*\(/)) {
      call = substr(rest, RSTART, RLENGTH)
      kind = call
      sub(/[[:space:]]*\($/, "", kind)
      after = substr(rest, RSTART + RLENGTH)
      close_idx = index(after, ")")
      if (close_idx == 0) {
        rem = trim(after)
        if (rem == "") {
          pending_kind = kind
          pending_line = FNR
          break
        }
        printf "%s:%d: %s(%s) must pass a single documented constant\n",
          FILENAME, FNR, kind, rem > "/dev/stderr"
        err = 1
        break
      }
      arg = trim(substr(after, 1, close_idx - 1))
      if (!(arg in valid)) {
        printf "%s:%d: %s(%s) must pass a documented constant from %s\n",
          FILENAME, FNR, kind, arg, ENVIRON["ENV_RS_PATH"] > "/dev/stderr"
        err = 1
      }
      rest = substr(after, close_idx + 1)
    }
  }
  END {
    exit err
  }
' ENV_RS_PATH="$ENV_RS" "${prod_files[@]}"; then
  exit 1
fi

# Verify marker sanity in docs/usage.md.
marker_check=$(awk -v begin="$BEGIN_MARKER" -v end="$END_MARKER" '
  { line = $0; sub(/\r$/, "", line) }
  line == begin { b++; if (!b_line) b_line = NR }
  line == end { e++; if (!e_line) e_line = NR }
  END {
    if (b != 1 || e != 1 || b_line >= e_line) {
      printf "bad:%d:%d:%d:%d", b+0, e+0, b_line+0, e_line+0
    } else {
      printf "ok:%d:%d", b_line, e_line
    }
  }
' "$USAGE_MD")

if [[ "$marker_check" == bad:* ]]; then
  echo "env-vars: $USAGE_MD must contain exactly one '$BEGIN_MARKER' before one '$END_MARKER'" >&2
  exit 1
fi

if [[ "$mode" == "update" ]]; then
  updated_file=$(mktemp "${ORBIT_SCRATCH_DIR:-${TMPDIR:-/tmp}}/orbit-graph-usage.XXXXXX")
  awk -v begin="$BEGIN_MARKER" -v end="$END_MARKER" -v tfile="$table_file" '
    { line = $0; sub(/\r$/, "", line) }
    line == begin {
      while ((getline tline < tfile) > 0) print tline
      close(tfile)
      skip = 1
      next
    }
    skip && line == end {
      skip = 0
      next
    }
    !skip { print $0 }
  ' "$USAGE_MD" >"$updated_file"
  mv "$updated_file" "$USAGE_MD"
  echo "env-vars: updated $USAGE_MD from $ENV_RS ($(wc -w <<<"$known_idents" | tr -d ' ') variables)"
  exit 0
fi

existing_file=$(mktemp "${ORBIT_SCRATCH_DIR:-${TMPDIR:-/tmp}}/orbit-graph-env-existing.XXXXXX")
trap 'rm -f "$parsed_file" "$table_file" "$existing_file"' EXIT

awk -v begin="$BEGIN_MARKER" -v end="$END_MARKER" '
  { line = $0; sub(/\r$/, "", line) }
  line == begin { in_block = 1 }
  in_block { print line }
  line == end { in_block = 0 }
' "$USAGE_MD" >"$existing_file"

if ! cmp -s "$table_file" "$existing_file"; then
  echo "env-vars: $USAGE_MD environment table disagrees with $ENV_RS" >&2
  diff -u "$existing_file" "$table_file" >&2 || true
  echo "env-vars: regenerate with: scripts/check-env-vars.sh --update" >&2
  exit 1
fi

echo "env-vars: ok ($(wc -w <<<"$known_idents" | tr -d ' ') variables)"
