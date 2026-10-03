#!/usr/bin/env bash
# Canonical dump of resolved refs from an orbit-graph database, for checking
# that a resolver change leaves resolution output unchanged.
#
#   refs-dump.sh <graph.db> <out.tsv>
#
# Writes one tab-separated row per ref, sorted: from_file, span start, span
# end, target_name, target_qualified, confidence, kind, and the qualified name
# of the symbol target_symbol_hint points at (row ids differ between builds,
# the hinted symbol's name does not). Prints per-confidence counts to stdout.
# Two builds resolve identically when `cmp` finds their dumps equal.
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <graph.db> <out.tsv>" >&2
  exit 2
fi
db="$1"
out="$2"

sqlite3 -readonly -batch -noheader -separator $'\t' -nullvalue '\N' "$db" "
  SELECT r.from_file, r.from_span_start, r.from_span_end, r.target_name,
         r.target_qualified, r.confidence, r.kind, s.qualified
  FROM refs r LEFT JOIN symbols s ON s.id = r.target_symbol_hint
  ORDER BY 1, 2, 3, 4, 5, 6, 7, 8;" >"$out"

sqlite3 -readonly -batch -noheader -separator $'\t' "$db" \
  "SELECT confidence, count(*) FROM refs GROUP BY confidence ORDER BY confidence;"
printf 'total\t%s\n' "$(wc -l <"$out")"
