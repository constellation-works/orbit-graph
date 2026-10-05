"""Installed-plugin treatment, version 2. No provider or live-host configuration.

The existing runner owns confinement, clocks, telemetry and process supervision.
This module exports pinned Git objects and installs only into that episode's HOME.
The broker is the sole gateway to the private host; it never exposes Orbit's registry.
"""
import hashlib
import io
import json
import os
from pathlib import Path
import re
import shutil
import tarfile
import time

import eval_broker as broker

PROFILE = "installed-plugin-skill-v2"
PROSPECTIVE_PROFILE = "installed-plugin-skill-v3"
PROFILES = {2: PROFILE, 3: PROSPECTIVE_PROFILE}
RAW_KIND = "installed-plugin-agent-eval-raw-episode"
WORKSPACE = "agent-eval-episode"
VERBS = ("version", "status", "recommend", "maintain", "search", "show", "refs",
         "callees", "impact", "trace", "deps", "overview", "changes")
TOOLS = tuple("graph_" + verb for verb in VERBS)
PIN_FIELDS = {"commit", "backend_sha256", "orbit_sha256", "inventory_sha256", "skill_sha256"}
POLICY = {"version": 1, "maintenance": ["graph_sync"],
          "authority": "private-host-operator-for-maintain-only", "workspace": WORKSPACE,
          "namespace": "local-export-graph", "source_view": "code-files-v1",
          "writable_workspace_state": [".orbit", ".orbit-graph"], "reply": "lossless-product_reply-in-broker-envelope"}
MAX_SETUP_OUTPUT = 4 * 1024 * 1024


def digest(value):
    return hashlib.sha256(broker.canonical(value).encode()).hexdigest()


def check(condition, message):
    if not condition:
        raise ValueError(message)


def validate_pin(pin):
    check(isinstance(pin, dict) and set(pin) == PIN_FIELDS, "plugin pin fields differ")
    check(isinstance(pin["commit"], str) and re.fullmatch(r"[0-9a-f]{40}", pin["commit"]),
          "plugin.commit must be an immutable full Git commit")
    for key in PIN_FIELDS - {"commit"}:
        check(isinstance(pin[key], str) and re.fullmatch(r"[0-9a-f]{64}", pin[key]),
              f"plugin.{key} must be a SHA-256")


def evidence(result):
    return {key: value.decode("utf-8", errors="replace") if isinstance(value, bytes) else value
            for key, value in result.items()}


def clean(result):
    return (result["exit_code"] == 0 and result["stopped"] is None
            and result["error_type"] is None and result["supervision"]["survivors"] == []
            and not result["stderr_truncated"])


class McpExchange:
    """Exactly initialize -> initialized/call -> matching result -> stdin EOF.

    The supervisor bounds aggregate wire bytes and the entire session lifetime.
    Reject unsolicited messages, JSON-RPC errors and incomplete frames; product
    isError results remain intact for the caller. Continue checking through stdout
    EOF so a duplicate or malformed trailer cannot turn into successful evidence.
    """
    def __init__(self, method, params):
        self.start = self.encode({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2024-11-05", "capabilities": {},
            "clientInfo": {"name": PROFILE, "version": "2"}}})
        self.call = (self.encode({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}})
                     + self.encode({"jsonrpc": "2.0", "id": 2, "method": method, "params": params}))
        check(len(self.call) <= broker.MAX_MESSAGE_BYTES, "MCP request exceeds input bound")
        self.phase, self.pending, self.reply = "send_init", bytearray(), None

    @staticmethod
    def encode(message):
        return (broker.canonical(message) + "\n").encode()

    def sent(self):
        check(self.phase in {"send_init", "send_call"}, "unexpected MCP write")
        self.phase = "init" if self.phase == "send_init" else "call"

    def receive(self, chunk):
        self.pending.extend(chunk)
        if b"\n" not in self.pending:
            check(self.phase in {"init", "call"}, "unexpected MCP trailer")
            return b""
        line, _, tail = self.pending.partition(b"\n")
        # No second response is legal before the next request, or after id 2.
        check(not tail, "unexpected MCP transcript")
        self.pending.clear()
        row = json.loads(line.decode("utf-8"), object_pairs_hook=broker._unique,
                         parse_constant=broker._reject_constant)
        check(isinstance(row, dict) and set(row) == {"jsonrpc", "id", "result"}
              and row["jsonrpc"] == "2.0" and type(row["id"]) is int
              and isinstance(row["result"], dict), "invalid MCP response")
        if self.phase == "init":
            check(row["id"] == 1, "unexpected initialize id")
            init = row["result"]
            check(isinstance(init.get("serverInfo"), dict)
                  and init["serverInfo"].get("name") == "orbit-mcp", "unexpected host")
            check(init.get("protocolVersion") == "2024-11-05", "unexpected protocol")
            check(isinstance(init.get("capabilities"), dict), "missing server capabilities")
            self.phase = "send_call"
            return self.call
        check(self.phase == "call" and row["id"] == 2, "unexpected call response")
        self.reply, self.phase = row["result"], "done"
        return None

    def eof(self):
        check(self.phase == "done" and not self.pending, "incomplete MCP transcript")


