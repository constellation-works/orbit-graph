"""Shared fixtures for the agent-eval tests (no network, no provider, no host config)."""
import json
import os
from pathlib import Path
import select
import shutil
import signal
import subprocess
import sys
import tempfile
import time

sys.dont_write_bytecode = True
TESTS = Path(__file__).resolve().parent
TOOL = TESTS.parent
REPO = TOOL.parent.parent
sys.path.insert(0, str(TOOL))
import eval_broker  # noqa: E402
import eval_runner  # noqa: E402

RG = shutil.which("rg")
# Every fixture wait is bounded (STD-03 R17): an MCP reply or write, and a
# broker's exit after its stdin closes.
MCP_TIMEOUT_S = 30.0
CLOSE_TIMEOUT_S = 10.0
TERMINATE_GRACE_S = 0.5
MAX_REPLY_BYTES = 8 * 1024 * 1024
FAKE_CODEX = TESTS / "fake_codex.py"
FAKE_BWRAP = TESTS / "fake_bwrap.py"
FAKE_GRAPH = TESTS / "fake_orbit_graph.py"
LIMITS = {"wall_ms": 60000, "tool_calls": 40, "output_bytes": 131072, "answer_items": 32,
          "call_bytes": 16384}
HEAD = {"src/lib.rs": "pub fn helper(v: i32) -> i32 {\n    v + 1\n}\n\n"
                      "pub fn price(v: i32) -> i32 {\n    helper(v) * 3\n}\n",
        "README.md": "fixture \"quoted\" café\n--files\n"}
BASE = {"src/lib.rs": "pub fn helper(v: i32) -> i32 {\n    v + 1\n}\n\n"
                      "pub fn price(v: i32) -> i32 {\n    helper(v) * 2\n}\n",
        "README.md": "fixture \"quoted\" café\n--files\n"}
ANSWER = {"items": ["symbol:src/lib.rs#price:function"], "abstain": False, "reason": "found",
          "evidence": [{"item": "symbol:src/lib.rs#price:function", "file": "src/lib.rs",
                        "line": 5, "quote": "pub fn price(v: i32) -> i32 {"}]}


def scratch():
    root = Path(os.environ.get("AGENT_EVAL_TEST_TMP", REPO / ".orbit" / "tmp" / "agent-eval-tests"))
    root.mkdir(parents=True, exist_ok=True)
    return Path(tempfile.mkdtemp(dir=root))


def remove(path):
    for directory, _, _ in os.walk(path):
        os.chmod(directory, 0o700)
    shutil.rmtree(path)


def write_tree(root, files):
    root = Path(root)
    for relative, text in files.items():
        target = root / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8")
    return root


def revision(root):
    entries = eval_runner.tree_entries(root, "t")
    return eval_runner.content_revision(entries, "t")["content_revision"]


def make_request(head, base, arm="baseline", limits=None, **overrides):
    request = {"schema_version": 1, "case_id": "rust-01", "arm": arm, "split": "development",
               "fixture": "rust", "source_revision": revision(head),
               "base_revision": revision(base), "corpus_sha256": "0" * 64,
               "prompt": "Find price.\nReturn only a JSON object.", "limits": limits or LIMITS,
               "tools": list(eval_broker.ARM_TOOLS[arm]),
               "cache_policy": eval_runner.CACHE_POLICY, "order": 0 if arm == "baseline" else 1}
    request.update(overrides)
    request["request_sha256"] = eval_runner.digest(request)
    return request


def snapshot_repo(work, files_head=HEAD, files_base=BASE):
    """A real two-commit snapshot built by the runner's own materializer."""
    head = write_tree(work / "head", files_head)
    base = write_tree(work / "base", files_base)
    state = work / "state"
    (state / "home").mkdir(parents=True)
    repo = work / "repo"
    snapshot = eval_runner.materialize_snapshot(
        repo, state, "/usr/bin/git", base_entries=eval_runner.tree_entries(base, "base"),
        head_entries=eval_runner.tree_entries(head, "head"))
    return repo, snapshot


