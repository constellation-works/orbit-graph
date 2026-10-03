# Linux plugin readiness: candidate ac6d5b9

Measured on 2026-10-03 in execution `jrun-20261003-0153-c2` for ORB-13712.
The Linux CLI/MCP query and live task-callback surfaces work on Orbit 0.25.1.
This is a bounded readiness report, not an unconditional production certificate:
delivered-run import remains **incomplete**, four doctests fail in the required
scratch environment, and diagnostic-prefixed callback refusals lose their
structured Orbit error code. Neither denial nor an empty sync is a passing
import check. Repairs are tracked below; no runtime code was changed here.

## Candidate and measurement provenance

All candidate gates and installed tests ran before documentation edits, at the
clean, immutable source commit
`ac6d5b91f973874325bac789e9c433a71c805766`. Starting `git status --short` was
empty. The source is the standalone workspace at
`https://github.com/constellation-works/orbit-graph.git`.

| Input | Measured identity |
| --- | --- |
| Host | Linux x86_64, kernel `6.8.0-142-generic` |
| Orbit executable | `/home/daniel/.orbit/bin/orbit`, resolved from `/home/daniel/.cargo/bin/orbit` |
| Orbit version / SHA-256 | `0.25.1` / `f9bf822eab881ae8a9a81605929f026db79cd742231354420c888326d87891c9` |
| Rust / Cargo | `rustc 1.96.0 (ac68faa20 2026-05-25)` / `cargo 1.96.0 (30a34c682 2026-05-25)` |
| cargo-deny | `0.19.9` |
| cargo-nextest | Run-local `0.9.146`, commit `8af696ddcce8fff2962d6a5168b6d138b8616a35` |
| nextest downloaded archive SHA-256 | `682c21b777c333e96fd532e114d3a5a894e0729ab88d94c0a9f20f8419695428` (the CI pin) |
| Graph build | Fresh `cargo build --workspace --locked`, debug executable, wrappers disabled, `CARGO_INCREMENTAL=0` |
| Graph binary SHA-256 | `fdbfff46d20e3fcf367c81efd0d01143a43df50134ed8c87bc0e615e37bac6d1` |
| Graph contracts | crate `0.10.0`; extractor `24`; history schema `5`; plugin schema `1`; store schema `2` |
| Clean source manifest SHA-256 | `c39bbe1dce5702e9a7de022e85cc99a16a52e68ba7ac087f33570700a0d7c655` |
| Commit export archive SHA-256 | `016683ae0ed8d584dac2f968dfddc7ce2645f12c3e50dcfc731a03ed3633eb77` |
| Installed local manifest SHA-256 | `68c25296ffd9ce69a56b31416ca1ded78919900efb0333c05227fd17160658ac` |

The export is `git archive --format=tar <full commit> .orbit-plugin
scripts/bundle-plugin-binary.sh`. The installed local copy removes only
`metadata.origin` and bundles the measured executable with
`--backend-sha256`; these deliberate transformations explain its new manifest
digest. The pristine export retains `origin: orbit` and
`--allow-unbound-backend` and is checked using `--first-party` with the measured
binary first on `PATH`. These are different certification contexts.

The build, exports, private HOME directories, fixtures and evidence are under
`<worktree>/.orbit/tmp/readiness/`. `CARGO_TARGET_DIR` is its `target/` and
`TMPDIR` is its `temp/`. Disk readings before the new target and exports were
26–27%, below the 80% stop threshold; the initial checkout reading was 23%.
No extra worktree or global tool installation was created. The older host
nextest 0.9.136 was not used for testing. Binary digests identify these measured
bytes, not all future builds of this source; rebuilds require new provenance.

## Readiness matrix

