#!/usr/bin/env bash
# Re-run the change-explorer fixture corpus harness at the current commit and
# record the per-case result table.
#
# Usage: fixture-corpus.sh
# Writes $OUT/fixture-corpus.txt and prints the case table.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

mkdir -p "$OUT"
log="$OUT/fixture-corpus.txt"
{
  echo "# change-explorer fixture corpus at $(git -C "$REPO_ROOT" rev-parse HEAD)"
  echo "# cargo test -p orbit-graph-cli --locked --test change_explorer_fixtures -- --nocapture"
} > "$log"
( cd "$REPO_ROOT" && cargo test -p orbit-graph-cli --locked --test change_explorer_fixtures \
    -- --nocapture ) >> "$log" 2>&1
status=$?

echo
printf 'case\tlanguages\tchanged_symbols\texpected_references\texpected_impact\tcandidate_tests\tknown_gaps\n'
for manifest in "$REPO_ROOT"/crates/orbit-graph-cli/tests/fixtures/change-explorer/*/expected.json; do
  jq -r '[.case_id, (.languages | join("+")), (.changed_symbols | length),
          (.expected_references | length), (.expected_impact | length),
          (.candidate_tests | length), (.known_gaps | length)] | @tsv' "$manifest"
done
echo
grep -E '^test |^test result:' "$log" | tail -20
exit $status
