#!/usr/bin/env python3
"""Allowlisted stdio MCP tool broker for one agent-navigation episode.

The runner stages this broker and its shared reply-provenance module in each
episode's runtime directory. The provider starts it as its only MCP server. It exposes the arm's tools and
nothing else: bounded reads, an rg search with a fixed argument vector,
structured read-only Git history, and (graph arm only) orbit-graph queries.
Every value an agent supplies is validated data; nothing it supplies, and
nothing a file contains, becomes an option, a command or a tool instruction.

`--launch` is the in-sandbox entry point: it records the containment probe and
then replaces itself with the provider command.

Standard library only; Linux. No network access.
"""
import argparse
import ctypes
import json
import os
import re
import select
import selectors
import signal
import stat
import subprocess
import sys
import time

sys.dont_write_bytecode = True

import reply_provenance as replies

BROKER_NAME = "agent-eval-broker"
BROKER_VERSION = "4"
EXITED_LIFECYCLE_CONTRACT = "eof-exited-at-term-observation-v1"
LIFECYCLE_CONTRACT = "eof-idle-at-term-observation-v2"
MAX_LIFECYCLE_LOG = 64 * 1024 * 1024
COMMON_TOOLS = ("read", "rg", "git")
GRAPH_TOOLS = ("graph_sync", "search", "show", "refs", "callees", "impact", "changes")
ARM_TOOLS = {"baseline": COMMON_TOOLS, "graph": COMMON_TOOLS + GRAPH_TOOLS}
PROTOCOL_VERSIONS = ("2025-06-18", "2025-03-26", "2024-11-05")
# One JSON-RPC line from the client; larger lines are discarded unparsed.
MAX_MESSAGE_BYTES = 256 * 1024
# Captured stdout of one rg/git/orbit-graph child; beyond it the child is
# killed and the call is reported `truncated`.
MAX_CHILD_CAPTURE = 4 * 1024 * 1024
MAX_STDERR_CAPTURE = 16 * 1024
MAX_READ_FILE_BYTES = 32 * 1024 * 1024
MAX_LINE_CHARS = 1000
CALL_TIMEOUT_CAP_S = 60.0
KILL_GRACE_S = 2.0
# Refused tool names (for example a graph call in the baseline arm) are
# answered and logged, but a run of them ends the episode.
MAX_REFUSALS = 20
MAX_PROTOCOL_ERRORS = 200
HIDDEN_NAMES = frozenset({".git", ".orbit-graph"})
SELECTOR_KINDS = ("symbol", "file", "dir", "module", "command")
REVISION = re.compile(r"\A(?:HEAD|[0-9a-f]{7,64})(?:\^[0-9]?|~[0-9]{1,4})*\Z")
GLOB = re.compile(r"\A[A-Za-z0-9_.*?/\[\]{},!+-]{1,200}\Z")
LANGUAGE = re.compile(r"\A[a-z0-9_+-]{1,32}\Z")
CONFIDENCE = ("exact", "import", "same_module", "fuzzy")
REF_KINDS = ("call", "type", "use", "trait_bound", "impl", "extends", "implements")
DIRECTIONS = ("inbound", "outbound", "both")
SEARCH_KINDS = ("symbol", "string", "config")
GIT_OPS = ("log", "show", "diff", "file_at")
PR_SET_PDEATHSIG = 1
PROBE_REFUSED_EXIT = 86


class Refusal(Exception):
    """A validated, agent-visible refusal with a stable code."""

    def __init__(self, code, message):
        super().__init__(message)
        self.code = code
        self.message = message


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False,
                      allow_nan=False)


def now_ms():
    return time.monotonic_ns() // 1_000_000


def set_parent_death_signal():
    """Kill on launching-thread exit; use only beneath our stable supervisor thread."""
    try:
        libc = ctypes.CDLL(None, use_errno=True)
        return libc.prctl(PR_SET_PDEATHSIG, signal.SIGKILL, 0, 0, 0) == 0
    except (OSError, AttributeError):
        return False


def start_identity():
    """PID, kernel start time and PID namespace: a PID is never trusted alone."""
    with open("/proc/self/stat", "rb") as stream:
        fields = stream.read().rsplit(b")", 1)[1].split()
    return {"pid": os.getpid(), "start_ticks": int(fields[19]),
            "pid_namespace": os.readlink("/proc/self/ns/pid")}


# --------------------------------------------------------------------------
# Argument validation. Every tool declares its fields; unknown fields refuse.


def _string(maximum, pattern=None, option_like=False):
    def check(name, value):
        if not isinstance(value, str) or not value:
            raise Refusal("invalid_argument", f"{name} must be a non-empty string")
        if len(value.encode()) > maximum:
            raise Refusal("invalid_argument", f"{name} exceeds {maximum} bytes")
        if any(ord(ch) < 32 or ord(ch) == 127 for ch in value):
            raise Refusal("invalid_argument", f"{name} contains control characters")
        if not option_like and value.startswith("-"):
            raise Refusal("option_refused", f"{name} must not start with '-'")
        if pattern is not None and not pattern.match(value):
            raise Refusal("invalid_argument", f"{name} has an unsupported form")
        return value
    return check


def _integer(low, high):
    def check(name, value):
        if type(value) is not int or not low <= value <= high:
            raise Refusal("invalid_argument", f"{name} must be an integer in {low}..{high}")
        return value
    check.json_schema = {"type": "integer", "minimum": low, "maximum": high}
    return check


def _boolean(name, value):
    if type(value) is not bool:
        raise Refusal("invalid_argument", f"{name} must be a boolean")
    return value


def _choice(options):
    def check(name, value):
        if value not in options:
            raise Refusal("invalid_argument", f"{name} must be one of {', '.join(options)}")
        return value
    check.json_schema = {"type": "string", "enum": list(options)}
    return check


def _revision(name, value):
    _string(80)(name, value)
    if not REVISION.match(value):
        raise Refusal("revision_refused",
                      f"{name} must be HEAD or a hex commit id, optionally with ^/~N suffixes")
    return value


def _glob(name, value):
    _string(200, GLOB)(name, value)
    if ".." in value or value.lstrip("!").startswith("/"):
        raise Refusal("invalid_argument", f"{name} must stay inside the repository")
    return value


def _rel_path(name, value):
    return "/".join(split_path(value, name)) or "."


def _selector(name, value):
    _string(512)(name, value)
    kind, separator, rest = value.partition(":")
    if not separator or kind not in SELECTOR_KINDS or not rest:
        raise Refusal("selector_refused",
                      f"{name} must start with one of {', '.join(k + ':' for k in SELECTOR_KINDS)}")
    if kind in ("symbol", "file", "dir"):
        path = rest.split("#", 1)[0]
        split_path(path, name)
    return value


def _selector_list(name, value):
    if not isinstance(value, list) or not 1 <= len(value) <= 20:
        raise Refusal("invalid_argument", f"{name} must be a list of 1..20 selectors")
    return [_selector(f"{name}[{index}]", item) for index, item in enumerate(value)]


_boolean.json_schema = {"type": "boolean"}
_selector_list.json_schema = {"type": "array", "items": {"type": "string"},
                              "minItems": 1, "maxItems": 20}


OFFSET = _integer(0, 1 << 40)