| Surface | Linux outcome | Evidence and limits |
| --- | --- | --- |
| Standalone workspace / real executable | 685/685 regular tests pass | Three installed tests are visibly skipped in the normal suite and run separately below. Four doctests fail, reproduced on the unchanged source. |
| `version`, `status`, `recommend` | CLI and MCP pass | Query recommendations are nonempty; live task text and lexical search callbacks also pass. |
| `search`, `show`, `refs`, `callees`, `impact`, `trace`, `deps`, `overview` | CLI and MCP pass | Nonempty search/reference/callee/impact records, actual command-handler root, fresh indexes and resolved selectors. |
| `maintain`: history/code synchronization | CLI and MCP pass with authorized outer caller | History and code indexes are explicitly built; ordinary CLI/MCP callers are denied mutations. |
| `changes` | CLI and MCP pass with writable plugin state | Nonempty changed-symbol records; source, Git and repository graph state remain unchanged. It writes plugin caches under the documented exception. |
| Live task-ID recommendation | CLI and MCP pass | Real disposable task; `adapter.task_text = orbit.task.show_public_observation`, nonempty recommendations. |
| Hybrid task search | CLI and MCP pass | Real task hit; `orbit.search_lexical_rank`, zero dropped hits, no fallback warnings. |
| Nonempty `orbit_sync.task_ids` | CLI and MCP pass for an undelivered task | One task examined, zero failed, zero discovered runs, zero verified deliveries. This is task-read coverage only. |
| Delivered-run import | **Incomplete** | Nonempty synthetic `run_ids` demonstrate the operator denial, not an eligible delivered run. No installed host-verified import was performed. |
| Unknown fields / mutation / ownership | Expected denials on CLI and MCP | Unknown search field; ordinary maintenance caller; foreign workspace task despite a shared remote; foreign repository blocked by sandbox routing. |
| Eleven read tools with read-only state | CLI and MCP pass | `version`, `status`, `recommend` and eight query tools succeed with files mode 0400/directories 0500; modes, bytes, paths and mtimes remain unchanged. |
| First-party source conformance | Validate passes; 42/42 cases pass | Clean commit export, `--first-party`; no first-party `git+` installation was performed. |
| Bound local installed source conformance | 42/42 cases pass | Matching source export certifies the installed local manifest for Orbit 0.25.1 in the private HOME. |
| Local add / enable / upgrade | Pass in private roots | Upgrade of the same version from the prepared source succeeds and retains grants for unchanged permission requests. |
| macOS / dashboard / scheduled routines | Not run here | Existing Mac evidence is background only; no new Mac certification, dashboard check or recurring automation. |

There are **13 distinct tools**; `maintain` has multiple operations. The real
installed test compares the manifest tool set with requests and MCP `tools/list`.
Installed CLI coverage uses `orbit tool run graph.<verb> --input ... --full`;
MCP uses `graph_<verb>` through `orbit mcp serve`. This report does not separately
certify every derived human command rendering.

The [existing Mac report](../evals/quality-20260929/README.md) describes other
commits and Orbit 0.25.0. It was read without modification and is not a Linux
measurement or proof of this candidate's missing delivered-run coverage.

## Sandbox and authority context

The canonical installed fixture and the additional transcript fixture use
private HOME/Orbit roots and cleared child environments. Grants are exactly
`fs,orbit_tools`, with `network: none`, `env_pass` absent and
`unsandboxed: false`; the manifest uses `sandbox: default`. The rendered profile
and successful real invocations are captured, not inferred from configuration
alone. A foreign repository invocation is actually denied filesystem access.

Only private fixture setup and outer mutating test calls use the explicitly
audited operator path (`ORBIT_OPERATOR=1` or MCP `--operator`). Read tools also
pass with an ordinary caller. Plugin callbacks never use `--operator` and do not
inherit the operator variable. The callback allowlist names
`orbit.workspace.list`, `orbit.task.show`, `orbit.search` and
`orbit.workflow.run.show`; listing a tool there does not grant its capability.

For task ownership, Orbit supplies the registered checkout path. Graph checks
that it equals the requested repository, reads the task with the same workspace
filter, and checks the host's public workspace owner ID/name and discovery row.
The shared-remote negative fixture fails rather than borrowing another
workspace's task. Remote matching is an accident guard; Orbit authorization is
the security boundary. No user's live `~/.orbit/state/plugins` or live grants
were inspected, copied or changed.

