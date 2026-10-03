#!/usr/bin/env python3
"""Operator runner for one paired agent-navigation episode through a confined tool broker.

Subcommands (all print JSON on stdout):
  revision   content revision of a source tree (the ORB-13710 source digest)
  preflight  verify containment and the provider's effective tool surface; no episode
  run        run one baseline or graph episode and write a sealed raw artifact
  adapt      convert sealed raw artifacts into the ORB-13710 public episode bundle

Exit status: 0 success (for `run`: an artifact was written, whatever the episode
status), 1 invalid input or runtime error, 2 command-line usage error,
3 capability refusal (no provider episode was started).

Standard library only; Linux. Never reads corpus answers or truth.
"""
import argparse
import calendar
from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import signal
import stat
import subprocess
import sys
import threading
import time

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import eval_broker as broker  # noqa: E402  (sibling module, same checkout)

RUNNER_VERSION = "1"
RAW_KIND = "agent-eval-raw-episode"
CACHE_POLICY = "cold-per-episode-including-graph-setup"
ARMS = ("baseline", "graph")
REQUEST_REQUIRED = {"schema_version", "case_id", "arm", "split", "source_revision",
                    "base_revision", "prompt", "limits", "tools", "order"}
REQUEST_OPTIONAL = {"fixture", "corpus_sha256", "cache_policy", "request_sha256"}
LIMIT_RANGES = {"wall_ms": (1000, 3_600_000), "tool_calls": (1, 1000),
                "output_bytes": (1024, 16 * 1024 * 1024), "answer_items": (1, 1000),
                "call_bytes": (1024, 1024 * 1024)}
# Codex features that expose an ambient capability. Each must read back as
# effectively disabled from the same configuration, or the run is refused.
DISABLED_FEATURES = ("shell_tool", "apps", "plugins", "remote_plugin", "multi_agent",
                     "multi_agent_v2", "browser_use", "browser_use_external", "computer_use",
                     "view_image", "image_generation", "in_app_browser", "code_mode_host",
                     "tool_suggest", "hooks", "memories", "skill_mcp_dependency_install",
                     "worktrees", "in_app_local_automation")
# Codex runs some models' tool calls through a V8 "code mode" host. Disabling it
# (the default) makes those models fail closed; --allow-code-mode re-enables it.
CODE_MODE_FEATURE = "code_mode_host"
CODE_MODE_HOST = "codex-code-mode-host"
CODE_MODE_DISABLED = ("code-mode host is disabled", "Code Mode is unavailable")
# Shell implementation selectors. They only choose which shell tool exists,
# so they are recorded but not required while shell_tool reads back false;
# any shell use is still caught by the transcript audit.
SHELL_GATED_FEATURES = ("unified_exec", "unified_exec_tty", "shell_snapshot",
                        "write_stdin_approval")
SETTING_KEYS = ("model_reasoning_effort", "model_reasoning_summary", "model_verbosity",
                "service_tier")
SETTING_VALUE = re.compile(r"\A[A-Za-z0-9._-]{1,64}\Z")
MODEL_NAME = re.compile(r"\A[A-Za-z0-9._:/-]{1,128}\Z")
ENV_NAME = re.compile(r"\A[A-Z_][A-Z0-9_]{0,63}\Z")
SECRET_NAME = re.compile(r"KEY|TOKEN|SECRET|PASS|AUTH|CREDENTIAL|COOKIE|SESSION")
HEX64 = re.compile(r"\A[0-9a-f]{64}\Z")
CASE_ID = re.compile(r"\A[A-Za-z0-9._-]{1,128}\Z")
BROKER_SERVER = "eval_broker"
# The outer approval_policy stays "never", so codex refuses any MCP call that
# still needs approval before it reaches the broker. Each of the arm's broker
# tools is approved explicitly (codex 0.160.0 `mcp_servers.<id>.tools.<tool>.
# approval_mode`); any other tool keeps the "prompt" default and is refused.
TOOL_APPROVAL = "approve"
DEFAULT_TOOL_APPROVAL = "prompt"
# Codex has no readback of per-tool approvals; it does reject an unknown value
# at a key it parses, which proves the key is not silently ignored.
APPROVAL_PROBE_VALUE = "agent-eval-unsupported"
APPROVAL_DENIED = "requires approval, but approval policy is never"
ALLOWED_ITEM_TYPES = {"agent_message", "reasoning", "mcp_tool_call", "todo_list", "error"}
USAGE_SOURCE = "codex exec --json turn.completed.usage"
MAX_JSON_BYTES = 2 * 1024 * 1024
MAX_PROVIDER_STDOUT = 8 * 1024 * 1024
MAX_PROVIDER_STDERR = 256 * 1024
MAX_BROKER_LOG = 64 * 1024 * 1024
TREE_LIMITS = {"files": 50_000, "bytes": 512 * 1024 * 1024, "file_bytes": 16 * 1024 * 1024}
COMMIT_EPOCH = calendar.timegm((2026, 10, 3, 0, 0, 0))
PREFLIGHT_TIMEOUT_S = 60.0
TERMINATION_GRACE_S = 5.0
EXIT_OK, EXIT_INVALID, EXIT_REFUSED = 0, 1, 3
SANDBOX = {"repo": "/eval/repo", "runtime": "/eval/runtime", "state": "/eval/state",
           "codex": "/eval/codex", "bin": "/eval/bin"}
ETC_ENTRIES = ("/etc/resolv.conf", "/etc/hosts", "/etc/host.conf", "/etc/gai.conf",
               "/etc/nsswitch.conf", "/etc/passwd", "/etc/group", "/etc/ssl",
               "/etc/ca-certificates", "/etc/pki", "/etc/ld.so.cache", "/etc/localtime",
               "/etc/alternatives")
CREDENTIAL_SHAPES = (
    re.compile(r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{8,}"),
    re.compile(r"(?<![A-Za-z0-9_-])sk-[A-Za-z0-9_-]{16,}"),
    re.compile(r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}"),
)


class Invalid(ValueError):
    """Bad operator input or a malformed artifact (exit 1)."""


class CapabilityRefusal(Exception):
    """Safe containment or the required tool surface is unavailable (exit 3)."""

    def __init__(self, code, message, evidence=None):
        super().__init__(message)
        self.code = code
        self.message = message
        self.evidence = evidence or {}


def require(condition, message):
    if not condition:
        raise Invalid(message)


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False).encode()


def digest(value):
    return hashlib.sha256(canonical(value)).hexdigest()


def sha256_bytes(data):
    return hashlib.sha256(data).hexdigest()


def sha256_file(path):
    hasher = hashlib.sha256()
    with open(path, "rb") as stream:
        for block in iter(lambda: stream.read(1 << 20), b""):
            hasher.update(block)
    return hasher.hexdigest()


def load_json(path, limit=MAX_JSON_BYTES):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
    with os.fdopen(fd, "rb") as stream:
        data = stream.read(limit + 1)
    require(len(data) <= limit, f"{path}: exceeds {limit} bytes")
    return parse_json(data, str(path))


def parse_json(data, label):
    try:
        return json.loads(data, object_pairs_hook=broker._unique,
                          parse_constant=broker._reject_constant)
    except (ValueError, RecursionError) as error:
        raise Invalid(f"{label}: {error}") from None


# --------------------------------------------------------------------------
# Requests and source trees.


def validate_request(request):
    require(isinstance(request, dict), "request must be a JSON object")
    fields = set(request)
    missing = sorted(REQUEST_REQUIRED - fields)
    unknown = sorted(fields - REQUEST_REQUIRED - REQUEST_OPTIONAL)
    require(not missing, f"request is missing {missing}")
    require(not unknown, f"request has unknown fields {unknown}")
    require(type(request["schema_version"]) is int and request["schema_version"] == 1,
            "request schema_version must be 1")
    require(isinstance(request["case_id"], str) and CASE_ID.match(request["case_id"]),
            "case_id must match [A-Za-z0-9._-]{1,128}")
    require(request["arm"] in ARMS, f"arm must be one of {ARMS}")
    require(isinstance(request["split"], str) and 0 < len(request["split"]) <= 64,
            "split must be a short string")
    for key in ("source_revision", "base_revision"):
        require(isinstance(request[key], str) and HEX64.match(request[key]),
                f"{key} must be a 64-hex content revision")
    prompt = request["prompt"]
    require(isinstance(prompt, str) and prompt.strip() and len(prompt.encode()) <= 65536,
            "prompt must be a non-empty string of at most 64 KiB")
    limits = request["limits"]
    require(isinstance(limits, dict) and set(limits) == set(LIMIT_RANGES),
            f"limits must have exactly {sorted(LIMIT_RANGES)}")
    for key, (low, high) in LIMIT_RANGES.items():
        require(type(limits[key]) is int and low <= limits[key] <= high,
                f"limits.{key} must be an integer in {low}..{high}")
    expected = list(broker.ARM_TOOLS[request["arm"]])
    tools = request["tools"]
    require(isinstance(tools, list) and all(isinstance(tool, str) for tool in tools),
            "tools must be a list of tool names")
    if request["arm"] == "baseline":
        graph = sorted(set(tools) & set(broker.GRAPH_TOOLS))
        require(not graph, f"baseline request names graph tools {graph}")
    require(tools == expected, f"tools for the {request['arm']} arm must be exactly {expected}")
    require(type(request["order"]) is int and 0 <= request["order"] <= 100_000,
            "order must be a non-negative integer")
    if "fixture" in request:
        require(isinstance(request["fixture"], str) and CASE_ID.match(request["fixture"]),
                "fixture must match [A-Za-z0-9._-]{1,128}")
    if "corpus_sha256" in request:
        require(isinstance(request["corpus_sha256"], str) and HEX64.match(request["corpus_sha256"]),
                "corpus_sha256 must be 64 hex")
    if "cache_policy" in request:
        require(request["cache_policy"] == CACHE_POLICY, f"cache_policy must be {CACHE_POLICY}")
    if "request_sha256" in request:
        body = {key: value for key, value in request.items() if key != "request_sha256"}
        require(request["request_sha256"] == digest(body), "request_sha256 does not match")
    return request


