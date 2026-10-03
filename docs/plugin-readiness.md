# Linux plugin readiness: integrated candidate da5009b

Measured on 2026-10-03 for ORB-13773, execution `jrun-20261003-0550-c2`.
The clean integrated source, installed CLI/MCP tools, positive public-delivery
fixtures and fresh exports pass the checks below on the exact measured Linux
host. Actual historical executor deliveries are also supported by separately
identified final-candidate backend evidence. The required advisory gate is
**denied** by the worker's read-only host cache; the same policy passes with a
scratch-local cache. This is bounded readiness evidence, with no live upgrade
or release. [Agent measurements](../evals/readiness-20261003/README.md) establish
no agent benefit or superiority.

## Candidate identities

All source/build/installed/export checks ran before the five report edits, at
clean HEAD `da5009b7bf8f1a328aa3923252b3b049b1e8bd17`; starting
`git status --short` was empty. Full machine-readable identities and outcomes
are in [results.json](../evals/readiness-20261003/results.json).

| Input | Measured identity |
| --- | --- |
| Host | Linux x86_64, kernel `6.8.0-142-generic` |
| Orbit path | `/home/daniel/.orbit/bin/orbit`, resolved from `/home/daniel/.cargo/bin/orbit` |
| Orbit version / SHA-256 | `0.25.1` / `6481d488a87955a3f8c29bbee029b8ef1f07babc502c8d1000c37a0850df0972` |
| Rust / Cargo | `rustc 1.96.0 (ac68faa20 2026-05-25)` / `cargo 1.96.0 (30a34c682 2026-05-25)` |
| cargo-deny / nextest | `0.19.9` / run-local `0.9.146` |
| nextest archive SHA-256 (CI pin) | `682c21b777c333e96fd532e114d3a5a894e0729ab88d94c0a9f20f8419695428` |
| Worker graph binary SHA-256 | `22f0580ca89f7aab5930194dd24d5946a05ba3beda20ccf93e194a19cb147a8b` |
| Graph contracts | crate `0.10.0`; extractor `24`; history schema `5`; plugin schema `1`; store schema `2` |
| Pristine plugin manifest SHA-256 | `162b695ec204aca3bca996f47632874f31f6b3bb5c198d55e8a89a9b96bde6a7` |
| Commit export tar SHA-256 | `6ddf2deda2efa37c1641d6b5dc89c9920d1276a7b49af6d9e9fb19c8fda54e2b` |
| Bound local manifest SHA-256 | `59b41656899605763366433e078eac2bbb38a176ec50e8c9b40ce2fd16453f43` |

**The host binary includes the public `orbit.workflow.run.delivery` read that
landed after tagged Orbit 0.25.1.** Its semantic version alone cannot identify
this capability. CI's pinned 0.25.0 and tagged 0.25.1 are not certified for
positive delivered-run import by these measurements. Strict installed testing
uses `ORBIT_GRAPH_TEST_REQUIRE_RUN_DELIVERY=1`, which fails on an absent read.

The debug build used a fresh target under `.orbit/tmp/certification/`,
`CARGO_INCREMENTAL=0`, cleared Rust wrappers and scratch-local `TMPDIR`.
Disk use was below 30% before all new build/export directories, below the 80%
stop threshold. No new worktree or global tool installation was created.
Binary digests identify these bytes; another build requires its own digest.

Pristine and bound exports come from `git archive --format=tar <full commit>
.orbit-plugin scripts/bundle-plugin-binary.sh`. The pristine source retains
`origin: orbit` and the explicit unbound backend override and is checked with
`--first-party` and the measured binary first on `PATH`. The bound copy removes
only `metadata.origin` and bundles that binary with `--backend-sha256`; its
manifest digest matches the installed private copy. Both export names include
the full candidate commit and their certification context.

## Surface evidence

| Check | Outcome and provenance |
| --- | --- |
| Normal workspace suite | 698 passed; three ignored installed tests explicitly run separately |
| Doctests | 14 passed with scratch-local TMPDIR, including the four historically failing examples |
| Installed tools | All 13 tools through real CLI and MCP; manifest/request/MCP inventory equality; nonempty query results |
| Task recommendation / hybrid search | Real private task, `orbit.task.show_public_observation`, `orbit.search_lexical_rank`, no dropped hits or fallback warnings |
| Maintenance | Code and Git-history sync pass; ordinary callers denied mutations on CLI and MCP |
| Installed public delivery | Seeded private host checkpoint fixture: landed pair inserted, replay `already_indexed`, task ID follows current run |
| Delivery negatives | Unlanded pair excluded; foreign task/run binding refused with typed `invalid_input`; bare `run_ids` rejected |
| Request negative | Unknown search field rejected through MCP |
| Pristine export | Validate passes; 44/44 conformance cases pass |
| Bound export / installed matching source | Validate passes; 44/44 cases pass before and after private installation; certification recorded on private host |
| Private install/enable/upgrade | Pass; unchanged grant set retained and bound manifest digest verified |
| Actual executor-delivery evidence | Final-candidate operator backend: four inserted, two content exclusions, four idempotent replays; independent public-read/Git spot check |
| macOS / dashboard / schedules / official `git+` install | Not run; no new certification for these environments |