## Repository gates and executable reproduction

Run at the pinned source using the environment above. Every row has a captured
producer exit status in the task's `readiness/logs/` artifacts.

| Required command | Candidate outcome | Log |
| --- | --- | --- |
| `sh docs/standards/check.sh` | Passed | `gate-01.json` |
| `scripts/check-dependency-direction.sh` | Passed, four crates | `gate-02.json` |
| `scripts/check-terminal-guard.sh` | Passed | `gate-03.json` |
| `scripts/check-orphan-modules.sh` | Passed | `gate-04.json` |
| `scripts/test-repo-gates.sh` | Passed, 52 seeded cases | `gate-05.json` |
| `cargo deny --locked check` | Denied advisory-cache lock; reproduced on unchanged baseline | `gate-06.json`, `baseline-deny.json` |
| `cargo fmt --all --check` | Passed | `gate-07.json` |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | Passed | `gate-08.json` |
| `cargo nextest run --workspace --locked --no-tests=fail` | Passed, 685 tests; three skipped | `gate-09.json` |
| `cargo test --workspace --doc --locked` | Failed, 10 passed / four failed; same baseline result | `gate-10.json`, `baseline-doctest.json` |
| `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked` | Passed | `gate-11.json` |
| `cargo build --workspace --locked` | Passed | `gate-12.json` |
| `git diff --check` | Passed | `gate-13.json` |

The original deny command cannot acquire `/home/daniel/.cargo/advisory-dbs/db.lock`:
“attempted to take an exclusive lock on a read-only path”. A copy of `deny.toml`
with **only** `[advisories].db-path` set to the absolute run-scratch advisory
location passes all four categories with a freshly fetched database:

```sh
cargo deny --locked check --config .orbit/tmp/readiness/deny.toml
```

No advisory, license, source, or ban rule was relaxed. This alternate command
passed (`isolated-deny.json`); the originally denied command is not marked passed.

All ignored real-Orbit tests were explicitly selected, with no missing-binary
skip, using:

```sh
ORBIT_GRAPH_TEST_ORBIT_BIN=/home/daniel/.orbit/bin/orbit \
  cargo test -p orbit-graph-cli --test plugin_integration --test plugin_v2 --locked -- --ignored
```

`gate-14.json` records two legacy tests plus
`installed_v2_plugin_serves_every_tool_over_cli_and_mcp`: **3/3 passed**.
The canonical test asserts all thirteen tools, nonempty task recommendations,
public lexical task search, undelivered task sync, unknown fields and mutation
denials through real CLI and MCP.

Clean export checks, in an initialized private workspace/HOME:

```sh
orbit plugin validate --first-party .orbit/tmp/readiness/export-ac6d5b9
PATH="$PWD/.orbit/tmp/readiness/target/debug:$PATH" \
  orbit plugin test --first-party .orbit/tmp/readiness/export-ac6d5b9
```

`export-validate.json` and `export-test.json` record success and 42/42 cases.
Conformance mainly checks contracts and negative requests against an empty
workspace; it cannot substitute for the installed callback test. The matching
bound local source also passes all 42 cases and records certification in the
private host (`installed-source-conformance.json`).

## Remaining limitations and repair ownership

1. **Doctest fixture isolation — ORB-13737.** At the clean source, set
   `TMPDIR="$PWD/.orbit/tmp/readiness/temp"` and run
   `cargo test --workspace --doc --locked`. Both initial and baseline repetitions
   exit 101. The examples for `resolve_worktree_db_path`,
   `Graph::impact_with_direction`, `Graph::runtime_invocations` and `CalleeOpts`
   discover the enclosing ignored Git checkout: the first observes its branch
   instead of `HEAD`, and the others miss expected symbols. The regular
   hermetic suite passes; it does not cover these examples. This report's docs
   changes do not cause the failure. Fixing fixtures is outside this deliverable.