def mcp(orbit, repo, env, method, params, timeout, operator=False, prefix=()):
    """One bounded stdio session, holding stdin until both replies are validated.

    No shell, inherited environment or persistent operator connection. Full wire
    output and stderr remain in the returned transport evidence, even on errors.
    """
    exchange = McpExchange(method, params)
    argv = [*prefix, orbit, "mcp", "serve", "--workspace", WORKSPACE]
    if operator:
        argv.append("--operator")
    result = broker.run_child(argv, repo, env, timeout, capture_limit=MAX_SETUP_OUTPUT,
                              input_exchange=exchange)
    return (exchange.reply if clean(result) and not result["supervision"]["signals"] else None), result


def plugin_inventory(reply):
    check(isinstance(reply, dict) and set(reply) == {"tools"}, "unsupported tools/list shape")
    all_tools = reply["tools"]
    check(isinstance(all_tools, list), "missing tool inventory")
    selected = [tool for tool in all_tools if tool.get("name", "").startswith("graph_")]
    check(len(selected) == len(TOOLS) and {tool.get("name") for tool in selected} == set(TOOLS),
          "installed graph inventory changed; version the profile before accepting it")
    by_name = {tool["name"]: tool for tool in selected}
    for tool in selected:
        check(isinstance(tool.get("description"), str) and isinstance(tool.get("inputSchema"), dict),
              "incomplete installed schema/description")
    return [by_name[name] for name in TOOLS]


def validate_arguments(tool, arguments, root):
    """Constrain authority and routing, leaving product validation to the installed host.

    Do not translate CLI spellings or silently inject/rewrite arguments. Source
    mounts are immutable; supplied path selectors cannot reach host/config state.
    """
    if tool not in TOOLS:
        raise broker.Refusal("tool_not_permitted", "not an installed graph tool")
    if not isinstance(arguments, dict):
        raise broker.Refusal("invalid_argument", "arguments must be an object")
    if "repository" in arguments and arguments["repository"] != root:
        raise broker.Refusal("repository_refused", "repository must equal the episode repository")
    if "workspace" in arguments and arguments["workspace"] != WORKSPACE:
        raise broker.Refusal("workspace_refused", "workspace must equal the private episode workspace")
    if tool == "graph_maintain" and arguments.get("operation") != "graph_sync":
        raise broker.Refusal("maintenance_refused", "this navigation profile permits only graph_sync")
    for name in ("scope", "selector", "symbols"):
        if name not in arguments:
            continue
        values = arguments[name] if name == "symbols" else [arguments[name]]
        if not isinstance(values, list):
            raise broker.Refusal("invalid_argument", f"{name} must be a list")
        for value in values:
            if not isinstance(value, str):
                raise broker.Refusal("invalid_argument", f"{name} must contain strings")
            path = value
            if name != "scope" and ":" in value:
                kind, path = value.split(":", 1)
                if kind not in ("symbol", "file", "dir"):
                    raise broker.Refusal("selector_refused", "unsupported path selector")
                path = path.split("#", 1)[0]
            parts = broker.split_path(path)
            if ".orbit" in parts:
                raise broker.Refusal("path_refused", "Orbit state is hidden")
            target = Path(root).joinpath(*parts)
            # Snapshot files are immutable; refuse any unexpected symlink component.
            cursor = Path(root)
            for part in parts:
                cursor = cursor / part
                if cursor.is_symlink():
                    raise broker.Refusal("symlink_refused", "path traverses a symbolic link")
            if not target.resolve().is_relative_to(Path(root).resolve()):
                raise broker.Refusal("path_refused", "path escapes the repository")
    for name in ("base", "head", "revision", "branch"):
        if name in arguments:
            broker._revision(name, arguments[name])
    return arguments