SCHEMAS = {
    "read": {"path": _rel_path, "start_line": _integer(1, 10_000_000),
             "max_lines": _integer(1, 2000)},
    "rg": {"pattern": _string(1024, option_like=True), "path": _rel_path,
           "fixed_strings": _boolean, "ignore_case": _boolean, "glob": _glob,
           "max_matches": _integer(1, 500), "offset": OFFSET},
    "git": {"op": _choice(GIT_OPS), "rev": _revision, "base": _revision, "head": _revision,
            "path": _rel_path, "max_count": _integer(1, 200), "stat": _boolean,
            "offset": OFFSET},
    "graph_sync": {"full": _boolean},
    "search": {"query": _string(256, option_like=True), "kind": _choice(SEARCH_KINDS),
               "language": _string(32, LANGUAGE), "limit": _integer(1, 200), "offset": OFFSET},
    "show": {"selector": _selector, "max_bytes": _integer(0, 65536), "offset": OFFSET},
    "refs": {"selector": _selector, "confidence": _choice(CONFIDENCE),
             "kind": _choice(REF_KINDS), "offset": OFFSET},
    "callees": {"selector": _selector, "include_unresolved": _boolean, "offset": OFFSET},
    "impact": {"selector": _selector, "depth": _integer(0, 10),
               "confidence": _choice(CONFIDENCE), "direction": _choice(DIRECTIONS),
               "offset": OFFSET},
    "changes": {"base": _revision, "head": _revision, "symbol": _selector_list,
                "confidence": _choice(CONFIDENCE), "language": _string(32, LANGUAGE),
                "scope": _rel_path, "offset": OFFSET},
}
REQUIRED = {"rg": ("pattern",), "git": ("op",), "search": ("query",), "show": ("selector",),
            "refs": ("selector",), "callees": ("selector",), "impact": ("selector",)}

DESCRIPTIONS = {
    "read": "Read a UTF-8 file as numbered lines, or list a directory, inside the episode "
            "repository. Paths are repository-relative; .git and .orbit-graph are hidden. "
            "Page with start_line.",
    "rg": "Search file contents with ripgrep (regex unless fixed_strings). Returns "
          "path:line:text matches, paged with offset.",
    "git": "Read-only Git history of the repository: op=log (rev, path, max_count), "
           "show (rev, path, stat), diff (base, head, path, stat) or file_at (rev, path). "
           "Revisions are HEAD or hex commit ids with optional ^ or ~N suffixes.",
    "graph_sync": "Build the code-graph index for this repository. Call once before other "
                  "graph tools; its time counts toward the episode.",
    "search": "Search indexed symbols, strings and configuration keys.",
    "show": "Show source and metadata for a graph selector such as "
            "symbol:src/lib.rs#run:function or file:src/lib.rs.",
    "refs": "List inbound references to a symbol selector.",
    "callees": "List outbound calls from a symbol selector.",
    "impact": "Traverse the bounded graph around a selector.",
    "changes": "Changed symbols between two revisions (default: the snapshot base..head) "
               "with callers, entry points and candidate tests.",
}

# MCP tool annotations: client hints, never authority. Source and Git stay
# immutable. graph_sync builds/replaces the private index; changes writes
# disposable snapshot indexes in private TMPDIR even with --no-cache.
READ_ONLY = {"readOnlyHint": True, "destructiveHint": False, "idempotentHint": True,
             "openWorldHint": False}
ANNOTATIONS = dict({tool: READ_ONLY for tool in COMMON_TOOLS + GRAPH_TOOLS},
                   graph_sync={"readOnlyHint": False, "destructiveHint": True,
                               "idempotentHint": True, "openWorldHint": False},
                   changes={"readOnlyHint": False, "destructiveHint": False,
                            "idempotentHint": True, "openWorldHint": False})


def input_schema(tool):
    properties = {field: dict(getattr(check, "json_schema", {"type": "string"}))
                  for field, check in SCHEMAS[tool].items()}
    return {"type": "object", "properties": properties,
            "required": list(REQUIRED.get(tool, ())), "additionalProperties": False}


def validate_arguments(tool, arguments):
    if not isinstance(arguments, dict):
        raise Refusal("invalid_argument", "arguments must be an object")
    schema = SCHEMAS[tool]
    unknown = sorted(set(arguments) - set(schema))
    if unknown:
        raise Refusal("unknown_field", f"unknown field(s) for {tool}: {', '.join(unknown)}")
    for field in REQUIRED.get(tool, ()):
        if field not in arguments:
            raise Refusal("missing_field", f"{tool} requires {field}")
    return {field: schema[field](field, value) for field, value in arguments.items()}


def split_path(value, name="path"):
    """Lexically validate a repository-relative path; return its components."""
    if not isinstance(value, str):
        raise Refusal("invalid_argument", f"{name} must be a string")
    if len(value.encode()) > 1024:
        raise Refusal("invalid_argument", f"{name} exceeds 1024 bytes")
    if any(ord(ch) < 32 or ord(ch) == 127 for ch in value) or "\\" in value:
        raise Refusal("path_refused", f"{name} contains control characters or backslashes")
    if value.startswith("/") or value.startswith("~"):
        raise Refusal("path_refused", f"{name} must be relative to the repository root")
    parts = [part for part in value.split("/") if part not in ("", ".")]
    for part in parts:
        if part == "..":
            raise Refusal("path_refused", f"{name} must not contain '..'")
        if part in HIDDEN_NAMES:
            raise Refusal("path_refused", f"{name} must not reach {part}")
    if parts and parts[0].startswith("-"):
        raise Refusal("option_refused", f"{name} must not start with '-'")
    return parts


# --------------------------------------------------------------------------
# Bounded child processes (own process group, cleared environment).


def _preexec():
    set_parent_death_signal()