def tree_entries(root, label):
    """Sorted (relative path, physical path) of a source tree; refuses unsafe entries."""
    root = Path(root)
    require(not root.is_symlink() and root.is_dir(), f"{label}: {root} must be a real directory")
    entries, total = [], 0
    pending = [(root, "")]
    while pending:
        directory, prefix = pending.pop()
        with os.scandir(directory) as iterator:
            children = list(iterator)
        for child in children:
            name = child.name
            require(name not in broker.HIDDEN_NAMES, f"{label}: {prefix}{name} is reserved")
            require(not any(ord(ch) < 32 or ord(ch) == 127 for ch in name) and "\\" not in name,
                    f"{label}: {prefix!r}{name!r} has control characters or backslashes")
            try:
                name.encode("utf-8")
            except UnicodeEncodeError:
                raise Invalid(f"{label}: {prefix}{name!r} is not a UTF-8 name") from None
            relative = f"{prefix}{name}"
            mode = child.stat(follow_symlinks=False).st_mode
            if stat.S_ISLNK(mode):
                raise Invalid(f"{label}: symlink refused: {relative}")
            if stat.S_ISDIR(mode):
                pending.append((Path(child.path), relative + "/"))
                continue
            require(stat.S_ISREG(mode), f"{label}: special file refused: {relative}")
            size = child.stat(follow_symlinks=False).st_size
            require(size <= TREE_LIMITS["file_bytes"], f"{label}: {relative} exceeds file bound")
            total += size
            entries.append((relative, child.path))
            require(len(entries) <= TREE_LIMITS["files"], f"{label}: too many files")
            require(total <= TREE_LIMITS["bytes"], f"{label}: tree exceeds byte bound")
    require(entries, f"{label}: empty source tree")
    entries.sort(key=lambda entry: entry[0])
    return entries


def read_text_file(path, relative, label):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
    with os.fdopen(fd, "rb") as stream:
        data = stream.read(TREE_LIMITS["file_bytes"] + 1)
    require(len(data) <= TREE_LIMITS["file_bytes"], f"{label}: {relative} exceeds file bound")
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError:
        raise Invalid(f"{label}: {relative} is not UTF-8 text") from None
    return data, text


def content_revision(entries, label, sink=None):
    """SHA-256 of canonical JSON {path: text}, streamed; sink(relative, data) sees each file."""
    hasher = hashlib.sha256(b"{")
    total = 0
    for index, (relative, path) in enumerate(entries):
        data, text = read_text_file(path, relative, label)
        if index:
            hasher.update(b",")
        hasher.update(json.dumps(relative).encode() + b":" + json.dumps(text).encode())
        total += len(data)
        if sink is not None:
            sink(relative, data)
    hasher.update(b"}")
    return {"content_revision": hasher.hexdigest(), "files": len(entries), "bytes": total}


# --------------------------------------------------------------------------
# Private files, redaction and the disposable Git snapshot.


class Redactor:
    """Masks live secret values and high-confidence credential shapes (STD-05 R13/R14)."""

    def __init__(self, values):
        self.values = sorted({value for value in values if len(value) >= 8}, key=len,
                             reverse=True)

    def text(self, text):
        count = 0
        for value in self.values:
            if value in text:
                count += text.count(value)
                text = text.replace(value, "[REDACTED]")
        for pattern in CREDENTIAL_SHAPES:
            text, hits = pattern.subn("[REDACTED]", text)
            count += hits
        return text, count

    def value(self, value):
        """Redact every string inside a JSON value; return (value, count)."""
        if isinstance(value, str):
            return self.text(value)
        if isinstance(value, list):
            total, items = 0, []
            for item in value:
                item, count = self.value(item)
                items.append(item)
                total += count
            return items, total
        if isinstance(value, dict):
            total, result = 0, {}
            for key, item in value.items():
                item, count = self.value(item)
                result[key] = item
                total += count
            return result, total
        return value, 0


def make_private_dir(path):
    os.mkdir(path, 0o700)
    os.chmod(path, 0o700)


def write_private(path, data, redactor, truncated=False):
    """The one writer for published episode files: redact, then create 0600, never overwrite."""
    if isinstance(data, bytes):
        text = data.decode("utf-8", errors="replace")
    else:
        text = data
    text, redactions = redactor.text(text)
    payload = text.encode()
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
    with os.fdopen(fd, "wb") as stream:
        stream.write(payload)
    return {"bytes": len(payload), "sha256": sha256_bytes(payload), "redactions": redactions,
            "truncated": truncated}


def git_env(home):
    return {"PATH": "/usr/bin:/bin", "HOME": str(home), "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8",
            "TZ": "UTC", "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": "/dev/null",
            "GIT_TERMINAL_PROMPT": "0", "GIT_AUTHOR_NAME": "agent-eval",
            "GIT_AUTHOR_EMAIL": "agent-eval@example.invalid",
            "GIT_COMMITTER_NAME": "agent-eval", "GIT_COMMITTER_EMAIL": "agent-eval@example.invalid"}


def git_run(git, repo, home, *args, stdin_path=None):
    argv = [git, "-c", "core.hooksPath=/dev/null", "-c", "core.autocrlf=false",
            "-c", "core.fsmonitor=false", "-c", "commit.gpgSign=false",
            "-c", f"safe.directory={repo}", "-C", str(repo), *args]
    stdin = open(stdin_path, "rb") if stdin_path else subprocess.DEVNULL
    try:
        result = subprocess.run(argv, stdin=stdin, capture_output=True, env=git_env(home),
                                timeout=600, check=False, start_new_session=True)
    finally:
        if stdin_path:
            stdin.close()
    if result.returncode != 0:
        raise Invalid(f"git {args[0]} failed: {result.stderr.decode(errors='replace')[:2000]}")
    return result.stdout.decode().strip()


def fast_import_path(relative):
    if re.search(r'["\\]', relative) or relative.startswith('"'):
        return json.dumps(relative, ensure_ascii=False)
    return relative


def materialize_snapshot(repo, state, git, base_entries, head_entries):
    """Two commits (base, head) with fixed identity; the work tree checks out head."""
    stream_path = state / "snapshot.fast-import"
    home = state / "home"
    marks = {"next": 1}
    fd = os.open(stream_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "wb") as stream:
        def tree(entries, label):
            files = []

            def sink(relative, data):
                mark = marks["next"]
                marks["next"] += 1
                stream.write(b"blob\nmark :%d\ndata %d\n" % (mark, len(data)) + data + b"\n")
                files.append((relative, mark))
            revision = content_revision(entries, label, sink)
            return revision, files

        def commit(files, message, parent):
            mark = marks["next"]
            marks["next"] += 1
            identity = b"agent-eval <agent-eval@example.invalid> %d +0000\n" % COMMIT_EPOCH
            stream.write(b"commit refs/heads/main\nmark :%d\nauthor " % mark + identity +
                         b"committer " + identity +
                         b"data %d\n%s\n" % (len(message), message))
            if parent:
                stream.write(b"from :%d\ndeleteall\n" % parent)
            for relative, blob in files:
                stream.write(b"M 100644 :%d %s\n" % (blob, fast_import_path(relative).encode()))
            stream.write(b"\n")
            return mark

        base_revision, base_files = tree(base_entries, "base")
        base_mark = commit(base_files, b"base\n", None)
        head_revision, head_files = tree(head_entries, "head")
        commit(head_files, b"head\n", base_mark)
        stream.write(b"done\n")
    git_run(git, repo.parent, home, "init", "--quiet", "--template=", "--initial-branch=main",
            str(repo.name))
    git_run(git, repo, home, "fast-import", "--quiet", "--done", stdin_path=stream_path)
    os.unlink(stream_path)
    git_run(git, repo, home, "reset", "--quiet", "--hard", "HEAD")
    info = repo / ".git" / "info"
    info.mkdir(mode=0o700, exist_ok=True)
    fd = os.open(info / "exclude", os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "w") as exclude:
        exclude.write(".orbit-graph/\n")
    snapshot = {
        "head_commit": git_run(git, repo, home, "rev-parse", "HEAD"),
        "base_commit": git_run(git, repo, home, "rev-parse", "HEAD^"),
        "head_tree": git_run(git, repo, home, "rev-parse", "HEAD^{tree}"),
        "base_tree": git_run(git, repo, home, "rev-parse", "HEAD^^{tree}"),
        "commit_count": int(git_run(git, repo, home, "rev-list", "--count", "HEAD")),
        "commit_epoch": COMMIT_EPOCH, "base": base_revision, "head": head_revision}
    require(snapshot["commit_count"] == 2, "snapshot must hold exactly two commits")
    checked_out = [entry for entry in tree_entries_with_git(repo)]
    snapshot["work_tree_revision"] = content_revision(checked_out, "snapshot")["content_revision"]
    return snapshot


def tree_entries_with_git(repo):
    """Entries of a snapshot work tree, skipping only its top-level .git and .orbit-graph."""
    entries = []
    for top in sorted(os.listdir(repo)):
        if top in broker.HIDDEN_NAMES:
            continue
        path = repo / top
        if path.is_dir() and not path.is_symlink():
            entries.extend((f"{top}/{relative}", physical)
                           for relative, physical in tree_entries(path, "snapshot"))
        else:
            require(path.is_file() and not path.is_symlink(), f"snapshot: unexpected {top}")
            entries.append((top, str(path)))
    entries.sort(key=lambda entry: entry[0])
    return entries


# --------------------------------------------------------------------------
# Binaries, containment and the provider command.


def system_sandbox_argv(bwrap):
    """Namespaces plus the read-only system mounts every sandboxed process needs.

    The /bin and /lib links matter: without them a dynamically linked binary
    cannot find its loader. The LiveContainment capability probe uses this same
    layout, so it cannot disagree with the runner.
    """
    argv = [bwrap, "--die-with-parent", "--unshare-user", "--unshare-pid", "--unshare-ipc",
            "--unshare-uts", "--unshare-cgroup-try", "--hostname", "agent-eval",
            "--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp", "--ro-bind", "/usr", "/usr"]
    for name in ("bin", "sbin", "lib", "lib32", "lib64"):
        host = f"/{name}"
        if os.path.islink(host):
            argv += ["--symlink", os.readlink(host), host]
        elif os.path.isdir(host):
            argv += ["--ro-bind", host, host]
    etc = list(ETC_ENTRIES) + sorted(str(path) for path in Path("/etc").glob("python3*"))
    resolved = os.path.realpath("/etc/resolv.conf")
    for entry in etc:
        argv += ["--ro-bind-try", entry, entry]
    if resolved != "/etc/resolv.conf":
        argv += ["--ro-bind-try", resolved, resolved]
    return argv


