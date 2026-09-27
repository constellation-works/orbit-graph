# Plugin surface: decisions

Decisions for ORB-13166, which makes the Orbit plugin surface honest: config
keys take effect, fields an operation does not read are refused, schemas match
the code, the backend binary is bound to consent, and the installed-Orbit tests
run in CI. The user-facing contract is in [`docs/plugin.md`](../../plugin.md);
the parity and drift checks are in
`crates/orbit-graph-cli/tests/plugin_contract.rs`.

## D1. `branch` reaches the backend as `context.config`, not as an activity template

The task suggested templating the history-sync activity input with
`branch: "{{config.branch}}"`. Orbit's `plugin.tool_call` action does not render
templates in its `input` (only filesystem roots are rendered), so the backend
would receive the literal string. Orbit does send the effective
`[plugins.graph]` section, manifest defaults under operator values, as the
envelope's `context.config`. The backend therefore reads `context.config.branch`
as the default for every `status`, `recommend` and `maintain` call that names no
`branch`, and the seeded activity keeps omitting `branch` (a comment there says
why). A call's own `branch` still wins. `history_sync`, `import` and
`orbit_sync` responses now report the `branch` they used, an additive field
(`schemas/maintain.response.json`).

`plugin_contract::every_config_key_is_consumed` checks every key in
`schemas/config.json`: it must be either rendered as `{{config.<key>}}` by the
manifest or a definition, or read by the backend from `context.config`. The
second case is proven by sending the key through the real binary and checking
that no `ignoring plugin config key` notice appears.

## D2. `index_dir` is removed (deviation from `STD-02@2 §R16`)

No runtime path read `index_dir`: the history index lives in the repository's
`.orbit-graph/` and the code graph under plugin state, whatever the key said.
The task allows removal when no real path honours it, and keeping it would
advertise a setting that does nothing (`STD-01@2 §R36`). `STD-02@2 §R16` asks
that a removed key be warned about and ignored rather than rejected. The backend
does exactly that for any `context.config` key it does not read: the key is
named on stderr and the call proceeds. Orbit, however, validates the operator's
config against the closed `schemas/config.json` (`additionalProperties: false`)
at load and refuses a config that still sets `index_dir`, naming the key. That
refusal is Orbit's, it names the fix, and the value it refuses never had an
effect. Reopening the schema to accept a dead key would contradict D1's parity
check, so the loud refusal is accepted and documented in `docs/plugin.md`.

## D3. `schema_version` stays optional, and its absence stays absent (`STD-02@2 §R16`)

Not every in-repo caller sends `schema_version`: the history-sync and code-sync
activities omit it, and so does the derived `orbit graph <verb>` CLI. The field
therefore stays optional. The old `#[serde(default = "default_schema_version")]`
fabricated `1` for an absent value. Every request struct now holds
`Option<u32>`, and `validate_schema` accepts `None` or `Some(1)`. Absence is read
as version 1, the only version there has been, and is not rewritten into the
decoded request. Each request schema's description says so.

## D4. Inapplicable fields are refused per operation (`STD-01@2 §R29`)

`maintain` declares, per operation, the optional fields it reads
(`MaintenanceOperation::reads`). A request that supplies any other field is
refused with `invalid_request`, naming every such field and the ones the
operation does read. The check runs after schema validation and before bounds,
routing or any index is opened. To keep the refusal about the field and not
its shape, `delivery` and `task_snapshots` are held as raw JSON until the
operation that reads them decodes them. `recommend` refuses `hybrid_limit`
without `hybrid: true` in the same way. `hybrid_limit`, `limit` and `branch`
are now `Option`s, so "not supplied" is distinguishable from "supplied the
default".

An empty string in a top-level field, or as an item of a top-level list, is
refused naming the field. It used to be read as a value (an empty `branch`, an
empty `run_ids` item), which the schemas' `minLength: 1` already forbade.

## D5. Schema parity is proven through the binary

`plugin_contract::request_schemas_equal_the_serde_structs_and_runtime_validation`
walks every tool in the root manifest and checks the following against the real
executable:

- **properties** equal the fields serde accepts (read from the
  `deny_unknown_fields` error for a probe field);
- **enums** equal the variants serde accepts (read from the unknown-variant
  error);
- **`const`** values pass, and any other value is refused;
- **`minimum`/`maximum`** bounds pass, and one past each bound is refused
  naming the field;
- **`minLength`** holds for fields and list items;
- **`required`** fields are required;
- **`not.required`** combinations are refused.

A schema edit without a code change, or the reverse, fails `make ci`.

## D6. `plugin/plugin.yaml` is generated under a check gate

The compatibility tree under `plugin/` had drifted from the root manifest
(fewer tools, hand-copied schemas). The task offered either agreement checks or
generation. It is now generated: `plugin_contract::plugin_tree_is_generated_from_the_root_plugin`
derives it from the root `plugin.yaml` with every `$ref` inlined, and without
`definitions` or `config`, so the tree seeds no schedule and needs no config
schema. The test also fails when the committed tree differs. Run
`UPDATE_GOLDENS=1 cargo test -p orbit-graph-cli --test plugin_contract --locked`
to regenerate (`STD-04@1 §R11`). The same run mirrors `skills/orbit-graph/`
into `plugin/skills/orbit-graph/`.

