#!/usr/bin/env python3
"""Deterministic stand-in for the Codex CLI in agent-eval tests.

It never contacts a provider. `exec` starts the broker named by the
`-c mcp_servers.eval_broker.*` overrides, speaks real MCP to it, follows a
JSON script named by $FAKE_CODEX_SCRIPT (or inline in $FAKE_CODEX_SCRIPT_JSON)
and prints codex-shaped JSONL events. Like codex 0.160.0 under
approval_policy="never", it refuses a tool call that is not approved before the
broker sees it, and `mcp get` validates approval modes and reads back
enabled_tools and default_tools_approval_mode.
"""
import ctypes
import json
import os
import signal
import subprocess
import sys
import time

sys.dont_write_bytecode = True

FEATURES = {
    "shell_tool": True, "apps": True, "plugins": True, "remote_plugin": True,
    "multi_agent": True, "multi_agent_v2": False, "browser_use": True,
    "browser_use_external": True, "computer_use": True, "view_image": True,
    "image_generation": True, "in_app_browser": True, "code_mode_host": True,
    "tool_suggest": True, "hooks": True, "memories": False,
    "skill_mcp_dependency_install": True, "worktrees": True, "in_app_local_automation": True,
    "unified_exec": True, "unified_exec_tty": True, "shell_snapshot": True,
    "write_stdin_approval": True, "sleep_tool": True,
}
VALUE_FLAGS = {"-c", "--disable", "--enable", "-C", "-s", "-m", "--color",
               "--output-last-message"}
SERVER = "mcp_servers.eval_broker"
APPROVAL_MODES = ("auto", "prompt", "writes", "approve")
APPROVAL_DENIED = "MCP tool call requires approval, but approval policy is never"


def parse(argv):
    options = {"config": {}, "disable": [], "enable": [], "positional": []}
    index = 0
    while index < len(argv):
        arg = argv[index]
        if arg in VALUE_FLAGS:
            value = argv[index + 1]
            index += 2
            if arg == "-c":
                key, _, raw = value.partition("=")
                try:
                    options["config"][key] = json.loads(raw)
                except ValueError:
                    options["config"][key] = raw
            elif arg == "--disable":
                options["disable"].append(value)
            elif arg == "--enable":
                options["enable"].append(value)
            else:
                options[arg] = value
            continue
        if arg.startswith("-") and arg != "-":
            options[arg] = True
        else:
            options["positional"].append(arg)
        index += 1
    return options


def load_script():
    if os.environ.get("FAKE_CODEX_SCRIPT_JSON"):  # for sandboxes that hide host files
        return json.loads(os.environ["FAKE_CODEX_SCRIPT_JSON"])
    path = os.environ.get("FAKE_CODEX_SCRIPT")
    if not path:
        return {}
    with open(path) as stream:
        return json.load(stream)


def approved(config, tool, annotations):
    """The codex 0.160.0 gate under approval_policy=never, as observed against a mock model.

    `approve` passes; `auto` (the default) passes only a tool annotated
    readOnlyHint; `prompt` refuses whatever the annotations say.
    """
    if config.get("approval_policy") != "never":
        return True
    mode = config.get(f"{SERVER}.tools.{tool}.approval_mode",
                      config.get(f"{SERVER}.default_tools_approval_mode", "auto"))
    return mode == "approve" or (mode == "auto" and
                                 annotations.get(tool, {}).get("readOnlyHint") is True)


def invalid_approval_mode(config, script):
    """The first approval-mode override codex would refuse at config load, if any."""
    if script.get("unvalidated_tool_approvals"):
        return None
    for key, value in config.items():
        if key.startswith(SERVER + ".") and key.endswith("approval_mode") and \
                value not in APPROVAL_MODES:
            return key, value
    return None


def emit(event):
    sys.stdout.write(json.dumps(event) + "\n")
    sys.stdout.flush()


class Client:
    def __init__(self, command, args):
        self.process = subprocess.Popen([command, *args], stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE)
        self.next_id = 0

    def send_raw(self, line):
        self.process.stdin.write(line.encode() + b"\n")
        self.process.stdin.flush()

    def read(self):
        line = self.process.stdout.readline()
        return json.loads(line) if line else None

    def request(self, method, params):
        self.next_id += 1
        self.send_raw(json.dumps({"jsonrpc": "2.0", "id": self.next_id, "method": method,
                                  "params": params}))
        return self.read()

    def close(self):
        self.process.stdin.close()
        self.process.wait(timeout=30)