Tools are `version`, `status`, `recommend`, `maintain`, `search`, `show`, `refs`,
`callees`, `impact`, `trace`, `deps`, `overview`, and `changes`. The installed
canonical test uses `orbit tool run graph.<verb> --input ... --full` and MCP
`graph_<verb>` through `orbit mcp serve`; it does not separately certify every
derived human command rendering.

Private HOME and Orbit roots have grants exactly `fs,orbit_tools`, no network,
no `env_pass` and `unsandboxed: false`. Only fixture setup and outer mutating
calls use the audited operator path. Plugin callbacks use ordinary public tools;
they do not inherit operator authority. Public delivery binds one task/run pair
in the workspace and Git verifies its landing. Listing a callback in the
manifest is not a caller grant. No live plugin tree or live grants were changed.

The installed delivery fixture is **seeded host-record proof**, not a run
produced by an executor. Separately, operator artifact
`certification/actual-delivery-evidence.json` records the final candidate backend
with SHA-256 `af2fe5682eaa4871b0c6aa5ef4572253076cecc9c43da59d2ccbe65e3579293b`,
ordinary public callbacks, fresh private graph state and actual historical host
checkpoints. It has no installed wrapper or seeded host records. ORB-13170,
ORB-13159, ORB-13158 and ORB-13156 import; ORB-13157 and ORB-13229 remain excluded
because their landed contents/modes differ from the checkpoint head. This worker
read ORB-13170/run `jrun-20260926-1851-c3` through the granted public tool and
verified its landing is reachable from the candidate and all 28 changed paths
match the checkpoint postimages. Same source, distinct worker/operator builds:
the authentic backend evidence does not certify the worker binary's installed
path against a new live pipeline run. Import coverage stays explicitly bounded,
not a complete historical scan.

## Required gates

ORB-13773 task artifacts `certification/logs/` contain each exact command,
run ID, tested HEAD, producer exit code and captured stdout/stderr.

| Required command | Outcome | Log |
| --- | --- | --- |
| `sh docs/standards/check.sh` | Passed | `gate-01.json` |
| `scripts/check-dependency-direction.sh` | Passed, four crates | `gate-02.json` |
| `scripts/check-terminal-guard.sh` | Passed | `gate-03.json` |
| `scripts/check-orphan-modules.sh` | Passed | `gate-04.json` |
| `scripts/test-repo-gates.sh` | Passed, 52 seeded cases | `gate-05.json` |
| `python3 -B evals/agent-navigation/eval.py check` | Passed, 23 tests / 12 scripted cases; not agent effectiveness | `gate-06.json` |
| `cargo deny --locked check` | **Denied**, same denial on clean baseline | `gate-07.json`, `baseline-deny.json` |
| `cargo fmt --all --check` | Passed | `gate-08.json` |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | Passed | `gate-09.json` |
| `cargo nextest run --workspace --locked --no-tests=fail` | Passed, 698 tests; three ignored | `gate-10.json` |
| `cargo test --workspace --doc --locked` | Passed, 14 doctests | `gate-11.json` |
| `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked` | Passed | `gate-12.json` |
| `cargo build --workspace --locked` | Passed | `gate-13.json` |
| `git diff --check` | Passed; repeated after report edits | `gate-14.json`, `final-review.json` |

The advisory denial is `failed to obtain lock file
/home/daniel/.cargo/advisory-dbs/db.lock: attempted to take an exclusive lock on
a read-only path`. It reproduced once before tracked edits at the same clean
candidate. A copy of `deny.toml` changing only advisory `db-path` into ignored
scratch passed:

```sh
cargo deny --locked check --config .orbit/tmp/certification/deny.toml
```

No advisory, license, source or ban policy was relaxed; the original gate remains
labeled denied. The task's baseline rule permits this documented environmental
failure to be handed off with the scoped alternate check.

Strict installed validation (3/3 passes, including two legacy tests):

```sh
ORBIT_GRAPH_TEST_ORBIT_BIN=/home/daniel/.orbit/bin/orbit \
ORBIT_GRAPH_TEST_REQUIRE_RUN_DELIVERY=1 \
  cargo test -p orbit-graph-cli --test plugin_integration --test plugin_v2 --locked -- --ignored
```

## Historical findings and outstanding limits

