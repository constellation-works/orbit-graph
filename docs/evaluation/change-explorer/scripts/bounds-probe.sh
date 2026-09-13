#!/usr/bin/env bash
# Question 6: which bounds were hit, and does raising them change the answer?
#
# Usage: bounds-probe.sh <study-id>
#
# Runs the whole changed-symbol set twice: once at the shipped bounds
# (depth 3, IMPACT_NODE_CAP 200, 5000 ms) and once at raised bounds
# (depth 10 — MAX_EVIDENCE_DEPTH —, node cap 5000, 60000 ms), then reports the
# per-symbol difference in reachable nodes, paths and entry points.
#
# Writes $OUT/study-<id>/bounds-probe.tsv.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
require_binaries

want="${1:?usage: bounds-probe.sh <study-id>}"
repo=""; base=""; head=""
for line in "${STUDIES[@]}"; do
  [ "$(study_field "$line" 1)" = "$want" ] || continue
  repo=$(study_field "$line" 2); base=$(study_field "$line" 3); head=$(study_field "$line" 4)
done
: "${repo:?unknown study $want}"

outdir="$OUT/study-$want"; mkdir -p "$outdir"
tsv="$outdir/bounds-probe.tsv"
printf 'profile\tselector\tpaths\tnodes\tentry_points\ttruncated_by\tbounds_hit\n' > "$tsv"

probe() { # probe <profile> <depth> [serve args...]
  local profile="$1" depth="$2"; shift 2
  start_service "$repo" "$base" "$head" "study-$want" "$@"
  trap stop_service EXIT
  wait_ready 1800
  api /api/changed-symbols > "$outdir/bounds-changed-symbols.json"
  while IFS= read -r sel; do
    enc=$(jq -rn --arg s "$sel" '$s|@uri')
    ev=$(api "/api/evidence?selector=$enc&side=head&depth=$depth")
    ep=$(api "/api/entry-points?selector=$enc&side=head&depth=$depth")
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$profile" "$sel" \
      "$(echo "$ev" | jq '.paths | length')" \
      "$(echo "$ev" | jq '[.paths[].from.selector, .paths[].edges[].from_selector] | unique | length')" \
      "$(echo "$ep" | jq '.entry_points | length')" \
      "$(echo "$ev" | jq -r '.truncated_by // "none"')" \
      "$(echo "$ev" | jq -r '[.bounds_hit[]? | "\(.bound)=\(.value)"] | join(",") | if . == "" then "none" else . end')" \
      >> "$tsv"
  done < <(jq -r '.symbols[] | select(.head) | .head.selector' "$outdir/bounds-changed-symbols.json")
  stop_service
  trap - EXIT
}

probe shipped 3
probe raised 10 --node-cap 5000 --time-budget-ms 60000

echo
echo "## symbols whose answer changed when the bounds were raised"
awk -F'\t' 'NR > 1 {key = $2; if ($1 == "shipped") {p[key]=$3; n[key]=$4; e[key]=$5}
            else if (p[key] != $3 || n[key] != $4 || e[key] != $5)
              printf "%s\tpaths %s->%s\tnodes %s->%s\tentry_points %s->%s\n", key, p[key], $3, n[key], $4, e[key], $5}' "$tsv" \
  | tee "$outdir/bounds-diff.tsv"
echo
printf 'symbols_probed\t%s\n' "$(awk -F'\t' 'NR>1 && $1=="shipped"' "$tsv" | wc -l)"
printf 'symbols_changed_by_raising\t%s\n' "$(wc -l < "$outdir/bounds-diff.tsv")"