def run_exec(options, script):
    marker = script.get("exec_marker")
    if marker:
        with open(marker, "w") as stream:
            stream.write("exec started\n")
    sys.stdin.read()
    server = "mcp_servers.eval_broker"
    client = Client(options["config"][f"{server}.command"], options["config"][f"{server}.args"])
    emit({"type": "thread.started", "thread_id": "fake-thread"})
    emit({"type": "turn.started"})
    client.request("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                                  "clientInfo": {"name": "fake-codex", "version": "0"}})
    client.send_raw(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}))
    listed = client.request("tools/list", {})
    annotations = {tool["name"]: tool.get("annotations", {})
                   for tool in listed["result"]["tools"]}
    emit({"type": "item.completed", "item": {"id": "tools", "type": "reasoning",
                                             "text": json.dumps(listed["result"]["tools"][0]
                                                                ["name"])}})
    steps = script.get("steps", [])
    if script.get("requires_code_mode") and "code_mode_host" in options["disable"]:
        # What codex 0.160.0 did live for a code-mode model with the host disabled.
        emit({"type": "item.completed", "item": {
            "id": "item_0", "type": "error",
            "message": "Code Mode is unavailable because code-mode host is disabled. Code mode "
                       "will fail closed; enable `features.code_mode_host` and install "
                       "`codex-code-mode-host`."}})
        sys.stderr.write("ERROR codex_core::tools::router: error=code-mode host is disabled\n")
        steps = [{"final": json.dumps({"items": [], "abstain": True, "evidence": [],
                                       "reason": "the tool execution host is disabled"})}]
    item = 0
    for step in steps:
        item += 1
        if "call" in step:
            started = {"id": f"item_{item}", "type": "mcp_tool_call", "server": "eval_broker",
                       "tool": step["call"], "arguments": step.get("arguments", {}),
                       "status": "in_progress"}
            emit({"type": "item.started", "item": started})
            if script.get("deny_tool_approvals") or \
                    not approved(options["config"], step["call"], annotations):
                emit({"type": "item.completed",
                      "item": dict(started, result=None, error={"message": APPROVAL_DENIED},
                                   status="failed")})
                continue
            reply = client.request("tools/call", {"name": step["call"],
                                                  "arguments": step.get("arguments", {})})
            result = reply.get("result", reply.get("error"))
            emit({"type": "item.completed",
                  "item": dict(started, result=result,
                               status="failed" if result.get("isError") else "completed")})
        elif "raw" in step:
            client.send_raw(step["raw"])
            if step.get("expect_reply"):
                client.read()
        elif "emit" in step:
            emit(step["emit"])
        elif "emit_raw" in step:
            sys.stdout.write(step["emit_raw"] + "\n")
            sys.stdout.flush()
        elif "sleep" in step:
            time.sleep(step["sleep"])
        elif "spawn_sleeper" in step:
            sleeper = subprocess.Popen(["/usr/bin/sleep", "300"])
            with open(step["spawn_sleeper"], "w") as stream:
                stream.write(str(sleeper.pid))
        elif "echo_env" in step:
            value = os.environ.get(step["echo_env"], "")
            emit({"type": "item.completed", "item": {"id": f"item_{item}",
                                                     "type": "agent_message", "text": value}})
            sys.stderr.write(f"debug token {value}\n")
        elif "spam_stdout" in step:
            chunk = b"x" * 65536
            for _ in range(step["spam_stdout"] // len(chunk) + 1):
                sys.stdout.buffer.write(chunk)
            sys.stdout.flush()
        elif "unbrokered" in step:
            emit({"type": "item.completed", "item": {"id": f"item_{item}",
                                                     "type": step["unbrokered"],
                                                     "server": step.get("server"),
                                                     "tool": step.get("tool"),
                                                     "status": "completed"}})
        elif "final" in step:
            last = options.get("--output-last-message")
            if last and not step.get("jsonl_only"):
                with open(last, "w") as stream:
                    stream.write(step["final"])
            emit({"type": "item.completed", "item": {"id": f"item_{item}",
                                                     "type": "agent_message",
                                                     "text": step["final"]}})
    if not script.get("no_turn_completed"):
        completed = {"type": "turn.completed"}
        if script.get("usage", "default") == "default":
            completed["usage"] = {"input_tokens": 1200, "cached_input_tokens": 100,
                                  "output_tokens": 80, "reasoning_output_tokens": 20}
        elif script.get("usage") is not None:
            completed["usage"] = script["usage"]
        emit(completed)
    if script.get("turn_failed"):
        emit({"type": "turn.failed", "error": {"message": script["turn_failed"]}})
    client.close()
    return script.get("exit_code", 0)


def mcp_get(options, script):
    names = options["positional"]
    config = options["config"]
    if names != ["eval_broker"] or f"{SERVER}.command" not in config:
        print(f"Error: No MCP server named '{' '.join(names)}' found.", file=sys.stderr)
        return 1
    enabled_tools = None if script.get("drop_enabled_tools") else \
        config.get(f"{SERVER}.enabled_tools")
    if options.get("--json"):
        print(json.dumps({"name": "eval_broker", "enabled": True, "disabled_reason": None,
                          "transport": {"type": "stdio", "command": config[f"{SERVER}.command"],
                                        "args": config[f"{SERVER}.args"], "env": None,
                                        "env_vars": [], "cwd": None},
                          "enabled_tools": enabled_tools, "disabled_tools": None,
                          "startup_timeout_sec": None, "tool_timeout_sec": None}, indent=2))
        return 0
    print("eval_broker\n  enabled: true")
    if enabled_tools is not None:
        print(f"  enabled_tools: {', '.join(enabled_tools)}")
    print(f"  transport: stdio\n  command: {config[f'{SERVER}.command']}")
    mode = config.get(f"{SERVER}.default_tools_approval_mode")
    if mode is not None and not script.get("drop_default_approval"):
        print(f"  default_tools_approval_mode: {mode}")
    print("  remove: codex mcp remove eval_broker")
    return 0


def main():
    argv = sys.argv[1:]
    script = load_script()
    # Die with the runner, as a real provider does inside bwrap --die-with-parent.
    ctypes.CDLL(None).prctl(1, signal.SIGKILL, 0, 0, 0)  # PR_SET_PDEATHSIG
    if argv[:1] == ["--version"]:
        print("codex-cli 0.0.0-fake")
        return 0
    options = parse(argv[2:] if argv[:1] in (["features"], ["mcp"]) else argv[1:])
    invalid = invalid_approval_mode(options["config"], script)
    if invalid:
        print("Error: failed to load bootstrap configuration\n\nCaused by:\n"
              f"    unknown variant `{invalid[1]}`, expected one of "
              f"{', '.join(f'`{mode}`' for mode in APPROVAL_MODES)}\n    in `{invalid[0]}`",
              file=sys.stderr)
        return 1
    if argv[:2] == ["features", "list"]:
        for name, default in FEATURES.items():
            if name in script.get("drop_features", []):
                continue
            enabled = default
            if name in options["disable"] and name not in script.get("sticky", []):
                enabled = False
            print(f"{name:<40} stable             {'true' if enabled else 'false'}")
        return 0
    if argv[:2] == ["mcp", "list"]:
        servers = []
        prefix = "mcp_servers."
        names = sorted({key[len(prefix):].split(".")[0] for key in options["config"]
                        if key.startswith(prefix)})
        for name in names:
            servers.append({"name": name, "enabled": True, "disabled_reason": None,
                            "transport": {"type": "stdio",
                                          "command": options["config"][f"{prefix}{name}.command"],
                                          "args": options["config"][f"{prefix}{name}.args"],
                                          "env": None, "env_vars": [], "cwd": None}})
        if script.get("extra_mcp"):
            servers.append({"name": "user_server", "enabled": True,
                            "transport": {"type": "stdio", "command": "x", "args": []}})
        print(json.dumps(servers))
        return 0
    if argv[:2] == ["mcp", "get"]:
        return mcp_get(options, script)
    if argv[:1] == ["exec"]:
        return run_exec(options, script)
    print(f"fake codex: unsupported {argv[:2]}", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main())