def resolve_binary(value, name):
    if value is None:
        return None
    found = value if "/" in value else shutil.which(value)
    if not found:
        raise CapabilityRefusal("expected_tool_unavailable", f"{name} binary not found: {value}")
    path = os.path.realpath(found)
    if not (os.path.isfile(path) and os.access(path, os.X_OK)):
        raise CapabilityRefusal("expected_tool_unavailable", f"{name} is not executable: {path}")
    return path


def probe_version(argv, env):
    result = broker.run_child(argv, "/", env, 30.0, capture_limit=64 * 1024)
    if result["exit_code"] != 0 or result["stopped"]:
        raise CapabilityRefusal("expected_tool_unavailable",
                                f"{os.path.basename(argv[0])} version probe failed",
                                {"argv": argv, "exit_code": result["exit_code"],
                                 "stderr": result["stderr"].decode(errors="replace")[:2000]})
    text = result["stdout"].decode(errors="replace").strip()
    return " ".join(text.split())[:400] if text else "unknown"


class Episode:
    """Paths, binaries and options of one episode; host and in-sandbox views."""

    def __init__(self, args, request):
        self.args = args
        self.request = request
        self.arm = request["arm"]
        self.contained = args.containment == "bwrap"
        self.out = Path(os.path.abspath(args.out))
        self.repo = self.out / "snapshot" / "repo"
        self.runtime = self.out / "runtime"
        self.state = self.out / "state"
        self.binaries = {
            "codex": resolve_binary(args.codex, "codex"),
            "python": resolve_binary(args.python, "python3"),
            "git": resolve_binary(args.git, "git"),
            "rg": resolve_binary(args.rg, "rg"),
            "orbit_graph": resolve_binary(args.orbit_graph, "orbit-graph")
            if self.arm == "graph" else None,
            "bwrap": resolve_binary(args.bwrap, "bwrap") if self.contained else None,
        }
        if self.arm == "graph" and not self.binaries["orbit_graph"]:
            raise CapabilityRefusal("expected_tool_unavailable",
                                    "the graph arm needs --orbit-graph")
        codex = Path(self.binaries["codex"])
        self.codex_root = codex.parent.parent if codex.parent.name == "bin" else codex.parent
        self.code_mode = bool(getattr(args, "allow_code_mode", False))
        self.disabled = tuple(name for name in DISABLED_FEATURES
                              if not (self.code_mode and name == CODE_MODE_FEATURE))
        if self.code_mode and not os.access(codex.parent / CODE_MODE_HOST, os.X_OK):
            raise CapabilityRefusal("expected_tool_unavailable",
                                    f"--allow-code-mode needs {CODE_MODE_HOST} next to codex",
                                    {"expected": str(codex.parent / CODE_MODE_HOST)})
        self.auth_file = None
        if self.contained:
            for name in ("python", "git"):
                if not self.binaries[name].startswith("/usr/"):
                    raise CapabilityRefusal("containment_unsupported",
                                            f"{name} must live under /usr to run in the sandbox",
                                            {name: self.binaries[name]})
            self.auth_file = self.resolve_auth(args.auth_file)
        self.redactor = Redactor(os.environ[name] for name in args.provider_env
                                 if SECRET_NAME.search(name) and name in os.environ)

    @staticmethod
    def resolve_auth(value):
        if value == "none":
            return None
        if value is None:
            home = os.environ.get("CODEX_HOME") or os.path.expanduser("~/.codex")
            value = os.path.join(home, "auth.json")
        try:
            info = os.lstat(value)
        except FileNotFoundError:
            raise CapabilityRefusal("provider_auth_unavailable",
                                    f"provider credential store not found: {value}") from None
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or \
                info.st_mode & (stat.S_IWGRP | stat.S_IWOTH):
            raise CapabilityRefusal("provider_auth_unsafe",
                                    "auth file must be a regular file owned by you and not "
                                    "group/world-writable (STD-05 R9)", {"path": value})
        return value

    # -- path views -------------------------------------------------------
    def inside(self, key):
        host = {"repo": self.repo, "runtime": self.runtime, "state": self.state}[key]
        return SANDBOX[key] if self.contained else str(host)

    def state_in(self, *names):
        return "/".join([self.inside("state"), *names])

    def codex_in(self):
        if not self.contained:
            return self.binaries["codex"]
        relative = os.path.relpath(self.binaries["codex"], self.codex_root)
        return f"{SANDBOX['codex']}/{relative}"

    def tool_in(self, name):
        host = self.binaries[name]
        if host is None or not self.contained or name in ("git", "python"):
            return host
        return f"{SANDBOX['bin']}/{name.replace('_', '-')}"

    def forbidden(self):
        home = os.path.expanduser("~")
        codex_home = os.environ.get("CODEX_HOME") or os.path.join(home, ".codex")
        paths = list(self.args.truth_path) + list(self.args.forbid) + [
            str(HERE.parent.parent), os.path.abspath(self.args.head),
            os.path.abspath(self.args.base) if getattr(self.args, "base", None) else None,
            str(self.out.parent), home, codex_home]
        return sorted({os.path.abspath(path) for path in paths if path})

    def env(self):
        env = {"PATH": "/usr/bin:/bin", "HOME": self.state_in("home"),
               "CODEX_HOME": self.state_in("codex-home"), "TMPDIR": self.state_in("tmp"),
               "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8", "TERM": "dumb", "NO_COLOR": "1"}
        for name in self.args.provider_env:
            env[name] = os.environ[name]
        return env

    def sandbox_prefix(self):
        if not self.contained:
            return []
        argv = system_sandbox_argv(self.binaries["bwrap"])
        argv += ["--dir", "/eval", "--dir", SANDBOX["bin"],
                 "--ro-bind", str(self.repo), SANDBOX["repo"]]
        if self.arm == "graph":
            argv += ["--bind", str(self.repo / ".orbit-graph"), f"{SANDBOX['repo']}/.orbit-graph"]
        argv += ["--ro-bind", str(self.runtime), SANDBOX["runtime"],
                 "--bind", str(self.state), SANDBOX["state"],
                 "--ro-bind", str(self.codex_root), SANDBOX["codex"],
                 "--ro-bind", self.binaries["rg"], self.tool_in("rg")]
        if self.arm == "graph":
            argv += ["--ro-bind", self.binaries["orbit_graph"], self.tool_in("orbit_graph")]
        if self.auth_file:
            argv += ["--ro-bind", self.auth_file, f"{SANDBOX['state']}/codex-home/auth.json"]
        return argv + ["--chdir", SANDBOX["repo"], "--"]

    def codex_config(self):
        """Flags shared by preflight and exec, so the verified surface is the run surface."""
        flags = []
        for feature in self.disabled:
            flags += ["--disable", feature]
        server = f"mcp_servers.{BROKER_SERVER}"
        tool_timeout = max(1, self.request["limits"]["wall_ms"] // 1000)
        flags += ["-c", 'web_search="disabled"', "-c", 'approval_policy="never"',
                  "-c", f"{server}.command={json.dumps(self.binaries['python'])}",
                  "-c", f"{server}.args={json.dumps(self.broker_args())}",
                  "-c", f"{server}.startup_timeout_sec=30",
                  "-c", f"{server}.tool_timeout_sec={tool_timeout}",
                  "-c", f"{server}.required=true",
                  "-c", f"{server}.enabled_tools={json.dumps(self.request['tools'])}",
                  "-c", f"{server}.default_tools_approval_mode={json.dumps(DEFAULT_TOOL_APPROVAL)}"]
        # Tool names are the validated arm contract, so they are safe TOML keys.
        for tool in self.request["tools"]:
            flags += ["-c", f"{server}.tools.{tool}.approval_mode={json.dumps(TOOL_APPROVAL)}"]
        for key, value in sorted(self.settings().items()):
            flags += ["-c", f"{key}={json.dumps(value)}"]
        return flags

    def broker_args(self):
        runtime = self.inside("runtime")
        return [f"{runtime}/eval_broker.py", "--config", f"{runtime}/broker-config.json"]

    def settings(self):
        settings = {}
        for item in self.args.setting:
            key, separator, value = item.partition("=")
            require(separator and key in SETTING_KEYS and SETTING_VALUE.match(value),
                    f"--setting must be KEY=VALUE with KEY in {SETTING_KEYS}")
            require(key not in settings, f"--setting {key} given twice")
            settings[key] = value
        return settings

    def recorded_settings(self):
        """Provider settings as attributed in the artifact; code mode changes the tool path."""
        settings = self.settings()
        if self.code_mode:
            settings[CODE_MODE_FEATURE] = "enabled"
        return settings

    def exec_argv(self):
        return [self.codex_in(), "exec", "--json", "--ephemeral", "--ignore-user-config",
                "--ignore-rules", "--skip-git-repo-check", "--color", "never",
                "-C", self.inside("repo"), "-s", "read-only", "-m", self.args.model,
                "--output-last-message", self.state_in("final-message.txt"),
                *self.codex_config(), "-"]


def prepare_layout(episode):
    require(not os.path.lexists(episode.out), f"output already exists: {episode.out}")
    require(episode.out.parent.is_dir(), f"output parent must exist: {episode.out.parent}")
    make_private_dir(episode.out)
    for path in (episode.out / "snapshot", episode.runtime, episode.state,
                 episode.state / "home", episode.state / "codex-home", episode.state / "tmp"):
        make_private_dir(path)
    if episode.auth_file:
        # The mount point for the read-only bind of the provider's own store.
        os.close(os.open(episode.state / "codex-home" / "auth.json",
                         os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600))
    shutil.copyfile(HERE / "eval_broker.py", episode.runtime / "eval_broker.py")
    os.chmod(episode.runtime / "eval_broker.py", 0o500)


def preflight(episode, versions):
    """Read back the effective feature and MCP inventory under the exact run flags."""
    env = episode.env()
    prefix = episode.sandbox_prefix()
    started = time.monotonic()
    evidence = {"sandbox_argv": prefix, "env_names": sorted(env)}

    def run(args, expect_failure=False):
        """Codex stdout; with expect_failure, (exit code, stderr) of a codex-level failure."""
        argv = prefix + [episode.codex_in(), *args]
        result = broker.run_child(argv, str(episode.state), env, PREFLIGHT_TIMEOUT_S,
                                  capture_limit=1024 * 1024)
        stderr = result["stderr"].decode(errors="replace")
        sandbox_failed = episode.contained and "bwrap:" in stderr[:200]
        if expect_failure and not result["stopped"] and not sandbox_failed:
            return result["exit_code"], stderr
        if result["exit_code"] != 0 or result["stopped"]:
            if sandbox_failed:
                raise CapabilityRefusal("containment_unavailable",
                                        "bubblewrap could not create the sandbox",
                                        dict(evidence, stderr=stderr[:2000],
                                             exit_code=result["exit_code"]))
            raise CapabilityRefusal("provider_preflight_failed",
                                    f"codex {' '.join(args[:2])} failed",
                                    dict(evidence, stderr=stderr[:2000],
                                         exit_code=result["exit_code"]))
        return result["stdout"].decode(errors="replace")

    shared = episode.codex_config()
    features = {}
    for line in run(["features", "list", *shared]).splitlines():
        match = re.match(r"\A(\S+)\s+(.+?)\s+(true|false)\s*\Z", line)
        if match:
            features[match.group(1)] = match.group(3) == "true"
    evidence["features"] = features
    unknown = [name for name in episode.disabled if name not in features]
    if unknown:
        raise CapabilityRefusal("provider_feature_unknown",
                                "codex no longer reports features the runner must disable",
                                dict(evidence, unknown=unknown))
    sticky = [name for name in episode.disabled if features[name]]
    if sticky:
        raise CapabilityRefusal("provider_feature_not_disableable",
                                "codex still reports ambient capabilities enabled",
                                dict(evidence, enabled=sticky))
    servers = parse_json(run(["mcp", "list", "--json", *shared]).encode(), "codex mcp list")
    evidence["mcp_servers"] = servers
    expected = {"type": "stdio", "command": episode.binaries["python"],
                "args": episode.broker_args()}
    ok = (isinstance(servers, list) and len(servers) == 1 and isinstance(servers[0], dict)
          and servers[0].get("name") == BROKER_SERVER and servers[0].get("enabled") is True
          and isinstance(servers[0].get("transport"), dict)
          and all(servers[0]["transport"].get(key) == value for key, value in expected.items()))
    if not ok:
        raise CapabilityRefusal("mcp_inventory_mismatch",
                                f"the effective MCP inventory must be exactly {BROKER_SERVER}",
                                evidence)
    evidence["tool_approvals"] = verify_tool_approvals(episode, run, shared, evidence)
    evidence.update(shell_gated={name: features.get(name) for name in SHELL_GATED_FEATURES},
                    enabled_features=sorted(name for name, on in features.items() if on),
                    disabled_verified=list(episode.disabled),
                    code_mode_allowed=episode.code_mode, versions=versions,
                    elapsed_ms=int((time.monotonic() - started) * 1000))
    return evidence


def verify_tool_approvals(episode, run, shared, evidence):
    """Read back the broker's tool inventory and approvals as far as codex exposes them."""
    tools = list(episode.request["tools"])
    server = parse_json(run(["mcp", "get", BROKER_SERVER, "--json", *shared]).encode(),
                        "codex mcp get")
    evidence["mcp_server"] = server
    if not isinstance(server, dict) or server.get("enabled_tools") != tools or \
            server.get("disabled_tools") not in (None, []):
        raise CapabilityRefusal("mcp_tool_inventory_mismatch",
                                f"{BROKER_SERVER} must enable exactly the {episode.arm} arm's "
                                f"tools {tools}", evidence)
    shown = re.search(r"^\s*default_tools_approval_mode:\s*(\S+)\s*$",
                      run(["mcp", "get", BROKER_SERVER, *shared]), re.MULTILINE)
    if not shown or shown.group(1) != DEFAULT_TOOL_APPROVAL:
        raise CapabilityRefusal("provider_tool_approval_unverified",
                                f"codex did not read back default_tools_approval_mode="
                                f"{DEFAULT_TOOL_APPROVAL} for {BROKER_SERVER}",
                                dict(evidence, readback=shown.group(1) if shown else None))
    # No per-tool readback exists: an invalid value at the same key must be refused
    # by name, or codex is ignoring the key and every broker call would be denied.
    key = f"mcp_servers.{BROKER_SERVER}.tools.{tools[0]}.approval_mode"
    code, stderr = run(["mcp", "get", BROKER_SERVER, "--json", *shared,
                        "-c", f"{key}={json.dumps(APPROVAL_PROBE_VALUE)}"], expect_failure=True)
    if code == 0 or APPROVAL_PROBE_VALUE not in stderr or key not in stderr:
        raise CapabilityRefusal("provider_tool_approval_unsupported",
                                f"codex does not validate {key}; per-tool approvals cannot be "
                                "shown to take effect",
                                dict(evidence, probe_exit_code=code, probe_stderr=stderr[:2000]))
    return {"approval_policy": "never", "default_tools_approval_mode": shown.group(1),
            "approval_mode": TOOL_APPROVAL, "approved_tools": tools,
            "enabled_tools": server["enabled_tools"], "per_tool_key_validated": key,
            "per_tool_readback": None}


def tool_versions(episode):
    env = {"PATH": "/usr/bin:/bin", "HOME": str(episode.state / "home"), "LC_ALL": "C.UTF-8",
           "CODEX_HOME": str(episode.state / "codex-home")}
    broker_sha = sha256_file(HERE / "eval_broker.py")
    rg_version = probe_version([episode.binaries["rg"], "--version"], env).split(" (")[0]
    versions = {
        "read": f"{broker.BROKER_NAME} {broker.BROKER_VERSION} sha256:{broker_sha}",
        "rg": f"{rg_version} sha256:{sha256_file(episode.binaries['rg'])}",
        "git": f"{probe_version([episode.binaries['git'], '--version'], env)} "
               f"sha256:{sha256_file(episode.binaries['git'])}",
    }
    if episode.arm == "graph":
        graph = probe_version([episode.binaries["orbit_graph"], "version", "--json"], env)
        versions["orbit-graph"] = f"{graph} sha256:{sha256_file(episode.binaries['orbit_graph'])}"
    provider = {"version": probe_version([episode.binaries["codex"], "--version"], env),
                "sha256": sha256_file(episode.binaries["codex"])}
    if episode.contained:
        provider["bwrap"] = {"version": probe_version([episode.binaries["bwrap"], "--version"],
                                                      env),
                             "sha256": sha256_file(episode.binaries["bwrap"])}
    return versions, provider, broker_sha


# --------------------------------------------------------------------------
# Supervision of the provider process.


class BoundedReader(threading.Thread):
    """Drain a pipe concurrently with the wait; keep at most `limit` bytes."""

    def __init__(self, stream, limit, overflow=None):
        super().__init__(daemon=True)
        self.stream = stream
        self.limit = limit
        self.overflow = overflow
        self.data = bytearray()
        self.truncated = False

    def run(self):
        fd = self.stream.fileno()
        while True:
            try:
                chunk = os.read(fd, 65536)
            except OSError:
                break
            if not chunk:
                break
            room = self.limit - len(self.data)
            if room > 0:
                self.data.extend(chunk[:room])
            if len(chunk) > room:
                self.truncated = True
                if self.overflow is not None:
                    self.overflow.set()


def supervise(argv, env, cwd, prompt, deadline, sentinel):
    process = subprocess.Popen(argv, cwd=cwd, env=env, stdin=subprocess.PIPE,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               start_new_session=True)
    overflow = threading.Event()
    stdout = BoundedReader(process.stdout, MAX_PROVIDER_STDOUT, overflow)
    stderr = BoundedReader(process.stderr, MAX_PROVIDER_STDERR)
    stdout.start()
    stderr.start()

    def feed():
        try:
            process.stdin.write(prompt.encode())
            process.stdin.close()
        except (BrokenPipeError, OSError):
            pass
    writer = threading.Thread(target=feed, daemon=True)
    writer.start()
    reason = None
    while True:
        if broker.wait_leader(process.pid, 0.05):
            break
        if time.monotonic() >= deadline:
            reason = "timeout"
            break
        if os.path.exists(sentinel):
            reason = "budget"
            break
        if overflow.is_set():
            reason = "provider_output_truncated"
            break
    ended = time.monotonic()
    supervision = broker.end_group(process, terminate=reason is not None,
                                   grace_s=TERMINATION_GRACE_S)
    joined = True
    for thread in (stdout, stderr, writer):
        thread.join(timeout=TERMINATION_GRACE_S)
        joined = joined and not thread.is_alive()
    for stream in (process.stdout, process.stderr):
        try:
            stream.close()
        except OSError:
            pass
    returncode = process.returncode
    return {"returncode": returncode,
            "exit": ({"code": returncode} if returncode >= 0 else
                     {"signal": signal.Signals(-returncode).name}),
            "terminated_by": reason, "ended": ended, "pipes_closed": joined,
            "signals": supervision["signals"], "survivors": supervision["survivors"],
            "stdout": bytes(stdout.data), "stdout_truncated": stdout.truncated,
            "stderr": bytes(stderr.data), "stderr_truncated": stderr.truncated}


def sweep_broker(starts):
    """Kill a broker that escaped the provider's group, by PID plus start identity."""
    swept = []
    own_namespace = os.readlink("/proc/self/ns/pid")
    for identity in starts:
        if identity.get("pid_namespace") != own_namespace:
            continue  # a sandbox PID namespace; it died with the sandbox
        pid = identity.get("pid")
        try:
            with open(f"/proc/{pid}/stat", "rb") as stream:
                fields = stream.read().rsplit(b")", 1)[1].split()
        except (OSError, IndexError, TypeError):
            continue
        if int(fields[19]) == identity.get("start_ticks") and fields[0] not in (b"Z", b"X"):
            try:
                os.kill(pid, signal.SIGKILL)
                swept.append(pid)
            except ProcessLookupError:
                pass
    return swept


# --------------------------------------------------------------------------
# Transcript, broker log and answer analysis.


def audit_mcp_item(kind, item, pending, completed, summary):
    """Bind lifecycle events by provider ID; keep compact exact-match fingerprints.

    Provider IDs are not broker request IDs. They detect replayed or altered
    lifecycle events; tool/arguments/status/result match the broker independently.
    Fingerprints avoid duplicating bounded tool payloads in the audit summary.
    """
    def invalid(message):
        if len(summary["call_audit_errors"]) < 50:
            summary["call_audit_errors"].append(message)

    identity = item.get("id")
    try:
        valid_id = isinstance(identity, str) and bool(identity) and len(identity.encode()) <= 128
    except UnicodeError:
        valid_id = False
    if not valid_id:
        invalid("provider call id must be a non-empty string of at most 128 bytes")
        return
    label = f"provider call {identity!r} ({item['tool']})"
    if not isinstance(item.get("arguments"), dict):
        invalid(f"{label}: missing or malformed arguments")
        return
    try:
        key = (item["tool"], digest(item["arguments"]))
    except (ValueError, RecursionError):
        invalid(f"{label}: arguments cannot be canonicalized")
        return
    if kind == "item.started":
        if identity in pending or identity in completed:
            invalid(f"{label}: duplicate start id")
        elif item.get("status") != "in_progress":
            invalid(f"{label}: invalid started status")
        else:
            pending[identity] = key
        return
    if kind == "item.completed":
        if identity in completed:
            invalid(f"{label}: duplicate completion id")
            return
        completed.add(identity)
        started = pending.pop(identity, None)
    elif kind == "item.updated":
        started = pending.get(identity)
    else:
        invalid(f"{label}: unexpected lifecycle event {kind}")
        return
    if started is None:
        invalid(f"{label}: unexpected event without a pending start")
    elif started != key:
        invalid(f"{label}: tool or arguments differ from its start")
    if kind != "item.completed":
        return
    status, result = item.get("status"), item.get("result")
    if status not in ("completed", "failed"):
        invalid(f"{label}: invalid completed status")
        return
    if item.get("error") is not None:
        invalid(f"{label}: provider error has no broker result")
        return
    content = result.get("content") if isinstance(result, dict) else None
    if not isinstance(content, list) or len(content) != 1 or \
            not isinstance(content[0], dict) or content[0].get("type") != "text" or \
            not isinstance(content[0].get("text"), str):
        invalid(f"{label}: missing or malformed text result")
        return
    if "isError" in result and (type(result["isError"]) is not bool or
                                 result["isError"] != (status == "failed")):
        invalid(f"{label}: result isError disagrees with completed status")
        return
    if result.get("structured_content") is not None or result.get("structuredContent") is not None:
        invalid(f"{label}: unexpected structured result")
        return
    try:
        output_hash = sha256_bytes(content[0]["text"].encode())
    except UnicodeError:
        invalid(f"{label}: result is not valid UTF-8 text")
        return
    summary["mcp_call_audit"].append((*key, status, output_hash))


def analyse_transcript(raw, truncated, tools):
    """Audit provider JSONL; `tools` are the arm's broker tools, the only permitted calls."""
    lines = raw.split(b"\n")
    partial = lines.pop() if lines else b""
    summary = {"events": 0, "event_types": {}, "item_types": {}, "malformed_lines": 0,
               "partial_line": bool(partial), "truncated": truncated, "turns_started": 0,
               "turns_completed": 0, "turn_failures": [], "errors": [], "thread_id": None,
               "unbrokered": [], "mcp_calls": [], "approval_denied": [], "last_agent_message": None,
               "usage": None, "mcp_call_audit": [], "call_audit_errors": []}
    pending, completed = {}, set()
    usage = {}
    for line in lines:
        if not line.strip():
            continue
        try:
            event = json.loads(line, object_pairs_hook=broker._unique,
                               parse_constant=broker._reject_constant)
        except (ValueError, RecursionError):
            summary["malformed_lines"] += 1
            continue
        if not isinstance(event, dict):
            summary["malformed_lines"] += 1
            continue
        summary["events"] += 1
        kind = str(event.get("type"))[:64]
        summary["event_types"][kind] = summary["event_types"].get(kind, 0) + 1
        if kind == "thread.started":
            summary["thread_id"] = str(event.get("thread_id"))[:128]
        elif kind == "turn.started":
            summary["turns_started"] += 1
        elif kind == "turn.completed":
            summary["turns_completed"] += 1
            reported = event.get("usage")
            if isinstance(reported, dict):
                for key, value in reported.items():
                    if type(value) is int and value >= 0:
                        usage[key] = usage.get(key, 0) + value
        elif kind == "turn.failed":
            error = event.get("error")
            message = error.get("message") if isinstance(error, dict) else error
            summary["turn_failures"].append(str(message)[:1000])
        elif kind == "error":
            if len(summary["errors"]) < 50:
                summary["errors"].append(str(event.get("message"))[:1000])
        elif kind.startswith("item."):
            item = event.get("item")
            if not isinstance(item, dict):
                summary["malformed_lines"] += 1
                continue
            item_type = str(item.get("type"))[:64]
            summary["item_types"][item_type] = summary["item_types"].get(item_type, 0) + 1
            # Codex also routes its native MCP resource tools (read_mcp_resource, ...)
            # through a server name, so a broker item must name a broker tool too.
            violation = item_type not in ALLOWED_ITEM_TYPES or (
                item_type == "mcp_tool_call" and (item.get("server") != BROKER_SERVER
                                                  or item.get("tool") not in tools))
            if violation and len(summary["unbrokered"]) < 50:
                summary["unbrokered"].append({"event": kind, "type": item_type,
                                              "server": str(item.get("server"))[:64]
                                              if "server" in item else None,
                                              "tool": str(item.get("tool"))[:64]
                                              if "tool" in item else None})
            if kind == "item.completed" and item_type == "error" and \
                    len(summary["errors"]) < 50:
                summary["errors"].append(str(item.get("message"))[:1000])
            if kind == "item.completed" and item_type == "agent_message":
                text = item.get("text")
                summary["last_agent_message"] = text if isinstance(text, str) else None
            if item_type == "mcp_tool_call" and not violation:
                audit_mcp_item(kind, item, pending, completed, summary)
            if kind == "item.completed" and item_type == "mcp_tool_call" and not violation:
                summary["mcp_calls"].append({"tool": str(item.get("tool"))[:64],
                                             "status": str(item.get("status"))[:32]})
                error = item.get("error")
                message = str(error.get("message") if isinstance(error, dict) else error)
                if APPROVAL_DENIED in message and len(summary["approval_denied"]) < 50:
                    summary["approval_denied"].append({"tool": str(item.get("tool"))[:64],
                                                       "message": message[:300]})
    for identity, (tool, _) in pending.items():
        if len(summary["call_audit_errors"]) >= 50:
            break
        summary["call_audit_errors"].append(f"incomplete provider call {identity!r} ({tool})")
    if usage:
        summary["usage_raw"] = usage
        if "input_tokens" in usage or "output_tokens" in usage:
            summary["usage"] = {"input_tokens": usage.get("input_tokens"),
                                "output_tokens": usage.get("output_tokens"),
                                "cost_usd": None, "source": USAGE_SOURCE}
    return summary


def reconcile_calls(transcript, calls):
    """One-to-one multiset match, independent of either stream's completion order.

    Broker measurements remain authoritative and unchanged. Identical calls
    must match with their full multiplicity; a tool-name set is insufficient.
    """
    if transcript["call_audit_errors"]:
        return "; ".join(transcript["call_audit_errors"][:5])
    expected = Counter()
    for index, call in enumerate(calls):
        try:
            arguments = parse_json(call["input"].encode(), "broker call input")
            require(call["status"] in {"ok", "failed", "timeout", "truncated"}
                    and isinstance(call["output"], str), "invalid broker call status or output")
            key = (call["tool"], digest(arguments),
                   "completed" if call["status"] == "ok" else "failed",
                   sha256_bytes(call["output"].encode()))
            expected[key] += 1
        except (Invalid, AttributeError, TypeError, ValueError, RecursionError):
            return f"broker call {index + 1}: malformed measured input, status or output"
    reported = Counter(transcript["mcp_call_audit"])
    missing, unexpected = expected - reported, reported - expected
    if missing or unexpected:
        missing_tools = sorted({key[0] for key in missing})
        unexpected_tools = sorted({key[0] for key in unexpected})
        return f"unmatched calls: missing {sum(missing.values())} broker call(s) {missing_tools}; " \
            f"unexpected {sum(unexpected.values())} provider call(s) {unexpected_tools}; " \
            "tool, arguments, status and text result must match exactly"
    return None


def read_broker_log(path):
    log = {"starts": [], "initialized": False, "tools_listed": [], "calls": [], "refusals": [],
           "budget": [], "protocol_errors": 0, "malformed_lines": 0, "truncated": False,
           "supervisors": [], "exits": []}
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
    except FileNotFoundError:
        return log, b""
    with os.fdopen(fd, "rb") as stream:
        data = stream.read(MAX_BROKER_LOG + 1)
    if len(data) > MAX_BROKER_LOG:
        data, log["truncated"] = data[:MAX_BROKER_LOG], True
    if data and not data.endswith(b"\n"):
        log["malformed_lines"] += 1
    for line in data.splitlines():
        try:
            entry = json.loads(line, object_pairs_hook=broker._unique,
                               parse_constant=broker._reject_constant)
        except (ValueError, RecursionError):
            entry = None
        if not isinstance(entry, dict):
            log["malformed_lines"] += 1
            continue
        kind = entry.get("type")
        if kind == "start":
            log["starts"].append(entry.get("identity") or {})
        elif kind == "supervisor_start":
            log["supervisors"].append(entry.get("identity") or {})
        elif kind == "broker_exit":
            cleanup = entry.get("cleanup")
            if (type(entry.get("exit_code")) not in (int, type(None))
                    or not isinstance(cleanup, dict)
                    or not isinstance(entry.get("stderr"), str)
                    or len(entry["stderr"].encode()) > 3 * broker.MAX_STDERR_CAPTURE
                    or type(entry.get("stderr_truncated")) is not bool):
                log["malformed_lines"] += 1
                continue
            log["exits"].append({key: entry.get(key) for key in
                                 ("exit_code", "stopped", "error_type", "stderr",
                                  "stderr_truncated", "cleanup")})
        elif kind == "initialize":
            log["initialized"] = True
        elif kind == "tools_list":
            log["tools_listed"].append(entry.get("tools"))
        elif kind == "call":
            log["calls"].append({key: entry.get(key) for key in
                                 ("tool", "input", "output", "elapsed_ms", "status")})
        elif kind == "refusal":
            log["refusals"].append({"tool": entry.get("tool"), "code": entry.get("code")})
        elif kind == "budget":
            log["budget"].append({"code": entry.get("code"), "message": entry.get("message")})
        elif kind == "protocol_error":
            log["protocol_errors"] += 1
    return log, data


def state_object(raw, label):
    """A JSON object from the writable state mount, or None if absent or malformed."""
    try:
        value = parse_json(raw, label) if raw else None
    except Invalid:
        return None
    return value if isinstance(value, dict) else None


def read_bounded(path, limit):
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
    except FileNotFoundError:
        return None
    with os.fdopen(fd, "rb") as stream:
        return stream.read(limit + 1)


def check_answer(answer, limits):
    """The public answer shape (no truth): exactly items/abstain/reason/evidence."""
    require(isinstance(answer, dict) and set(answer) == {"items", "abstain", "reason",
                                                         "evidence"},
            "answer must have exactly items, abstain, reason, evidence")
    items, evidence = answer["items"], answer["evidence"]
    require(type(answer["abstain"]) is bool, "abstain must be a boolean")
    require(isinstance(answer["reason"], str) and len(answer["reason"].encode()) <= 4096,
            "reason must be a string of at most 4096 bytes")
    require(isinstance(items, list) and len(items) <= limits["answer_items"]
            and all(isinstance(item, str) for item in items) and len(set(items)) == len(items),
            "items must be unique strings within answer_items")
    require(isinstance(evidence, list) and len(evidence) <= limits["answer_items"],
            "evidence must be a bounded list")
    for citation in evidence:
        require(isinstance(citation, dict) and set(citation) == {"item", "file", "line", "quote"},
                "citation must have exactly item, file, line, quote")
        require(isinstance(citation["item"], str) and isinstance(citation["file"], str)
                and type(citation["line"]) is int and citation["line"] >= 1
                and isinstance(citation["quote"], str), "citation field types are invalid")
    if answer["abstain"]:
        require(not items and not evidence and answer["reason"].strip(),
                "an abstention needs empty items/evidence and a reason")
    else:
        require(items and evidence, "a non-abstaining answer needs items and evidence")


def decide(supervision, transcript, log, final, limits, sentinel_code, output_bytes, wall_ms):
    """First matching failure wins; only a clean, complete, well-formed run is ok."""
    calls = log["calls"]
    if supervision["terminated_by"] == "timeout":
        return "timeout", "wall_time_exceeded", "the episode reached its wall-time limit"
    if sentinel_code or log["budget"]:
        code = sentinel_code or log["budget"][0]["code"]
        return "failed", code, "the broker stopped the episode on a budget violation"
    if supervision["stdout_truncated"] or transcript["truncated"]:
        return "failed", "provider_output_truncated", "provider JSONL exceeded its capture bound"
    if transcript["unbrokered"]:
        return "invalid", "unbrokered_tool_use", \
            f"provider used tools outside the broker: {transcript['unbrokered'][:5]}"
    if any(marker in error for error in transcript["errors"] for marker in CODE_MODE_DISABLED) \
            or any(marker.encode() in supervision["stderr"] for marker in CODE_MODE_DISABLED):
        return "invalid", "provider_code_mode_required", \
            "the provider routes tool calls through its code-mode host, which this run " \
            "disabled; no tool could run (see --allow-code-mode)"
    if transcript["approval_denied"]:
        denied = sorted({denial["tool"] for denial in transcript["approval_denied"]})
        return "invalid", "provider_tool_approval_required", \
            f"codex refused {len(transcript['approval_denied'])} broker call(s) ({denied}) " \
            "before they reached the broker: they still required approval under " \
            "approval_policy=never, so the per-tool approvals did not take effect"
    if not log["initialized"] or not log["tools_listed"]:
        return "failed", "broker_not_initialized", "the provider never listed the broker tools"
    if any(listed != log["tools_listed"][0] for listed in log["tools_listed"]):
        return "failed", "tool_inventory_mismatch", "the broker served differing tool lists"
    if transcript["malformed_lines"] or transcript["partial_line"]:
        return "failed", "provider_output_malformed", "provider JSONL contains malformed lines"
    if log["truncated"] or log["malformed_lines"]:
        return "failed", "broker_output_malformed", "broker JSONL is malformed or truncated"
    mismatch = reconcile_calls(transcript, calls)
    if mismatch:
        return "failed", "telemetry_mismatch", mismatch
    if any(entry["exit_code"] != 0 or entry["stopped"] is not None or
           (entry["cleanup"] or {}).get("survivors") != [] for entry in log.get("exits", [])):
        return "failed", "broker_failed", "the broker worker did not exit cleanly"
    if log.get("supervisors") and len(log.get("exits", [])) != len(log["supervisors"]):
        return "failed", "broker_exit_missing", "broker supervision ended without exit evidence"
    for call in calls:
        if call["status"] in ("failed", "timeout"):
            try:
                body = parse_json(call["output"].encode(), "failed call")
            except Invalid:
                return "failed", "broker_output_malformed", "failed call output is not JSON"
            # An ordinary input refusal remains an agent-visible recoverable error.
            # Execution failures must not become successful episodes after a final answer.
            if not isinstance(body, dict) or not isinstance(body.get("error", {}), dict):
                return "failed", "broker_output_malformed", "failed call output is not an envelope"
            if "exit_code" in body or body.get("error", {}).get("code") == "tool_exception":
                return "failed", "tool_execution_failed", "a broker tool could not execute cleanly"
    if supervision["returncode"] != 0 or transcript["turns_completed"] < 1 or \
            transcript["turn_failures"]:
        return "failed", "provider_incomplete", \
            f"provider exit {supervision['exit']} without a completed turn " \
            f"({transcript['turn_failures'][:1] or transcript['errors'][:1]})"
    if final["text"] is None or not final["text"].strip():
        return "failed", "final_answer_missing", "the provider produced no final message"
    if final["truncated"]:
        return "invalid", "final_output_oversized", \
            f"final message exceeds {limits['call_bytes']} bytes"
    try:
        check_answer(parse_json(final["text"].encode(), "final answer"), limits)
    except Invalid as error:
        return "invalid", "answer_malformed", str(error)[:1000]
    if not calls:
        return "invalid", "no_tool_calls", "an ok episode needs at least one captured tool call"
    if any(call["status"] == "truncated" for call in calls):
        return "failed", "call_output_truncated", "a tool call output was truncated"
    if output_bytes > limits["output_bytes"]:
        return "failed", "output_budget_exceeded", "captured output exceeds the episode budget"
    if wall_ms > limits["wall_ms"]:
        return "timeout", "wall_time_exceeded", "the episode exceeded its wall-time limit"
    return "ok", None, None


def seal(record, field):
    record.pop(field, None)
    record[field] = digest(record)
    return record


# --------------------------------------------------------------------------
# Commands.


def refuse(episode_out, refusal, extra=None):
    report = {"status": "refused", "provider_started": False,
              "refusal": {"code": refusal.code, "message": refusal.message,
                          "evidence": refusal.evidence}}
    if extra:
        report.update(extra)
    if episode_out is not None and os.path.isdir(episode_out) and \
            not os.path.lexists(os.path.join(episode_out, "refusal.json")):
        redactor = Redactor([])
        report["refusal"], _ = redactor.value(report["refusal"])
        write_private(os.path.join(episode_out, "refusal.json"),
                      json.dumps(report, indent=2, sort_keys=True) + "\n", redactor)
    return report


def command_revision(args):
    entries = tree_entries(args.tree, "tree")
    return content_revision(entries, "tree")


def setup(args):
    request = validate_request(load_json(args.request))
    for name in args.provider_env:
        require(ENV_NAME.match(name) and not name.startswith("ORBIT_"),
                f"--provider-env {name}: invalid or privileged name (STD-05 R11)")
        require(name in os.environ, f"--provider-env {name} is not set")
    require(MODEL_NAME.match(args.model), "--model has an unsupported form")
    if args.containment == "bwrap":
        require(args.truth_path, "--containment bwrap needs at least one --truth-path to prove "
                                 "inaccessible")
    return request


def write_broker_config(episode, deadline_unix_ms, snapshot):
    limits = episode.request["limits"]
    config = {"schema_version": 1, "arm": episode.arm, "tools": episode.request["tools"],
              "repo_root": episode.inside("repo"),
              "log_path": episode.state_in("broker-calls.jsonl"),
              "sentinel_path": episode.state_in("budget-exceeded.json"),
              "home": episode.state_in("home"), "tmp": episode.state_in("tmp"),
              "path_env": "/usr/bin:/bin", "deadline_unix_ms": deadline_unix_ms,
              "limits": limits,
              "binaries": {"rg": episode.tool_in("rg"), "git": episode.tool_in("git"),
                           "orbit_graph": episode.tool_in("orbit_graph")},
              "snapshot": {"base_commit": snapshot["base_commit"],
                           "head_commit": snapshot["head_commit"]}}
    path = episode.runtime / "broker-config.json"
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o400)
    with os.fdopen(fd, "w") as stream:
        stream.write(json.dumps(config, indent=2, sort_keys=True) + "\n")
    return config