def complete_reply(reply):
    """This pinned host emits matching JSON text and structured product results."""
    if not isinstance(reply, dict) or type(reply.get("isError")) is not bool:
        return False
    content = reply.get("content")
    structured = reply.get("structuredContent")
    if (not isinstance(content, list) or len(content) != 1 or not isinstance(content[0], dict)
            or content[0].get("type") != "text" or not isinstance(content[0].get("text"), str)
            or not isinstance(structured, dict)):
        return False
    try:
        return json.loads(content[0]["text"], object_pairs_hook=broker._unique,
                          parse_constant=broker._reject_constant) == structured
    except (ValueError, RecursionError):
        return False


def execute(instance, tool, arguments):
    config = instance.config["plugin"]
    validate_arguments(tool, arguments, instance.root)
    env = dict(instance.git_env, HOME=config["home"], XDG_CONFIG_HOME=config["home"])
    reply, transport = mcp(config["orbit"], instance.root, env, "tools/call",
                           {"name": tool, "arguments": arguments}, instance.deadline_s(),
                           operator=tool == "graph_maintain")
    record = evidence(transport)
    # Keep the complete MCP result, including its text and structured content.
    # The duplicate raw wire is hashed; stderr and cleanup are kept verbatim.
    record["stdout_sha256"] = hashlib.sha256(transport["stdout"]).hexdigest()
    if reply is not None:
        del record["stdout"]
    status = "ok" if complete_reply(reply) and reply["isError"] is False else "failed"
    if transport["stopped"] == "timeout":
        status = "timeout"
    body = {"tool": tool, "status": status, "product_reply": reply, "transport": record}
    if reply is None:
        body["error"] = {"code": "plugin_transport_failed"}
    output = instance.mask_view(broker.canonical(body), "plugin-result")
    if instance.output_size(output) > instance.limits["call_bytes"]:
        # Never trim a product response into apparently valid evidence.
        instance.record({"type": "plugin_output", "tool": tool, "reply": json.loads(output)})
        return "truncated", broker.canonical({"tool": tool, "status": "truncated",
                                               "error": {"code": "call_output_truncated"}})
    return status, output


def recoverable(body):
    transport = body.get("transport", {})
    reply = body.get("product_reply")
    if not (complete_reply(reply) and reply.get("isError") is True
            and transport.get("exit_code") == 0 and transport.get("stopped") is None
            and transport.get("error_type") is None and transport.get("stderr_truncated") is False
            and transport.get("supervision") == {"signals": [], "survivors": []}):
        return False
    error = reply.get("structuredContent", {})
    return (isinstance(error, dict) and error.get("code") in {"index_missing", "invalid_request"}
            and isinstance(error.get("message"), str) and bool(error["message"].strip())
            and type(error.get("retryable")) is bool)


