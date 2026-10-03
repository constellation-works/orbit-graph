# agent-eval: paired navigation episodes through a confined tool broker

Operator tooling that runs **one** baseline or graph episode of the ORB-13710
paired navigation study with the Codex CLI. Each episode runs from a
solution-free pinned source tree, and the model reaches the repository only
through an allowlisted MCP tool broker. The tooling captures bounded, hashed
telemetry and converts sealed episodes into the ORB-13710 public bundle that
`eval.py score` accepts.

The tooling is standard-library Python 3 and runs on Linux only. It is not part
of the Cargo workspace, CI or the Makefile; run its tests explicitly (see
[Tests](#tests)).

| File | Role |
|---|---|
| `eval_runner.py` | Operator CLI: `revision`, `preflight`, `run`, `adapt`. |
| `eval_broker.py` | Stdio MCP server copied into each episode: `read`, `rg`, `git`, plus `graph_sync`, `search`, `show`, `refs`, `callees`, `impact`, `changes` on the graph arm only. Also the in-sandbox containment probe (`--launch`). |
| `tests/` | Deterministic tests. A fake provider (`fake_codex.py`) speaks real MCP to the real broker. Fake bubblewrap and orbit-graph are included. |

Mocked runs, including the tests and any `--containment none` episode, are
never effectiveness evidence.

## Requirements

- Linux with **unprivileged user namespaces** for bubblewrap (`bwrap`). Hosts
  that deny them (for example `kernel.apparmor_restrict_unprivileged_userns=1`
  without a bwrap AppArmor profile, or a managed sandbox) get a
  `containment_unavailable` refusal and no episode.
- Linux `pidfd_open` support must be available to the broker supervisor. It
  watches the provider process, so a transient launching thread may exit safely.
- `/usr/bin/python3` (verified with 3.12) and `/usr/bin/git`. Both must live under
  `/usr`, because only `/usr` is mounted inside the sandbox.
- `rg` (ripgrep). Any static binary works; it is bound into the sandbox.
- Codex CLI, verified with `codex-cli 0.160.0`. It must already be logged in
  through its normal supported path (`codex login`), and the runner never
  changes that login.
- For the graph arm, an `orbit-graph` binary with the global `--json` flag:
  `cargo build --workspace --locked` here gives `target/debug/orbit-graph`.
  Older installed builds (0.9.x) lack `--json`, so every graph call fails.

## Operator procedure

Every episode directory is new. The runner never reuses or overwrites one. In
the variables below, `ORB13710` is a checkout that contains
`evals/agent-navigation/eval.py`. The run directory must not be
inside that checkout or inside the export.

```sh
TOOL=scripts/agent-eval/eval_runner.py           # from this checkout
ORB13710=/path/to/orb-13710-checkout
EVAL="$ORB13710/evals/agent-navigation"
EXPORT=/path/to/new/export                       # must not exist yet
RUNS=/path/to/new/runs
MODEL=gpt-5-codex                                # one fixed model for the whole cohort
COMMON="--codex $(command -v codex) --model $MODEL \
  --setting model_reasoning_effort=medium \
  --rg $(command -v rg) --orbit-graph $PWD/target/debug/orbit-graph \
  --containment bwrap --truth-path $EVAL --forbid $EXPORT"
```

1. **Export the solution-free requests** (ORB-13710; offline, files only):

   ```sh
   python3 "$EVAL/eval.py" export --output "$EXPORT" --crew "codex-cli/$MODEL"
   mkdir -m 700 "$RUNS"
   ```

   Each `$EXPORT/NN-<case>-<arm>/` holds `request.json`, `repository/` (HEAD)
   and `before/` (base).

2. **Check that containment works on this host.** This step needs no provider
   tokens. It reads back Codex's effective features, its MCP inventory and
   the broker's tool approvals inside the sandbox:

   ```sh
   AGENT_EVAL_REQUIRE_BWRAP=1 \
     python3 -B -m unittest discover -s scripts/agent-eval/tests -k LiveContainment -v
   EP=$(ls -d "$EXPORT"/00-*)
   python3 $TOOL preflight --request "$EP/request.json" --head "$EP/repository" \
     --out "$RUNS/preflight" $COMMON
   ```

   On success it prints `"status": "ready"`. Exit code 3 is a capability
   refusal; see [Refusals](#refusals). Do not continue past a refusal.

   Then rehearse **one** episode outside the cohort (a new out directory) and
   check its `episode.json`. The cohort is ready only when the rehearsal
   shows:
   - `calls` is non-empty;
   - `transcript.mcp_calls` lists the same tools;
   - `isolation.unbrokered` and `transcript.approval_denied` are empty;
   - the error is not a harness failure such as `provider_code_mode_required`
     (see [Code mode](#code-mode)) or `provider_tool_approval_required` (see
     [Tool approvals](#tool-approvals)).

3. **Run every episode in plan order.** This is the paid, live step:

   ```sh
   for EP in "$EXPORT"/[0-9]*-*/; do
     NAME=$(basename "$EP")
     df --output=pcent "$RUNS" | tail -1          # stop at >= 80%
     python3 $TOOL run --request "$EP/request.json" --head "$EP/repository" \
       --base "$EP/before" --out "$RUNS/$NAME" $COMMON || break
   done
   ```

   Exit code 0 means a sealed `episode.json` was written, whatever the episode
   status. Failed, timed-out and invalid episodes are part of the study and
   must be kept. Exit code 1 is invalid input; exit code 3 is a refusal. Both
   stop the loop with no episode written. The baseline arm never stages
   `orbit-graph`, even though `--orbit-graph` is passed to it.

4. **Convert and score:**

   ```sh
   python3 $TOOL adapt $(for d in "$RUNS"/[0-9]*-*/; do printf -- '--episode %s ' "$d"; done) \
     --output "$RUNS/bundle.json"
   python3 "$EVAL/eval.py" score --input "$RUNS/bundle.json" > "$RUNS/score.json"
   ```

   `adapt` re-verifies every artifact and file hash. It refuses an
   uncontained, non-cold, incomplete (orders not `0..n-1`) or model-drifting
   cohort, and it never overwrites its output.

`revision --tree DIR` prints the ORB-13710 content revision of a tree, so you
can check a source tree before running.

### Credentials

The provider gets a private, empty `CODEX_HOME` for each episode. Your existing
`auth.json` is **bind-mounted read-only** into it; it is never copied, printed
or written. The default source is `$CODEX_HOME/auth.json`, or
`~/.codex/auth.json` if `CODEX_HOME` is unset. Use `--auth-file PATH` to point
elsewhere. The file must be a regular file that you own and that is not
group- or world-writable (STD-05 R9).

Because the mount is read-only, a token refresh during an episode cannot be
saved. If the login has expired, run `codex login` normally, then rerun.

For API-key auth, pass `--auth-file none --provider-env OPENAI_API_KEY`.
Variables named like keys, tokens or secrets are redacted from every episode
file. No other host variable reaches the provider, and `ORBIT_*` names are
refused.

### Code mode

Codex 0.160.0 runs some models' tool calls through its **code-mode host**,
`bin/codex-code-mode-host`, a V8 runtime next to `codex`. By default the
runner disables `code_mode_host` along with the other ambient features. A
model that needs code mode then cannot call any tool. A live, contained
operator rehearsal on 2026-10-03 (model `gpt-6.1-sol`) showed exactly this:
Codex reported `code-mode host is disabled`, no broker call was made, and the
model abstained. The runner classifies such an episode as `invalid` /
`provider_code_mode_required`, and `adapt` refuses it, because it measures the
harness rather than the arm.

`--allow-code-mode` (on both `preflight` and `run`) is the explicit opt-in:
- It leaves only `code_mode_host` enabled and requires the host binary next
  to `codex`.
- It records `code_mode_host: enabled` in the model settings, so a cohort
  cannot mix the two configurations.
- The host runs inside the same sandbox, so it sees only the sandbox mounts.

It is **unverified**: no live episode has yet shown how code-mode tool calls
appear in the JSONL. The audit still fails closed:
- a new item type → `unbrokered_tool_use`
- broker calls missing from the provider's reported calls →
  `telemetry_mismatch`
- no broker call → `no_tool_calls`

Before using it for a cohort, rehearse once with it and apply the checks in
step 2.

### Tool approvals

Codex asks for approval before an MCP tool call it cannot classify as safe.
The runner keeps `approval_policy="never"`, so such a call is refused before
it reaches the broker. A developer rehearsal on 2026-10-03 with Codex
0.160.0 and the broker's tools unapproved showed this: every `rg`, `read` and
`git` call failed with `MCP tool call requires approval, but approval policy
is never`, and the broker logged no call.

The private invocation therefore approves the arm's broker tools, and only
those, with Codex 0.160.0's per-server settings:

```text
mcp_servers.eval_broker.required=true
mcp_servers.eval_broker.enabled_tools=[<arm tools>]
mcp_servers.eval_broker.default_tools_approval_mode="prompt"
mcp_servers.eval_broker.tools.<tool>.approval_mode="approve"   # one per arm tool
```

Any other tool keeps the `prompt` default and is still refused under `never`.
User configuration is ignored and never changed. The explicit `prompt`
default matters. Against a local mock model, Codex 0.160.0's own default
(`auto`) passed tools that the broker annotates `readOnlyHint` but refused
`graph_sync`. Under `prompt`, it refused an unlisted tool whatever its
annotations. Approval therefore depends only on the runner's configuration,
never on hints the broker sends. Preflight records what Codex
reads back in `preflight.json` and `isolation.inventory.tool_approvals`:
- `codex mcp get eval_broker --json` must report exactly the arm's tools in
  `enabled_tools` and no `disabled_tools`;
- `codex mcp get eval_broker` must report `default_tools_approval_mode:
  prompt`;
- Codex has no readback for per-tool approvals. Instead, preflight sets an
  invalid value at `tools.<first tool>.approval_mode` and requires Codex to
  reject it by key. That proves the installed CLI parses the key rather than
  silently ignoring it, as it does unknown keys.

If Codex still refuses a broker call at the approval gate, the episode is
`invalid` / `provider_tool_approval_required`. The refused calls are listed in
`transcript.approval_denied`, and `adapt` refuses the episode because it
measures the harness, not the arm.

## Containment and evidence model

- **OS confinement (`--containment bwrap`).** Codex, the broker and the broker's
  children run in new user, PID, IPC and UTS namespaces. The sandbox contains:
  - `/usr`, read-only
  - a few `/etc` entries (DNS, TLS, passwd)
  - the snapshot repository at `/eval/repo`, read-only; the graph arm's
    `.orbit-graph/` is writable
  - the episode runtime, read-only
  - an episode state directory, the only writable mount
  - the Codex release directory, read-only
  - `rg` and `orbit-graph` at `/eval/bin`

  Your home directory, the export, the truth path, this checkout and the run
  directory are not mounted.
- **Probe before exec.** In the same sandbox instance, the broker's `--launch`
  wrapper checks every forbidden host path: the `--truth-path`s, the
  `--forbid`s, this checkout, `--head`/`--base`, the run directory, `$HOME` and
  `$CODEX_HOME`. It also checks that the sandbox paths are visible. It execs
  Codex only if every forbidden path is absent; otherwise the run is refused
  (`containment_ineffective`) and Codex never starts. The probe result is
  sealed into the artifact.
- **Effective tool surface.** The flags used for the run are read back first:
  - `codex features list` must report every ambient capability feature
    disabled: shell, apps, plugins, multi-agent, browser/computer use, image
    tools, hooks, memories, worktrees and others (see `DISABLED_FEATURES`).
  - `codex mcp list --json` must report exactly one server, `eval_broker`,
    with the runner's own command.
  - The broker's tool inventory and approvals must read back as described in
    [Tool approvals](#tool-approvals).
  - User config, rules and history are ignored (`--ignore-user-config
    --ignore-rules --ephemeral`), web search is disabled, and the approval
    policy is `never`. Only the arm's broker tools are approved.
- **Transcript audit.** Any JSONL item that is not an agent message,
  reasoning, todo list, error or `eval_broker` MCP call of one of the arm's
  tools makes the episode `invalid` (`unbrokered_tool_use`). Examples:
  `command_execution`, `file_change`, `web_search`, another MCP server, or
  Codex's native MCP resource tools. Codex 0.160.0 reports
  `list_mcp_resources` under server `codex` and `read_mcp_resource` under the
  server it targets, so a broker server name with a non-broker tool name also
  counts. Broker-logged calls must also appear, in order, among the provider's
  reported calls (`telemetry_mismatch`).
- **Broker.** The broker serves the arm's tools only; graph tools on the
  baseline arm are refused and not counted. It refuses:
  - absolute paths, `..`, `.git`/`.orbit-graph`, `~`, backslashes and
    control characters
  - symlinks, opened component by component with `O_NOFOLLOW`
  - option-shaped values, unknown fields and malformed MCP

  `tools/list` carries MCP annotations, which are client hints only. Every tool
  has `openWorldHint: false`. `graph_sync` builds and, with `full`, replaces
  the episode's private `.orbit-graph` index: `readOnlyHint: false`,
  `destructiveHint: true`, `idempotentHint: true`. `changes` also has
  `readOnlyHint: false`: even with `--no-cache`, it writes disposable snapshot
  trees and indexes in the episode's private `TMPDIR`. It has
  `destructiveHint: false` because it does not replace existing source or
  index state, and `idempotentHint: true` because repeating the comparison
  leaves the same state after its temporary snapshots are removed. All other
  tools have `readOnlyHint: true`. Source files and Git remain immutable;
  these hints never change the explicit per-tool approvals above.
  Children run with fixed argv and environments.
  Each call has a deadline and bounded output. Timeouts and unexpected supervision
  exceptions clean up the child's process group and produce failed call records;
  exception diagnostics contain the class, never exception text or a traceback.
  A supervisor watches provider process death and the episode deadline, and
  captures worker exit status and at most 16 KiB of worker stderr. The worker
  inherits only the broker's allowlisted environment. Child failure stderr keeps
  its existing 2,048-character capture bound, with truncation marked.
  The raw schema stays at version 1: `broker-calls.jsonl` gains `call_start`,
  `child`, `supervisor_start` and `broker_exit` diagnostic entries, and
  `episode.json` gains optional `broker.exits`. Existing sealed artifacts remain
  readable. Call records and exact provider/broker reconciliation are unchanged;
  an interrupted call is never synthesized as completed. These diagnostics use
  the same redacting, private artifact writer as the existing captures.
  Budgets for calls, input bytes, call output and episode output stop the
  episode through a sentinel the runner watches. Tool output is returned as
  data inside a JSON envelope; it is never instructions to the broker.
- **Fresh state.** Each episode builds a disposable two-commit Git snapshot:
  base, then head, with a fixed identity and epoch, so the commit ids are
  deterministic. The snapshot is checked against the request's
  `source_revision` and `base_revision`. Graph state starts empty, and
  `graph_sync` runs inside the timed episode (`timing.graph_sync_ms`;
  `cache_policy` is `cold-per-episode-including-graph-setup`).
- **`--containment none`** is the named escape hatch for tests and debugging.
  It still applies the broker, the feature and MCP readback, and the audit,
  but the artifact records `contained: false` / `truth_inaccessible: false`
  and `adapt` refuses it.

## Episode artifacts

`$RUNS/<name>/` (directories are 0700, files 0600):

| File | Content |
|---|---|
| `episode.json` | Sealed raw artifact (`kind: agent-eval-raw-episode`, `artifact_sha256`). It records the request, model (`codex-cli`, name, CLI version, settings), tool versions with binary sha256s, snapshot ids, isolation (probe, sandbox argv and digest, verified inventory, unbrokered items, graph state), timing (`wall_ms`, `setup_ms`, `provider_ms`, `graph_sync_ms`, `preflight_ms`), status and error, answer, final output, calls, `output_bytes`, usage, transcript histogram, cleanup (signals, survivors, swept brokers), and a byte count and sha256 for every file below. |
| `provider.jsonl` | Codex `--json` events; at most 8 MiB, redacted. |
| `provider-stderr.txt` | At most 256 KiB, redacted. |
| `broker-calls.jsonl` | Broker log: start identity, initialize, tool list, every call (input, output, status, elapsed), refusals and budget stops. |
| `final-message.txt` | `--output-last-message`, or else the last JSONL agent message. |
| `probe.json`, `preflight.json` | In-sandbox probe and the feature/MCP readback. |
| `snapshot/`, `runtime/`, `state/` | Working directories; not evidence. |

`usage` is `{input_tokens, output_tokens, cost_usd: null, source}`, summed from
`turn.completed.usage`. It is `null` when the provider reported none. Cached
and reasoning token counts are kept in `usage_raw`. Cost is never estimated.

Episode status follows ORB-13710; the first matching rule wins:

1. `timeout` (`wall_time_exceeded`)
2. a budget stop:
   - `tool_call_budget_exceeded`
   - `call_input_budget_exceeded`
   - `output_budget_exceeded`
   - `call_output_truncated`
   - `refusal_budget_exceeded`
3. `provider_output_truncated`
4. `invalid` `unbrokered_tool_use`
5. `invalid` `provider_code_mode_required` (a harness failure; `adapt`
   refuses it)
6. `invalid` `provider_tool_approval_required` (a harness failure; `adapt`
   refuses it)
7. `broker_not_initialized`
8. `tool_inventory_mismatch`
9. `provider_output_malformed`
10. `telemetry_mismatch`
11. `broker_failed` or `broker_exit_missing` (failed worker/cleanup or incomplete supervision)
12. `tool_execution_failed` (a subprocess failure, timeout or execution exception;
    ordinary broker argument/path refusals remain recoverable agent errors).
    A graph query's typed CLI `index_missing` error is also recoverable through
    `graph_sync`: exit 1, complete `{code, error}` JSON on stderr, empty stdout,
    no timeout/exception/cleanup signals, and no survivors are required. Its
    call retains `failed` status and all measured costs; the episode remains
    eligible for final-answer grading. Other nonzero CLI errors (including
    generic `graph_error`, timeout, incompatible index and unknown codes) stay
    fail-closed. Missing, malformed or truncated evidence cannot qualify.
13. `provider_incomplete` (non-zero exit, no completed turn, or a failed turn)
14. `final_answer_missing`
15. `invalid` `final_output_oversized`
16. `invalid` `answer_malformed`
17. `invalid` `no_tool_calls`
18. otherwise `ok`

### Refusals

Exit code 3 prints `{"status": "refused", "provider_started": false,
"refusal": {code, message, evidence}}`. It also writes `refusal.json` to the
episode directory when that directory exists. Codes:

- `containment_unavailable`: bubblewrap cannot create namespaces.
- `containment_unsupported`: python or git is outside `/usr`.
- `containment_ineffective` / `containment_probe_missing`: the probe saw a
  forbidden path or did not run.
- `provider_feature_unknown`: a feature to disable is no longer reported (the
  Codex version changed).
- `provider_feature_not_disableable`: a feature still reads back as enabled.
- `mcp_inventory_mismatch`.
- `mcp_tool_inventory_mismatch`: `enabled_tools` does not read back as
  exactly the arm's tools.
- `provider_tool_approval_unverified`: `default_tools_approval_mode` does not
  read back as `prompt`.
- `provider_tool_approval_unsupported`: Codex accepted an invalid per-tool
  approval mode, so it is not applying the per-tool key.
- `provider_preflight_failed`.
- `provider_auth_unavailable` / `provider_auth_unsafe`.
- `expected_tool_unavailable`: rg, git, python, codex, orbit-graph or (with
  `--allow-code-mode`) the code-mode host is missing or not executable, or a
  version probe failed.

## Limits

- **bwrap is required for evidence.** Where unprivileged namespaces are denied,
  no episode runs. The contained path is covered by `LiveContainmentTests`.
  Its capability probe runs `/usr/bin/true` in the runner's own namespaces
  and system mounts (`system_sandbox_argv`). It skips, with bubblewrap's
  reason, only when bubblewrap itself refuses. A sandbox that starts but
  cannot run the command fails the test. Run the test on the episode host
  first with `AGENT_EVAL_REQUIRE_BWRAP=1`, which turns the skip into a
  failure.
- **Per-tool approvals have no readback.** Codex 0.160.0 reports
  `enabled_tools` and `default_tools_approval_mode`, not
  `tools.<tool>.approval_mode`. The evidence is that the key is parsed (see
  [Tool approvals](#tool-approvals)), plus the transcript: any approval-gate
  refusal invalidates the episode.
- **Some native tools leave no JSONL item.** Codex 0.160.0 also offers the
  model `request_user_input` and the `goals` tools (`get_goal`, `create_goal`,
  `update_goal`). Against a local mock provider, it refused each of them in
  `exec --ephemeral` without effect (`request_user_input is unavailable in
  Default mode`, `Goal tools require a persistent thread`). It reported no
  item, so the transcript audit cannot see such attempts.
- **The network is not isolated.** The provider needs it. The broker's
  children (`rg`, `git`, `orbit-graph`) do not use it, and the model has no
  network tool: web search and shell are disabled and audited.
- **Codex has no command that lists the model's tool inventory.** The evidence
  is the feature and MCP readback under identical flags plus the transcript
  audit, not a tool list. The shell-implementation selectors (`unified_exec`,
  `unified_exec_tty`, `shell_snapshot`, `write_stdin_approval`) cannot be
  disabled in 0.160.0. They are recorded as `shell_gated` and are inert while
  `shell_tool` reads back false.
- **Content revisions hash file bytes decoded as UTF-8, with no newline
  translation.** ORB-13710 hashes `read_text()`. The digests agree for LF text
  (all current fixtures) but differ for files with CR line endings. Trees must
  be UTF-8 text with UTF-8 names. Symlinks, special files and nested `.git` or
  `.orbit-graph` entries are refused. Snapshot files are mode 100644, and
  empty directories are not represented.
- **Code mode is not verified.** See [Code mode](#code-mode). Models that
  require it produce no valid episodes without `--allow-code-mode`, and that
  path has no live evidence yet.
- **Model identity** is the configured model name plus the Codex CLI version
  and sha256. The provider does not report a finer model revision.
- **The read-only `auth.json`** means expired tokens cannot refresh
  mid-episode.
- **One episode per invocation.** Cohort pacing, retries and repetitions are
  the operator's decisions. Rerunning a case needs a new episode directory,
  and the cohort must still contain exactly one episode per planned order.

## Tests

```sh
python3 -B -m unittest discover -s scripts/agent-eval/tests -v
# Optional: the real graph binary behind the broker (fake provider)
AGENT_EVAL_ORBIT_GRAPH=$PWD/target/debug/orbit-graph \
  python3 -B -m unittest discover -s scripts/agent-eval/tests -k RealGraph -v
```

Scratch directories go under `.orbit/tmp/agent-eval-tests/`; set
`AGENT_EVAL_TEST_TMP` to put them elsewhere. Without `rg` on `PATH`, the broker
and runner tests skip visibly. On the episode host, set
`AGENT_EVAL_REQUIRE_BWRAP=1` so that `LiveContainmentTests` must run. The tests
never contact a provider, read host Codex configuration, or install anything.

The fake provider mirrors Codex 0.160.0's approval gate: it refuses an
unapproved broker call before the broker sees it. Fixture I/O is bounded
(STD-03 R17/R18):
- every MCP read and write in `support.McpClient` has a deadline and checks
  the broker process;
- every broker and runner fixture runs in its own process group, which is
  killed and reaped on close, on timeout and in test cleanup;
- the MCP fixture observes leader exit with `waitid(WNOWAIT)` and retains
  that leader until its group is swept, preventing PID/group reuse during
  signalling; TERM grace, group survival checks and final reaping are bounded;
- `FixtureBoundsTests` proves that a broker that stalls, never reads, exits
  mid-request or ignores end of input fails fast and leaves no live process.
  It also covers exited leaders with live descendants and closed pipes with
  live children, with independent pidfd cleanup if an assertion fails.

## Installed-plugin profile (schema 2)

The opt-in `installed-plugin-skill-v2` profile uses this same runner, broker and
supervision to install a pinned graph plugin in each episode's private Orbit
HOME, discover its actual MCP inventory and supply its unchanged shipped skill.
It preserves schema-1 CLI-proxy behavior and uses a distinct raw kind and offline
replay validator. See [the profile contract and operator checks](../../evals/plugin-agent-navigation/README.md).
`plugin-inspect` captures the real inventory without a provider; schema-2 `run`
requires pinned source commits and install provenance. Strict profile-2 runs also
require observable finite cgroup-v2 memory and process ceilings. Historical
`adapt` deliberately refuses profile-2 captures. No new effectiveness outcomes
or corpus are provided by this profile.
