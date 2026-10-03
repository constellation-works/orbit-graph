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
import select
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
        self.config_path = args[args.index("--config") + 1]
        self.process = subprocess.Popen([command, *args], stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE)
        self.next_id = 0

    def send_raw(self, line):
        self.process.stdin.write(line.encode() + b"\n")
        self.process.stdin.flush()

    def read(self):
        line = self.process.stdout.readline()
        return json.loads(line) if line else None

    def send_request(self, method, params):
        self.next_id += 1
        self.send_raw(json.dumps({"jsonrpc": "2.0", "id": self.next_id, "method": method,
                                  "params": params}))
        return self.next_id

    def request(self, method, params):
        self.send_request(method, params)
        return self.read()

    def close(self, shutdown=None):
        if shutdown in ("eof_then_sigterm", "eof_then_sigterm_stalled", "eof_with_pending_request"):
            with open(self.config_path) as stream:
                config = json.load(stream)
            with open(config["log_path"]) as stream:
                identity = next(json.loads(line)["identity"] for line in stream
                                if json.loads(line).get("type") == "start")
            worker = identity["pid"]
            fd = os.pidfd_open(worker)
            try:
                signal.pidfd_send_signal(fd, signal.SIGSTOP)
                deadline = time.monotonic() + 10
                while True:
                    with open(f"/proc/{worker}/stat") as stream:
                        fields = stream.read().rsplit(")", 1)[1].split()
                    if int(fields[19]) != identity["start_ticks"]:
                        raise RuntimeError("worker identity changed")
                    if fields[0] == "T":
                        break
                    if time.monotonic() >= deadline:
                        raise TimeoutError("worker did not stop")
                if shutdown == "eof_with_pending_request":
                    self.send_request("ping", {})
                self.process.stdin.close()
                os.kill(self.process.pid, signal.SIGTERM)
                # Acknowledge the supervisor's observation before the worker can
                # consume EOF or exit. Old supervisors instead forward TERM; its
                # pending kernel bit provides the baseline acknowledgement.
                while True:
                    with open(config["log_path"]) as stream:
                        observed = any(json.loads(line).get("type") == "supervisor_signal"
                                       for line in stream if line.endswith("\n"))
                    with open(f"/proc/{worker}/status") as stream:
                        pending = [int(line.split()[1], 16) for line in stream
                                   if line.startswith(("SigPnd:", "ShdPnd:"))]
                    if ((observed and shutdown != "eof_then_sigterm_stalled")
                            or any(mask & (1 << (signal.SIGTERM - 1)) for mask in pending)):
                        break
                    if time.monotonic() >= deadline:
                        raise TimeoutError("supervisor did not acknowledge TERM")
            finally:
                try:
                    signal.pidfd_send_signal(fd, signal.SIGCONT)
                finally:
                    os.close(fd)
            self.process.wait(timeout=30)
            return
        if shutdown in ("exited_then_sigterm", "exited_then_sigint"):
            # Freeze only the supervisor, not the worker. Kernel acknowledgements
            # establish EOF -> worker exit -> TERM -> supervisor resume without
            # timing sleeps or changes to the broker under test. This models a
            # possible client teardown ordering, not a claim about real Codex.
            with open(self.config_path) as stream:
                config = json.load(stream)
            with open(config["log_path"]) as stream:
                worker = next(json.loads(line)["identity"]["pid"] for line in stream
                              if json.loads(line).get("type") == "start")
            fd = os.pidfd_open(worker)
            try:
                os.kill(self.process.pid, signal.SIGSTOP)
                deadline = time.monotonic() + 10
                while os.waitid(os.P_PID, self.process.pid,
                                os.WSTOPPED | os.WNOHANG | os.WNOWAIT) is None:
                    if time.monotonic() >= deadline:
                        raise TimeoutError("supervisor did not stop")
                self.process.stdin.close()
                if not select.select([fd], [], [], 10)[0]:
                    raise TimeoutError("worker did not exit after EOF")
                os.kill(self.process.pid, signal.SIGTERM if shutdown == "exited_then_sigterm"
                        else signal.SIGINT)
            finally:
                os.close(fd)
                os.kill(self.process.pid, signal.SIGCONT)
            self.process.wait(timeout=30)
            return
        self.process.stdin.close()
        self.process.wait(timeout=30)


def run_calls(client, options, script, annotations, calls, first_item, order):
    """Keep several MCP requests in flight; report completions in a chosen order.

    The broker executes serially, while Codex can publish their completions in
    another order. All starts and requests precede reads; no sleeps or races.
    Report overrides deliberately corrupt telemetry without changing the calls.
    """
    pending, completed = [], []
    for index, step in enumerate(calls):
        started = {"id": f"item_{first_item + index}", "type": "mcp_tool_call",
                   "server": "eval_broker", "tool": step["call"],
                   "arguments": step.get("arguments", {}), "status": "in_progress"}
        if not step.get("omit_started"):
            emit({"type": "item.started", "item": dict(started, **step.get("started", {}))})
        if script.get("deny_tool_approvals") or \
                not approved(options["config"], step["call"], annotations):
            pending.append(None)
            completed.append(dict(started, result=None, error={"message": APPROVAL_DENIED},
                                  status="failed"))
        else:
            pending.append(client.send_request("tools/call", {"name": step["call"],
                                                               "arguments": started["arguments"]}))
            completed.append(started)
    for index, request_id in enumerate(pending):
        if request_id is None:
            continue
        reply = client.read()
        if not reply or reply.get("id") != request_id:
            raise RuntimeError(f"missing broker reply for request {request_id}")
        result = reply.get("result", reply.get("error"))
        # Real Codex JSONL omits MCP isError and includes null structured_content.
        completed[index] = dict(completed[index],
                                result={"content": result["content"], "structured_content": None},
                                error=None,
                                status="failed" if result.get("isError") else "completed")
    for index in order:
        step = calls[index]
        reported = dict(completed[index], **step.get("completed", {}))
        if "result_from" in step:
            reported["result"] = completed[step["result_from"]]["result"]
        for field in step.get("omit_fields", []):
            reported.pop(field, None)
        if not step.get("omit_completed"):
            event = {"type": "item.completed", "item": reported}
            emit(event)
            if step.get("duplicate_completed"):
                emit(event)


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
        if "call" in step or "parallel" in step:
            calls = step.get("parallel", [step])
            run_calls(client, options, script, annotations, calls, item,
                      step.get("completion_order", list(range(len(calls)))))
            item += len(calls) - 1
        elif "raw" in step:
            client.send_raw(step["raw"])
            if step.get("expect_reply"):
                client.read()
        elif "emit" in step:
            emit(step["emit"])
        elif "emit_raw" in step:
            sys.stdout.write(step["emit_raw"] + ("\n" if step.get("newline", True) else ""))
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
    client.close(script.get("shutdown"))
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
