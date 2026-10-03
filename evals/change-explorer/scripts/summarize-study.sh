#!/usr/bin/env bash
# Reduce one study's captured service output to the tables the study report
# quotes: status counts, every non-`added`/`modified` row, entry points by
# rule, candidate tests by source, evidence resolution, unresolved impact and
# bounds hit at each depth.
#
# Usage: summarize-study.sh <study-id>
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

want="${1:?usage: summarize-study.sh <study-id>}"
d="$OUT/study-$want"
[ -d "$d" ] || { echo "no captured output at $d; run service-study.sh $want" >&2; exit 1; }

echo "# study $want summary"
echo
echo "## comparison"
jq -r '{mode, base_sha, head_sha, effective_base_sha, extractor_version,
        dirty: .working_tree.dirty, indexing_status} | to_entries[] | "\(.key)\t\(.value)"' \
  "$d/comparison.json"
jq -r '.snapshots[] | "\(.side)\tfiles_indexed=\(.files_indexed)\tfiles_written=\(.files_written)\texcluded=\(.excluded|length)"' \
  "$d/comparison.json"
jq -r '.indexing | to_entries[] | "\(.key)\telapsed_ms=\(.value.elapsed_ms)\tfiles_seen=\(.value.files_seen)\tfiles_indexed=\(.value.files_indexed)\tfiles_ignored=\(.value.files_ignored)\tunsupported_constructs=\(.value.unsupported_constructs)\tlanguages=\(.value.languages|join(","))"' \
  "$d/comparison.json"

echo
echo "## changed symbols by status"
jq -r '.symbols[].status' "$d/changed-symbols.json" | sort | uniq -c | sort -rn
printf 'out_of_scope\t%s\n' "$(jq '.out_of_scope | length' "$d/changed-symbols.json")"

echo
echo "## every uncertain / removed / renamed / moved / signature_changed row"
jq -r '.symbols[] | select(.status | IN("uncertain","removed","renamed","moved","signature_changed"))
       | [.status, .pairing, (.base.selector // "-"), (.head.selector // "-"), .file_change,
          ((.uncertain_candidates // []) | length)] | @tsv' "$d/changed-symbols.json"

echo
echo "## out-of-scope paths (path, reason, snapshot)"
jq -r '.out_of_scope[]? | [.path, .reason, .snapshot] | @tsv' "$d/changed-symbols.json" \
  | sort | uniq -c | sort -rn | head -40

echo
echo "## entry points by rule"
jq -s -r '[.[] | .entry_points[]?] | group_by(.rule)[] | "\(.[0].rule)\t\(length)"' \
  "$d/entry-points.json"
echo "queries with no entry point:"
jq -s -r '[.[] | select((.entry_points // []) | length == 0)] | length' "$d/entry-points.json"
echo "distinct no-entry-point reasons:"
jq -s -r '[.[] | .no_entry_point_reasons[]?] | group_by(.)[] | "\(length)\t\(.[0])"' \
  "$d/entry-points.json" | sort -rn | head -10

echo
echo "## candidate tests by source"
jq -s -r '[.[] | .candidates[]?] | group_by(.source)[] | "\(.[0].source)\t\(length)"' \
  "$d/candidate-tests.json"
echo "distinct candidate tests:"
jq -s -r '[.[] | .candidates[]?.test.selector] | unique | length' "$d/candidate-tests.json"

echo
echo "## evidence by depth: resolution, paths, truncation, bounds"
for depth in 1 2 3; do
  jq -s -r --arg d "$depth" '
    {depth: $d,
     queries: length,
     resolved: ([.[] | select(.resolved == true)] | length),
     resolved_qualified: ([.[] | select(.resolved_qualified != null)] | length),
     paths: ([.[] | .paths[]?] | length),
     truncated_queries: ([.[] | select(.truncated == true)] | length),
     truncated_by: ([.[] | .truncated_by // empty] | group_by(.) | map({(.[0]): length}) | add),
     bounds_hit: ([.[] | .bounds_hit[]?.bound] | group_by(.) | map({(.[0]): length}) | add),
     no_path_queries: ([.[] | select((.paths // []) | length == 0)] | length),
     skipped_low_confidence: ([.[] | .skipped_low_confidence // 0] | add)
    } | to_entries[] | "d\($d)\t\(.key)\t\(.value|tostring)"' "$d/evidence-d$depth.json"
done

echo
echo "## edge categories at depth 3"
jq -s -r '[.[] | .paths[]?.edges[]?.category] | group_by(.)[] | "\(.[0])\t\(length)"' \
  "$d/evidence-d3.json" | sort -k2 -rn

echo
echo "## no-path reasons at depth 3"
jq -s -r '[.[] | .no_path_reasons[]?] | group_by(.)[] | "\(length)\t\(.[0])"' \
  "$d/evidence-d3.json" | sort -rn | head -10

echo
echo "## search"
jq -s -r '{queries: length,
           matches: ([.[] | .matches[]?] | length),
           truncated: ([.[] | select(.truncated == true)] | length),
           changed_labels: ([.[] | .matches[]?.changed] | group_by(.) | map({(.[0]): length}) | add)}
          | to_entries[] | "\(.key)\t\(.value|tostring)"' "$d/search.json"
