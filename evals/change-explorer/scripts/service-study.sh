#!/usr/bin/env bash
# Answer the six fixed study questions for one study through the loopback
# service, and record the wall-clock of the whole pass.
#
# Usage: service-study.sh <study-id> [--warm]
#
# Writes into $OUT/study-<id>/:
#   comparison.json changed-symbols.json
#   evidence-d1.json evidence-d2.json evidence-d3.json   (one object per changed symbol)
#   entry-points.json candidate-tests.json search.json
#   service-timing.tsv
#
# `--warm` asserts the snapshot cache is already populated (second launch).
# Without it the launch is a cold build and the cache directory is cleared
# first.
#
# AGENT-ONLY comparison: this is the same question set baseline.sh answers,
# issued over HTTP. It is not a human usability study.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
require_binaries

want="${1:?usage: service-study.sh <study-id> [--warm]}"
mode="${2:-cold}"
repo=""; base=""; head=""; label=""
for line in "${STUDIES[@]}"; do
  [ "$(study_field "$line" 1)" = "$want" ] || continue
  repo=$(study_field "$line" 2); base=$(study_field "$line" 3)
  head=$(study_field "$line" 4); label=$(study_field "$line" 5)
done
: "${repo:?unknown study $want}"

outdir="$OUT/study-$want"; mkdir -p "$outdir"
cachekey="study-$want"
if [ "$mode" != "--warm" ]; then rm -rf "${CACHE:?}/$cachekey"; fi

start=$(date +%s.%N)
start_service "$repo" "$base" "$head" "$cachekey"
trap stop_service EXIT
wait_ready 1800
ready=$(date +%s.%N)

api /api/comparison            > "$outdir/comparison.json"
api /api/changed-symbols       > "$outdir/changed-symbols.json"

# Every changed symbol that has a head-side selector is queried on head; a
# removed symbol is queried on base instead. Nothing is filtered out.
jq -r '.symbols[] | if .head then "head\t" + .head.selector
                    elif .base then "base\t" + .base.selector
                    else empty end' "$outdir/changed-symbols.json" \
  | sort -u > "$outdir/queried-selectors.tsv"

for depth in 1 2 3; do
  : > "$outdir/evidence-d$depth.json"
done
: > "$outdir/entry-points.json"
: > "$outdir/candidate-tests.json"

while IFS=$'\t' read -r side selector; do
  enc=$(jq -rn --arg s "$selector" '$s|@uri')
  for depth in 1 2 3; do
    api "/api/evidence?selector=$enc&side=$side&depth=$depth" >> "$outdir/evidence-d$depth.json"
    echo >> "$outdir/evidence-d$depth.json"
  done
  api "/api/entry-points?selector=$enc&side=$side&depth=3" >> "$outdir/entry-points.json"
  echo >> "$outdir/entry-points.json"
  api "/api/candidate-tests?selector=$enc&side=$side" >> "$outdir/candidate-tests.json"
  echo >> "$outdir/candidate-tests.json"
done < "$outdir/queried-selectors.tsv"

# One search per distinct changed-symbol name, so /api/search is exercised on
# the same vocabulary the rest of the study uses.
: > "$outdir/search.json"
cut -f2 "$outdir/queried-selectors.tsv" | sed -E 's/.*#([^:]+):.*/\1/' | sort -u \
  | while IFS= read -r name; do
      enc=$(jq -rn --arg s "$name" '$s|@uri')
      api "/api/search?q=$enc&side=head&limit=20" >> "$outdir/search.json"
      echo >> "$outdir/search.json"
    done

end=$(date +%s.%N)
stop_service
trap - EXIT

{
  printf 'study\t%s\nrepo\t%s\nlabel\t%s\nmode\t%s\n' "$want" "$repo" "$label" \
    "$([ "$mode" = "--warm" ] && echo warm || echo cold)"
  printf 'index_ready_seconds\t%.2f\n' "$(echo "$ready - $start" | bc)"
  printf 'question_pass_seconds\t%.2f\n' "$(echo "$end - $ready" | bc)"
  printf 'service_wall_seconds\t%.2f\n' "$(echo "$end - $start" | bc)"
  printf 'changed_symbols\t%s\n' "$(jq '.symbols | length' "$outdir/changed-symbols.json")"
  printf 'selectors_queried\t%s\n' "$(wc -l < "$outdir/queried-selectors.tsv")"
} | tee "$outdir/service-timing-$([ "$mode" = "--warm" ] && echo warm || echo cold).tsv"
