# Real repository paired navigation study (unmeasured)

This is a preregistered, offline corpus and scoring tool for eight navigation
questions. It supplies no agent-effectiveness measurements. The managed worker
runs deterministic fixtures only. Live provider episodes belong to an operator
using the separately delivered confined runner in `scripts/agent-eval/`.

The development cases are `graph-inbound-identity`,
`orbit-crew-resolution-entrypoints`, and `pulsar-bounded-error-commit`. The other
five cases are held out for task-level evaluation. Both splits share repositories:
this is **not an independent-repository holdout**. Prompts were recovered from
ORB-13709 discovery/recovery artifacts and preserved verbatim. Source review
expanded the current Orbit resolver evidence and verified the actual pulsar diff;
it did not change a prompt. `corpus.json` records source-reviewed identities,
behavior claims, exact line ranges, accepted explanations and uncertainty limits.
There is no regex parser pretending to establish arbitrary-code completeness.

| Repository | Original source commit | Exported view |
| --- | --- | --- |
| orbit-graph | `ac6d5b91f973874325bac789e9c433a71c805766` | pinned source |
| Orbit | `0db9356eef631ada11569d0ca4e1e407fad5bee3` | pinned source |
| pulsar | `6d33fb9d4609618c13d46deb2df6af53b7f504a6` | pinned source, except change case |
| pulsar change head | `cfe30af2dc8d05c10c9b476eb53a577c224aa898` | actual change head |
| pulsar change base | `9b7742a11158061cee7d6de6a60e3e97d286262a` | actual head's parent |

Each arm/case gets one fresh session, repository and graph cache. Cases stay in
frozen corpus order; even case indices run baseline first and odd indices run
graph first (four pairs in each order; eight episodes per arm). Within each pair,
model, settings, provider version, read/rg/git versions and budgets match. The
graph binary version is fixed throughout the cohort. Pre-register operator,
installed binary hashes and settings before episodes; do not tune against heldout
answers. Repeats require a new cohort and are not replacements for failed episodes.

Budgets are **300,000 ms wall time**, **60 tool calls**, **1,048,576 bytes total
returned tool/final output**, **65,536 bytes per call**, and **32 answer items**.
The answer explanation is at most 4096 UTF-8 bytes. Wall time includes disposable
source setup and cold graph setup. These bounds are larger than the synthetic
study and frozen before real results. The ORB-13723 request validator reviewed at
implementation accepts these explicit bounds; if another installed runner refuses
them, stop and document a contract adaptation before measurements. Never clamp.

## Freeze and solution-free export

`corpus.lock.json` seals the corpus, truth, split, exact request plan, and full included-file manifests
for all five snapshots. Its canonical hash is also pinned in `eval.py`. There is
no lock-rewrite command. Changes before any real episode must be reviewed as a
new preregistration; changes after results require a new cohort/version. Editing
truth and silently replacing its lock is refused.

Export reads only explicit local Git checkout roots (with their own `.git` marker; parent discovery and bare repositories are refused) and immutable objects, never
working-tree files. It does not change those repositories, configure a plugin,
install dependencies, or call a provider. Every included blob and Git tree is
checked against the lock before writing. The view includes root `Cargo.toml`,
`Cargo.lock`, `pyproject.toml`, plus `.rs`, `.py`, and `Cargo.toml` below `src/`,
`crates/`, and `tests/`. Hidden path components and `docs`, `skills`,
`instructions`, `evaluation`, `agent-eval`, `generated`, `vendor`, `target`,
`node_modules`, `secrets`, and `credentials` components are excluded. Only regular
UTF-8 Git blobs are accepted, with 4 MiB/file, 128 MiB/tree and 10,000-file caps.
This reproducible narrow source view preserves source regression tests, including
inert fake credential strings. It copies no live credential store, user config,
Git hooks, instructions, evaluator, corpus answers, or generated state. It is
not a runnable reconstruction of every repository or its full Git history.

Content revision is SHA-256 of canonical JSON `{relative_path: UTF-8_text}`:
sorted keys, separators `(',', ':')`, Python `ensure_ascii=True`, no NaN. This
matches the synthetic request and confined runner's streaming content revision.
Request hashes use the same canonical encoding excluding `request_sha256`.
Original Git commits and trees are separately retained in each sealed
`source-manifest.json`; content hashes are never presented as original commits.
The change source uses its actual head, not later pulsar HEAD. For other cases
base and head content are identical. Runner-created snapshot Git commits are
new identities and must remain labeled as such. Git `HEAD`/`HEAD^` in a runner
correspond to the exported original head/base source views, not the original
commit objects. The original commit mentioned in the change prompt is provenance;
use the supplied snapshot diff to answer it.

