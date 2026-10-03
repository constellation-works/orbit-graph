#!/usr/bin/env bash
# Re-run a change-explorer study through both agent surfaces of change
# analysis: `orbit-graph changes` and the `orbit.graph.changes` plugin tool.
#
#   SCRATCH=<dir outside any checkout you care about> run-study.sh <2|3>
#
# Each surface runs twice against a fresh cache, cold then warm, and the wall
# clock of each call is appended to $OUT/timings.tsv. The corpus is cloned
# with `--no-hardlinks` into $SCRATCH/repos, so nothing here touches the
# sibling checkout. Every run writes to a new directory under $SCRATCH/runs;
# nothing is deleted.
#
# Outputs, in $OUT (default: this directory's parent):
#   <n>-plugin.json       plugin response at the shipped plugin defaults, as
#                         an agent receives it (bounded to 512 KiB)
#   <n>-cli.summary.json  projection of the CLI document at the shipped CLI
#                         defaults (same_module floor): per changed symbol, its
#                         pairing and every caller, entry point and candidate
#                         test with source, confidence and first reference
#   <n>-cli-fuzzy.summary.json
#                         the same projection with --confidence fuzzy and
#                         raised per-symbol caps, the profile closest to the
#                         explorer studies
#   timings.tsv           study, surface, cache state, wall ms, complete
# The full CLI documents (1-6 MB) stay in the run directory.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="${REPO_ROOT:-$(cd "$here/../../.." && pwd -P)}"
SCRATCH="${SCRATCH:?set SCRATCH to a scratch directory}"
CODEBASES="${CODEBASES:-$HOME/workspace/constellation/codebases}"
GRAPH="${GRAPH:-$REPO_ROOT/target/release/orbit-graph}"
OUT="${OUT:-$(cd "$here/.." && pwd -P)}"

case "${1:-}" in
  2) base=1ca6416e0ba2c6a713e42561b7b35a72b045aa25
     head=5a5b45fec9a83b765b485043398d3abadaa468de ;;
  3) base=ab6135e11aeba511c7dbb2c20e766c78d166c208
     head=156dc93d940ee60c4b1c7cd84db620e128977ff2 ;;
  *) echo "usage: run-study.sh <2|3>" >&2; exit 2 ;;
esac
study="$1"

[ -x "$GRAPH" ] || { echo "build first: cargo build --release -p orbit-graph-cli" >&2; exit 1; }
repo="$SCRATCH/repos/orbit"
if [ ! -d "$repo/.git" ]; then
  mkdir -p "$SCRATCH/repos"
  git clone --no-hardlinks --quiet "file://$CODEBASES/orbit" "$repo"
fi
run="$SCRATCH/runs/$study-$(date +%Y%m%dT%H%M%S)-$$"
mkdir -p "$run" "$OUT"
timings="$OUT/timings.tsv"
[ -f "$timings" ] || printf 'study\tsurface\tcache\twall_ms\tcomplete\tanalysed\tnot_analysed\n' > "$timings"

now_ms() { date +%s%3N; }

record() { # record <surface> <cache> <ms> <document>
  jq -r --arg s "$study" --arg surface "$1" --arg cache "$2" --arg ms "$3" '
    (.result // .) as $d
    | [$s, $surface, $cache, $ms, ($d.complete|tostring),
       ($d.summary.analysed_symbols|tostring), ($d.summary.not_analysed_symbols|tostring)]
    | @tsv' "$4" >> "$timings"
}

cli() { # cli <cache-state> <out> [extra args...]
  local state="$1" out="$2"; shift 2
  local start end
  start=$(now_ms)
  (cd "$repo" && "$GRAPH" changes "$base..$head" --json --cache-dir "$run/cli-cache" "$@") > "$out"
  end=$(now_ms)
  record cli "$state" $((end - start)) "$out"
}

plugin() { # plugin <cache-state> <out>
  local state="$1" out="$2" start end response
  start=$(now_ms)
  response=$(jq -cn --arg repo "$repo" --arg base "$base" --arg head "$head" '{
      schema_version: 1, tool: "orbit.graph.changes",
      input: {repository: $repo, base: $base, head: $head},
      context: {workspace_root: $repo, agent: "evaluation", model: "none"}}' \
    | (cd "$repo" && ORBIT_TOOL_NAME=orbit.graph.changes ORBIT_PLUGIN_STATE="$run/plugin-state" "$GRAPH"))
  end=$(now_ms)
  printf '%s\n' "$response" | jq '.output' > "$out"
  record plugin "$state" $((end - start)) "$out"
}

summarize() { # summarize <document> <out>
  jq '{
    comparison: {base: .comparison.base.commit_sha, head: .comparison.head.commit_sha},
    summary, truncated,
    truncation_bounds: (.truncation | group_by(.bound) | map({(.[0].bound): length}) | add),
    symbols: [.symbols[] | {
      selector, status, pairing,
      callers_found, entry_points_found, candidate_tests_found,
      callers: [.callers[] | {selector: .caller.selector, source, confidence, distance,
                              reference: .evidence.edges[0].reference}],
      entry_points: [.entry_points[] | {selector: .entry_point.selector, source, confidence,
                                        distance}],
      candidate_tests: [.candidate_tests[] | {test: .test.selector, source, confidence}]
    }],
    not_analysed
  }' "$1" > "$2"
}

cli cold "$run/cli-cold.json"
cli warm "$run/cli.json"
cli warm "$run/cli-fuzzy.json" --confidence fuzzy --max-callers 100 --max-tests 100 \
  --max-entry-points 50
plugin cold "$run/plugin-cold.json"
plugin warm "$OUT/$study-plugin.json"
summarize "$run/cli.json" "$OUT/$study-cli.summary.json"
summarize "$run/cli-fuzzy.json" "$OUT/$study-cli-fuzzy.summary.json"

# Neither surface may write into the corpus checkout.
if [ -e "$repo/.orbit-graph" ] || [ -n "$(git -C "$repo" status --porcelain)" ]; then
  echo "the corpus checkout was written to" >&2
  exit 1
fi
echo "study $study: outputs in $OUT, scratch in $run"