def write_probe_spec(episode):
    spec = {"forbidden": episode.forbidden(),
            "expect_visible": [episode.inside("repo"), episode.inside("runtime"),
                               episode.inside("state"), episode.codex_in()],
            "output": episode.state_in("probe.json"), "require_hidden": episode.contained}
    fd = os.open(episode.runtime / "probe-spec.json",
                 os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o400)
    with os.fdopen(fd, "w") as stream:
        stream.write(json.dumps(spec, indent=2, sort_keys=True) + "\n")
    return spec


def command_preflight(args):
    request = setup(args)
    args.base = None
    episode = Episode(args, request)
    prepare_layout(episode)
    if episode.arm == "graph":
        make_private_dir(episode.repo)
        make_private_dir(episode.repo / ".orbit-graph")
    else:
        make_private_dir(episode.repo)
    versions, provider, _ = tool_versions(episode)
    evidence = preflight(episode, versions)
    evidence["provider"] = provider
    write_private(episode.out / "preflight.json",
                  json.dumps(evidence, indent=2, sort_keys=True) + "\n", episode.redactor)
    return {"status": "ready", "containment": args.containment, "arm": episode.arm,
            "provider": provider, "disabled_verified": evidence["disabled_verified"],
            "mcp_servers": [server.get("name") for server in evidence["mcp_servers"]],
            "tool_approvals": evidence["tool_approvals"], "out": str(episode.out)}


