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

## Prospective orderly teardown contract

Runner version **4** / broker **3** introduced (and runner **5** / broker **4**
retains)
`lifecycle_contract: eof-idle-at-term-observation-v2` in both raw profiles.
Request and raw schema numbers stay 1 (CLI proxy) and 2 (installed plugin).
Harness hashes and the explicit contract pin prospective classification:

- Historical runner version 2 without a lifecycle contract retains its original
  cancellation rules. Exit-zero/cancelled captures remain `failed/broker_failed`.
- The first prospective draft, runner version 3 with
  `eof-exited-at-term-observation-v1`, required an already exited worker. Its
  actual-Codex rehearsal failed in both arms: TERM was observed with the worker
  still alive, although all replies and EOF were subsequently logged and the
  worker exited zero. Replay preserves those failed v1 rehearsal outcomes.
- Version 4 adds the proven EOF/idle drain described below. Unsupported
  runner/contract combinations are refused by profile-2 replay.

A single supervisor SIGTERM observation can qualify by either the original
already-exited-zero/empty-group proof or this additional shutdown proof:

1. Before cleanup, the supervisor reads a bounded, complete broker log whose last
   exchange boundary is a **published reply** or completed EOF. It captures
   request and call counts. A busy request/call, missing reply, partial/oversized
   log or unknown counts cannot qualify. The worker publishes a request's reply
   checkpoint after the request's work completes and immediately before writing
   that response (see [Reply checkpoint ordering](#reply-checkpoint-ordering)).
2. After that checkpoint, polling the inherited MCP stdin must show exactly
   `POLLHUP`: an empty pipe with no writers. Open input, queued input, non-pipe
   input and unknown state cannot qualify. The provider process pidfd must still
   be alive. These facts justify waiting for shutdown, not continuing tool work.
3. Only this EOF/idle case defers forwarding TERM. The existing event loop waits
   at most `KILL_GRACE_S` (2 seconds), bounded also by the episode deadline, for
   natural worker exit and diagnostic EOF. There is no retry or unconditional
   grace sleep. A second signal, parent death, deadline or stalled drain fails;
   normal TERM/KILL group cleanup still runs. An expired drain never qualifies,
   even if the worker subsequently exits zero.
4. Final validation requires exactly the checkpoint's request and call counts.
   This fence rejects additional requests prefetched into Python's input buffer,
   even if the kernel pipe was already empty. The complete serial ledger must
   reconcile every request, completed call and reply, then `stdin_eof`, whose
   reply count only advances after a successful flush. A completed tool log or a
   published reply before a broken reply pipe is insufficient.

Both paths still require worker exit zero, bounded diagnostic EOF without
truncation or supervision errors, no surviving descendants or SIGKILL cleanup,
and provider liveness at the final supervisor observation. Existing exact
provider/broker reconciliation, provider exit zero, matched started/completed
turns, final-answer checks, budgets and containment all remain required.
Zero-call provider failures and abstentions remain ineligible. Provider
cancellation, in-flight calls (even with stdin closed), lost replies, missing
exit/EOF, cleanup intervention or the runner's last-resort sweep remain failures.

`broker_exit.stopped` retains `cancelled` when the supervisor observed a signal,
including a justified drain; cleanup signals remain the actual actions taken.
`supervisor_signal` and `broker_exit.lifecycle` retain kernel exit/group/parent
facts, input EOF, the idle checkpoint and drain expiration. Shared `run_child`
stop meanings and private Orbit transport success checks remain strict. Only the
versioned broker classifier interprets this additional evidence.

The original offline `exited_then_sigterm` fixture reproduces the
exit-zero/cancelled signature against unchanged baseline
`a6cba7bfaa1bd4a7c117762db2a580642d7d3be5` (the broker/runner bytes match study
`6540da961968d80a7fa2958cf6d862e5260dfe43`). It exposed a sufficient but narrower
ordering than the actual client. The new `eof_then_sigterm` fixture stops an idle
worker before it can consume EOF, closes client input, sends TERM, acknowledges
the supervisor's observation, then releases the worker. It deterministically
fails on the first draft and passes with the EOF/idle drain. Kernel state and
logged acknowledgements replace scheduling guesses. Negative fixtures retain
queued input, a stalled drain, in-flight work, missing replies, SIGINT, provider
failure, changed checkpoint counts and descendant cleanup as failures.

These observations describe handler-time state before supervisor cleanup, not
signal sender identity or kernel signal-generation time. Python signals can be
deferred. No old cohort or rehearsal is rewritten or reclassified. The first
actual-client rehearsal and its failure remain evidence; the worker never runs
a model or real-client rehearsal.

Operator admission remains required before further measurements: run the full
strict fake-provider suite with real private Orbit and exact final hashes, plus
repository gates and host CI. Then run a new small synthetic actual-Codex normal
shutdown rehearsal outside the cohort in both arms; inspect checkpoint counts,
EOF/replies, exits, telemetry and cleanup. Any remaining failure blocks
measurement and needs new diagnosis. Retain all 12 frozen cohort outcomes and
the original scoring attempt, including strict provenance refusals. An accepted
old score is not a prerequisite for reviewing prospective delivery.

```sh
AGENT_EVAL_REQUIRE_BWRAP=1 \
AGENT_EVAL_ORBIT=/absolute/orbit \
AGENT_EVAL_ORBIT_GRAPH="$PWD/target/debug/orbit-graph" \
  python3 -B -m unittest discover -s scripts/agent-eval/tests -v
```

### Reply checkpoint ordering

Earlier broker bytes flushed each MCP response before logging its `reply`
checkpoint. A client that tore down as soon as it held its final response could
have the supervisor observe EOF while the checkpoint was still unpublished; the
null checkpoint correctly refused the drain, so the episode failed
`broker_failed` depending on scheduling (ORB-13847). The worker now logs the
`reply` checkpoint after any wire redaction and immediately before writing the
response, so a delivered response always has a published checkpoint. Only that
write can remain. It is proven by worker exit zero and the `stdin_eof` stop's
reply count. A broken or full reply pipe still fails, as does an expired drain.
The drain never performs tool work.

The two orderings are equivalent for a client. Before, a flushed response could
already sit unread in the pipe at observation. Now the drain may also finish
writing a published response to a pipe that is still open. Telemetry
reconciliation still requires the provider to report every broker call.

Classification rules, request/raw schemas, runner/broker version strings and
`eof-idle-at-term-observation-v2` are unchanged. The broker source hash changes,
and each capture records it (`tool_versions.read`, harness hashes). Captures
from earlier broker bytes keep their post-flush `reply` meaning and their
outcomes. Freeze the new harness hashes before any future use. Corpus locks
that pin the previous broker bytes refuse it intentionally.

`test_broker.py` stops a runtime copy of the worker immediately after its final
response is flushed. The client then reads that response, closes input, sends
TERM, waits for the logged supervisor observation and resumes the worker. With
the earlier ordering, this fails deterministically: the observation has a null
checkpoint and cleanup sends SIGTERM to the worker, whose exit status then
depends on scheduling. With the repair, it passes with the full checkpoint, no
cleanup signals and exit zero. A companion test stops the worker after publication but before the
write and closes the client's reader. The justified drain then still fails.

## Prospective source-reply provenance (runner 5)

Installed-plugin **schema-2** episodes now use runner 5, broker 4 and
`reply_contract.version: safe-tool-text-v1`. Both baseline and graph arms use
this same boundary for `read`, `rg`, Git and installed product replies. The
schema-1 writer keeps its historical runner-4 path; existing schema-1/2 captures
are never rewritten, migrated or rescored. Lifecycle semantics remain exactly
`eof-idle-at-term-observation-v2`. Runner 2 and 3 replay dispatch remains intact,
and runner 4 keeps its existing redaction refusal.

The former runner sent raw source text to the provider, then independently
masked the provider JSONL, broker log and artifact. Credential-shaped regression
literals therefore caused a correct exact-provenance refusal. Synthetic `read`
and `rg` fixtures reproduce this on delivered baseline
`46009e6b4091c1918f81c0dd0f2e0f146f8c54cd`; no live credential or old study was
needed to establish the cause. A match is never assumed harmless.

The prospective path masks text before broker paging/line cuts and before the
common reply is delivered and recorded. Ripgrep's own column preview is disabled
for this path, so it cannot cut a credential before masking. The outer JSON
reply discloses `text_view` (contract, whether transformed, and that source
metadata still describes the original). Original source commits, content
revisions, selected Git blob hashes, product spans and raw transport hashes
retain their original meanings. Paging offsets into transformed text describe
that transformed view; they are not original-source byte offsets.

Page and read-line producers fit the **final** delivered envelope, including
`text_view`, JSON escaping, UTF-8 bytes and any redaction expansion. Sizing uses
the same transformation as delivery without adding candidate views to the
proof. Ordinary long `rg`/Git results retain advancing offsets even at the
1024-byte minimum call budget. Reads retain line continuations: a first line
that cannot fit is explicitly cut and marked in the transformed view, then the
continuation advances to the next line. This can omit the rest of that line;
`rg` or Git supplies character paging when the whole transformed text is needed.
Installed product replies cannot be paged without changing their lossless
contract, so their bound includes the final envelope too. Oversized replies
retain complete private diagnostic evidence and deliver a bounded structured
`call_output_truncated` error. The final per-call and episode byte guard stays
in force; an exhausted episode budget still ends the episode.

`reply_provenance.py` supplies the shared masking policy and replay verifier.
It recognizes the previous bearer/provider-key/JWT shapes and host values of at
least eight characters, including escaped forms in nested JSON. The runner
passes only salted SHA-256 fingerprints and lengths to the broker, never copies
credential values into its configuration or broadens subprocess environments.
Fingerprints and raw-content commitments are private evidence: they are **not**
a guarantee against guessing low-entropy preimages and must not be published as
anonymized data. The source snapshot stays the original input, not a redacted
substitute.

For each measured call, the private log and sealed artifact record:

- The versioned policy and a digest binding the request, original input manifests
  and immutable source provenance; all four harness module hashes are pinned.
- Changed intermediate views before paging (read lines, child stdout, installed
  product envelopes), with original text digests/UTF-8 byte lengths, safe text,
  and ordered replacement spans/rules. Original text is not copied into evidence.
- The final safe envelope, its original/safe digest and byte accounting, the
  exact delivered digest/bytes, any subsequent budget cut, and a digest of the
  measured tool/input/status/time/output call.

Only the delivered text plus the answer counts toward `output_bytes`; diagnostic
views and JSON-RPC framing do not count as model tool text. Masking time is inside
the measured call and episode clocks. Token usage still comes only from provider
telemetry, with no inferred dollar cost. Product `stdout_sha256` continues to
identify the raw private transport; safe text has its own explicitly named hash.

Replay cross-checks the artifact against the broker log, source binding,
transformation spans, provider results and exact costs. The original-text
commitments are attestations from the pinned capture producer, not a standalone
proof of secret preimages. Like the existing artifact seal, they do not
authenticate a malicious operator who rewrites every capture and preregistration.
No source/hash, outcome, containment, authorization or exact-result check is
waived. Provider telemetry mismatches remain failed outcomes, including missing
or forged tool results; transformations cannot turn them into success.

Durable writers and the final broker wire retain a masking backstop. Any
unplanned broker audit/wire masking, or any late masking of provider JSONL,
stderr, final answer, setup/treatment or artifact, still makes exact replay
refuse. Broker logs never store newly encountered raw credential strings. No
provider-generated secret is legitimized by the prospective reply contract.

Admission remains root-owned: freeze all four module hashes plus evaluator,
provider, Orbit and backend hashes, run the strict zero-skip fake-provider suite,
then explicitly admit a new synthetic actual-Codex rehearsal in **both** arms on
those exact bytes. The worker performs only fake-provider/private-plugin tests.
A denied namespace check is not containment evidence. The frozen twelve-episode
cohort, its failed outcomes and original adapter refusal remain authoritative,
with no accepted quality score. A later measured study needs a fresh frozen
preregistration/corpus under this contract; no old episode may be replaced.

## Prospective accounting (ORB-14003; consumer interface for ORB-14002)

Opt in with request `schema_version: 3` and
`profile: "installed-plugin-skill-v3"`. The remaining request fields, source
selection, plugin pin, cold treatment, authority and budgets are those of profile
2. Raw captures use the existing installed-plugin kind, schema 3, runner `6`,
broker `5`, `safe-tool-text-v1` and the delivered
`eof-idle-at-term-observation-v2` lifecycle contract. The five harness hashes
include `telemetry.py`. `plugin-inspect` still exports the same treatment pin and
inventory without starting a provider; that pin is also accepted by profile 3.
Schema 1 and 2 writers and replay retain their old token sums, classifications
and result shapes. No historical corpus, score, study lock or result is updated.
The historical `adapt` entry point refuses schema 3.

Use `evals/plugin-agent-navigation/eval.py replay` with a schema-3 preregistration
and profile above. Its `load_episode(directory, diagnostic=False)` verifies and
returns a sealed raw capture; each replay `costs[]` entry also includes the full
`telemetry` object. Replay recomputes this object from the hashed provider JSONL,
broker JSONL and `phase-timing.json`, compares types as well as values, and checks
existing call proofs, input/output bytes, provider reconciliation, source,
containment and lifecycle outcomes. A malformed or missing ledger, altered
coverage/bytes/duration, or mixed schema/profile/runner is rejected. It cannot
fall back to a historical validator. Failed captures can replay as failed;
replaying diagnostics does not qualify them as contained effectiveness evidence.
Freezing the merged harness hashes and qualifying the actual runtime are the
root operator's responsibility. This worker uses no live model.

The schema-3 raw artifact has `telemetry.contract:
"prospective-accounting-v1"` and these fields:

- `usage.turns[]`: ordinal, nullable supplied `session_id`/`turn_id`, start event,
  terminal `outcome` and all terminal `observations[]`. Each observation retains
  raw usage, truly reported normalized field names, typed nullable values and
  invalid-field diagnostics, including usage on failed/cancelled turns.
  Every event reference gives its one-based JSONL line, zero-based byte offset,
  exact line byte count **including its newline**, and SHA-256 of those bytes.
  Duplicate terminals/IDs/JSON keys and malformed or partial streams are never
  silently deduplicated into complete accounting. Missing optional identities
  are allowed; supplied identities must be non-empty strings of at most 128
  UTF-8 bytes. Invalid types and cross-session turn identities are inconsistent.
- `usage.fields`: `input_tokens`, `cached_input_tokens`, `output_tokens`,
  `reasoning_tokens` (also observes `reasoning_output_tokens`), and `total_tokens`.
  Each has `expected_turns`, `reported_turns`, `valid_observations`, nullable
  `observed_sum`, `coverage` (`complete`, `partial`, `inconsistent`), nullable
  `total`, and `total_state`. `observed_sum` is the exact sum of valid observed
  counters, **a diagnostic rather than a session token total**. Zero survives.
  Invalid bool/negative counters, competing reasoning aliases and cached input
  greater than input invalidate completeness. Missing usage, failed/cancelled
  turns, abnormal provider exit and bounded/truncated JSONL prevent complete
  coverage. Pending turns remain visible. `session_complete` concerns turn
  lifecycle; it never asserts complete token accounting or cost.
- Counter scope is pinned as `codex-jsonl-scope-unqualified-v1`: neither
  incremental-per-turn nor cumulative-across-turns is established by the
  available captures. `usage.source` pins the provider binary hash/version and
  format; `semantics_evidence` remains null. A single clean turn can expose its reported counter with
  `total_state: "observed_single_turn"`; even complete multi-turn field coverage
  has `total: null`, `total_state: "unknown_semantics"`. Cache subset semantics
  are also unqualified, so `derived.uncached_input_tokens` is null, labelled
  `unknown_cache_semantics`. Cached input is never added to input. No dollar
  cost or model tokens per tool are invented. Top-level `usage` contains only
  qualified single-turn input/output counters; `usage_raw` is always null in
  schema 3. Consumers must use the ledger rather than legacy partial sums.
- `attempts[]`: every parsed broker `tools/call`, logged before protocol,
  allowlist and budget checks. `(run_id, attempt_id)` is stable and unique;
  `request_seq` and supplied JSON-RPC `request_id` link it to the request and
  published reply checkpoint. IDs such as `request-1` are episode-local and
  independent of provider item IDs. Each attempt retains tool/arguments,
  canonical safe argument UTF-8 bytes/hash, event references, monotonic
  `started_ns`, nullable `ended_ns`/`elapsed_ns`, nullable `call_seq`, outcome,
  error, completion/success flags, output text/UTF-8 bytes/hash and
  `reply_published`. `observed_outcome` retains the handler return separately
  from pending/interrupted request outcome. `call_completed`/`call_event` and
  measured output bytes/hash retain tool completion before an interrupted
  return or unpublished reply; this never counts as successful use. Protocol
  refusals have no tool text; unknown-tool and budget refusals do. Missing
  endpoints/results remain null; partial measured observations are explicitly
  retained and interrupted requests reference broker exit.
  Invalid/unparseable frames cannot be identified as `tools/call` and remain
  protocol-error evidence. Duplicate JSON-RPC IDs are disambiguated by sequence.
- `tools`: all-attempt denominator, attempts, responses (finished requests,
  including refusals), completed **admitted tool calls**, successes, graph
  attempts/completions/successes, and per-tool summaries. Successful use requires
  a published reply and exact provider reconciliation; uncorroborated captures
  conservatively expose zero successes while retaining observed producer
  outcomes. `input_bytes` and
  `output_bytes` sum the ledger exactly, including refusal text. Output bytes
  describe prepared safe tool text, excluding JSON-RPC framing; they are not
  model tokens. `provider_received_output_bytes` is null unless exact provider
  result reconciliation succeeds. The legacy top-level `output_bytes` still
  counts measured call text plus the final answer, so it can differ by refusal
  text. Graph refusals count as attempts, never completed or successful graph
  use. `provider_call_observations` separately retains provider item identity,
  status/error and event location, including pre-broker approval refusals;
  these observations are not added again to the broker denominator.
  `coverage` reports its state, issues, observed `reported_attempts` and nullable
  `expected_attempts`. A cut broker-log tail retains the valid prefix with
  partial coverage and a hashed tail location; expected attempts and the
  all-attempt denominator are unknown/null. This remains a failed capture, with
  zero qualified successes and null received bytes. Duplicate request or reply
  sequences are rejected before indexing, rather than silently overwritten.
- `reconciliation`: exact-result match verdict and nullable error. Extra
  refusal results must match with full multiplicity. After removing precisely
  those results, the original completed-call audit runs unchanged. Arguments,
  status or text mismatches, missing results, partial input and interrupted
  calls still fail. The reply checkpoint remains published immediately before
  writing/flushing; `reply_published` alone does not prove delivery or orderly
  EOF. ORB-13847's lifecycle negatives remain mandatory.

`telemetry.timing` partitions integer milliseconds as
`wall_ms = setup_ms + provider_ms`; rounding remainder belongs to provider.
`phase-timing.json` retains the three monotonic nanosecond boundaries. Wall
starts after initial admission preflight and ends at the supervisor's provider
exit/stop observation, before post-exit cleanup/capture work. Setup includes
snapshot materialization, source verification, cold plugin installation and its
second preflight. Provider phase includes the launch/containment probe and broker
work; it is not pure model compute. Separately `overlapping` contains
`plugin_install_ms`, `graph_sync_ms` and `preflight_within_setup_ms` observations.
Do not add these again to wall. `outside_wall.preflight_ms` records the initial
preflight interval; source export and runtime qualification costs are null
because they occur outside episodes and are not measured here. `end_to_end_ms`
is null. No warm-cache support or full end-to-end cost is claimed.

The study owns the cohort attempt registry, including attempts that never start
an episode. Schema-3 admission/setup runner refusal responses (also written to
`refusal.json` when the private output layout already exists)
include `request_digest`, equal stable `request_identity`, phase `stage`, typed
refusal evidence and `provider_started: false`. There is no invented provider
run ID, token usage or timing for these. An invalid pinned source yields a setup
refusal; a capability/feature denial yields an admission refusal. Registry-level
attempt IDs, retries, runtime-qualification/source-export costs and custody of
these refusal records belong to ORB-14002/the root operator, not this harness.

For example, these excerpts describe two turns with input reported only once,
and two observed attempts before a cut broker-log tail. They are fields of a
schema-3 capture, not a complete preregistration or raw artifact:

```json
{
  "schema_version": 3,
  "profile": "installed-plugin-skill-v3",
  "telemetry": {
    "contract": "prospective-accounting-v1",
    "usage": {
      "counter_semantics": "codex-jsonl-scope-unqualified-v1",
      "fields": {
        "input_tokens": {
          "expected_turns": 2, "reported_turns": 1,
          "valid_observations": 1, "observed_sum": 10,
          "coverage": "partial", "total": null, "total_state": "partial"
        }
      },
      "derived": {"uncached_input_tokens": null, "state": "unknown_cache_semantics"},
      "cost_usd": null, "per_tool_tokens": null
    },
    "tools": {
      "attempts": 2, "completed": 1, "successful": 0,
      "all_attempt_denominator": null,
      "coverage": {"state": "partial", "reported_attempts": 2, "expected_attempts": null}
    }
  }
}
```

An admission-refusal excerpt instead has `"stage": "admission"`,
`"provider_started": false` and equal 64-hex `request_digest` and
`request_identity`. The study gives that refusal its own cohort attempt ID;
there is no provider session, observed usage or episode timing to infer.

Run the prospective real-entry-point regressions with:

```sh
AGENT_EVAL_ORBIT=/absolute/orbit \
AGENT_EVAL_ORBIT_GRAPH="$PWD/target/debug/orbit-graph" \
  python3 -B -m unittest discover -s scripts/agent-eval/tests -p test_telemetry.py -v
```

The provider is fake; the installed Orbit plugin is real and private. Set
`AGENT_EVAL_REQUIRE_BWRAP=1` to make unavailable user namespaces fail strict
qualification rather than skip. Diagnostic passes and namespace refusals are
reported separately. No live plugin, Codex auth or user configuration is changed.
