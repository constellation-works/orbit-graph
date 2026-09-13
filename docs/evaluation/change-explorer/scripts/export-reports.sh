#!/usr/bin/env bash
# Export the machine-contract change report (JSON + self-contained HTML) for
# one study, or for every study when no id is given.
#
# Usage: export-reports.sh [study-id]
#
# The committed exports are bounded on purpose. `orbit-graph-explorer report`
# with no `--selector` covers every changed symbol at the requested depth, which
# for these corpora produces 3-8 MB of JSON per study; the same two selectors at
# `--depth 2` still produce 2.4 MB for the `orbit` studies. This repository's
# whole packfile is under 100 KB, so the committed profile is the two symbols
# each study report answers question 4 about, at `--depth 1`, with
# `--excerpts none`. The wider depth-2 and depth-3 numbers the Markdown quotes
# come from service-study.sh and bounds-probe.sh, which are equally
# reproducible; raise DEPTH here to regenerate them.
#
# `--generated-at` is pinned so the exports are byte-identical across runs.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
require_binaries

GENERATED_AT="${GENERATED_AT:-2026-09-13T00:00:00Z}"
DEPTH="${DEPTH:-1}"
dest="${DEST:-$REPO_ROOT/docs/evaluation/change-explorer/reports}"
mkdir -p "$dest"

# The two "most consequential changed symbols" each study report analyses.
focus_selectors() { # focus_selectors <study-id>
  case "$1" in
    1) printf '%s\n' \
         'symbol:src/sync/pass2.rs#resolve_ref:function' \
         'symbol:src/query/refs.rs#resolve_target:function' ;;
    2) printf '%s\n' \
         'symbol:crates/orbit-cli/src/command/workspace/teardown.rs#resolve_teardown_target:function' \
         'symbol:crates/orbit-cli/src/command/workspace/teardown.rs#execute:method' ;;
    3) printf '%s\n' \
         'symbol:crates/orbit-types/src/task/model.rs#TaskComplexity:enum' \
         'symbol:crates/orbit-core/src/adapter/tool_host/input.rs#parse_task_complexity:function' ;;
    4) printf '%s\n' \
         'symbol:experiments/physics/fput-recurrence-reproduction/run.py#assessment:function' \
         'symbol:experiments/physics/fput-recurrence-reproduction/fput/metrics.py#evaluate_metrics:function' ;;
    5) printf '%s\n' \
         'symbol:scripts/research_records.py#supporting_paths:function' \
         'symbol:lab/sims/oscillating-electron-retarded-fields/browser-check.py#summary:function' ;;
  esac
}

: > "$OUT/report-export-timing.tsv"
for line in "${STUDIES[@]}"; do
  id=$(study_field "$line" 1)
  [ -z "${1:-}" ] || [ "$1" = "$id" ] || continue
  repo=$(study_field "$line" 2); base=$(study_field "$line" 3)
  head=$(study_field "$line" 4); label=$(study_field "$line" 5)
  name="$id-$repo-${head:0:12}"

  args=()
  while IFS= read -r selector; do args+=(--selector "$selector"); done < <(focus_selectors "$id")

  start=$(date +%s.%N)
  "$EXPLORER" report \
    --repo "$REPOS/$repo" --base "$base" --head "$head" \
    --cache-dir "$CACHE/study-$id" \
    --out "$dest" --name "$name" --force \
    --excerpts none --depth "$DEPTH" --generated-at "$GENERATED_AT" \
    "${args[@]}"
  end=$(date +%s.%N)
  printf 'study\t%s\tlabel\t%s\treport\t%s\texport_seconds\t%.2f\tjson_bytes\t%s\thtml_bytes\t%s\n' \
    "$id" "$label" "$name" "$(echo "$end - $start" | bc)" \
    "$(stat -c %s "$dest/$name.json")" "$(stat -c %s "$dest/$name.html")" \
    | tee -a "$OUT/report-export-timing.tsv"
done