def _install(episode, deadline, *, inventory_only=False):
    """Timed setup using exact commit bytes; returns sealed-treatment ingredients.

    All mutation is in the episode fixture. The uncontained diagnostic path is
    explicit and never eligible for evidence. Strict runs use the same outer
    namespace layout as the provider, with a writable repo only during init.
    """
    import eval_runner as runner
    pin = episode.request["plugin"]
    started = time.monotonic()
    setup_limits = episode.request["setup_limits"]
    deadline = min(deadline, started + setup_limits["wall_ms"] / 1000)
    records = episode.plugin_setup_records
    staged = episode.runtime / "bin"
    staged.mkdir(mode=0o700)
    binary_staging_bytes = 0
    for name in ("orbit", "orbit_graph"):
        size = Path(episode.binaries[name]).stat().st_size
        check(size <= 512 * 1024 * 1024, "executable exceeds the 512 MiB staging bound")
        target = staged / name.replace("_", "-")
        shutil.copyfile(episode.binaries[name], target)
        target.chmod(0o500)
        episode.binaries[name] = str(target)
        binary_staging_bytes += size
    env = {"PATH": str(Path(episode.tool_in("orbit")).parent) + ":/usr/bin:/bin",
           "HOME": episode.state_in("orbit-home"), "XDG_CONFIG_HOME": episode.state_in("orbit-home"),
           "TMPDIR": episode.state_in("tmp"), "GIT_CONFIG_GLOBAL": "/dev/null",
           "GIT_CONFIG_NOSYSTEM": "1", "GIT_CEILING_DIRECTORIES": episode.inside("repo"),
           "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8"}
    prefix = episode.sandbox_prefix(installing=True)

    def remaining():
        return max(0, min(60, deadline - time.monotonic()))

    def run(label, argv, *, host=False, capture=MAX_SETUP_OUTPUT):
        used = sum(len(r.get("stdout", "").encode()) + len(r.get("stderr", "").encode()) for r in records)
        check(used < setup_limits["output_bytes"], "plugin setup output budget exceeded")
        result = broker.run_child(argv if host else prefix + argv,
                                  str(episode.repo), env if not host else source_git_env(episode.state / "home"), remaining(), capture_limit=min(capture, setup_limits["output_bytes"] - used))
        records.append({"role": label, "argv": argv, **evidence(result)})
        if not clean(result):
            raise runner.CapabilityRefusal("plugin_setup_failed", f"{label} failed", records[-1])
        return result["stdout"]

    check(runner.sha256_file(episode.binaries["orbit"]) == pin["orbit_sha256"], "Orbit hash mismatch")
    check(runner.sha256_file(episode.binaries["orbit_graph"]) == pin["backend_sha256"],
          "backend hash mismatch")
    kind = run("verify-plugin-commit", [episode.binaries["git"], "-C", episode.args.plugin_repo,
                                       "cat-file", "-t", pin["commit"]], host=True)
    check(kind == b"commit\n", "plugin pin is not a commit object")
    archive = run("export-plugin", [episode.binaries["git"], "-C", episode.args.plugin_repo,
                  "archive", "--format=tar", pin["commit"], ".orbit-plugin"], host=True)
    source = episode.runtime / "plugin-source"
    source.mkdir(mode=0o700)
    original = {}
    with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
        for member in tar:
            parts = Path(member.name).parts
            check(parts and parts[0] == ".orbit-plugin" and ".." not in parts
                  and not Path(member.name).is_absolute(), "unsafe plugin archive path")
            check(member.isdir() or member.isfile(), "plugin archive has links or special files")
            target = source / member.name
            if member.isdir():
                target.mkdir(mode=0o700, parents=True, exist_ok=True)
            else:
                check(member.size <= MAX_SETUP_OUTPUT, "oversized plugin file")
                data = tar.extractfile(member).read()
                original[member.name] = hashlib.sha256(data).hexdigest()
                target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
                target.write_bytes(data)
                target.chmod(0o700 if member.mode & 0o111 else 0o600)
    plugin = source / ".orbit-plugin"
    manifest_path = plugin / "plugin.yaml"
    original_manifest = manifest_path.read_bytes().decode("utf-8")
    check(original_manifest.count("  origin: orbit\n") == 1, "unsupported local namespace binding")
    bound_manifest, count = re.subn(r"(?m)^    args: \[--allow-unbound-backend\]$",
                                   "    args: [--backend-sha256, " + pin["backend_sha256"] + "]",
                                   original_manifest.replace("  origin: orbit\n", ""))
    check(count == 1, "unsupported backend binding")
    manifest_path.write_text(bound_manifest)
    shutil.copyfile(episode.binaries["orbit_graph"], plugin / "bin/orbit-graph.bin")
    (plugin / "bin/orbit-graph.bin").chmod(0o700)
    skill_root = plugin / "skills/orbit-graph"
    skill = skill_root.joinpath("SKILL.md").read_bytes()
    check(hashlib.sha256(skill).hexdigest() == pin["skill_sha256"], "shipped skill hash mismatch")
    context_files = {str(path.relative_to(skill_root)): path.read_bytes().decode("utf-8")
                     for path in sorted(skill_root.rglob("*")) if path.is_file()}
    check(sum(len(x.encode()) for x in context_files.values()) <= 64 * 1024,
          "shipped skill context exceeds profile bound")
    # workspace init touches .gitignore. Restore precisely its prior bytes; Orbit
    # state is excluded via .git/info/exclude and hidden from the broker.
    ignore = episode.repo / ".gitignore"
    before_ignore = ignore.read_bytes() if ignore.exists() else None
    orbit = episode.tool_in("orbit")
    try:
        run("workspace-init", [orbit, "workspace", "init", "--name", WORKSPACE, "--ship-mode", "local"])
        if before_ignore is None:
            if ignore.exists():
                ignore.unlink()
        else:
            ignore.write_bytes(before_ignore)
        with (episode.repo / ".git/info/exclude").open("a") as stream:
            stream.write(".orbit/\n")
        run("plugin-add", [orbit, "plugin", "add", episode.inside("runtime") + "/plugin-source/.orbit-plugin"])
        run("plugin-enable", [orbit, "plugin", "enable", "graph", "--grant", "fs,orbit_tools"])
        reply, transport = mcp(orbit, str(episode.repo), env, "tools/list", {}, remaining(), prefix=prefix)
        records.append({"role": "tools-list", **evidence(transport)})
        inventory = plugin_inventory(reply)
        if not inventory_only:
            check(digest(inventory) == pin["inventory_sha256"], "installed inventory hash mismatch")
        # An ordinary caller must be denied maintenance by the real host. No index
        # is built during setup: every cold build remains an agent choice.
        denied, transport = mcp(orbit, str(episode.repo), env, "tools/call", {
            "name": "graph_maintain", "arguments": {"operation": "graph_sync"}},
            remaining(), prefix=prefix)
        records.append({"role": "maintenance-authority-negative", **evidence(transport)})
        check(clean(transport) and denied and denied.get("isError") is True
              and denied.get("structuredContent", {}).get("code") == "capability_denied",
              "ordinary host caller was not denied maintenance")
    finally:
        runner.write_private(episode.out / "plugin-setup.json", json.dumps(records, indent=2),
                             episode.redactor)
    check(time.monotonic() <= deadline, "plugin setup wall budget exceeded")
    check(sum(len(r.get("stdout", "").encode()) + len(r.get("stderr", "").encode()) for r in records)
          <= setup_limits["output_bytes"], "plugin setup output budget exceeded")
    binding = ("Installed-plugin evaluation context. The question is unchanged. "
               f"Repository: {episode.inside('repo')}; private Orbit workspace: {WORKSPACE}. "
               "Use the discovered graph_* tool spelling for this local export. "
               "Only graph_sync maintenance is permitted. Broker responses preserve the "
               "complete installed MCP reply in product_reply. Skill files below are shipped "
               "bytes; their headings identify relative reference paths.\n")
    context = binding + "".join("\n--- shipped file: " + name + " ---\n" + text
                                for name, text in context_files.items())
    return {"profile": PROFILES[episode.request["schema_version"]], "pin": pin, "policy": POLICY, "inventory": inventory,
            "inventory_sha256": digest(inventory), "source_files": original,
            "manifest_original": original_manifest, "manifest_installed": bound_manifest,
            "skill_files": context_files, "binding_context": binding,
            "developer_context": context, "developer_context_sha256": hashlib.sha256(context.encode()).hexdigest(),
            "setup": records, "install_ms": int((time.monotonic() - started) * 1000),
            "setup_output_bytes": sum(len(r.get("stdout", "").encode()) + len(r.get("stderr", "").encode())
                                      for r in records), "binary_staging_bytes": binary_staging_bytes, "index_setup": "agent-choice-cold"}