class McpClient:
    """Minimal MCP stdio client speaking to the real broker process.

    Reads and writes have a deadline and check the process, so a hung or dead
    broker fails the test instead of hanging the suite. The broker runs in its
    own process group; `terminate` kills and reaps it on every path (STD-03 R18),
    and tests register it with addCleanup. Only this client may reap the leader:
    WNOWAIT observations reserve its PID/group until the sweep is finished.
    """

    def __init__(self, config_path=None, argv=None, timeout_s=MCP_TIMEOUT_S):
        argv = argv or [sys.executable, "-B", str(TOOL / "eval_broker.py"), "--config",
                        str(config_path)]
        self.timeout_s = timeout_s
        self.process = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.DEVNULL, start_new_session=True)
        os.set_blocking(self.process.stdin.fileno(), False)
        self.buffer = b""
        self.next_id = 0

    def _exit_code(self):
        """Observe exit without releasing ownership of the process group."""
        if self.process.returncode is not None:
            return self.process.returncode  # already reaped; never signal again
        info = os.waitid(os.P_PID, self.process.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
        if info is None:
            return None
        return info.si_status if info.si_code == os.CLD_EXITED else -info.si_status

    def _remaining(self, deadline, action):
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError(f"broker pid {self.process.pid}: no {action} within "
                               f"{self.timeout_s}s (exit {self._exit_code()})")
        return min(remaining, 0.25)

    def send_raw(self, data):
        view = memoryview(data if isinstance(data, bytes) else data.encode())
        fd = self.process.stdin.fileno()
        deadline = time.monotonic() + self.timeout_s
        while view:
            wait = self._remaining(deadline, "write")
            if not select.select([], [fd], [], wait)[1]:
                continue
            try:
                view = view[os.write(fd, view[:65536]):]
            except BlockingIOError:
                continue

    def read(self):
        fd = self.process.stdout.fileno()
        deadline = time.monotonic() + self.timeout_s
        while b"\n" not in self.buffer:
            wait = self._remaining(deadline, "reply")
            if not select.select([fd], [], [], wait)[0]:
                continue
            chunk = os.read(fd, 65536)
            if not chunk:
                raise EOFError(f"broker pid {self.process.pid} closed stdout "
                               f"(exit {self._exit_code()})")
            self.buffer += chunk
            if len(self.buffer) > MAX_REPLY_BYTES:
                raise ValueError(f"MCP reply exceeds {MAX_REPLY_BYTES} bytes")
        line, _, self.buffer = self.buffer.partition(b"\n")
        return json.loads(line)

    def request(self, method, params=None):
        self.next_id += 1
        self.send_raw(json.dumps({"jsonrpc": "2.0", "id": self.next_id, "method": method,
                                  "params": params or {}}) + "\n")
        return self.read()

    def call(self, tool, arguments):
        reply = self.request("tools/call", {"name": tool, "arguments": arguments})
        result = reply["result"]
        return json.loads(result["content"][0]["text"]), result["isError"]

    def close(self, timeout_s=CLOSE_TIMEOUT_S):
        """End of input, then a bounded wait; a broker still running is killed. Exit code."""
        try:
            self.process.stdin.close()
        except OSError:
            pass
        deadline = time.monotonic() + timeout_s
        while self._exit_code() is None and time.monotonic() < deadline:
            time.sleep(min(0.02, max(0, deadline - time.monotonic())))
        return self.terminate()

    def terminate(self):
        """TERM, bounded grace, KILL survivors, then bounded reap. Idempotent."""
        def signal_group(sig):
            # ChildProcessError refuses a signal if another caller reaped the
            # leader. A zombie retained with WNOWAIT still reserves the PGID.
            self._exit_code()
            try:
                os.killpg(self.process.pid, sig)
            except ProcessLookupError:
                pass

        def settled(timeout_s):
            deadline = time.monotonic() + timeout_s
            while True:
                members = eval_broker.live_group_members(self.process.pid)
                if members == []:
                    return True
                if time.monotonic() >= deadline:
                    return False  # an unknown group probe counts as survival
                time.sleep(0.02)

        try:
            if self.process.returncode is None:
                signal_group(signal.SIGTERM)
                if not settled(TERMINATE_GRACE_S):
                    signal_group(signal.SIGKILL)
                group_settled = settled(CLOSE_TIMEOUT_S)
                self.process.wait(timeout=CLOSE_TIMEOUT_S)  # release identity last
                if not group_settled:
                    raise TimeoutError(f"broker group {self.process.pid} did not settle")
        finally:
            for stream in (self.process.stdin, self.process.stdout):
                try:
                    stream.close()
                except OSError:
                    pass
        return self.process.returncode


def broker_config(work, repo, snapshot, arm="baseline", limits=None, graph=None):
    state = work / "broker-state"
    (state / "home").mkdir(parents=True, exist_ok=True)
    (state / "tmp").mkdir(exist_ok=True)
    config = {"schema_version": 1, "arm": arm, "tools": list(eval_broker.ARM_TOOLS[arm]),
              "repo_root": str(repo), "log_path": str(state / "calls.jsonl"),
              "sentinel_path": str(state / "budget.json"), "home": str(state / "home"),
              "tmp": str(state / "tmp"), "path_env": "/usr/bin:/bin",
              "deadline_unix_ms": int((time.time() + 120) * 1000),
              "limits": limits or LIMITS,
              "binaries": {"rg": RG, "git": "/usr/bin/git",
                           "orbit_graph": graph if arm == "graph" else None},
              "snapshot": {"base_commit": snapshot["base_commit"],
                           "head_commit": snapshot["head_commit"]}}
    path = work / f"config-{arm}.json"
    path.write_text(json.dumps(config))
    return path, state


def alive(pid):
    """True while pid exists and is not a zombie."""
    try:
        with open(f"/proc/{pid}/stat", "rb") as stream:
            return stream.read().rsplit(b")", 1)[1].split()[0] not in (b"Z", b"X")
    except OSError:
        return False


def log_entries(state):
    path = state / "calls.jsonl"
    return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []
