"""Shared fixtures for the agent-eval tests (no network, no provider, no host config)."""
import json
import os
from pathlib import Path
import shutil
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
    """Minimal MCP stdio client speaking to the real broker process."""

    def __init__(self, config_path):
        self.process = subprocess.Popen(
            [sys.executable, "-B", str(TOOL / "eval_broker.py"), "--config", str(config_path)],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.next_id = 0

    def send_raw(self, data):
        self.process.stdin.write(data if isinstance(data, bytes) else data.encode())
        self.process.stdin.flush()

    def read(self):
        return json.loads(self.process.stdout.readline())

    def request(self, method, params=None):
        self.next_id += 1
        self.send_raw(json.dumps({"jsonrpc": "2.0", "id": self.next_id, "method": method,
                                  "params": params or {}}) + "\n")
        return self.read()

    def call(self, tool, arguments):
        reply = self.request("tools/call", {"name": tool, "arguments": arguments})
        result = reply["result"]
        return json.loads(result["content"][0]["text"]), result["isError"]

    def close(self):
        self.process.stdin.close()
        code = self.process.wait(timeout=30)
        self.process.stdout.close()
        self.process.stderr.close()
        return code


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
