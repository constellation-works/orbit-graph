# Linux plugin readiness: candidate 1523af8

As of 2026-10-03, candidate `1523af82c7a9055adb5fd474035ee9a05b505732`
has bounded Linux runtime and export qualification. The completed installed-plugin
study shows **no measured agent advantage**: frozen combined correctness is
baseline **3/6**, graph **2/6**, with **1/4 each** on held-out questions. Two graph
transport failures remain in those denominators. Later transport repairs and
prospective scorer tests do not change that measurement.

The [current evidence report](../evals/readiness-20261003-v2/README.md) and
[machine-readable summary](../evals/readiness-20261003-v2/results.json) retain all
12 attempts, nullable costs, attributed reviews and exact artifact hashes.
This ORB-13801 update changes documentation only. Daniel owns release and install
timing; no live install, upgrade, grant, host upgrade or schedule was changed.

## Exact candidate and export

| Identity | Value |
| --- | --- |
| Candidate commit | `1523af82c7a9055adb5fd474035ee9a05b505732` |
| Candidate tree | `97a6d17c348ecac5968f4be69cb6b738d94201b8` |
| Qualified graph binary SHA-256 | `81ffdbdaa935faee1dbe311cf3494e5b24f2c268d132be24725dcf89593c0227` |
| Graph binary source | `e5c8109296227eece9cda6184fef98372c77fb37`; preserved **debug** build |
| Qualified Orbit version / SHA-256 | `0.25.1` / `3dfbde617f1bbe4ec4befbbd8a54d76fafc050d803cee387c5ad1eeba5048927` |
| Export source commit | `cf4ea61274d7dfa6b031b573eb88c150888385ea` |
| Bound export manifest SHA-256 | `7503b6e2f3b20b4bb22e5977c87a7fb650f0474a4bc6d53648c1ae465b7d40a0` |
| Export provenance SHA-256 | `c99b34d82706b4924e6f7ab534c4d2d36c559da50d07a5307c9a111521282503` |
| Graph contracts | crate `0.10.0`; extractor `24`; history schema `5`; plugin schema `1`; store schema `2` |

The prepared export is
`/home/daniel/workspace/constellation/codebases/orbit-graph/.orbit/tmp/ORB-13709-handoff-cf4ea61/source`.
It contains 45 files, 159,792,175 bytes, including the exact graph binary above.
Its original export identity remains `cf4ea61`; it is not a new build from
`1523af8`. ORB-13709 artifacts `certification/handoff-cf4ea61-provenance.json`
and `certification/handoff-cf4ea61-validation.json` record its file hashes,
byte counts, modes, private-host validation and 44 passing conformance cases.
That export-only check created **no installed certification record**.

Root's `certification/candidate-1523af8-correspondence.json` verifies 380
qualified runtime files, 14 scorer files, all 45 export files, both executable
digests and unchanged sealed study results. This report independently rechecked
those files and the empty Rust/Cargo/plugin/bundler diff from the binary's source
to the candidate. Correspondence extends the stated source scope of earlier
checks; it is **not a fresh test or installed-host certification**. The candidate
was clean before the report edits. Root merged the integrated scorer at
14:27:02Z; root reports combined CI run `37129363533` with five passing jobs.
This worker could not independently fetch that run because GitHub authentication
was unavailable; the local gates below were executed here.

## Runtime evidence and its boundaries

| Evidence | Observed outcome and attribution |
| --- | --- |
| Installed CLI/MCP | ORB-13790 `operator/runtime-draft-installed-and-strict.json`: three real private-install tests passed, including all 13 v2 tools, CLI/MCP inventory and public-delivery fixtures. The installed test built the preserved `81ff…` binary; its source correspondence is verified, rather than relabelling the draft as a fresh final-head run. |
| Prepared export | ORB-13709 `certification/handoff-cf4ea61-validation.json`: fresh private-host validate and 44/44 conformance; export-only. |
| Repaired evaluator | ORB-13802 `operator/strict116-qualified-v1.json`: 116 strict tests, zero skips, at `8a0484db6532fe064ae0e5cbbcd9edad7735c0b5`, root Linux host, local fake provider only. SHA-256 `7aaf3e04254146c98ecaf945ead973622e49b3d50d711330da52f353e0e7e7e8`. |
| Actual Codex admission | ORB-13802 `operator/actual-admission-index.json` and `actual-admission-actual-v{1,2}.tar.gz`: baseline and graph smoke, masking/replay/cleanup checked. First graph rehearsal passed functionally at 2,188 ms but missed the >5 s criterion; retained. Final larger synthetic fixture passed at **9,841 ms**. |
| Separate slow-call proof | ORB-13802 `validation/private-slow-proof.json`: worker private real-host call **23,562 ms**; distinct from root's actual Codex smoke. |
| Prospective scorer | ORB-13804 `operator/final-admission.json`: 34 tests, 12 independent public CLI cases and normal-host helper policy pass; 14 exact files and stable helper binary. See the [contract and limits](../evals/source-identity-v1/README.md). |

