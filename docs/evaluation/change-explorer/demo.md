# Demo: exploring a real change end to end

A scripted walkthrough of the seven steps a developer follows to understand
one Git change with `orbit-graph-explorer`: **open a comparison, read the
change list, inspect potential impact, review candidate tests, read the
source evidence, explore further (outbound callees or search), and export a
report to share.** Those seven steps are the shipped contract's three-pane
[UI sketch](../../design/change-explorer.md#ui-sketch) plus the two capabilities
the UI sketch's panes summarize but that this walkthrough exercises directly
(directional exploration and the `report` export) — the explorer's own spec
document that first enumerated "seven workflow steps" was not available in
this task's context, so this list is derived from, and cross-checked against,
the real running service rather than transcribed from memory.

Every command below is copy-pasteable and was run for real against this
repository's own history — `orbit-graph-explorer` exploring two commits of
`orbit-graph` itself — while writing this document. Total time from a cold
`serve` launch through the export in step 7 was under two minutes on the
machine that ran it; budget under ten even on a slower one, since the
one-time cost is the two cold index builds in step 1.

Prerequisites: follow
[`crates/orbit-graph-explorer/README.md`](../../../crates/orbit-graph-explorer/README.md)
to install the binary first. This walkthrough uses `curl` for the JSON
routes so it runs the same in a terminal or a CI log; open the printed URL in
a browser to see the same evidence in the three-pane UI instead.

## 0. Pick a real base and head

Any two commits work. This walkthrough uses two consecutive merged fixes in
`orbit-graph`'s own history:

```sh
BASE=f3ab99c   # fix: per-side indexing progress (ORB-12412)
HEAD=c1ce566   # fix: indexing progress stalls at 100% (ORB-12424)
```

## 1. Open a comparison

```sh
orbit-graph-explorer serve --repo . --base "$BASE" --head "$HEAD" --port 18732
```

```text
orbit-graph-explorer listening on http://127.0.0.1:18732
Open: http://127.0.0.1:18732/#token=5b5cc3d4548f7286ab45dd78e1bb0208513491baa987c5abe22086aaac5a0eb6
Authorization: Bearer 5b5cc3d4548f7286ab45dd78e1bb0208513491baa987c5abe22086aaac5a0eb6
Loopback only; the token is per-launch and is not written to disk.
```

Both revisions are materialized into isolated temporary trees and indexed
from a cold start; on this run that took about 17 seconds for both sides of
a 225-file repository (see
[`performance.md`](performance.md) for cold-build numbers on a larger
corpus). Poll readiness before querying:

```sh
TOKEN=5b5cc3d4548f7286ab45dd78e1bb0208513491baa987c5abe22086aaac5a0eb6
BASE_URL=http://127.0.0.1:18732
until [ "$(curl -s -H "Authorization: Bearer $TOKEN" "$BASE_URL/api/health" \
            | python3 -c 'import json,sys;print(json.load(sys.stdin)["indexing_status"])')" = ready ]; do
  sleep 1
done
```

**Screenshot: pending.** No headless browser was available on the host that
authored this walkthrough (the same constraint the
[evaluation](README.md#limitations-handoff) recorded); the file list below is
ready for whoever captures them next.

- `screenshots/01-launch-terminal.png` (desktop) — the launch banner above.
- `screenshots/01-launch-terminal-narrow.png` (narrow) — same, narrow viewport.

## 2. Read the change list

```sh
curl -s -H "Authorization: Bearer $TOKEN" "$BASE_URL/api/changed-symbols" \
  | python3 -m json.tool | head -20
```

This comparison has 79 changed symbols and 6 out-of-scope files. Pane 1 of
the UI groups them by status (`added`, `removed`, `modified`, `uncertain`,
…); the header always shows both short SHAs, the comparison mode, and a
dirty-working-tree notice when the repository you point `--repo` at has
uncommitted changes — snapshots index committed revisions only.

- `screenshots/02-change-list.png` / `screenshots/02-change-list-narrow.png` — pending.

## 3. Inspect potential impact for one changed symbol

```sh
SELECTOR='symbol:crates/orbit-graph-explorer/src/evidence.rs#entry_points:method'
ENCODED=$(python3 -c "import urllib.parse,sys; print(urllib.parse.quote(sys.argv[1]))" "$SELECTOR")
curl -s -H "Authorization: Bearer $TOKEN" \
  "$BASE_URL/api/evidence?selector=$ENCODED&side=head&depth=2" \
  | python3 -c 'import json,sys; d=json.load(sys.stdin); print("resolved:", d["resolved"], "| paths:", len(d["paths"]), "| truncated:", d["truncated"])'
```

```text
resolved: True | paths: 7 | truncated: True
```

Pane 2 shows this as the "potential impact" list: callers and neighbours,
each with its relationship, evidence category, and confidence, with the
bound (`depth=2` here) and the truncation flag printed above the list rather
than hidden in a tooltip.

- `screenshots/03-impact.png` / `screenshots/03-impact-narrow.png` — pending.

## 4. Review candidate tests

```sh
curl -s -H "Authorization: Bearer $TOKEN" \
  "$BASE_URL/api/candidate-tests?selector=$ENCODED&side=head" \
  | python3 -c 'import json,sys; [print(c["source"], c["category"], c["test"]["selector"]) for c in json.load(sys.stdin)["candidates"]]'
```

```text
import_relationship import_relationship file:crates/orbit-graph-explorer/tests/changed_symbols.rs
naming_heuristic heuristic_match symbol:crates/orbit-graph-explorer/tests/exploration.rs#entry_points_report_the_rule_that_fired_and_the_shortest_path:test
```

Two candidates, one from each of two disclosed sources: an import
relationship and a naming heuristic. Neither is presented as coverage —
that claim is never made by this tool (see
[Evidence categories](../../design/change-explorer.md#evidence-categories)).

- `screenshots/04-candidate-tests.png` / `screenshots/04-candidate-tests-narrow.png` — pending.

## 5. Read the source evidence

```sh
curl -s -H "Authorization: Bearer $TOKEN" \
  "$BASE_URL/api/source?selector=$ENCODED&side=head" \
  | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d["file"], d["span"]); print(d["bytes_or_text"][:120])'
```

```text
crates/orbit-graph-explorer/src/evidence.rs {'start': 57248, 'end': 61143}
pub fn entry_points(
        &mut self,
        selector: &str,
        query: &
```

Pane 3 shows this same bounded excerpt with the snapshot's SHA in the header
and the cited line highlighted; for a `modified` symbol it shows base and
head side by side.

- `screenshots/05-source.png` / `screenshots/05-source-narrow.png` — pending.

## 6. Explore further: outbound callees, or search

Every evidence route defaults to inbound (what could reach this symbol).
Ask what it reaches instead:

```sh
curl -s -H "Authorization: Bearer $TOKEN" \
  "$BASE_URL/api/evidence?selector=$ENCODED&side=head&direction=outbound&depth=1" \
  | python3 -c 'import json,sys; d=json.load(sys.stdin); print("resolved:", d["resolved"], "| paths:", len(d["paths"]))'
```

```text
resolved: True | paths: 4
```

Or search the snapshot directly instead of starting from a changed symbol:

```sh
curl -s -H "Authorization: Bearer $TOKEN" "$BASE_URL/api/search?q=entry_points&side=head" \
  | python3 -c 'import json,sys; print(len(json.load(sys.stdin)["matches"]), "matches")'
```

```text
13 matches
```

- `screenshots/06-outbound-or-search.png` / `screenshots/06-outbound-or-search-narrow.png` — pending.

## 7. Export a report to share

```sh
curl -s -X POST -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d "{\"selection\":[\"$SELECTOR\"],\"excerpts\":\"none\",\"depth\":1}" \
  "$BASE_URL/api/report" | python3 -c 'import json,sys; print(sorted(json.load(sys.stdin).keys()))'
```

or, without starting the service at all, the equivalent CLI export used for
[the committed sample exports](samples/):

```sh
orbit-graph-explorer report --repo . --base "$BASE" --head "$HEAD" \
  --selector "$SELECTOR" --excerpts none --depth 1 \
  --out /tmp/demo-report --name demo --generated-at 2026-09-13T00:00:00Z
```

Both produce the same self-describing payload — comparison, index identity,
query options, changed symbols, evidence, candidate tests, and the complete
truncated/unsupported/excluded scope — either as JSON over HTTP or as the
`<name>.json` + `<name>.html` file pair `report` writes to disk.

Stop the service when finished (`Ctrl-C`, or `kill` the process started
above); both snapshot trees and their indexes are removed with it unless
`--cache-dir`/the default cache kept them (see
[the explorer README's cache section](../../../crates/orbit-graph-explorer/README.md#cache-directory-and-clean)).

## Screenshot backlog

None of the eight files listed above (steps 1–6, desktop + narrow each,
except step 1 which needs only the terminal banner) could be captured while
authoring this document: no headless browser (`chromium`, `google-chrome`)
was installed on the host, matching the same gap the evaluation recorded for
its UI timing. Whoever next has a headless browser available should:

1. Run steps 1–7 above against a real repository with the UI open at the
   printed URL instead of `curl`.
2. Capture each pending file at both a desktop width (≥1280px) and a narrow
   width (≤480px), per the mandatory-fallback rule in
   [the UI sketch](../../design/change-explorer.md#ui-sketch): the plain-table
   fallback must be legible at both.
3. Save them into `screenshots/` using the file names listed under each step.
