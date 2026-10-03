#!/usr/bin/env bash
# Performance envelope for one study: cold index per side, warm launch, and
# p50/p95 latency for the query endpoints over N repetitions, plus the
# service's peak RSS and the bounds that truncated.
#
# Usage: performance.sh <study-id> [repetitions]   (default 25, minimum 20)
#
# Writes $OUT/study-<id>/performance.tsv and appends to $OUT/performance.tsv.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
require_binaries

want="${1:?usage: performance.sh <study-id> [repetitions]}"
reps="${2:-25}"
[ "$reps" -ge 20 ] || { echo "at least 20 repetitions are required" >&2; exit 1; }

repo=""; base=""; head=""; label=""
for line in "${STUDIES[@]}"; do
  [ "$(study_field "$line" 1)" = "$want" ] || continue
  repo=$(study_field "$line" 2); base=$(study_field "$line" 3)
  head=$(study_field "$line" 4); label=$(study_field "$line" 5)
done
: "${repo:?unknown study $want}"

outdir="$OUT/study-$want"; mkdir -p "$outdir"
tsv="$outdir/performance.tsv"
cachekey="perf-$want"
: > "$tsv"

emit() { printf '%s\t%s\t%s\t%s\n' "$want" "$repo" "$1" "$2" >> "$tsv"; }

# --- cold build, per side -------------------------------------------------
rm -rf "${CACHE:?}/$cachekey"
cold_start=$(date +%s.%N)
start_service "$repo" "$base" "$head" "$cachekey"
trap stop_service EXIT
# Poll /api/status until both sides are ready, keeping each side's own
# elapsed_ms (the service reports per-side build time directly).
while :; do
  status=$(api /api/status)
  state=$(echo "$status" | jq -r '[.indexing.base.state, .indexing.head.state] | join(",")')
  case "$state" in
    ready,ready) break ;;
    *failed*|*cancelled*) echo "index $state" >&2; exit 1 ;;
  esac
  sleep 0.2
done
cold_end=$(date +%s.%N)
emit cold_wall_seconds "$(printf '%.2f' "$(echo "$cold_end - $cold_start" | bc)")"
emit cold_base_elapsed_ms "$(echo "$status" | jq -r '.indexing.base.elapsed_ms')"
emit cold_head_elapsed_ms "$(echo "$status" | jq -r '.indexing.head.elapsed_ms')"
emit cold_base_files_indexed "$(echo "$status" | jq -r '.indexing.base.files_indexed')"
emit cold_head_files_indexed "$(echo "$status" | jq -r '.indexing.head.files_indexed')"
emit cold_base_files_ignored "$(echo "$status" | jq -r '.indexing.base.files_ignored')"
emit cold_head_files_ignored "$(echo "$status" | jq -r '.indexing.head.files_ignored')"
emit cold_base_unsupported "$(echo "$status" | jq -r '.indexing.base.unsupported_constructs')"
emit cold_head_unsupported "$(echo "$status" | jq -r '.indexing.head.unsupported_constructs')"
emit cold_languages "$(echo "$status" | jq -r '[.indexing.base.languages[]?] | join(",")')"

# The query subject is the study's primary question-4 symbol: a real code symbol
# with a non-trivial neighbourhood, not whichever changed row happens to sort
# first (which is a Markdown heading in three of the five studies).
api /api/changed-symbols > "$outdir/perf-changed-symbols.json"
case "$want" in
  1) subject='symbol:src/sync/pass2.rs#resolve_ref:function' ;;
  2) subject='symbol:crates/orbit-cli/src/command/workspace/teardown.rs#resolve_teardown_target:function' ;;
  3) subject='symbol:crates/orbit-types/src/task/model.rs#TaskComplexity:enum' ;;
  4) subject='symbol:experiments/physics/fput-recurrence-reproduction/fput/metrics.py#evaluate_metrics:function' ;;
  5) subject='symbol:scripts/research_records.py#supporting_paths:function' ;;
  *) subject=$(jq -r '[.symbols[] | select(.head) | .head.selector] | .[0] // empty' \
       "$outdir/perf-changed-symbols.json") ;;
esac
emit query_subject "${subject:-none}"
enc=$(jq -rn --arg s "$subject" '$s|@uri')
searchterm=$(printf '%s' "$subject" | sed -E 's/.*#([^:]+):.*/\1/')
senc=$(jq -rn --arg s "$searchterm" '$s|@uri')

# --- latency --------------------------------------------------------------
percentile() { # percentile <file-of-ms> <fraction>
  sort -n "$1" | awk -v p="$2" '{v[NR]=$1} END {
    if (NR == 0) {print "na"; exit}
    i = int(p * NR + 0.9999); if (i < 1) i = 1; if (i > NR) i = NR;
    printf "%.1f", v[i]}'
}

measure() { # measure <label> <path>
  local label="$1" path="$2" f="$outdir/lat-$label.txt"
  : > "$f"
  for _ in $(seq 1 "$reps"); do
    curl -sS -o /dev/null -H "Authorization: Bearer $SERVICE_TOKEN" \
      -w '%{time_total}\n' "$SERVICE_ORIGIN$path" \
      | awk '{printf "%.1f\n", $1 * 1000}' >> "$f"
  done
  emit "${label}_p50_ms" "$(percentile "$f" 0.50)"
  emit "${label}_p95_ms" "$(percentile "$f" 0.95)"
  emit "${label}_max_ms" "$(sort -n "$f" | tail -1)"
  emit "${label}_runs" "$reps"
}

measure changed_symbols "/api/changed-symbols"
measure evidence_d1 "/api/evidence?selector=$enc&side=head&depth=1"
measure evidence_d2 "/api/evidence?selector=$enc&side=head&depth=2"
measure evidence_d3 "/api/evidence?selector=$enc&side=head&depth=3"
measure entry_points "/api/entry-points?selector=$enc&side=head&depth=3"
measure candidate_tests "/api/candidate-tests?selector=$enc&side=head"
measure search "/api/search?q=$senc&side=head&limit=20"
measure comparison "/api/comparison"

# --- truncation counts over the whole changed-symbol set ------------------
for depth in 1 2 3; do
  hit=0; total=0
  while IFS= read -r sel; do
    e=$(jq -rn --arg s "$sel" '$s|@uri')
    t=$(api "/api/evidence?selector=$e&side=head&depth=$depth" | jq -r '.truncated_by // "none"')
    total=$((total + 1)); [ "$t" = "none" ] || hit=$((hit + 1))
  done < <(jq -r '.symbols[] | select(.head) | .head.selector' "$outdir/perf-changed-symbols.json")
  emit "truncated_evidence_d${depth}" "$hit/$total"
done

# --- peak RSS -------------------------------------------------------------
emit peak_rss_kb "$(awk '/^VmHWM:/ {print $2}' "/proc/$SERVICE_PID/status")"
emit rss_kb "$(awk '/^VmRSS:/ {print $2}' "/proc/$SERVICE_PID/status")"

stop_service
trap - EXIT

# --- warm launch (cache hit), same cache directory ------------------------
warm_start=$(date +%s.%N)
start_service "$repo" "$base" "$head" "$cachekey"
trap stop_service EXIT
wait_ready 1800
warm_end=$(date +%s.%N)
emit warm_wall_seconds "$(printf '%.2f' "$(echo "$warm_end - $warm_start" | bc)")"
emit warm_peak_rss_kb "$(awk '/^VmHWM:/ {print $2}' "/proc/$SERVICE_PID/status")"
stop_service
trap - EXIT

cat "$tsv"
cat "$tsv" >> "$OUT/performance.tsv"