def inspect(args):
    """Capture real installation provenance/inventory for preregistration; no provider."""
    from types import SimpleNamespace
    import eval_runner as runner
    check(re.fullmatch(r"[0-9a-f]{40}", args.plugin_commit), "need a full immutable plugin commit")
    orbit = runner.resolve_binary(args.orbit, "orbit")
    binary = runner.resolve_binary(args.orbit_graph, "orbit-graph")
    # Read a pinned blob with a config-isolated Git command, never a working-tree skill.
    result = broker.run_child([args.git, "-C", args.plugin_repo, "show",
                               args.plugin_commit + ":.orbit-plugin/skills/orbit-graph/SKILL.md"],
                              "/", {"PATH": "/usr/bin:/bin", "HOME": "/nonexistent",
                                    "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": "/dev/null",
                                    "GIT_NO_REPLACE_OBJECTS": "1"}, 30)
    check(clean(result), "cannot read pinned skill")
    pin = {"commit": args.plugin_commit, "orbit_sha256": runner.sha256_file(orbit),
           "backend_sha256": runner.sha256_file(binary), "inventory_sha256": "0" * 64,
           "skill_sha256": hashlib.sha256(result["stdout"]).hexdigest()}
    request = {"schema_version": 2, "arm": "graph", "plugin": pin,
               "setup_limits": {"wall_ms": 60000, "output_bytes": 8 * 1024 * 1024}}
    options = SimpleNamespace(**vars(args))
    options.codex, options.python, options.rg = "/usr/bin/true", "/usr/bin/python3", "/usr/bin/true"
    options.provider_env, options.auth_file, options.allow_code_mode = [], "none", False
    episode = runner.Episode(options, request)
    runner.prepare_layout(episode)
    runner.make_private_dir(episode.repo)
    runner.git_run(args.git, episode.repo, episode.state / "home", "init", "--quiet", "-b", "main")
    (episode.repo / ".git/info/exclude").touch()
    runner.make_private_dir(episode.repo / ".orbit-graph")
    treatment = install(episode, time.monotonic() + 60, inventory_only=True)
    treatment["pin"]["inventory_sha256"] = treatment["inventory_sha256"]
    runner.seal(treatment, "treatment_sha256")
    runner.write_private(episode.out / "plugin-treatment.json", json.dumps(treatment, indent=2), episode.redactor)
    return {"profile": PROFILE, "provider_started": False, "pin": treatment["pin"],
            "inventory": treatment["inventory"], "out": str(episode.out),
            "contained": episode.contained}


