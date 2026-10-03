#!/usr/bin/env bash
# Verify whether the ORB-12372 known omission — calls nested in closures — is
# closed at the current EXTRACTOR_VERSION.
#
# Builds a throwaway single-file crate under $SCRATCH containing each shape
# ORB-12379's own extractor tests name, indexes it with the real `orbit-graph`
# binary, and prints the extracted call refs.
#
# Usage: closure-gap-probe.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
require_binaries

probe="$SCRATCH/repro/closures"
rm -rf "$probe"
mkdir -p "$probe/src"
cat > "$probe/src/lib.rs" <<'RS'
pub struct Conn;
pub struct Graph;

impl Graph {
    pub fn with_read_connection<T>(&self, run: impl FnOnce(&Conn) -> T) -> T {
        run(&Conn)
    }
}

pub fn resolve_target(_conn: &Conn) -> i32 {
    7
}

pub fn qualified_matches_import(candidate: &i32) -> bool {
    *candidate > 0
}

pub fn fetch() -> Option<i32> {
    Some(1)
}

// (a) plain closure argument — a call inside a closure passed to one method call
pub fn plain_closure(graph: &Graph) -> i32 {
    graph.with_read_connection(|conn| resolve_target(conn))
}

// (b) closure inside a method chain
pub fn chained_closure(candidates: Vec<i32>) -> Vec<i32> {
    candidates
        .into_iter()
        .filter(|candidate| qualified_matches_import(candidate))
        .collect()
}

// (c) `?`-wrapped call as a chain receiver
pub fn try_chain() -> Option<i32> {
    fetch()?.checked_add(1)
}

// (d) turbofish method call on a chain
pub fn turbofish(values: Vec<i32>) -> Vec<i32> {
    values.iter().copied().collect::<Vec<_>>()
}
RS

git -C "$probe" init -q .
git -C "$probe" add -A
git -C "$probe" -c user.email=evaluation@invalid -c user.name=evaluation commit -qm probe

( cd "$probe" && "$GRAPH" --format json sync --full )
echo
echo "## extractor version"
"$GRAPH" --format json version 2>/dev/null || "$GRAPH" version

echo
echo "## call refs extracted (name, confidence)"
python3 - "$probe" <<'PY'
import glob, sqlite3, sys
db = glob.glob(f"{sys.argv[1]}/.orbit-graph/*.db")[0]
conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
for name, confidence in conn.execute(
    "SELECT target_name, confidence FROM refs WHERE kind = 'call' ORDER BY target_name"
):
    print(f"  {name}\t{confidence}")
PY

echo
echo "## refs to the closure-nested callees"
for selector in \
  'symbol:src/lib.rs#resolve_target:function' \
  'symbol:src/lib.rs#qualified_matches_import:function' \
  'symbol:src/lib.rs#fetch:function'; do
  printf '%s\n  ' "$selector"
  ( cd "$probe" && "$GRAPH" --format json refs "$selector" ) \
    | jq -c '[.refs[] | {file, line, confidence}]'
done