def run_child(argv, cwd, env, timeout_s, capture_limit=MAX_CHILD_CAPTURE, line_limit=None,
              passthrough=False, parent_fd=None, cancelled=lambda: False, grace_s=KILL_GRACE_S,
              input_bytes=None, on_spawn=lambda process: None):
    """Bounded capture and group cleanup, including unexpected supervision exceptions.

    passthrough is only for the broker worker: MCP pipes stay directly connected
    to the provider while this process drains the worker's diagnostic stream.
    """
    started = time.monotonic()
    out, err = bytearray(), bytearray()
    stopped, error_type, stderr_truncated = None, None, False
    process, selector = None, None
    terminate = True
    supervision = {"signals": [], "survivors": []}
    stderr_eof = False
    try:
        if input_bytes is not None and (passthrough or len(input_bytes) > MAX_MESSAGE_BYTES):
            raise ValueError("invalid bounded child input")
        process = subprocess.Popen(argv, cwd=cwd, env=env,
                                   stdin=subprocess.PIPE if input_bytes is not None else
                                   None if passthrough else subprocess.DEVNULL,
                                   stdout=None if passthrough else subprocess.PIPE,
                                   stderr=subprocess.PIPE,
                                   start_new_session=True, preexec_fn=_preexec)
        on_spawn(process)
        selector = selectors.DefaultSelector()
        pending_input = memoryview(input_bytes or b"")
        if process.stdin is not None:
            os.set_blocking(process.stdin.fileno(), False)
            selector.register(process.stdin, selectors.EVENT_WRITE, "in")
        if process.stdout is not None:
            selector.register(process.stdout, selectors.EVENT_READ, "out")
        selector.register(process.stderr, selectors.EVENT_READ, "err")
        if parent_fd is not None:
            selector.register(parent_fd, selectors.EVENT_READ, "parent")
        open_streams = 1 if passthrough else 2
        while not stopped:
            remaining = timeout_s - (time.monotonic() - started)
            if cancelled():
                stopped = "cancelled"
                break
            if remaining <= 0:
                stopped = "timeout"
                break
            # Do not reap until after the group sweep. Even closed pipes do not
            # mean the child exited; keep watching the parent and deadline.
            if not open_streams and wait_leader(process.pid, 0):
                terminate = False
                break
            for key, _ in selector.select(timeout=min(remaining, 0.05)):
                if key.data == "in":
                    if pending_input:
                        try:
                            pending_input = pending_input[os.write(key.fd, pending_input[:65536]):]
                        except BlockingIOError:
                            continue
                    if not pending_input:
                        selector.unregister(key.fileobj)
                        process.stdin.close()
                    continue
                if key.data == "parent":
                    stopped = "parent_exit"
                    break
                chunk = os.read(key.fileobj.fileno(), 65536)
                if not chunk:
                    if key.data == "err":
                        stderr_eof = True
                    selector.unregister(key.fileobj)
                    open_streams -= 1
                    continue
                if key.data == "err":
                    room = max(0, MAX_STDERR_CAPTURE - len(err))
                    err.extend(chunk[:room])
                    stderr_truncated |= len(chunk) > room
                    continue
                out.extend(chunk)
                if len(out) > capture_limit:
                    del out[capture_limit:]
                    stopped = "truncated"
                elif line_limit is not None and out.count(b"\n") >= line_limit:
                    stopped = "line_limit"
    except Exception as error:
        # Never persist exception text: it may embed env, argv or source data.
        error_type = type(error).__name__[:64]
        stopped = "spawn_error" if process is None else "exception"
    finally:
        try:
            if process is not None:
                try:
                    # Cancellation can race an already exited worker. Preserve its
                    # diagnostic tail before closing pipes; never wait here or infer
                    # EOF from exit alone (a descendant may still hold the pipe).
                    if passthrough and not stderr_eof:
                        os.set_blocking(process.stderr.fileno(), False)
                        while True:
                            try:
                                chunk = os.read(process.stderr.fileno(), 65536)
                            except BlockingIOError:
                                break
                            if not chunk:
                                stderr_eof = True
                                break
                            room = max(0, MAX_STDERR_CAPTURE - len(err))
                            err.extend(chunk[:room])
                            stderr_truncated |= len(chunk) > room
                            if stderr_truncated:
                                break
                finally:
                    supervision = end_group(process, terminate=terminate, grace_s=grace_s)
        finally:
            if selector is not None:
                selector.close()
            if process is not None:
                for stream in (process.stdin, process.stdout, process.stderr):
                    if stream is not None:
                        stream.close()
    return {"exit_code": process.returncode if process is not None else None,
            "stdout": bytes(out), "stderr": bytes(err), "stderr_truncated": stderr_truncated,
            "stopped": stopped, "error_type": error_type, "supervision": supervision,
            "stderr_eof": stderr_eof,
            "elapsed_ms": int((time.monotonic() - started) * 1000)}


def wait_leader(pid, timeout_s):
    """Wait for the leader to exit without reaping it, so its group id stays reserved."""
    deadline = time.monotonic() + timeout_s
    while True:
        try:
            if os.waitid(os.P_PID, pid, os.WEXITED | os.WNOHANG | os.WNOWAIT) is not None:
                return True
        except ChildProcessError:
            return True
        if time.monotonic() >= deadline:
            return False
        time.sleep(0.02)


def live_group_members(pgid):
    """PIDs in process group pgid that are not zombies; None when /proc cannot answer."""
    members = []
    try:
        names = os.listdir("/proc")
    except OSError:
        return None
    for name in names:
        if not name.isdigit():
            continue
        try:
            with open(f"/proc/{name}/stat", "rb") as stream:
                fields = stream.read().rsplit(b")", 1)[1].split()
        except (OSError, IndexError):
            continue
        if int(fields[2]) == pgid and fields[0] not in (b"Z", b"X"):
            members.append(int(name))
    return members


def end_group(process, terminate, grace_s=KILL_GRACE_S):
    """STD-03 R11-R14: TERM, bounded grace, KILL; sweep; reap the leader last.

    The leader is never reaped before the sweep, so the group id cannot be
    reused while it is signalled. Survival is probed on the group itself; an
    unanswerable probe counts as survival.
    """
    signals = []

    def signal_group(sig):
        try:
            os.killpg(process.pid, sig)
            signals.append(signal.Signals(sig).name)
        except ProcessLookupError:
            pass

    def settled(timeout_s):
        deadline = time.monotonic() + timeout_s
        while True:
            members = live_group_members(process.pid)
            if members == []:
                return True
            if time.monotonic() >= deadline:
                return False
            time.sleep(0.02)

    if terminate:
        signal_group(signal.SIGTERM)
        if not settled(grace_s):
            signal_group(signal.SIGKILL)
    elif not settled(min(1.0, grace_s)):
        signal_group(signal.SIGKILL)  # sweep survivors of a clean leader exit
    survivors = live_group_members(process.pid)
    if survivors != []:
        signal_group(signal.SIGKILL)
        settled(grace_s)
        survivors = live_group_members(process.pid)
    process.wait(timeout=grace_s)
    return {"signals": signals, "survivors": survivors}


# --------------------------------------------------------------------------
# The broker.