2. **Refusal code preservation — ORB-13738.** In the private installed fixture:

   ```sh
   ORBIT_OPERATOR=1 orbit tool run graph.maintain --input \
     '{"operation":"orbit_sync","workspace":"readiness-primary","run_ids":["jrun-readiness-nonexistent"]}' --full
   ```

   The outer batch succeeds while `coverage.failed=1`, `excluded=0`, and its
   outcome is `failed`. Orbit refuses `orbit.workflow.run.show` with
   `capability_denied`; Graph reports `graph_error` because a WARN line precedes
   the refusal JSON on stderr. Real MCP shows the same result. This loses typed
   refusal metadata; it does not grant run access. Logs:
   `cli-run-callback-denied.json`, `mcp-surface.json`.

3. **Delivered-run coverage remains operator-owned.** No eligible real delivered
   run was imported by the installed plugin. The synthetic ID above reaches the
   capability check before a run could be examined. Undelivered task reads,
   fake-adapter successful imports, Git-only sync and caller-attested `import`
   cannot certify this surface. A sanctioned non-operator run read, or separately
   authorized operator evidence, is needed; boundaries were not bypassed.

4. **`changes` cache exception.** The existing
   [decision D6](design/changes-command/4_decisions.md#d6-where-snapshots-are-cached)
   records the deviation from `STD-01@2 §R31`. Cold calls create plugin snapshot
   and scratch state; calls with plugin state made read-only return
   `graph_error`/permission denied creating `changes-scratch`. With writable
   plugin state, source files, Git state and repository graph state are unchanged.
   Thus the eleven-tool no-state-write result must not be extended to `changes`.

Searches found terminal antecedents ORB-13169 and ORB-13167, but no open repair
covering the two current defects; the focused proposed tasks above carry exact
reproductions. No releases, tags, publishing, live host upgrade, grant changes or
recurring automation were performed. Production release work remains reserved.

## Candidate install and upgrade handoff

These commands prepare the **measured source candidate**, not a new release.
Start in a checkout whose HEAD equals the full candidate commit above. Build and
check the candidate before an operator considers host installation. Keep exports
in a new, empty scratch directory and check disk usage before creating a build
or export directory; at 80% or higher, stop. Record the new binary SHA-256 and
repeat the gates if rebuilding; do not assume it equals this run's measured hash.

```sh
candidate=ac6d5b91f973874325bac789e9c433a71c805766
# Verify git rev-parse HEAD equals "$candidate" and git status --short is empty.
df --output=pcent .
mkdir -p .orbit/tmp
export_root=$(mktemp -d "$PWD/.orbit/tmp/graph-candidate.XXXXXX")
cargo build --workspace --locked
# For the measured run the binary was in its explicit CARGO_TARGET_DIR.
binary="$PWD/target/debug/orbit-graph"
sha256sum "$binary"
git archive "$candidate" .orbit-plugin scripts/bundle-plugin-binary.sh | tar -x -C "$export_root"
python3 - "$export_root/.orbit-plugin/plugin.yaml" <<'PY'
from pathlib import Path
import sys
path = Path(sys.argv[1])
path.write_text(path.read_text().replace('  origin: orbit\n', ''))
PY
sh "$export_root/scripts/bundle-plugin-binary.sh" --binary "$binary" "$export_root"
check_home="$export_root/check-home"
check_repo="$export_root/check-repo"
check_temp="$export_root/check-temp"
mkdir -p "$check_home" "$check_repo" "$check_temp"
env -i HOME="$check_home" PATH="$PATH" GIT_CONFIG_NOSYSTEM=1 \
  GIT_CONFIG_GLOBAL=/dev/null git -C "$check_repo" init -b main
(
  cd "$check_repo"
  env -i HOME="$check_home" XDG_CONFIG_HOME="$check_home" TMPDIR="$check_temp" PATH="$PATH" \
    orbit workspace init --name graph-candidate-check --ship-mode local
  env -i HOME="$check_home" XDG_CONFIG_HOME="$check_home" TMPDIR="$check_temp" PATH="$PATH" \
    orbit plugin validate "$export_root"
  env -i HOME="$check_home" XDG_CONFIG_HOME="$check_home" TMPDIR="$check_temp" PATH="$PATH" \
    orbit plugin test "$export_root"
)
```

The preparation checks use a disposable HOME and initialized Orbit workspace.
These commands do not install a plugin. When using `CARGO_TARGET_DIR`, set `binary` to that directory's
`debug/orbit-graph` instead. The prepared manifest binds this binary's digest.

Only after the operator authorizes a live installation, choose one:

```sh
orbit plugin add "$export_root"
# Existing graph installation instead:
# orbit plugin upgrade graph "$export_root"
orbit plugin show graph --format json
orbit plugin test "$export_root"
```

Then, separately, after reviewing the resolved roots and manifest digest:

```sh
orbit plugin enable graph --grant fs,orbit_tools
```

The installed local namespace is `graph.*` / MCP `graph_*`. A verified official
`git+https://github.com/constellation-works/orbit-graph#<immutable-ref>` source
retains `origin: orbit` and registers `orbit.graph.*` / `orbit_graph_*`.
The first-party release tree keeps the named unbound override when a matching
binary is bundled with `--unbound`; its manifest does not bind the binary digest.
See [the release procedure](plugin.md#install) for that distinct path. Installing
or upgrading replaces the tree, so re-bundle an unbound first-party install after
each replacement. Local bound exports already contain their binary.

Orbit 0.25.1 **refuses** `orbit plugin test <installed-version-root>` because
that directory has a top-level manifest instead of `.orbit-plugin/plugin.yaml`.
Test the matching prepared source; for a first-party installed tree, copy it
into an isolated `.orbit-plugin/` source wrapper and test with `--first-party`.
The private test confirmed the refusal and the successful matching-source
alternative. `plugin show` requires `--format json` here; `--json` is refused.
An upgrade with unchanged permission requests can preserve existing consent;
widened requests disable it without new grants. Neither source conformance nor
host enablement supplies the missing operator callback capability.

## Durable evidence and handoff boundary

ORB-13712 task artifacts under `readiness/` retain provenance, each gate's JSON
log, help output, the isolated scripts and full CLI/MCP request/response logs.
Each command log names the run, tested HEAD, command and producer exit status;
MCP transcripts record explicit fixture shutdown after the completed requests.
`read-only-state.json`, `changes-cache-state.json` and
`changes-repository-state.json` retain the state comparisons. The artifact index
lists exact paths and digests. Evidence under `.orbit/tmp/` is intentionally
ignored by Git; this report is the durable repository deliverable.

For executable replay, retrieve `evidence.py` and `reproduce-surfaces.py` into
a fresh `<checkout>/.orbit/tmp/<new-run>/` directory. With the checkout at the
pinned candidate and a binary built from it, run:

```sh
python3 .orbit/tmp/<new-run>/reproduce-surfaces.py \
  --binary /absolute/path/to/candidate/orbit-graph --orbit /absolute/path/to/orbit
```

The script checks disk usage, refuses existing fixture roots, exports the exact
commit and creates private HOME/workspaces. It captures all thirteen CLI/MCP
tools, nonempty callbacks, negative requests and state comparisons. A fresh
replay completed successfully in this run (`reproduce-surfaces.json`). Its
denied run callback remains an expected incomplete-import result.

This documentation diff changes only this report, `docs/plugin.md`, and the
bundled skill/setup guidance. It leaves candidate runtime code, manifests,
versions and `docs/evaluation/` unchanged. The pipeline owns committing the
report; this executor leaves the diff uncommitted. Documentation guidance is
checked after editing, while candidate runtime measurements remain pinned to the
clean source above.

The post-edit CONTRIBUTING sequence repeats the candidate outcomes: 685 regular
tests and all three installed tests pass; the same four doctests fail, and the
original deny command encounters the same read-only advisory-cache lock. The
isolated deny policy check and updated plugin validate/test also pass. Final
logs use `final-` prefixes; the failures remain explicitly recorded rather than
counted as passes.