The two skills must be identical. An agent that loads either tree gets the same
guidance, and a second copy that differs is exactly the drift this task removes.
The two launchers must also be identical. The check runs in `make ci`
(`STD-04@1 §R12`). The tree is retired later by ORB-12850.

## D7. The backend executable is bound to consent (`STD-05@1 §R21`, `§R4`)

Orbit records a digest of `plugin.yaml` at consent. `spec.backend.args` is part
of that manifest, so a binding recorded there is covered by the operator's
approval. The launcher accepts exactly one of these:

- **`--backend-sha256 <hex>`** runs only an executable with that SHA-256. The
  digest is checked before the executable runs, so a PATH impostor that answers
  the version probe correctly is refused with `incompatible_binary`. The
  refusal's `detail` gives both digests.
- **`--allow-unbound-backend`** is the named override. It runs a compatible
  executable whatever its digest, writes a stderr notice naming the path, and
  exports `ORBIT_GRAPH_BACKEND_OVERRIDE`. The executable then adds a top-level
  `backend_override` to every response envelope.

Anything else runs nothing. The launcher no longer forwards its arguments to the
executable.

`backend_override` is an additive top-level field beside `ok`. Orbit reads only
`ok` and `output`, so Orbit's own callers do not see it. It is visible to a
direct caller, and the stderr notice reaches Orbit's backend log. Putting it
inside `output` would have changed every tool's output schema.

A release carries no executable, so there is nothing to bind: both committed
manifests carry `--allow-unbound-backend`, which
`plugin_contract::committed_manifests_carry_the_named_unbound_override`
enforces. `scripts/bundle-plugin-binary.sh` binds by default. It hashes the
copied executable, rewrites that tree's `args` line, and prints the
re-approval step `orbit plugin add <tree> --force`, because the manifest digest
has changed.

**Known limitation.** A first-party tree installed from `git+` cannot be
re-approved in place: Orbit honours `origin: orbit` only for the `git+` source,
and refuses a local-directory re-add that keeps the claim. Such hosts either
bundle with `--unbound`, so the override stays and is reported on every call,
or install a local tree without the `origin` claim and bind it. Binding
first-party installs needs Orbit to support re-consent for an installed tree.

The deprecated installer applies the same rule, reading
`plugin/plugin.yaml`:

- a recorded digest must match, otherwise it exits 1 with
  `incompatible_binary`;
- the override is reported on stderr;
- anything else is refused.

It refuses a set `ORBIT_GRAPH_BIN` (exit 2) instead of silently honouring it.
All of this happens before anything is registered.

## D8. Installed-Orbit tests are ignored locally and required in CI (`STD-04@1 §R7`, `§R8`)

The two tests that register the plugin with a real Orbit used to `return`
silently when `ORBIT_GRAPH_TEST_ORBIT_BIN` was unset, so they never ran
anywhere. They are now
`#[ignore = "requires an Orbit binary in ORBIT_GRAPH_TEST_ORBIT_BIN"]`, which
names the missing capability in every run. They panic, rather than pass, when
run with `--ignored` and no binary. CI's `plugin-conformance` job runs
`cargo test -p orbit-graph-cli --test plugin_integration --locked -- --ignored`
against the pinned Orbit release.

Running them surfaced two latent failures:

- An inherited agent environment (`ORBIT_PROC_ALLOWED_PROGRAMS` and friends)
  leaked into the isolated Orbit root. Every Orbit invocation in these tests now
  strips all `ORBIT_*` variables.
- `status` was called before any history index existed, and failed with
  `index_missing` while `orbit tool run` exited 0. The tests now sync first and
  fail on any `ok: false` envelope.

The installer's binding and `ORBIT_GRAPH_BIN` checks need no Orbit. They are
also covered by an unignored test that uses a recording fake `orbit`.

## D9. Hermetic fixtures and derived latency ceilings (`STD-04@1 §R7`, `§R9`)

`plugin_integration.rs` fixture git commands now go through
`common::git_command`, which isolates them from host and user git config.

The latency assertions derive from the configured adapter timeout:

- **Ceiling.** It is `ADAPTER_TIMEOUT_SECONDS` plus the named `LATENCY_MARGIN`.
- **Stuck subprocesses.** The fake Orbit's stuck subprocesses (the `sleep`
  shim and the lingering MCP server) sleep `STUCK_PAST_CEILING` beyond that
  ceiling, so a call that waited for one instead of timing out or reaping it
  cannot pass.
- **Margin size.** The margin is generous, because a passing run never waits
  for the stuck process.

A 20 ms sleep between two idempotent syncs is also removed. Nothing depended on
it.