class Broker:
    def __init__(self, config):
        self.config = config
        self.reply_contract = config.get("reply_contract")
        self.redactor = (replies.Redactor(policy=self.reply_contract["policy"])
                         if self.reply_contract else None)
        self.arm = config["arm"]
        self.tools = tuple(config["tools"])
        self.plugin = config.get("plugin")
        self.hidden_names = HIDDEN_NAMES | ({".orbit"} if config["schema_version"] == 2 else set())
        self.root = config["repo_root"]
        self.limits = config["limits"]
        self.binaries = config["binaries"]
        self.views = []
        self.calls = 0
        self.refusals = 0
        self.protocol_errors = 0
        self.output_bytes = 0
        self.stopped = None
        self.root_fd = os.open(self.root, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
        self.log = os.open(config["log_path"],
                           os.O_WRONLY | os.O_APPEND | os.O_CREAT | os.O_NOFOLLOW | os.O_CLOEXEC,
                           0o600)
        self.child_env = {"PATH": config["path_env"], "HOME": config["home"],
                          "TMPDIR": config["tmp"], "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8",
                          "TZ": "UTC", "NO_COLOR": "1"}
        self.git_env = dict(self.child_env, GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL="/dev/null",
                            GIT_TERMINAL_PROMPT="0", GIT_OPTIONAL_LOCKS="0",
                            GIT_NO_REPLACE_OBJECTS="1", GIT_PAGER="cat", PAGER="cat",
                            GIT_CEILING_DIRECTORIES=os.path.dirname(self.root))

    # -- logging ---------------------------------------------------------
    def record(self, entry):
        entry = dict(entry, at_ms=now_ms())
        if entry["type"] == "protocol_error":
            self.protocol_errors += 1
            if self.protocol_errors > MAX_PROTOCOL_ERRORS:
                self.stop("protocol_error_budget_exceeded", "too many malformed MCP messages")
                return
        if self.redactor:
            entry, count = self.redactor.value(entry)
            if count:
                entry["audit_redactions"] = count
        os.write(self.log, (canonical(entry) + "\n").encode())

    def stop(self, code, message):
        """Record a budget breach once; the runner ends the episode on the sentinel."""
        if self.stopped:
            return
        self.stopped = code
        self.record({"type": "budget", "code": code, "message": message})
        try:
            fd = os.open(self.config["sentinel_path"],
                         os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        except FileExistsError:
            return
        with os.fdopen(fd, "w") as stream:
            stream.write(canonical({"code": code, "message": message}) + "\n")

    # -- authorization: the one decision every tools/call passes ----------
    def authorize(self, tool):
        if tool not in self.tools or (not self.plugin and tool not in ARM_TOOLS[self.arm]):
            raise Refusal("tool_not_permitted",
                          f"{tool!r} is not available in the {self.arm} arm")

    def deadline_s(self):
        remaining = self.config["deadline_unix_ms"] / 1000 - time.time()
        return max(0.0, min(CALL_TIMEOUT_CAP_S, remaining))

    # -- paging ----------------------------------------------------------
    def page(self, envelope, text, offset):
        """Fit envelope + text[offset:offset+k] into call_bytes, k maximal."""
        text = self.mask_view(text, "page-input")
        limit = self.limits["call_bytes"]
        offset = min(offset, len(text))
        low, high = 0, len(text) - offset

        def render(count):
            end = offset + count
            body = dict(envelope, offset=offset, total_chars=len(text),
                        next_offset=end if end < len(text) else None,
                        output=text[offset:end])
            return canonical(body)

        if len(render(high).encode()) <= limit:
            return render(high)
        while low < high:
            middle = (low + high + 1) // 2
            if len(render(middle).encode()) <= limit:
                low = middle
            else:
                high = middle - 1
        return render(low)

    def child_result(self, tool, result, offset, ok_codes=(0,)):
        status = "ok"
        if result["stopped"] == "timeout":
            status = "timeout"
        elif result["stopped"] == "truncated":
            status = "truncated"
        elif result["stopped"] not in (None, "line_limit") or result["exit_code"] not in ok_codes:
            status = "failed"
        if result["supervision"]["survivors"] != []:
            status = "failed"
        self.record({"type": "child", "seq": self.calls, "tool": tool,
                     "exit_code": result["exit_code"], "stopped": result["stopped"],
                     "error_type": result.get("error_type"), "cleanup": result["supervision"],
                     "stderr": result["stderr"].decode("utf-8", errors="replace")[:2048]
                     if status != "ok" else "",
                     "stderr_truncated": result.get("stderr_truncated", False)
                     or len(result["stderr"].decode("utf-8", errors="replace")) > 2048})
        text = result["stdout"].decode("utf-8", errors="replace")
        envelope = {"tool": tool, "status": status, "exit_code": result["exit_code"]}
        if status != "ok":
            # Leave room for structured evidence even at the minimum call budget.
            stderr = result["stderr"].decode("utf-8", errors="replace")
            stderr_limit = min(2048, max(0, (self.limits["call_bytes"] - 700) // 6))
            envelope.update(stderr=stderr[:stderr_limit],
                            stderr_truncated=result.get("stderr_truncated", False)
                            or len(stderr) > stderr_limit,
                            stopped=result["stopped"], error_type=result.get("error_type"),
                            cleanup={"signals": result["supervision"]["signals"],
                                     "survivors": result["supervision"]["survivors"][:8]
                                     if result["supervision"]["survivors"] is not None else None})
        return status, self.page(envelope, text, offset)

    # -- tools -----------------------------------------------------------
    def open_checked(self, parts):
        """Walk from the root fd with O_NOFOLLOW; the opened object is what was checked."""
        fd = os.dup(self.root_fd)
        try:
            for part in parts:
                before = os.stat(part, dir_fd=fd, follow_symlinks=False)
                if stat.S_ISLNK(before.st_mode):
                    raise Refusal("symlink_refused", "path traverses a symbolic link")
                if not (stat.S_ISDIR(before.st_mode) or stat.S_ISREG(before.st_mode)):
                    raise Refusal("path_refused", "path is not a regular file or directory")
                flags = os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC
                if stat.S_ISDIR(before.st_mode):
                    flags |= os.O_DIRECTORY
                child = os.open(part, flags, dir_fd=fd)
                after = os.fstat(child)
                os.close(fd)
                fd = child
                if (after.st_dev, after.st_ino) != (before.st_dev, before.st_ino):
                    raise Refusal("path_refused", "path changed while it was opened")
            return fd
        except FileNotFoundError:
            os.close(fd)
            raise Refusal("not_found", "path does not exist") from None
        except Refusal:
            os.close(fd)
            raise
        except OSError as error:
            os.close(fd)
            raise Refusal("path_refused", f"path cannot be opened: {error.strerror}") from None

    def tool_read(self, args):
        parts = split_path(args.get("path", "."))
        start = args.get("start_line", 1)
        count = args.get("max_lines", 400)
        fd = self.open_checked(parts)
        info = os.fstat(fd)
        shown = "/".join(parts) or "."
        if stat.S_ISDIR(info.st_mode):
            try:
                names = sorted(name for name in os.listdir(fd) if name not in self.hidden_names)
                entries = []
                for name in names:
                    mode = os.stat(name, dir_fd=fd, follow_symlinks=False).st_mode
                    kind = ("dir" if stat.S_ISDIR(mode) else "file" if stat.S_ISREG(mode)
                            else "symlink" if stat.S_ISLNK(mode) else "other")
                    entries.append(f"{name}/" if kind == "dir" else
                                   name if kind == "file" else f"{name} [{kind}]")
            finally:
                os.close(fd)
            window = entries[start - 1:start - 1 + count]
            return "ok", self.fit_lines({"tool": "read", "status": "ok", "path": shown,
                                         "kind": "dir", "total_entries": len(entries)},
                                        window, start, len(entries), numbered=False)
        with os.fdopen(fd, "rb") as stream:
            if info.st_size > MAX_READ_FILE_BYTES:
                raise Refusal("file_too_large", f"file exceeds {MAX_READ_FILE_BYTES} bytes; use rg")
            data = stream.read(MAX_READ_FILE_BYTES + 1)
        if b"\0" in data:
            return "ok", canonical({"tool": "read", "status": "ok", "path": shown,
                                    "kind": "binary", "bytes": len(data)})
        lines = data.decode("utf-8", errors="replace").splitlines()
        window = lines[start - 1:start - 1 + count]
        return "ok", self.fit_lines({"tool": "read", "status": "ok", "path": shown,
                                     "kind": "file", "total_lines": len(lines)},
                                    window, start, len(lines), numbered=True)

    def fit_lines(self, envelope, window, start, total, numbered):
        """Return as many whole lines as fit call_bytes; long lines are cut and marked."""
        limit = self.limits["call_bytes"]
        window = [self.mask_view(line, "read-line") for line in window]
        window = [line if len(line) <= MAX_LINE_CHARS else
                  line[:MAX_LINE_CHARS] + " [line cut at %d chars]" % MAX_LINE_CHARS
                  for line in window]
        taken = len(window)
        while True:
            chosen = window[:taken]
            rendered = [f"{start + index}\t{line}" if numbered else line
                        for index, line in enumerate(chosen)]
            end = start + len(chosen) - 1
            following = end + 1 if chosen and end < total else (start if not chosen and
                                                                   start <= total else None)
            body = dict(envelope, start_line=start, end_line=end if chosen else None,
                        next_start_line=following, content="\n".join(rendered))
            text = canonical(body)
            if len(text.encode()) <= limit or taken == 0:
                return text
            taken = taken // 2

    def tool_rg(self, args):
        path = args.get("path", ".")
        if path != ".":
            os.close(self.open_checked(split_path(path)))
        matches = args.get("max_matches", 100)
        argv = [self.binaries["rg"], "--no-config", "--no-ignore-parent", "--hidden",
                "--line-number", "--with-filename", "--no-heading", "--color=never",
                "--sort=path", "--max-filesize=4M"]
        if not self.redactor:
            argv += ["--max-columns=400", "--max-columns-preview"]
        if args.get("fixed_strings"):
            argv.append("--fixed-strings")
        if args.get("ignore_case"):
            argv.append("--ignore-case")
        if "glob" in args:
            argv.append(f"--glob={args['glob']}")
        # The last matching glob wins in rg, so the hidden names come after the caller's.
        argv += ["--glob=!" + name for name in sorted(self.hidden_names)]
        # Without a path rg searches its cwd (the root) and prints bare relative paths.
        argv += [f"--regexp={args['pattern']}", "--"] + ([path] if path != "." else [])
        result = run_child(argv, self.root, self.child_env, self.deadline_s(),
                           line_limit=matches)
        if result["stopped"] == "line_limit":
            kept = result["stdout"].split(b"\n")[:matches]
            result["stdout"] = b"\n".join(kept) + b"\n"
            result["stopped"] = None
            result["exit_code"] = 0
        status, text = self.child_result("rg", result, args.get("offset", 0), ok_codes=(0, 1))
        return status, text

    def git_argv(self):
        return [self.binaries["git"], "--no-pager", "--literal-pathspecs",
                "-c", "core.fsmonitor=false", "-c", "core.hooksPath=/dev/null",
                "-c", "core.pager=cat", "-c", "diff.external=", "-c", "color.ui=never",
                "-c", "core.quotePath=false", "-c", "log.showSignature=false",
                "-c", f"safe.directory={self.root}"]

    def tool_git(self, args):
        op = args["op"]
        allowed = {"log": {"op", "rev", "path", "max_count", "offset"},
                   "show": {"op", "rev", "path", "stat", "offset"},
                   "diff": {"op", "base", "head", "path", "stat", "offset"},
                   "file_at": {"op", "rev", "path", "offset"}}[op]
        extra = sorted(set(args) - allowed)
        if extra:
            raise Refusal("unknown_field", f"git op {op} does not accept {', '.join(extra)}")
        pathspec = ["--", args["path"]] if args.get("path", ".") != "." else []
        argv = self.git_argv()
        if op == "log":
            argv += ["log", "--no-color", "--date=iso-strict",
                     "--format=%H%x09%an%x09%ad%x09%s",
                     f"--max-count={args.get('max_count', 50)}", "--end-of-options",
                     args.get("rev", "HEAD")] + pathspec
        elif op == "show":
            if "rev" not in args:
                raise Refusal("missing_field", "git show requires rev")
            argv += ["show", "--no-color", "--no-ext-diff", "--no-textconv", "--format=fuller"]
            argv += ["--stat"] if args.get("stat") else []
            argv += ["--end-of-options", args["rev"]] + pathspec
        elif op == "diff":
            if "base" not in args or "head" not in args:
                raise Refusal("missing_field", "git diff requires base and head")
            argv += ["diff", "--no-color", "--no-ext-diff", "--no-textconv"]
            argv += ["--stat"] if args.get("stat") else []
            argv += ["--end-of-options", args["base"], args["head"]] + pathspec
        else:
            if "rev" not in args or args.get("path", ".") == ".":
                raise Refusal("missing_field", "git file_at requires rev and a file path")
            argv += ["show", "--no-textconv", "--end-of-options", f"{args['rev']}:{args['path']}"]
        result = run_child(argv, self.root, self.git_env, self.deadline_s())
        return self.child_result("git", result, args.get("offset", 0))

    def tool_graph(self, tool, args):
        binary = self.binaries.get("orbit_graph")
        if not binary:
            raise Refusal("tool_unavailable", "orbit-graph is not staged for this episode")
        offset = args.get("offset", 0)
        argv = [binary]
        if tool == "graph_sync":
            argv += ["sync", "--json"] + (["--full"] if args.get("full") else [])
        elif tool == "search":
            argv += ["search", "--json"]
            for field in ("kind", "language", "limit"):
                if field in args:
                    argv.append(f"--{field}={args[field]}")
            argv += ["--", args["query"]]
        elif tool == "show":
            argv += ["show", "--json", f"--max-bytes={args.get('max_bytes', 8192)}",
                     "--", args["selector"]]
        elif tool == "refs":
            argv += ["refs", "--json"]
            for field in ("confidence", "kind"):
                if field in args:
                    argv.append(f"--{field}={args[field]}")
            argv += ["--", args["selector"]]
        elif tool == "callees":
            argv += ["callees", "--json"]
            argv += ["--include-unresolved"] if args.get("include_unresolved") else []
            argv += ["--", args["selector"]]
        elif tool == "impact":
            argv += ["impact", "--json"]
            for field in ("depth", "confidence", "direction"):
                if field in args:
                    argv.append(f"--{field}={args[field]}")
            argv += ["--", args["selector"]]
        else:
            base = args.get("base", self.config["snapshot"]["base_commit"])
            head = args.get("head", self.config["snapshot"]["head_commit"])
            argv += ["changes", "--json", "--no-cache"]
            for selector in args.get("symbol", ()):
                argv.append(f"--symbol={selector}")
            for field in ("confidence", "language", "scope"):
                if field in args:
                    argv.append(f"--{field}={args[field]}")
            argv += ["--", f"{base}..{head}"]
        result = run_child(argv, self.root, self.git_env, self.deadline_s())
        return self.child_result(tool, result, offset)

    def execute(self, tool, args):
        if tool == "read":
            return self.tool_read(args)
        if tool == "rg":
            return self.tool_rg(args)
        if tool == "git":
            return self.tool_git(args)
        if self.plugin:
            import plugin_profile
            return plugin_profile.execute(self, tool, args)
        return self.tool_graph(tool, args)

    # -- MCP -------------------------------------------------------------
    def call_tool(self, params):
        if not isinstance(params, dict) or set(params) - {"name", "arguments", "_meta"}:
            raise ProtocolError(-32602, "tools/call params must hold name and arguments")
        tool = params.get("name")
        arguments = params.get("arguments", {})
        if not isinstance(tool, str):
            raise ProtocolError(-32602, "tools/call name must be a string")
        if self.stopped:
            return self.tool_error("episode_stopped", f"episode budget exhausted: {self.stopped}")
        try:
            self.authorize(tool)
        except Refusal as refusal:
            self.refusals += 1
            self.record({"type": "refusal", "tool": tool[:128], "code": refusal.code})
            if self.refusals > MAX_REFUSALS:
                self.stop("refusal_budget_exceeded", "too many refused tool names")
            return self.tool_error(refusal.code, refusal.message)
        if self.calls >= self.limits["tool_calls"]:
            self.stop("tool_call_budget_exceeded",
                      f"more than {self.limits['tool_calls']} tool calls")
            return self.tool_error("tool_call_budget_exceeded", "tool call budget exhausted")
        self.calls += 1
        self.views = []
        self.record({"type": "call_start", "seq": self.calls, "tool": tool})
        started = time.monotonic()
        raw_input = canonical(arguments) if _jsonable(arguments) else repr(arguments)
        limit = self.limits["call_bytes"]
        if len(raw_input.encode()) > limit:
            captured = raw_input.encode()[:limit].decode("utf-8", errors="ignore")
            output = canonical({"tool": tool, "status": "failed",
                                "error": {"code": "call_input_budget_exceeded",
                                          "message": f"arguments exceed {limit} bytes"}})
            output, proof = self.safe_output(output)
            self.finish_call(tool, captured, output, "truncated", started, proof)
            self.stop("call_input_budget_exceeded", f"{tool} arguments exceed {limit} bytes")
            return {"content": [{"type": "text", "text": output}], "isError": True}
        try:
            if self.plugin and tool not in ARM_TOOLS["baseline"]:
                checked = arguments  # product validates its advertised schema
            else:
                checked = validate_arguments(tool, arguments)
                if self.config["schema_version"] == 2 and "path" in checked:
                    if ".orbit" in split_path(checked["path"]):
                        raise Refusal("path_refused", "Orbit state is hidden")
            status, output = self.execute(tool, checked)
        except Refusal as refusal:
            status = "failed"
            output = canonical({"tool": tool, "status": "failed",
                                "error": {"code": refusal.code, "message": refusal.message}})
        except Exception as error:
            status = "failed"
            output = canonical({"tool": tool, "status": status,
                                "error": {"code": "tool_exception",
                                          "type": type(error).__name__[:64]}})
        output, proof = self.safe_output(output)
        remaining = self.limits["output_bytes"] - self.output_bytes
        delivery_limit = min(remaining, self.limits["call_bytes"]) if self.redactor else remaining
        if len(output.encode()) > delivery_limit:
            output = output.encode()[:max(0, delivery_limit)].decode("utf-8", errors="ignore")
            if proof is not None:
                proof["delivered_sha256"] = replies.sha(output)
                proof["delivered_bytes"] = len(output.encode())
                proof["truncated"] = True
            self.finish_call(tool, raw_input, output, "truncated", started, proof)
            code = "output_budget_exceeded" if delivery_limit == remaining else "call_output_truncated"
            self.stop(code, f"tool output exceeds the {delivery_limit}-byte delivery bound")
            return {"content": [{"type": "text", "text": output}], "isError": True}
        self.finish_call(tool, raw_input, output, status, started, proof)
        if status == "truncated":
            self.stop("call_output_truncated", f"{tool} output exceeded the capture bound")
        return {"content": [{"type": "text", "text": output}], "isError": status != "ok"}

    def mask_view(self, text, role):
        # Mask before paging/line cuts, so a cut cannot reveal a credential prefix.
        if not self.redactor:
            return text
        safe, proof = self.redactor.transform(text)
        if proof["spans"]:
            self.views.append({"role": role, "text": proof, "safe_text": safe})
        return safe

    def safe_output(self, output):
        if not self.redactor:
            return output, None
        body = json.loads(output)
        body["text_view"] = {"contract": replies.CONTRACT, "source_metadata": "original",
                             "redacted": bool(self.views or self.redactor.text(output)[1])}
        safe, text = self.redactor.transform(canonical(body))
        return safe, {"seq": self.calls, "views": self.views,
                      "contract_sha256": replies.digest(self.reply_contract),
                      "text": text, "safe_output": safe, "delivered_sha256": replies.sha(safe),
                      "delivered_bytes": len(safe.encode()), "truncated": False}

    def finish_call(self, tool, raw_input, output, status, started, proof=None):
        self.output_bytes += len(output.encode())
        entry = {"type": "call", "seq": self.calls, "tool": tool, "input": raw_input,
                 "output": output, "status": status,
                 "elapsed_ms": int((time.monotonic() - started) * 1000)}
        if proof is not None:
            proof["call_sha256"] = replies.digest({k: entry[k] for k in
                ("tool", "input", "output", "status", "elapsed_ms")})
            entry["reply_provenance"] = proof
        self.record(entry)

    @staticmethod
    def tool_error(code, message):
        text = canonical({"status": "refused", "error": {"code": code, "message": message}})
        return {"content": [{"type": "text", "text": text}], "isError": True}

    def handle(self, message):
        if not isinstance(message, dict) or message.get("jsonrpc") != "2.0":
            raise ProtocolError(-32600, "not a JSON-RPC 2.0 message")
        method = message.get("method")
        if not isinstance(method, str):
            return None  # a response to nothing we sent; ignore
        params = message.get("params", {})
        if method == "initialize":
            requested = params.get("protocolVersion") if isinstance(params, dict) else None
            version = requested if requested in PROTOCOL_VERSIONS else PROTOCOL_VERSIONS[0]
            self.record({"type": "initialize", "protocol_version": version,
                         "client": _bounded_json(params.get("clientInfo")
                                                 if isinstance(params, dict) else None)})
            return {"protocolVersion": version, "capabilities": {"tools": {"listChanged": False}},
                    "serverInfo": {"name": BROKER_NAME, "version": BROKER_VERSION}}
        if method == "ping":
            return {}
        if method == "tools/list":
            self.record({"type": "tools_list", "tools": list(self.tools)})
            common = ARM_TOOLS["baseline"] if self.plugin else self.tools
            listed = [{"name": tool, "description": DESCRIPTIONS[tool],
                               "inputSchema": input_schema(tool),
                               "annotations": dict(ANNOTATIONS[tool])} for tool in common]
            return {"tools": listed + (self.plugin["inventory"] if self.plugin else [])}
        if method == "tools/call":
            return self.call_tool(params)
        if method.startswith("notifications/"):
            return None
        raise ProtocolError(-32601, f"method not supported: {method[:64]}")

    def serve(self, stdin, stdout):
        requests = replies = 0
        self.record({"type": "start", "arm": self.arm, "tools": list(self.tools),
                     "identity": start_identity(),
                     **({"reply_contract": self.reply_contract} if self.reply_contract else {})})
        while True:
            line = stdin.readline(MAX_MESSAGE_BYTES + 1)
            if not line:
                break
            if len(line) > MAX_MESSAGE_BYTES and not line.endswith(b"\n"):
                while True:
                    rest = stdin.readline(MAX_MESSAGE_BYTES)
                    if not rest or rest.endswith(b"\n"):
                        break
                self.record({"type": "protocol_error", "code": -32700, "reason": "oversized"})
                self.reply(stdout, None, error=(-32700, "message exceeds size bound"))
                continue
            if not line.strip():
                continue
            try:
                message = json.loads(line, object_pairs_hook=_unique,
                                     parse_constant=_reject_constant)
            except (ValueError, RecursionError):
                self.record({"type": "protocol_error", "code": -32700, "reason": "parse"})
                self.reply(stdout, None, error=(-32700, "parse error"))
                continue
            request_id = message.get("id") if isinstance(message, dict) else None
            is_request = isinstance(message, dict) and "id" in message and "method" in message
            if is_request:
                requests += 1
                self.record({"type": "request", "seq": requests})
            try:
                result = self.handle(message)
            except ProtocolError as error:
                self.record({"type": "protocol_error", "code": error.code,
                             "reason": error.message[:200]})
                if is_request or not isinstance(message, dict):
                    self.reply(stdout, request_id, error=(error.code, error.message))
                    if is_request:
                        replies += 1
                        self.record({"type": "reply", "seq": requests, "calls": self.calls})
                continue
            if is_request:
                self.reply(stdout, request_id, result=result if result is not None else {})
                replies += 1
                self.record({"type": "reply", "seq": requests, "calls": self.calls})
        self.record({"type": "stop", "reason": "stdin_eof", "requests": requests,
                     "replies": replies, "calls": self.calls, "output_bytes": self.output_bytes})

    def reply(self, stdout, request_id, result=None, error=None):
        if not (request_id is None or isinstance(request_id, (str, int))) or \
                isinstance(request_id, bool):
            request_id = None
        body = {"jsonrpc": "2.0", "id": request_id}
        if error is not None:
            body["error"] = {"code": error[0], "message": error[1]}
        else:
            body["result"] = result
        if self.redactor:
            body, count = self.redactor.value(body)
            if count:
                self.record({"type": "wire_redaction", "audit_redactions": count})
        stdout.write((canonical(body) + "\n").encode())
        stdout.flush()


class ProtocolError(Exception):
    def __init__(self, code, message):
        super().__init__(message)
        self.code = code
        self.message = message


def _unique(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate key {key}")
        result[key] = value
    return result


def _reject_constant(value):
    raise ValueError(f"non-finite number {value}")


def _jsonable(value):
    try:
        canonical(value)
        return True
    except (TypeError, ValueError):
        return False


def _bounded_json(value):
    try:
        text = canonical(value)
    except (TypeError, ValueError):
        return None
    return text[:512]


# --------------------------------------------------------------------------
# Configuration and the in-sandbox launcher.

CONFIG_FIELDS = {"schema_version", "arm", "tools", "repo_root", "log_path", "sentinel_path",
                 "home", "tmp", "path_env", "deadline_unix_ms", "limits", "binaries", "snapshot"}
LIMIT_FIELDS = {"wall_ms", "tool_calls", "output_bytes", "answer_items", "call_bytes"}


def load_config(path):
    with open(path, "rb") as stream:
        config = json.loads(stream.read(1024 * 1024 + 1), object_pairs_hook=_unique,
                            parse_constant=_reject_constant)
    problems = []
    v2 = isinstance(config, dict) and config.get("schema_version") == 2
    fields = CONFIG_FIELDS | ({"plugin"} if v2 else set())
    if v2 and "reply_contract" in config:
        fields.add("reply_contract")
        replies.validate_contract(config["reply_contract"])
    if not isinstance(config, dict) or set(config) != fields:
        raise ValueError(f"broker config must have exactly {sorted(CONFIG_FIELDS)}")
    if config["schema_version"] not in (1, 2):
        problems.append("schema_version")
    expected = ARM_TOOLS.get(config["arm"])
    if v2 and config["arm"] == "graph":
        import plugin_profile
        expected = ARM_TOOLS["baseline"] + plugin_profile.TOOLS
        p = config["plugin"]
        if (not isinstance(p, dict) or set(p) != {"orbit", "home", "inventory", "policy"}
                or p["policy"] != plugin_profile.POLICY
                or plugin_profile.plugin_inventory({"tools": p["inventory"]}) != p["inventory"]):
            problems.append("plugin")
    elif v2 and config["plugin"] is not None:
        problems.append("baseline plugin must be absent")
    if config["arm"] not in ARM_TOOLS or tuple(config["tools"]) != expected:
        problems.append("arm/tools")
    if not isinstance(config["limits"], dict) or set(config["limits"]) != LIMIT_FIELDS or \
            not all(type(v) is int and v > 0 for v in config["limits"].values()):
        problems.append("limits")
    binaries = config["binaries"]
    if not isinstance(binaries, dict) or set(binaries) != {"rg", "git", "orbit_graph"}:
        problems.append("binaries")
    elif (binaries["orbit_graph"] is None) != (config["arm"] == "baseline"):
        problems.append("orbit_graph staged only for the graph arm")
    snapshot = config["snapshot"]
    if not isinstance(snapshot, dict) or set(snapshot) != {"base_commit", "head_commit"}:
        problems.append("snapshot")
    if problems:
        raise ValueError("invalid broker config: " + ", ".join(problems))
    return config


def probe(spec):
    """Report which host paths are visible from here, without reading them."""
    def visibility(path):
        try:
            os.lstat(path)
            return "visible"
        except FileNotFoundError:
            return "absent"
        except PermissionError:
            return "denied"
        except OSError as error:
            return f"error:{error.errno}"

    forbidden = {path: visibility(path) for path in spec["forbidden"]}
    expected = {path: visibility(path) for path in spec["expect_visible"]}
    try:
        with open("/proc/self/mountinfo", "rb") as stream:
            mountinfo = stream.read(1024 * 1024)
        mounts = sorted({line.split(b" ")[4].decode("utf-8", "replace")
                         for line in mountinfo.splitlines() if line.count(b" ") > 4})
    except OSError:
        mounts = None
    leaked = sorted(path for path, state in forbidden.items() if state != "absent"
                    and state != "denied")
    missing = sorted(path for path, state in expected.items() if state != "visible")
    return {"forbidden": forbidden, "expect_visible": expected, "leaked": leaked,
            "missing": missing, "mount_points": mounts[:400] if mounts else mounts,
            "mount_count": len(mounts) if mounts is not None else None,
            "identity": start_identity(), "uid": os.getuid(),
            "env_names": sorted(os.environ), "hidden": not leaked and not missing}


def launch(spec_path, argv):
    with open(spec_path, "rb") as stream:
        spec = json.loads(stream.read(1024 * 1024), object_pairs_hook=_unique)
    report = probe(spec)
    fd = os.open(spec["output"], os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "w") as stream:
        stream.write(canonical(report) + "\n")
    if spec["require_hidden"] and not report["hidden"]:
        print(f"{BROKER_NAME}: containment probe refused: leaked={report['leaked']} "
              f"missing={report['missing']}", file=sys.stderr)
        return PROBE_REFUSED_EXIT
    os.execv(argv[0], argv)
    return 127  # not reached


def idle_checkpoint(path):
    """Snapshot the last exchange boundary; final validation checks the full ledger.

    Any partial write, busy request/call, unknown count or oversized log refuses
    the drain. Counts fence off requests prefetched into the worker's buffer:
    final EOF must contain exactly these counts, not additional buffered work.
    """
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
        with os.fdopen(fd, "rb") as stream:
            data = stream.read(MAX_LIFECYCLE_LOG + 1)
        if len(data) > MAX_LIFECYCLE_LOG or not data.endswith(b"\n"):
            return None
        last = {}
        for line in data.splitlines():
            entry = json.loads(line, object_pairs_hook=_unique, parse_constant=_reject_constant)
            if not isinstance(entry, dict):
                return None
            if entry.get("type") in ("request", "reply", "call_start", "call", "stop"):
                last = entry
        if last.get("type") == "reply":
            checkpoint = {"requests": last.get("seq"), "calls": last.get("calls")}
        elif last.get("type") == "stop" and last.get("reason") == "stdin_eof":
            checkpoint = {"requests": last.get("requests"), "calls": last.get("calls")}
        else:
            return None
        return checkpoint if valid_checkpoint(checkpoint) else None
    except (OSError, ValueError, RecursionError):
        return None


def valid_checkpoint(value):
    return (isinstance(value, dict) and set(value) == {"requests", "calls"}
            and all(type(count) is int and count >= 0 for count in value.values())
            and value["requests"] > 0)


def input_pipe_eof(fd):
    """Only kernel-confirmed empty pipe + no writers permits an orderly drain."""
    if not stat.S_ISFIFO(os.fstat(fd).st_mode):
        return False
    watch = select.poll()
    watch.register(fd, select.POLLIN | select.POLLHUP | select.POLLERR)
    return watch.poll(0) == [(fd, select.POLLHUP)]


def eof_idle_signal(observed):
    return (observed.get("signal") == "SIGTERM" and observed.get("parent_alive") is True
            and observed.get("input_eof") is True and valid_checkpoint(observed.get("checkpoint")))


def orderly_broker_exit(entry, contract=LIFECYCLE_CONTRACT):
    """Prospective process proof only; the runner also requires EOF/reply evidence.

    Raw cancellation is retained. A TERM observation with an already exited
    worker does not prove the sender or kernel signal-generation ordering.
    """
    facts = entry.get("lifecycle")
    if (not isinstance(facts, dict) or facts.get("contract") != contract
            or facts.get("parent_alive") is not True or facts.get("stderr_eof") is not True
            or type(entry.get("exit_code")) is not int or entry["exit_code"] != 0
            or entry.get("error_type") is not None or entry.get("stderr_truncated") is not False
            or (entry.get("cleanup") or {}).get("survivors") != []):
        return False
    if contract == LIFECYCLE_CONTRACT and facts.get("drain_expired") is not False:
        return False
    received = facts.get("signals")
    if received == []:
        return entry.get("stopped") is None and entry["cleanup"].get("signals") == []
    if (not isinstance(received, list) or len(received) != 1
            or entry.get("stopped") not in (None, "cancelled")
            or entry["cleanup"].get("signals") not in ([], ["SIGTERM"])):
        return False
    observed = received[0]
    if not isinstance(observed, dict) or observed.get("signal") != "SIGTERM" \
            or observed.get("parent_alive") is not True:
        return False
    exited = all(observed.get(key) is True for key in ("worker_exited_zero", "group_empty"))
    return exited or (contract == LIFECYCLE_CONTRACT and eof_idle_signal(observed))


def supervise_broker(config_path, config):
    """Watch provider *process* lifetime, never its transient launching thread.

    pidfd pins identity and becomes readable on process exit. The worker's
    PDEATHSIG is safe here because this supervisor's main thread owns its wait.
    It also lets us retain worker exit/stderr even when the provider drops them.
    """
    parent = os.getppid()
    parent_fd = os.pidfd_open(parent)
    logger = Broker(config)
    cancelled = []
    child = []
    previous = {}
    drain_deadline = None
    drain_expired = False

    def parent_alive():
        with selectors.DefaultSelector() as watch:
            watch.register(parent_fd, selectors.EVENT_READ)
            return not watch.select(0)

    def receive(signum, frame):
        nonlocal drain_deadline
        # Snapshot before cleanup can affect the worker, retaining its zombie
        # identity with WNOWAIT. A running worker needs the separate EOF/idle
        # checkpoint and a successful bounded drain; exit alone cannot qualify.
        if len(cancelled) >= 2:
            return  # two observations already fail closed; bound signal storms
        observed = {"signal": signal.Signals(signum).name, "worker_exited_zero": False,
                    "group_empty": False, "parent_alive": False,
                    "checkpoint": None, "input_eof": False}
        cancelled.append(observed)
        try:
            # Read the exchange boundary BEFORE the pipe check. A request that
            # starts later changes the final counts and cannot qualify.
            observed["checkpoint"] = idle_checkpoint(config["log_path"])
            observed["input_eof"] = input_pipe_eof(sys.stdin.fileno())
            if child and child[0].returncode is None:
                info = os.waitid(os.P_PID, child[0].pid,
                                 os.WEXITED | os.WNOHANG | os.WNOWAIT)
                observed["worker_exited_zero"] = bool(
                    info is not None and info.si_code == os.CLD_EXITED and info.si_status == 0)
                observed["group_empty"] = live_group_members(child[0].pid) == []
            observed["parent_alive"] = parent_alive()
        except OSError:
            pass
        if len(cancelled) == 1 and eof_idle_signal(observed):
            drain_deadline = time.monotonic() + KILL_GRACE_S
        logger.record({"type": "supervisor_signal", **observed})

    def cancel_requested():
        nonlocal drain_expired
        if not cancelled:
            return False
        if len(cancelled) == 1 and drain_deadline is not None:
            if time.monotonic() < drain_deadline:
                return False  # proven EOF + idle checkpoint: await exit, not more work
            drain_expired = True
        return True
    try:
        if os.getppid() != parent:
            raise RuntimeError("parent changed before supervision")
        for sig in (signal.SIGTERM, signal.SIGINT):
            previous[sig] = signal.signal(sig, receive)
        logger.record({"type": "supervisor_start", "identity": start_identity(),
                       "lifecycle_contract": LIFECYCLE_CONTRACT})
        result = run_child([sys.executable, "-B", os.path.abspath(__file__),
                            "--worker", "--config", config_path],
                           config["repo_root"], logger.child_env,
                           max(0, config["deadline_unix_ms"] / 1000 - time.time()),
                           passthrough=True, parent_fd=parent_fd,
                           cancelled=cancel_requested, grace_s=3 * KILL_GRACE_S,
                           on_spawn=child.append)
        entry = {"type": "broker_exit", "exit_code": result["exit_code"],
                 "stopped": result["stopped"] or ("cancelled" if cancelled else None),
                 "error_type": result["error_type"],
                 "stderr": result["stderr"].decode("utf-8", errors="replace"),
                 "stderr_truncated": result["stderr_truncated"],
                 "cleanup": result["supervision"],
                 "lifecycle": {"contract": LIFECYCLE_CONTRACT, "signals": cancelled,
                               "parent_alive": parent_alive(), "stderr_eof": result["stderr_eof"],
                               "drain_expired": drain_expired}}
        logger.record(entry)
        return 0 if orderly_broker_exit(entry) else 1
    finally:
        for sig, handler in previous.items():
            signal.signal(sig, handler)
        os.close(parent_fd)
        os.close(logger.root_fd)
        os.close(logger.log)


def terminate_worker(signum, frame):
    # Unwind child supervision so its whole process group gets cleaned up.
    raise SystemExit(128 + signum)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--worker", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--config", help="broker configuration written by eval_runner.py")
    parser.add_argument("--launch", help="probe spec; run the probe, then exec the command")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
    if args.launch:
        command = args.command[1:] if args.command[:1] == ["--"] else args.command
        if not command:
            parser.error("--launch needs a command after --")
        return launch(args.launch, command)
    if not args.config or args.command:
        parser.error("serve mode takes only --config")
    try:
        config = load_config(args.config)
        if not args.worker:
            return supervise_broker(os.path.abspath(args.config), config)
        broker = Broker(config)
    except (OSError, ValueError) as error:
        print(f"{BROKER_NAME}: {error}", file=sys.stderr)
        return 2
    for sig in (signal.SIGTERM, signal.SIGINT):
        signal.signal(sig, terminate_worker)
    try:
        broker.serve(sys.stdin.buffer, sys.stdout.buffer)
        return 0
    except Exception as error:
        print(f"{BROKER_NAME}: {type(error).__name__[:64]}", file=sys.stderr)
        return 1
    finally:
        os.close(broker.root_fd)
        os.close(broker.log)


if __name__ == "__main__":
    # The profile uses this module's Refusal and supervisor. Keep their identity
    # when this file is the executable, rather than importing a second copy.
    sys.modules["eval_broker"] = sys.modules[__name__]
    sys.exit(main())