def command_run(args):
    request = setup(args)
    episode = Episode(args, request)
    limits = request["limits"]
    prepare_layout(episode)
    versions, provider, broker_sha = tool_versions(episode)
    # The preflight runs against an empty repository mount, before the clock.
    make_private_dir(episode.repo)
    if episode.arm == "graph":
        make_private_dir(episode.repo / ".orbit-graph")
    preflight_evidence = preflight(episode, versions)
    if episode.arm == "graph":
        os.rmdir(episode.repo / ".orbit-graph")
    os.rmdir(episode.repo)

    clock = time.monotonic()
    started_at = time.time()
    deadline = clock + limits["wall_ms"] / 1000
    head_entries = tree_entries(args.head, "head")
    base_entries = tree_entries(args.base, "base")
    snapshot = materialize_snapshot(episode.repo, episode.state, episode.binaries["git"],
                                    base_entries, head_entries)
    if snapshot["head"]["content_revision"] != request["source_revision"]:
        raise Invalid("head tree does not match request.source_revision")
    if snapshot["base"]["content_revision"] != request["base_revision"]:
        raise Invalid("base tree does not match request.base_revision")
    if snapshot["work_tree_revision"] != request["source_revision"]:
        raise Invalid("checked-out snapshot differs from the head tree")
    graph_dir = episode.repo / ".orbit-graph"
    graph_fresh = not os.path.lexists(graph_dir)
    if episode.arm == "graph":
        make_private_dir(graph_dir)
    write_broker_config(episode, int((started_at + limits["wall_ms"] / 1000) * 1000), snapshot)
    probe_spec = write_probe_spec(episode)
    sandbox = episode.sandbox_prefix()
    argv = sandbox + [episode.binaries["python"],
                      f"{episode.inside('runtime')}/eval_broker.py",
                      "--launch", f"{episode.inside('runtime')}/probe-spec.json", "--",
                      *episode.exec_argv()]
    setup_ms = int((time.monotonic() - clock) * 1000)
    sentinel = episode.state / "budget-exceeded.json"
    supervision = supervise(argv, episode.env(), str(episode.state), request["prompt"],
                            deadline, sentinel)
    wall_ms = int((supervision["ended"] - clock) * 1000)
    probe_raw = read_bounded(episode.state / "probe.json", 4 * 1024 * 1024)
    probe = state_object(probe_raw, "probe")
    if probe is None or (episode.contained and not probe.get("hidden")):
        raise CapabilityRefusal(
            "containment_ineffective" if probe else "containment_probe_missing",
            "the in-sandbox probe did not prove forbidden paths inaccessible; the provider "
            "was not started" if probe else "the containment probe did not run",
            {"probe": probe, "exit": supervision["exit"],
             "stderr": supervision["stderr"].decode(errors="replace")[:2000]})

    pending_log, _ = read_broker_log(episode.state / "broker-calls.jsonl")
    # A provider may exit without waiting for its MCP servers. Give the pidfd
    # supervisor its bounded cleanup window before the last-resort identity sweep.
    cleanup_deadline = time.monotonic() + 3 * broker.KILL_GRACE_S + 1
    while len(pending_log["exits"]) < len(pending_log["supervisors"]):
        if time.monotonic() >= cleanup_deadline:
            break
        time.sleep(0.02)
        pending_log, _ = read_broker_log(episode.state / "broker-calls.jsonl")
    swept = sweep_broker(pending_log["starts"] + pending_log["supervisors"])
    log, log_bytes = read_broker_log(episode.state / "broker-calls.jsonl")
    transcript = analyse_transcript(supervision["stdout"], supervision["stdout_truncated"],
                                    request["tools"])
    final_raw = read_bounded(episode.state / "final-message.txt", 4 * limits["call_bytes"])
    final_source = "output_last_message"
    if final_raw is None and transcript["last_agent_message"] is not None:
        final_raw, final_source = transcript["last_agent_message"].encode(), "jsonl_agent_message"
    if final_raw is None:
        final_source = None
    final_text = final_raw.decode("utf-8", errors="replace") if final_raw is not None else None
    final_bounded = final_text
    final_truncated = False
    if final_text is not None and len(final_text.encode()) > limits["call_bytes"]:
        final_bounded = final_text.encode()[:limits["call_bytes"]].decode("utf-8", "ignore")
        final_truncated = True
    calls = log["calls"][:limits["tool_calls"]]
    output_bytes = sum(len(str(call["output"]).encode()) for call in calls) + \
        len((final_bounded or "").encode())
    sentinel_raw = read_bounded(sentinel, 64 * 1024)
    sentinel_code = None
    if sentinel_raw is not None:  # its existence is the stop; the code is detail
        sentinel_code = str((state_object(sentinel_raw, "sentinel") or {}).get("code")
                            or "budget_exceeded")[:64]
    status, code, message = decide(supervision, transcript, log,
                                   {"text": final_text, "truncated": final_truncated},
                                   limits, sentinel_code, output_bytes, wall_ms)
    answer = parse_json(final_text.encode(), "final") if status == "ok" else None
    graph_state = None
    if episode.arm == "graph":
        files = [path for path in graph_dir.rglob("*") if path.is_file()]
        graph_state = {"files": len(files), "bytes": sum(path.stat().st_size for path in files)}
    graph_sync_ms = sum(call["elapsed_ms"] for call in calls if call["tool"] == "graph_sync")

    files = {}
    redactor = episode.redactor
    files["provider.jsonl"] = write_private(episode.out / "provider.jsonl", supervision["stdout"],
                                            redactor, supervision["stdout_truncated"])
    files["provider-stderr.txt"] = write_private(episode.out / "provider-stderr.txt",
                                                 supervision["stderr"], redactor,
                                                 supervision["stderr_truncated"])
    files["broker-calls.jsonl"] = write_private(episode.out / "broker-calls.jsonl", log_bytes,
                                                redactor, log["truncated"])
    files["final-message.txt"] = write_private(episode.out / "final-message.txt",
                                               final_raw or b"", redactor, False)
    files["probe.json"] = write_private(episode.out / "probe.json", probe_raw, redactor)
    files["preflight.json"] = write_private(
        episode.out / "preflight.json",
        json.dumps(preflight_evidence, indent=2, sort_keys=True) + "\n", redactor)

    run_id = f"aeval-{secrets.token_hex(8)}"
    contained = episode.contained and bool(probe.get("hidden"))
    artifact = {
        "schema_version": 1, "kind": RAW_KIND, "runner_version": RUNNER_VERSION,
        "run_id": run_id, "request": request, "request_digest": digest(request),
        "started_at_unix": round(started_at, 3),
        "model": {"provider": "codex-cli", "name": args.model, "version": provider["version"],
                  "settings": episode.recorded_settings()},
        "provider": {"binary_sha256": provider["sha256"], "argv": episode.exec_argv(),
                     "env_names": sorted(episode.env()), "exit": supervision["exit"],
                     "thread_id": transcript["thread_id"]},
        "tool_versions": versions,
        "inputs": {"head": snapshot["head"], "base": snapshot["base"]},
        "snapshot": {key: snapshot[key] for key in ("base_commit", "head_commit", "base_tree",
                                                    "head_tree", "commit_count",
                                                    "commit_epoch", "work_tree_revision")},
        "isolation": {
            "containment": args.containment, "contained": contained,
            "truth_inaccessible": contained, "cold_start": graph_fresh,
            "repository_id": f"{run_id}:repository", "cache_id": f"{run_id}:graph-state"
            if episode.arm == "graph" else f"{run_id}:no-graph-state",
            "probe": {key: probe.get(key) for key in ("forbidden", "leaked", "missing",
                                                       "mount_count", "identity", "hidden")},
            "probe_spec_forbidden": probe_spec["forbidden"],
            "sandbox_argv": sandbox, "sandbox_argv_sha256": digest(sandbox),
            "bwrap": provider.get("bwrap"),
            "broker_sha256": broker_sha, "tools_listed": log["tools_listed"][:1],
            "refused_tool_names": log["refusals"][:50],
            "inventory": {"disabled_verified": preflight_evidence["disabled_verified"],
                          "code_mode_allowed": episode.code_mode,
                          "enabled_features": preflight_evidence["enabled_features"],
                          "shell_gated": preflight_evidence["shell_gated"],
                          "tool_approvals": preflight_evidence["tool_approvals"],
                          "mcp_servers": [server.get("name") for server in
                                          preflight_evidence["mcp_servers"]]},
            "unbrokered": transcript["unbrokered"],
            "graph_state_after": graph_state},
        "timing": {"wall_ms": wall_ms, "setup_ms": setup_ms,
                   "provider_ms": wall_ms - setup_ms, "graph_sync_ms": graph_sync_ms,
                   "preflight_ms": preflight_evidence["elapsed_ms"],
                   "wall_limit_ms": limits["wall_ms"]},
        "status": status,
        "error": None if status == "ok" else {"code": code, "message": message},
        "answer": answer, "final_output": final_bounded if final_bounded is not None else "",
        "final_output_truncated": final_truncated, "final_source": final_source,
        "calls": calls, "calls_dropped": max(0, len(log["calls"]) - len(calls)),
        "output_bytes": output_bytes, "usage": transcript["usage"],
        "usage_raw": transcript.get("usage_raw"),
        "transcript": {key: transcript[key] for key in
                       ("events", "event_types", "item_types", "malformed_lines",
                        "partial_line", "turns_started", "turns_completed", "turn_failures",
                        "errors", "mcp_calls", "approval_denied")},
        "broker": {"protocol_errors": log["protocol_errors"], "budget": log["budget"],
                   "malformed_lines": log["malformed_lines"], "starts": len(log["starts"]),
                   "exits": log["exits"]},
        "cleanup": {"terminated_by": supervision["terminated_by"],
                    "signals": supervision["signals"], "survivors": supervision["survivors"],
                    "pipes_closed": supervision["pipes_closed"], "brokers_swept": swept},
        "files": files,
        "limitations": [
            "Provider tool inventory cannot be listed before the run; disabled features and "
            "the MCP inventory are read back, and any non-broker item invalidates the episode.",
            "Per-tool approvals cannot be read back; codex is shown to parse the key, and any "
            "approval-gate refusal of a broker call invalidates the episode.",
            "Model revision is the provider-reported model name plus the codex CLI version.",
            "Usage is copied from provider telemetry; cost is never estimated.",
        ],
    }
    artifact, redactions = redactor.value(artifact)
    artifact["redactions"] = redactions
    if redactions:
        # Keep the record internally consistent with what it now contains.
        artifact["output_bytes"] = sum(len(str(call["output"]).encode())
                                       for call in artifact["calls"]) + \
            len(artifact["final_output"].encode())
    seal(artifact, "artifact_sha256")
    files_written = write_private(episode.out / "episode.json",
                                  json.dumps(artifact, indent=2, sort_keys=True) + "\n", redactor)
    require(files_written["redactions"] == 0, "sealed artifact changed during redaction")
    return {"status": status, "error": artifact["error"], "run_id": run_id,
            "contained": contained, "wall_ms": wall_ms, "tool_calls": len(calls),
            "output_bytes": output_bytes, "usage": artifact["usage"],
            "artifact": str(episode.out / "episode.json"),
            "artifact_sha256": artifact["artifact_sha256"]}