A destination must be fresh, with an existing physical parent. No overwrite is
allowed. `complete.json` is written last; interrupted exports without it are
incomplete and must never be used. Source files and requests contain no truth
selectors, rubric, required behavior or accepted answers. Keep the evaluator and
all export manifests outside the evaluated agent's filesystem; only `head`/`base`
source is passed to the runner. Hashes are integrity checks, not signatures or a
proof that maliciously substituted tooling is trusted.

From the orbit-graph checkout (replace the explicit paths as needed):

```sh
EVAL=docs/evaluation/real-agent-navigation/eval.py
GRAPH_REPO=/home/daniel/workspace/constellation/codebases/orbit-graph
ORBIT_REPO=/home/daniel/workspace/constellation/codebases/orbit
PULSAR_REPO=/home/daniel/workspace/constellation/codebases/pulsar
python3 "$EVAL" validate --repo "orbit-graph=$GRAPH_REPO" \
  --repo "Orbit=$ORBIT_REPO" --repo "pulsar=$PULSAR_REPO"
mkdir -p .orbit/tmp/real-navigation
python3 "$EVAL" export --repo "orbit-graph=$GRAPH_REPO" \
  --repo "Orbit=$ORBIT_REPO" --repo "pulsar=$PULSAR_REPO" \
  --destination "$PWD/.orbit/tmp/real-navigation/cohort-01"
python3 -B -m unittest discover -s docs/evaluation/real-agent-navigation -p 'test_eval.py' -v
```

## Operator episodes and capture adapter

Do not expose the evaluator directory or answers to an agent. The ORB-13723
runner must verify OS confinement, hidden truth paths, effective feature/MCP
inventory, and cleanup. A `truth_inaccessible: true` boolean alone is insufficient.
No scored correct episode or follow-up decision is allowed without raw isolation
evidence. A capability refusal is not an effectiveness measurement; retain its
cause, repair the capability and start a new preregistered cohort. The managed
worker did not run the following paid/live command.

After installing/reviewing that runner through its normal delivery, use fixed
explicit binary paths, a fixed model and settings. Run each of the sixteen export
directories in numeric order, each to a new output directory. This example is one
baseline episode; use the same command/settings for both arms and all cases:

```sh
RUNNER=scripts/agent-eval/eval_runner.py
EPISODE="$PWD/.orbit/tmp/real-navigation/cohort-01/00-graph-inbound-identity-baseline"
python3 "$RUNNER" run --request "$EPISODE/request.json" \
  --head "$EPISODE/head" --base "$EPISODE/base" \
  --out "$PWD/.orbit/tmp/real-navigation/run-00" \
  --codex /absolute/path/to/codex --model MODEL \
  --setting model_reasoning_effort=high \
  --git /usr/bin/git --rg /absolute/path/to/rg \
  --orbit-graph /absolute/path/to/orbit-graph --python /usr/bin/python3 \
  --bwrap /absolute/path/to/bwrap --containment bwrap \
  --truth-path "$PWD/docs/evaluation/real-agent-navigation" \
  --truth-path "$PWD/.orbit/tmp/real-navigation/cohort-01"
```

Provider authentication must use the runner's supported, reviewed operator path;
this helper neither reads nor copies it. Follow the runner's README for capability
preflight and its explicit credential-store binding. No host config or plugin
grants are changed by this study. The runner version/schema is a separate input,
not a dependency imported from a sibling worktree. Real runner invocation and
OS containment remain operator verification; local fake fixtures do not prove
them. `adapt` consumes the runner's raw `episode.json` plus its six captured files,
checks seals/file hashes, and retains exact requests, statuses, captures, timings,
model/settings, versions, nullable usage and links to original raw evidence:

```sh
python3 "$EVAL" adapt --study-kind agent \
  --raw "$PWD/.orbit/tmp/real-navigation/run-00" \
  --raw "$PWD/.orbit/tmp/real-navigation/run-01" \
  --output "$PWD/.orbit/tmp/real-navigation/records.json"
```

Repeat `--raw` for **all sixteen** actual directories. That two-directory example
only demonstrates adapter syntax; scoring it is refused. Failed, invalid and
timeout records are retained, with null answers and the real error code/message.
Do not edit or replace runner failures. The raw runner schema is checked strictly;
new fields need a reviewed adapter change, not silent dropping. Captured content
hashes bind raw files to records but cannot distinguish fabricated data from an
honest operator's captures. Audit provenance and operator custody are required.
`--study-kind test-only` is mandatory for fake fixtures; they can exercise scoring
but can never trigger the real follow-up decision.

## Identity, citation and audited behavior judgments

The response shape is the broker's `items`, `abstain`, `reason`, `evidence` object.
Items are manually preregistered `symbol:path#name:kind` identities. Rust owner
names use `Owner::method`; Python methods use `Class.method`; free functions use
their declared name. Constants and classes have explicit kinds. Cite the exact
HEAD declaration line, stripped of surrounding whitespace. File-only locations
receive no identity credit; valid source citations for wrong names remain identity
false positives. Optional named support symbols may be included. These identity
metrics describe the preregistered bounded set, not arbitrary-source correctness.

