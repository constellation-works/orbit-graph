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
`docs/evaluation/agent-navigation/eval.py`. The run directory must not be
inside that checkout or inside the export.

```sh
TOOL=scripts/agent-eval/eval_runner.py           # from this checkout
ORB13710=/path/to/orb-13710-checkout
EVAL="$ORB13710/docs/evaluation/agent-navigation"
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
   tokens. It reads back Codex's effective features and its MCP inventory
   inside the sandbox:

   ```sh
   python3 -m unittest discover -s scripts/agent-eval/tests -k LiveContainment -v
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
   - `isolation.unbrokered` is empty;
   - the error is not a harness failure such as `provider_code_mode_required`
     (see [Code mode](#code-mode)).

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
  - User config, rules and history are ignored (`--ignore-user-config
    --ignore-rules --ephemeral`), web search is disabled, and approvals are
    `never`.
- **Transcript audit.** Any JSONL item that is not an agent message,
  reasoning, todo list, error or `eval_broker` MCP call (for example
  `command_execution`, `file_change`, `web_search`, or another MCP server)
  makes the episode `invalid` (`unbrokered_tool_use`). Broker-logged calls
  must also appear, in order, among the provider's reported calls
  (`telemetry_mismatch`).
- **Broker.** The broker serves the arm's tools only; graph tools on the
  baseline arm are refused and not counted. It refuses:
  - absolute paths, `..`, `.git`/`.orbit-graph`, `~`, backslashes and
    control characters
  - symlinks, opened component by component with `O_NOFOLLOW`
  - option-shaped values, unknown fields and malformed MCP

  Children run with fixed argv and environments. Each call has a deadline and
  bounded output, and a timed-out child's whole process group is ended.
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
6. `broker_not_initialized`
7. `tool_inventory_mismatch`
8. `provider_output_malformed`
9. `telemetry_mismatch`
10. `provider_incomplete` (non-zero exit, no completed turn, or a failed turn)
11. `final_answer_missing`
12. `invalid` `final_output_oversized`
13. `invalid` `answer_malformed`
14. `invalid` `no_tool_calls`
15. otherwise `ok`

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
- `provider_preflight_failed`.
- `provider_auth_unavailable` / `provider_auth_unsafe`.
- `expected_tool_unavailable`: rg, git, python, codex, orbit-graph or (with
  `--allow-code-mode`) the code-mode host is missing or not executable, or a
  version probe failed.

## Limits

- **bwrap is required for evidence.** Where unprivileged namespaces are denied,
  no episode runs. The contained path is covered by `LiveContainmentTests`,
  which skips visibly on such hosts. Run it on the episode host first.
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
and runner tests skip visibly. The tests never contact a provider, read host
Codex configuration, or install anything.