# --------------------------------------------------------------------------
# Adapter to the ORB-13710 public episode contract.

HARNESS_FAILURES = {"provider_code_mode_required", "provider_tool_approval_required"}
PUBLIC_FIELDS = ("request", "run_id", "model", "tool_versions", "isolation", "status", "error",
                 "answer", "final_output", "calls", "wall_ms", "output_bytes", "usage",
                 "record_sha256")


def check_public_record(record):
    """Self-check against the public contract before anything is written."""
    require(isinstance(record, dict) and set(record) == set(PUBLIC_FIELDS),
            "record fields drifted from the public contract")
    request = validate_request(record["request"])
    limits = request["limits"]
    require(set(record["model"]) == {"provider", "name", "version", "settings"},
            "model fields")
    expected_tools = ["read", "rg", "git"] + (["orbit-graph"] if request["arm"] == "graph"
                                               else [])
    require(set(record["tool_versions"]) == set(expected_tools), "tool_versions fields")
    isolation = record["isolation"]
    require(set(isolation) == {"repository_id", "cache_id", "truth_inaccessible", "cold_start"}
            and isolation["truth_inaccessible"] is True and isolation["cold_start"] is True,
            "isolation attestation")
    require(record["status"] in {"ok", "failed", "timeout", "invalid"}, "status")
    calls = record["calls"]
    require(isinstance(calls, list) and len(calls) <= limits["tool_calls"], "call count")
    for call in calls:
        require(set(call) == {"tool", "input", "output", "elapsed_ms", "status"}, "call fields")
        require(call["tool"] in request["tools"], "tool outside the arm contract")
        require(isinstance(call["input"], str) and isinstance(call["output"], str)
                and len(call["input"].encode()) <= limits["call_bytes"]
                and len(call["output"].encode()) <= limits["call_bytes"], "call capture bound")
        require(call["status"] in {"ok", "failed", "timeout", "truncated"}, "call status")
        require(type(call["elapsed_ms"]) is int and call["elapsed_ms"] >= 0, "call elapsed")
    require(sum(call["elapsed_ms"] for call in calls) <= record["wall_ms"],
            "sequential calls exceed wall time")
    require(isinstance(record["final_output"], str)
            and len(record["final_output"].encode()) <= limits["call_bytes"], "final output")
    accounted = sum(len(call["output"].encode()) for call in calls) + \
        len(record["final_output"].encode())
    require(record["output_bytes"] == accounted <= limits["output_bytes"],
            "output byte accounting")
    require(record["status"] == "timeout" or record["wall_ms"] <= limits["wall_ms"],
            "wall time over budget")
    if record["status"] == "ok":
        require(record["error"] is None and calls and
                all(call["status"] != "truncated" for call in calls), "ok episode shape")
        check_answer(record["answer"], limits)
        require(canonical(parse_json(record["final_output"].encode(), "final"))
                == canonical(record["answer"]), "answer differs from final output")
    else:
        require(isinstance(record["error"], dict) and set(record["error"]) == {"code", "message"}
                and record["answer"] is None, "failed episode shape")
    usage = record["usage"]
    if usage is not None:
        require(set(usage) == {"input_tokens", "output_tokens", "cost_usd", "source"}
                and usage["cost_usd"] is None and usage["source"] == USAGE_SOURCE, "usage")


