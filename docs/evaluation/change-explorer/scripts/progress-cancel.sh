#!/usr/bin/env bash
# Exercise cold-build progress reporting and cancellation on the large corpus.
#
# Usage: progress-cancel.sh <study-id>
#
# Phase 1 samples GET /api/status every second through a full cold build and
# writes the trace to $OUT/study-<id>/progress-trace.tsv.
# Phase 2 clears the cache, starts a second cold build, waits for the base side
# to reach `indexing`, POSTs /api/cancel, and records the state transition and
# whether a cache entry was published.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
require_binaries

want="${1:?usage: progress-cancel.sh <study-id>}"
repo=""; base=""; head=""
for line in "${STUDIES[@]}"; do
  [ "$(study_field "$line" 1)" = "$want" ] || continue
  repo=$(study_field "$line" 2); base=$(study_field "$line" 3); head=$(study_field "$line" 4)
done
: "${repo:?unknown study $want}"

outdir="$OUT/study-$want"; mkdir -p "$outdir"
cachekey="progress-$want"
trace="$outdir/progress-trace.tsv"
cancel="$outdir/cancel.txt"

# --- phase 1: progress through a full cold build --------------------------
rm -rf "${CACHE:?}/$cachekey"
start_service "$repo" "$base" "$head" "$cachekey"
trap stop_service EXIT
printf 'seconds\tbase_state\tbase_files_seen\tbase_files_indexed\tbase_phase\tbase_phase_done\tbase_phase_total\tbase_elapsed_ms\thead_state\thead_files_seen\thead_files_indexed\thead_phase\thead_phase_done\thead_phase_total\thead_elapsed_ms\tlanguages\n' > "$trace"
t=0
while :; do
  s=$(api /api/status)
  echo "$s" | jq -r --arg t "$t" '[$t,
    .indexing.base.state, .indexing.base.files_seen, .indexing.base.files_indexed,
    .indexing.base.phase, .indexing.base.phase_progress.done, .indexing.base.phase_progress.total,
    .indexing.base.elapsed_ms,
    .indexing.head.state, .indexing.head.files_seen, .indexing.head.files_indexed,
    .indexing.head.phase, .indexing.head.phase_progress.done, .indexing.head.phase_progress.total,
    .indexing.head.elapsed_ms,
    ([.indexing.base.languages[]?, .indexing.head.languages[]?] | unique | join(","))] | @tsv' >> "$trace"
  state=$(echo "$s" | jq -r '.indexing_status')
  [ "$state" = "ready" ] && break
  [ "$state" = "failed" ] && { echo "index failed" >&2; break; }
  sleep 1
  t=$((t + 1))
done
# A query endpoint answered while a side is still building must return 409.
printf 'health_while_indexing_probe\tsee %s\n' "$trace" > "$cancel"
stop_service
trap - EXIT

# --- phase 2: cancel a cold build ----------------------------------------
rm -rf "${CACHE:?}/$cachekey"
start_service "$repo" "$base" "$head" "$cachekey"
trap stop_service EXIT
{
  echo "## side_not_ready probe and cancel"
  # Wait until the base side is genuinely indexing before cancelling.
  until [ "$(api /api/status | jq -r '.indexing.base.state')" = "indexing" ]; do sleep 0.5; done
  echo "before_cancel_status:"
  api /api/status | jq -c '.indexing'
  echo "changed_symbols_while_indexing (expect 409 side_not_ready):"
  curl -sS -o /dev/null -w 'http_status=%{http_code}\n' \
    -H "Authorization: Bearer $SERVICE_TOKEN" "$SERVICE_ORIGIN/api/changed-symbols"
  curl -sS -H "Authorization: Bearer $SERVICE_TOKEN" "$SERVICE_ORIGIN/api/changed-symbols" \
    | jq -c '.error | {code, message}'
  echo "cancel_response:"
  curl -sS -X POST -w '\nhttp_status=%{http_code}\n' \
    -H "Authorization: Bearer $SERVICE_TOKEN" "$SERVICE_ORIGIN/api/cancel" | jq -c '.' 2>/dev/null \
    || curl -sS -X POST -o /dev/null -w 'http_status=%{http_code}\n' \
       -H "Authorization: Bearer $SERVICE_TOKEN" "$SERVICE_ORIGIN/api/cancel"
  echo "after_cancel_states (polled up to 60s):"
  for _ in $(seq 1 120); do
    st=$(api /api/status | jq -r '[.indexing.base.state, .indexing.head.state] | join(",")')
    echo "$st"
    case "$st" in *cancelled*) break ;; esac
    sleep 0.5
  done | uniq
  echo "cache_entries_published_after_cancel:"
  find "$CACHE/$cachekey" -maxdepth 2 -mindepth 1 -type d 2>/dev/null | wc -l
} >> "$cancel"
stop_service
trap - EXIT

echo "wrote $trace and $cancel"