The runtime includes the first-sync/checkpoint locking and guard-lifetime repairs.
The evaluator repairs cover EOF teardown, safe reply provenance/redaction,
bounded pagination and slow MCP replies. The frozen v2 cohort used the **earlier**
harness: its two lost-reply outcomes are retained. Repaired runtime admission is
functional evidence, not a replacement effectiveness cohort.

The 13 tools are `version`, `status`, `recommend`, `maintain`, `search`, `show`,
`refs`, `callees`, `impact`, `trace`, `deps`, `overview` and `changes`.
Private tests use isolated HOME/Orbit roots and `fs,orbit_tools` grants, with
no network, `env_pass` or unsandboxed permission. Ordinary callers still cannot
invoke mutation merely because a callback is listed in the manifest.
Public-delivery fixtures prove seeded host-record handling; the
[historical report](../evals/readiness-20261003/README.md) separately records
actual historical executor deliveries and content exclusions on its named bytes.
Neither is a new live executor-delivery certification of this export.

No broader macOS, dashboard, scheduler, official `git+` installation or optimized
production-latency certification follows. Orbit's semantic version alone does
not prove `orbit.workflow.run.delivery` capability. Import remains bounded to
named task/run pairs with partial coverage. Cold `changes` calls write their
snapshot/scratch cache under the existing
[D6 deviation](design/changes-command/4_decisions.md#d6-where-snapshots-are-cached);
an older read-only-state check must not be extended to that tool.

## Repository and report checks

ORB-13801 artifacts `validation/gate-00.json` through `gate-13.json` record the
complete [CONTRIBUTING sequence](../CONTRIBUTING.md), command, tested HEAD, exit
code and captured output. At this candidate:

| Commands | Outcome |
| --- | --- |
| Standards, dependency direction, terminal guard, orphan modules, repository gate self-tests | Passed; 52 seeded gate cases |
| `python3 -B evals/agent-navigation/eval.py check` | Passed, 23 tests / 12 scripted cases; no effectiveness inference |
| `cargo deny --locked check` | Default worker cache denied; reproduced once on the clean baseline. Same policy passed with only advisory `db-path` redirected to task scratch. |
| `cargo fmt --all --check`; `cargo clippy --workspace --all-targets --locked -- -D warnings` | Passed |
| `cargo nextest run --workspace --locked --no-tests=fail` | Passed, 705 tests; three intentional installed-test skips |
| `cargo test --workspace --doc --locked` | Passed, 14 doctests |
| `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked`; `cargo build --workspace --locked` | Passed |
| `git diff --check`; JSON, local links, metric/provenance and historical-byte reconciliation | Passed; final evidence in `validation/report-checks.json` |
| Real installed Orbit `plugin add/upgrade/show/test/enable/validate --help`, workspace/tool help | Passed; syntax checked without performing live actions |

The default advisory error was `failed to obtain lock file
'/home/daniel/.cargo/advisory-dbs/db.lock': attempted to take an exclusive lock
on a read-only path`. `validation/baseline-deny.json` preserves the reproduction;
`validation/deny-private.json` records `cargo deny --locked check --config
<task-scratch>/deny.toml` passing. No advisory, license, ban or source rule was
relaxed. The default command is not reported as a worker pass. Prior ORB-13804
`repair/*` evidence retains the same cache denial and old-nextest mismatch,
private equivalent-policy passes, 705 Rust tests, 14 doctests and three product
goldens. This run uses the supplied private nextest **0.9.146**, SHA-256
`aac21fea56e7ae3f45b5b7c606e02d62d4ec5b3578658fefe4a98a5f29c4bf1a`;
there was no global installation or version bypass.

## Optional installation handoff

After Daniel authorizes installation, select the prepared source and the intended
registered workspace. These commands are instructions, not actions performed by
this report:

```sh
source_root=/home/daniel/workspace/constellation/codebases/orbit-graph/.orbit/tmp/ORB-13709-handoff-cf4ea61/source
workspace=/absolute/path/to/registered/workspace
orbit plugin add "$source_root"
# Existing graph installation instead of add:
# orbit plugin upgrade graph "$source_root"
orbit plugin show graph --format json
orbit plugin test "$source_root"
# Separate host consent: review manifest, binary, filesystem roots and callbacks first.
orbit plugin enable graph --scope host --grant fs,orbit_tools --workspace "$workspace"
# If the selected workspace has its own disabled toggle, enable it separately:
orbit plugin enable graph --scope workspace --workspace "$workspace"
```

Host consent replaces the full grant set; workspace enable cannot grant
permissions and requires a host-enabled plugin. Host enable applies across the
host, not just the workspace named for seeding. Upgrade retains unchanged grants;
widened requests clear grants and disable the plugin until consent. The source
must contain `.orbit-plugin/plugin.yaml`; test the matching source, not the
installed version directory. Local sources remove `metadata.origin` and register
`graph.*` / MCP `graph_*`. Verified first-party `git+` sources may retain
`origin: orbit` and register `orbit.graph.*` / `orbit_graph_*`; see
[installation requirements](plugin.md#install). `fs` covers requested workspace
reads and plugin-state writes; `orbit_tools` covers bounded public callbacks.
Neither enables ordinary mutation authority. Seeded routines remain disabled.

For a **fresh export** of the current candidate, the following recipe preserves
the qualified debug binary and checks source correspondence before bundling.
Run from a clean checkout of the pinned commit. It creates only an export and a
private validation workspace; it does not install a plugin. The newly exported
source has its own provenance and must not be called the old `cf4ea61` export.

```sh
set -eu
candidate=1523af82c7a9055adb5fd474035ee9a05b505732
binary_source=e5c8109296227eece9cda6184fef98372c77fb37
binary=/home/daniel/workspace/constellation/codebases/orbit-graph/.orbit/tmp/ORB-13709-plugin-study-v1/runtime-13790/orbit-graph
test "$(git rev-parse HEAD)" = "$candidate"
test -z "$(git status --short)"
git diff --exit-code "$binary_source" "$candidate" -- \
  crates Cargo.toml Cargo.lock .orbit-plugin scripts/bundle-plugin-binary.sh
printf '%s  %s\n' 81ffdbdaa935faee1dbe311cf3494e5b24f2c268d132be24725dcf89593c0227 "$binary" | sha256sum -c -
orbit tool show orbit.workflow.run.delivery --format json
# Stop at 80% before each new export, worktree or build directory.
test "$(df --output=pcent . | tail -n 1 | tr -dc '0-9')" -lt 80
mkdir -p .orbit/tmp
export_root=$(mktemp -d "$PWD/.orbit/tmp/graph-$candidate.XXXXXX")
source_root="$export_root/source"
mkdir "$source_root"
git archive --format=tar "$candidate" .orbit-plugin scripts/bundle-plugin-binary.sh > "$export_root/source.tar"
tar -xf "$export_root/source.tar" -C "$source_root"
python3 - "$source_root/.orbit-plugin/plugin.yaml" <<'PY'
from pathlib import Path
import sys
path = Path(sys.argv[1])
text = path.read_text()
assert text.count('  origin: orbit\n') == 1
path.write_text(text.replace('  origin: orbit\n', ''))
PY
sh "$source_root/scripts/bundle-plugin-binary.sh" --binary "$binary" "$source_root"
sha256sum "$export_root/source.tar" "$source_root/.orbit-plugin/plugin.yaml" \
  "$source_root/.orbit-plugin/bin/orbit-graph.bin" "$(readlink -f "$(command -v orbit)")"
check_home="$export_root/home"
check_repo="$export_root/repo"
mkdir -p "$check_home" "$check_repo" "$export_root/temp"
env -i HOME="$check_home" PATH="$PATH" GIT_CONFIG_NOSYSTEM=1 \
  GIT_CONFIG_GLOBAL=/dev/null git -C "$check_repo" init -b main
(
  cd "$check_repo"
  export HOME="$check_home" XDG_CONFIG_HOME="$check_home" TMPDIR="$export_root/temp"
  env -i HOME="$HOME" XDG_CONFIG_HOME="$XDG_CONFIG_HOME" TMPDIR="$TMPDIR" PATH="$PATH" \
    orbit workspace init --name graph-candidate-check --ship-mode local
  env -i HOME="$HOME" XDG_CONFIG_HOME="$XDG_CONFIG_HOME" TMPDIR="$TMPDIR" PATH="$PATH" \
    orbit plugin validate "$source_root"
  env -i HOME="$HOME" XDG_CONFIG_HOME="$XDG_CONFIG_HOME" TMPDIR="$TMPDIR" PATH="$PATH" \
    orbit plugin test "$source_root"
)
```

The existing prepared-export logs exercise this procedure's bundler, private
workspace, validate and test operations on the identical plugin/bundler bytes;
this report checked the current CLI help and source, without rerunning an
unnecessary installation suite. Archive the new manifest, executable digests,
file inventory and command logs with each fresh export. If rebuilding instead,
check disk first, use a private target with `cargo build --workspace --locked`,
and qualify the new binary separately through the complete repository sequence
and strict private installed tests from CONTRIBUTING, including
`ORBIT_GRAPH_TEST_REQUIRE_RUN_DELIVERY=1`. Never inherit a previous binary's hash
or its latency claims. Maintainers own release versions, tags and publishing.