def load_raw_episode(directory):
    directory = Path(directory)
    artifact = load_json(directory / "episode.json", 64 * 1024 * 1024)
    require(isinstance(artifact, dict) and artifact.get("kind") == RAW_KIND
            and artifact.get("schema_version") == 1, f"{directory}: not a raw episode artifact")
    sealed = artifact.get("artifact_sha256")
    body = {key: value for key, value in artifact.items() if key != "artifact_sha256"}
    require(sealed == digest(body), f"{directory}: artifact hash mismatch")
    for name, record in artifact["files"].items():
        require("/" not in name, f"{directory}: unsafe file name {name}")
        require(sha256_file(directory / name) == record["sha256"],
                f"{directory}: {name} does not match its recorded hash")
    return artifact


def public_record(artifact):
    isolation = artifact["isolation"]
    require(isolation["contained"] is True and isolation["truth_inaccessible"] is True,
            f"{artifact['run_id']}: an uncontained episode cannot be agent evidence")
    require(isolation["cold_start"] is True, f"{artifact['run_id']}: not a cold start")
    code = (artifact["error"] or {}).get("code")
    require(code not in HARNESS_FAILURES, f"{artifact['run_id']}: {code} measures the harness, "
            "not the arm; fix the configuration and rerun the episode")
    record = {"request": artifact["request"], "run_id": artifact["run_id"],
              "model": artifact["model"], "tool_versions": artifact["tool_versions"],
              "isolation": {key: isolation[key] for key in ("repository_id", "cache_id",
                                                            "truth_inaccessible",
                                                            "cold_start")},
              "status": artifact["status"], "error": artifact["error"],
              "answer": artifact["answer"], "final_output": artifact["final_output"],
              "calls": artifact["calls"], "wall_ms": artifact["timing"]["wall_ms"],
              "output_bytes": artifact["output_bytes"], "usage": artifact["usage"]}
    seal(record, "record_sha256")
    check_public_record(record)
    return record


