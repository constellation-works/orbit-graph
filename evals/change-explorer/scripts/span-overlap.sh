#!/usr/bin/env bash
# For one file in one study, list every indexed head-side symbol span and say
# whether it overlaps a line the diff actually changed.
#
# This is how the study reports decide whether an `uncertain` row — whose
# collapsed selector prevents the explorer's own byte comparison — corresponds
# to a symbol that really changed.
#
# Usage: span-overlap.sh <study-id> <path-relative-to-repo-root>
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

want="${1:?usage: span-overlap.sh <study-id> <path>}"
path="${2:?usage: span-overlap.sh <study-id> <path>}"
repo=""; base=""; head=""
for line in "${STUDIES[@]}"; do
  [ "$(study_field "$line" 1)" = "$want" ] || continue
  repo=$(study_field "$line" 2); base=$(study_field "$line" 3); head=$(study_field "$line" 4)
done
: "${repo:?unknown study $want}"

db=$(find "$CACHE/study-$want/$head/index" -name '*.db' 2>/dev/null | head -1)
[ -n "$db" ] || { echo "no cached head index for study $want; run service-study.sh $want first" >&2; exit 1; }

python3 - "$REPOS/$repo" "$base" "$head" "$path" "$db" <<'PY'
import bisect, re, sqlite3, subprocess, sys

repo, base, head, path, db = sys.argv[1:6]


def git(*args):
    return subprocess.run(["git", "-C", repo, *args], capture_output=True, check=True).stdout


diff = git("diff", "-U0", base, head, "--", path).decode("utf-8", "replace")
ranges = []
for match in re.finditer(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@", diff, re.M):
    start = int(match.group(1))
    count = int(match.group(2) or 1)
    if count:
        ranges.append((start, start + count - 1))
print(f"changed head line ranges in {path}: {ranges or 'none'}")

blob = git("show", f"{head}:{path}")
offsets = [0]
for line in blob.split(b"\n"):
    offsets.append(offsets[-1] + len(line) + 1)


def lines_of(start, end):
    return bisect.bisect_right(offsets, start), bisect.bisect_right(offsets, end)


conn = sqlite3.connect(f"file:{db}?mode=ro&immutable=1", uri=True)
rows = conn.execute(
    "SELECT name, kind, span_start, span_end FROM symbols WHERE file_path = ? ORDER BY span_start",
    (path,),
).fetchall()
print(f"{'symbol':44s} {'kind':12s} {'head lines':>14s}  changed?")
for name, kind, start, end in rows:
    first, last = lines_of(start, end)
    overlaps = any(not (last < lo or first > hi) for lo, hi in ranges)
    print(f"{name:44.44s} {kind:12.12s} {first:6d}-{last:<7d}  {'YES' if overlaps else 'no'}")
PY