The [archived ORB-13712 report](https://github.com/constellation-works/orbit-graph/blob/da5009b7bf8f1a328aa3923252b3b049b1e8bd17/docs/plugin-readiness.md)
measured source `ac6d5b91f973874325bac789e9c433a71c805766`, graph SHA-256
`fdbfff46d20e3fcf367c81efd0d01143a43df50134ed8c87bc0e615e37bac6d1`, Orbit
SHA-256 `f9bf822eab881ae8a9a81605929f026db79cd742231354420c888326d87891c9`,
42/42 conformance cases, four failing doctests and incomplete delivered-run
import. Those measurements are historical, not silently recast as passes.

ORB-13737 (`9afc4c3`) repaired doctest Git isolation; ORB-13738 (`4f4521c`)
repaired diagnostic-prefixed structured refusal preservation; ORB-13763
(`da5009b`) integrated task-scoped public delivery. Evaluation harness and scorer
repairs, including ORB-13769 broker supervision/diagnostics (`a3dafc0`) and
ORB-13771 raw-redaction compatibility (`1c53261`), are mapped to full commits
in the [measurement report](../evals/readiness-20261003/README.md).
The supplied operator runner suite at this candidate passes 72 strict tests
with zero skips. Historical failed provider episodes remain failed; no provider
rerun proves their recovery or effectiveness.

The existing [changes cache decision D6](design/changes-command/4_decisions.md#d6-where-snapshots-are-cached)
records a deviation from `STD-01@2 §R31`: cold `changes` calls write plugin
snapshot/scratch caches. The historical eleven-tool read-only-state check must
not be extended to `changes`, and was not repeated as a new measurement here.
The [older Mac study](../evals/quality-20260929/README.md) uses other commits and
Orbit 0.25.0. Public import does not enumerate all retries; its two actual
content exclusions and `coverage.complete=false` remain explicit. The two
small navigation cohorts establish neither agent benefit nor a production
latency/cost advantage.

## Candidate install and upgrade handoff

Daniel can prepare a **fresh bound export of the measured source**, without
changing the live installation, from a clean checkout of this exact commit.
Check disk usage before every new build/export directory; at 80% stop. These
Linux commands use scratch under `.orbit/tmp/`; a rebuilt binary gets a new
recorded hash. Use an Orbit host that actually serves the public delivery read,
then run the complete `CONTRIBUTING.md` sequence and strict installed test above.

```sh
candidate=da5009b7bf8f1a328aa3923252b3b049b1e8bd17
test "$(git rev-parse HEAD)" = "$candidate"
test -z "$(git status --short)"
test "$(df --output=pcent . | tail -n 1 | tr -dc '0-9')" -lt 80
mkdir -p .orbit/tmp
export_root=$(mktemp -d "$PWD/.orbit/tmp/graph-$candidate.XXXXXX")
export CARGO_TARGET_DIR="$export_root/target"
export TMPDIR="$export_root/temp"
mkdir -p "$TMPDIR"
cargo build --workspace --locked
binary="$CARGO_TARGET_DIR/debug/orbit-graph"
sha256sum "$binary" "$(readlink -f "$(command -v orbit)")"
orbit tool show orbit.workflow.run.delivery --format json
test "$(df --output=pcent . | tail -n 1 | tr -dc '0-9')" -lt 80
source_root="$export_root/source-$candidate"
mkdir "$source_root"
git archive --format=tar "$candidate" .orbit-plugin scripts/bundle-plugin-binary.sh \
  > "$export_root/source-$candidate.tar"
sha256sum "$export_root/source-$candidate.tar"
tar -xf "$export_root/source-$candidate.tar" -C "$source_root"
python3 - "$source_root/.orbit-plugin/plugin.yaml" <<'PY'
from pathlib import Path
import sys
path = Path(sys.argv[1])
path.write_text(path.read_text().replace('  origin: orbit\n', ''))
PY
sh "$source_root/scripts/bundle-plugin-binary.sh" --binary "$binary" "$source_root"
sha256sum "$source_root/.orbit-plugin/plugin.yaml"
check_home="$export_root/check-home"
check_repo="$export_root/check-repo"
mkdir -p "$check_home" "$check_repo"
env -i HOME="$check_home" PATH="$PATH" GIT_CONFIG_NOSYSTEM=1 \
  GIT_CONFIG_GLOBAL=/dev/null git -C "$check_repo" init -b main
(
  cd "$check_repo"
  env -i HOME="$check_home" XDG_CONFIG_HOME="$check_home" TMPDIR="$TMPDIR" PATH="$PATH" \
    orbit workspace init --name graph-candidate-check --ship-mode local
  env -i HOME="$check_home" XDG_CONFIG_HOME="$check_home" TMPDIR="$TMPDIR" PATH="$PATH" \
    orbit plugin validate "$source_root"
  env -i HOME="$check_home" XDG_CONFIG_HOME="$check_home" TMPDIR="$TMPDIR" PATH="$PATH" \
    orbit plugin test "$source_root"
)
```

A build/export check does not authorize live installation. After Daniel's
separate authorization and review of the recorded manifest, binary and roots,
choose the applicable live command:

```sh
orbit plugin add "$source_root"
# Existing graph installation instead:
# orbit plugin upgrade graph "$source_root"
orbit plugin show graph --format json
orbit plugin test "$source_root"
# Separate permission consent, only after reviewing requested roots:
orbit plugin enable graph --grant fs,orbit_tools
```

Local namespaces are `graph.*` / `graph_*`. Upgrade can retain grants for
unchanged requests; widened requests require fresh consent. Orbit 0.25.1 tests
the matching source containing `.orbit-plugin/plugin.yaml`, not the installed
version directory. Official first-party `git+` installs use the distinct
[release procedure](plugin.md#install); maintainers own tags and publishing.
No live installation, grant change, host upgrade, release or agent-review
automation was performed by this certification.