def command_adapt(args):
    records = [public_record(load_raw_episode(directory)) for directory in args.episode]
    records.sort(key=lambda record: record["request"]["order"])
    orders = [record["request"]["order"] for record in records]
    require(orders == list(range(len(records))), f"episode orders must be 0..n-1, got {orders}")
    require(len({record["run_id"] for record in records}) == len(records), "duplicate run ids")
    require(len({canonical(record["model"]) for record in records}) == 1,
            "model/settings drift across the cohort")
    bundle = {"schema_version": 1, "study_kind": "agent", "episodes": records}
    output = Path(os.path.abspath(args.output))
    write_private(output, json.dumps(bundle, indent=2, sort_keys=True) + "\n", Redactor([]))
    return {"output": str(output), "episodes": len(records), "bundle_sha256": digest(bundle)}


def parser():
    root = argparse.ArgumentParser(description=__doc__.splitlines()[0],
                                   formatter_class=argparse.RawDescriptionHelpFormatter,
                                   epilog="\n".join(__doc__.splitlines()[2:]))
    commands = root.add_subparsers(dest="command", required=True)
    revision = commands.add_parser("revision", help="content revision of a source tree")
    revision.add_argument("--tree", required=True)

    def episode_options(sub, with_base):
        sub.add_argument("--request", required=True, help="one exported episode request JSON")
        sub.add_argument("--head", required=True, help="solution-free HEAD source tree")
        if with_base:
            sub.add_argument("--base", required=True, help="solution-free base source tree")
        sub.add_argument("--out", required=True, help="new episode directory (never reused)")
        sub.add_argument("--codex", required=True, help="installed Codex CLI binary")
        sub.add_argument("--model", required=True, help="fixed provider model name")
        sub.add_argument("--setting", action="append", default=[],
                         help=f"fixed provider setting KEY=VALUE, KEY in {SETTING_KEYS}")
        sub.add_argument("--git", default="/usr/bin/git")
        sub.add_argument("--rg", required=True, help="ripgrep binary")
        sub.add_argument("--orbit-graph", dest="orbit_graph",
                         help="orbit-graph binary (graph arm only)")
        sub.add_argument("--python", default="/usr/bin/python3", help="python3 for the broker")
        sub.add_argument("--containment", choices=("bwrap", "none"), default="bwrap",
                         help="bwrap: OS confinement (required for agent evidence); none: "
                              "test-only accident guard, never agent evidence")
        sub.add_argument("--bwrap", default="bwrap")
        sub.add_argument("--auth-file", help="provider credential store bound read-only "
                                             "(default $CODEX_HOME/auth.json; 'none' to skip)")
        sub.add_argument("--truth-path", action="append", default=[],
                         help="where answers/corpus live; must be invisible in the sandbox")
        sub.add_argument("--forbid", action="append", default=[],
                         help="additional host path that must be invisible in the sandbox")
        sub.add_argument("--allow-code-mode", action="store_true",
                         help="experimental: leave codex's code-mode host enabled, for models "
                              "that route tool calls through it (see README)")
        sub.add_argument("--provider-env", action="append", default=[],
                         help="environment variable name passed to the provider")

    episode_options(commands.add_parser("preflight", help="verify containment; no episode"),
                    False)
    episode_options(commands.add_parser("run", help="run one episode"), True)
    adapt = commands.add_parser("adapt", help="raw artifacts -> ORB-13710 public bundle")
    adapt.add_argument("--episode", action="append", required=True,
                       help="episode directory, repeatable, any order")
    adapt.add_argument("--output", required=True, help="new bundle path (never overwritten)")
    return root


def main(argv=None):
    args = parser().parse_args(argv)
    handlers = {"revision": command_revision, "preflight": command_preflight,
                "run": command_run, "adapt": command_adapt}
    try:
        result = handlers[args.command](args)
    except CapabilityRefusal as refusal:
        out = getattr(args, "out", None)
        print(json.dumps(refuse(os.path.abspath(out) if out else None, refusal), indent=2,
                         sort_keys=True))
        return EXIT_REFUSED
    except (Invalid, OSError, KeyError, TypeError) as error:
        print(json.dumps({"error": {"code": "invalid", "message": str(error)[:2000]}}),
              file=sys.stderr)
        return EXIT_INVALID
    print(json.dumps(result, indent=2, sort_keys=True))
    return EXIT_OK


if __name__ == "__main__":
    sys.exit(main())