def verify_source(episode, head_entries, base_entries, deadline):
    """Verify each selected source byte against immutable Git blobs, without exporting truth."""
    import eval_runner as runner
    repo = episode.args.source_repo
    check(repo, "profile 2 requires --source-repo for immutable source verification")
    result = {}
    for role, entries in (("head", head_entries), ("base", base_entries)):
        commit = episode.request["source_commits"][role]
        kind = broker.run_child([episode.binaries["git"], "-C", repo, "cat-file", "-t", commit],
                                str(episode.state), source_git_env(episode.state / "home"),
                                max(0, deadline - time.monotonic()), capture_limit=1024)
        check(clean(kind) and kind["stdout"] == b"commit\n", "source pin is not a commit object")
        transport = broker.run_child([episode.binaries["git"], "-C", repo, "ls-tree", "-rz",
                                      "--full-tree", commit], str(episode.state),
                                     source_git_env(episode.state / "home"),
                                     max(0, deadline - time.monotonic()), capture_limit=16 * 1024 * 1024)
        check(clean(transport), "cannot verify pinned source tree")
        objects = {}
        for record in transport["stdout"].split(b"\0"):
            if not record:
                continue
            header, name = record.split(b"\t", 1)
            mode, kind, oid = header.decode().split()
            objects[name.decode()] = (mode, kind, oid)
        selected = {}
        for name, path in entries:
            check(source_path_allowed(name), f"source export contains a non-source path: {name}")
            data, _ = runner.read_text_file(path, name, role)
            oid = hashlib.sha1(b"blob " + str(len(data)).encode() + b"\0" + data).hexdigest()
            obj = objects.get(name)
            check(obj is not None and obj[0] in ("100644", "100755") and obj[1:] == ("blob", oid),
                  f"{role} source differs from pinned commit: {name}")
            selected[name] = oid
        result[role] = {"commit": commit, "selected_blobs": selected}
    return result