The dynamic-dispatch case accepts supported contract/call-site/candidate identities
with `abstain=false` and an explicit explanation that no unconditional unique
callee is guaranteed. The runner's `abstain=true` contract requires empty items and
citations; that answer cannot satisfy this locating prompt. The rubric requires
uncertainty even when the X implementation is accurately cited. This separates
uncertainty judgment from the synthetic empty-item abstention oracle.

The scorer separately reports required-symbol recall, identity precision and
false positives, citation precision/coverage, objective pass, semantic pass and
combined correctness. Objective pass requires all required identities, exact
associated declaration citations, and no out-of-set items or invalid citations.
Behavior is **never inferred from identity presence**. Every successful episode
needs a reviewer-attributed judgment for every claim in its case's frozen rubric,
including admission ordering and uncertainty. Missing audits refuse scoring.
Failed/invalid/timeout episodes need no semantic audit and always score incorrect.

A separate `audits.json` has exactly `schema_version: 1` and `audits: [...]`.
For each successful record, the reviewer writes this structure (all claims must
appear once; replace placeholders with actual content):

```json
{
  "run_id": "CAPTURED_RUN_ID",
  "record_sha256": "EXACT_NEUTRAL_RECORD_HASH",
  "reviewer": "Human name or accountable operator identity",
  "signed_at": "2026-10-03T03:00:00Z",
  "attestation": "I reviewed the explanation against the pinned source and rubric, retaining uncertainty.",
  "judgments": [{
    "claim_id": "FROZEN_RUBRIC_CLAIM_ID",
    "pass": false,
    "rationale": "Explain the observed behavior claim and why it meets or fails the rubric.",
    "answer_quote": "Verbatim nonempty substring of the answer reason",
    "source_evidence": ["REPLACE WITH ONE OR MORE EXACT EVIDENCE OBJECTS FROM THIS CLAIM"]
  }],
  "attestation_sha256": "CANONICAL_HASH_OF_THIS_OBJECT_EXCLUDING_THIS_FIELD"
}
```

For a passing judgment, the quoted explanation must actually express the required
behavior, not merely contain a symbol's name. Use evidence objects from the
corresponding claim; their HEAD/base line ranges and excerpt hashes are validated
against pinned source. Failing judgments also record the observed mistaken
explanation and relevant source. The attestation hash detects edits but does not
authenticate a reviewer or verify their reading. Keep externally authenticated
review custody if a stronger signature is required. Reviewers should blind arm
identity while judging semantics; the record hash restores attribution afterward.
Hash with this helper's `canonical`/`digest` definitions, never a pretty-printed
JSON byte hash. There is deliberately no command that invents passing judgments.

```sh
python3 "$EVAL" score --repo "orbit-graph=$GRAPH_REPO" \
  --repo "Orbit=$ORBIT_REPO" --repo "pulsar=$PULSAR_REPO" \
  --records "$PWD/.orbit/tmp/real-navigation/records.json" \
  --audits "$PWD/.orbit/tmp/real-navigation/audits.json" \
  > "$PWD/.orbit/tmp/real-navigation/report.json"
```

Scoring requires all sixteen unique exact requests, fresh session/source/cache
identities, and matching model/settings/tool versions. Raw captures are re-read
and hash checked, not trusted by boolean. Wrong citations/identities and negative
semantic judgments score incorrect; failed isolation also prevents correctness.
Failures remain in eight-per-arm total, three-per-arm development, and
five-per-arm heldout denominators. Reports include each episode, errors, calls,
output bytes, wall/setup time, nullable usage/cost, split summaries and per-case
paired accuracy/call/byte deltas. Speed deltas and ratios are shown only for
both-correct pairs; zero graph duration yields null ratio. Missing token/cost
values stay null even in totals. There are no confidence intervals, significance
claims or speed claims for unequal-correctness pairs.

The practical follow-up rule only admits consideration of a larger, independently
sourced repository cohort: all sixteen genuine contained records/audits are
present; no heldout graph accuracy regression; at least one heldout improvement;
and no heldout graph identity false positives. Passing that rule never licenses a
superiority claim. These eight tasks within three repositories cannot establish
statistical superiority or generalization. Document failures and uncertainty even
when the exploratory rule is met.

All helpers are stdlib-only and require Linux with `/usr/bin/git`. Each command has a 600-second deadline; each Git read has a 30-second deadline with bounded capture and process-group cleanup. Successful stdout is JSON, errors are structured
JSON on stderr (exit 1); argparse usage errors exit 2. Validation and scoring
write no files. Export/adapt create explicitly named fresh outputs. Test and
validation logs live under `.orbit/tmp/`; graph scratch belongs under the
runner-owned fresh `.orbit-graph/`. No release, tag, publishing or review
automation is part of this deliverable.