def resource_bounds():
    """Observe finite enclosing cgroup-v2 ceilings; never create or alter a cgroup."""
    try:
        lines = Path("/proc/self/cgroup").read_text().splitlines()
        relative = next(line[3:] for line in lines if line.startswith("0::"))
        root = Path("/sys/fs/cgroup")
        current = root / relative.lstrip("/")
        check(current.resolve().is_relative_to(root.resolve()), "invalid cgroup path")
        values = {"memory.max": [], "pids.max": []}
        while True:
            for name in values:
                try:
                    value = (current / name).read_text().strip()
                except FileNotFoundError:
                    continue  # the hierarchy root has no controller ceiling files
                if value != "max":
                    values[name].append(int(value))
            if current == root:
                break
            current = current.parent
        return {name: min(limits) if limits else None for name, limits in values.items()}
    except (OSError, ValueError, StopIteration):
        return {"memory.max": None, "pids.max": None}


def install(episode, deadline, *, inventory_only=False):
    """Preserve completed setup evidence even when validation refuses early."""
    import eval_runner as runner
    episode.plugin_setup_records = []
    try:
        return _install(episode, deadline, inventory_only=inventory_only)
    finally:
        path = episode.out / "plugin-setup.json"
        if not path.exists():
            runner.write_private(path, json.dumps(episode.plugin_setup_records, indent=2), episode.redactor)


def source_git_env(home):
    import eval_runner as runner
    return dict(runner.git_env(home), GIT_NO_REPLACE_OBJECTS="1", GIT_OPTIONAL_LOCKS="0")


def source_path_allowed(name):
    """Fixed source-only boundary, independent of question and treatment arm."""
    parts = Path(name).parts
    excluded = {"docs", "skills", "instructions", "evaluation", "evals", "truth", "answers",
                "agent-eval", "target", "generated", "vendor", "node_modules", "secrets", "credentials"}
    if (not parts or Path(name).is_absolute() or ".." in parts or "\\" in name
            or any(part.startswith(".") or part.lower() in excluded for part in parts)):
        return False
    if name in {"Cargo.toml", "Cargo.lock", "pyproject.toml"}:
        return True
    return parts[0] in {"src", "crates", "tests"} and (
        parts[-1] == "Cargo.toml" or Path(name).suffix in {
            ".rs", ".py", ".js", ".jsx", ".ts", ".tsx", ".go", ".rb", ".java", ".kt",
            ".c", ".h", ".cc", ".cpp", ".hpp", ".cs"})
