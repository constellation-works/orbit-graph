"""Installed Orbit/plugin tests. A fake provider; never a model or a live install."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import sys
import textwrap
import time
import unittest
from unittest import mock

import support
from support import eval_broker as broker, eval_runner as runner
from test_runner import EpisodeCase, bwrap_unavailable
import plugin_profile as plugin

SPEC = importlib.util.spec_from_file_location("plugin_eval", support.REPO / "evals/plugin-agent-navigation/eval.py")


class McpExchangeTests(unittest.TestCase):
    def setUp(self):
        self.work = support.scratch()
        self.addCleanup(support.remove, self.work)
        self.host = self.work / "host.py"
        self.host.write_text("#!/usr/bin/python3\n" + textwrap.dedent('''
            import json, os, select, signal, subprocess, sys, time
            mode = os.environ.get("MODE", "slow")
            def send(value):
                print(json.dumps(value), flush=True)
            def read():
                line = b""
                while not line.endswith(b"\\n"):
                    part = os.read(0, 1)
                    if not part:
                        sys.exit(3)
                    line += part
                return json.loads(line)
            init = read()
            assert init["id"] == 1 and init["method"] == "initialize"
            ready = {"jsonrpc": "2.0", "id": 1, "result": {
                "serverInfo": {"name": "orbit-mcp", "version": "fixture"},
                "protocolVersion": "2024-11-05", "capabilities": {}}}
            if mode == "init_hang":
                time.sleep(30)
            if mode == "init_refused":
                send({"jsonrpc": "2.0", "id": 1, "error": {"code": -1, "message": "no"}})
                # Assert that no call was dispatched after refusal, even on EOF.
                if os.read(0, 1):
                    open("unexpected-call", "w").close()
                sys.exit(0)
            if mode == "bad_host":
                ready["result"]["serverInfo"]["name"] = "other"
            if mode == "bad_protocol":
                ready["result"]["protocolVersion"] = "other"
            if mode == "bad_capabilities":
                ready["result"]["capabilities"] = []
            if mode == "early_call":
                print(json.dumps(ready) + '\\n' + '{"jsonrpc":"2.0","id":2,"result":{}}', flush=True)
                time.sleep(30)
            # Detect pipelining before the initialize response.
            if mode == "sequence":
                assert not select.select([0], [], [], .05)[0]
            send(ready)
            assert read()["method"] == "notifications/initialized"
            assert read()["id"] == 2
            if mode == "missing":
                sys.exit(0)
            if mode == "hang_descendant":
                child = subprocess.Popen([sys.executable, "-c",
                    "import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); time.sleep(30)"],
                    stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                with open("descendant.pid", "w") as stream:
                    stream.write(str(child.pid))
                time.sleep(30)
            if mode == "hang":
                time.sleep(30)
            if mode == "stdout_flood":
                os.write(1, b"x" * (5 * 1024 * 1024))
                time.sleep(30)
            if mode == "stderr_flood":
                os.write(2, b"x" * (300 * 1024))
            result = {"jsonrpc": "2.0", "id": 2, "result": {"full": "delayed reply"}}
            if mode == "product_error":
                result["result"] = {"isError": True, "content": [{"type": "text", "text": "no"}]}
            if mode == "wrong_id":
                result["id"] = 3
            if mode == "bool_id":
                result["id"] = True
            if mode == "float_id":
                result["id"] = 2.0
            if mode == "rpc_error":
                result = {"jsonrpc": "2.0", "id": 2, "error": {"code": -1, "message": "no"}}
            if mode == "both":
                result["error"] = {"code": -1, "message": "no"}
            if mode == "bad_version":
                result["jsonrpc"] = "1.0"
            if mode == "bad_result":
                result["result"] = []
            malformed = {"malformed": b"{bad\\n", "array": b"[]\\n",
                         "constant": b'{"jsonrpc":"2.0","id":2,"result":{"v":NaN}}\\n',
                         "duplicate_key": b'{"jsonrpc":"2.0","id":2,"id":2,"result":{}}\\n',
                         "utf8": b"\\xff\\n", "partial": b'{"jsonrpc":"2.0"'}
            if mode in malformed:
                os.write(1, malformed[mode])
                sys.exit(0)
            # Model a host whose EOF drain is shorter than its tool operation.
            # The old batch closes stdin here and exits zero without id 2.
            if select.select([0], [], [], .2)[0]:
                assert os.read(0, 1) == b""
                print("EOF drain expired before tool reply", file=sys.stderr)
                sys.exit(0)
            if mode == "split":
                for byte in (json.dumps(result) + "\\n").encode():
                    os.write(1, bytes([byte]))
                    time.sleep(.001)
            else:
                send(result)
            assert os.read(0, 1) == b""
            if mode == "duplicate":
                send(result)
            if mode == "trailer":
                os.write(1, b"x")
            if mode == "exit_hang":
                time.sleep(30)
            if mode == "nonzero":
                sys.exit(9)
            if mode == "descendant":
                child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)"],
                                         stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                         stderr=subprocess.DEVNULL)
                open("descendant.pid", "w").write(str(child.pid))
        '''))
        self.host.chmod(0o700)

    def call(self, mode="slow", **kwargs):
        return plugin.mcp(str(self.host), str(self.work), {"PATH": "/usr/bin:/bin", "MODE": mode},
                          "tools/list", {}, kwargs.pop("timeout", 3), **kwargs)

    def test_delayed_reply_survives_eof_drain(self):
        reply, transport = self.call()
        self.assertTrue(plugin.clean(transport), transport)
        self.assertEqual(reply, {"full": "delayed reply"}, transport)
        self.assertEqual([json.loads(row)["id"] for row in transport["stdout"].splitlines()], [1, 2])
        self.assertEqual(transport["supervision"], {"signals": [], "survivors": []})

    def test_sequence_split_frames_and_product_error(self):
        for mode in ("sequence", "split", "product_error"):
            with self.subTest(mode=mode):
                reply, transport = self.call(mode)
                self.assertTrue(plugin.clean(transport), transport)
                self.assertIsNotNone(reply, transport)
                if mode == "product_error":
                    self.assertTrue(reply["isError"])

    def test_invalid_replies_never_succeed(self):
        for mode in ("init_refused", "bad_host", "bad_protocol", "bad_capabilities", "early_call",
                     "missing", "wrong_id", "bool_id", "float_id", "rpc_error", "both",
                     "bad_version", "bad_result", "malformed", "array", "constant",
                     "duplicate_key", "utf8", "partial", "duplicate", "trailer", "nonzero"):
            with self.subTest(mode=mode):
                reply, transport = self.call(mode)
                self.assertIsNone(reply, transport)
                self.assertEqual(transport["supervision"]["survivors"], [])
                self.assertFalse((self.work / "unexpected-call").exists())

    def test_deadline_covers_initialize_call_and_exit(self):
        for mode in ("init_hang", "hang", "exit_hang"):
            with self.subTest(mode=mode):
                reply, transport = self.call(mode, timeout=.5)
                self.assertIsNone(reply)
                self.assertEqual(transport["stopped"], "timeout", transport)
                self.assertEqual(transport["supervision"]["survivors"], [])
                self.assertLess(transport["elapsed_ms"], 2500)

    def test_output_caps_and_descendant_cleanup(self):
        for mode in ("stdout_flood", "stderr_flood", "descendant"):
            with self.subTest(mode=mode):
                reply, transport = self.call(mode)
                self.assertIsNone(reply, transport)
                self.assertLessEqual(len(transport["stdout"]), plugin.MAX_SETUP_OUTPUT)
                self.assertLessEqual(len(transport["stderr"]), broker.MAX_STDERR_CAPTURE)
                self.assertEqual(transport["supervision"]["survivors"], [])
                if mode == "descendant":
                    self.assertFalse(support.alive(int((self.work / "descendant.pid").read_text())))
                elif mode == "stdout_flood":
                    self.assertEqual(transport["stopped"], "truncated")
                else:
                    self.assertTrue(transport["stderr_truncated"])

    def test_exchange_cancellation_and_parent_exit(self):
        for reason in ("cancelled", "parent_exit"):
            with self.subTest(reason=reason):
                read_fd, write_fd = os.pipe()
                start = time.monotonic()
                parent_closed = False
                def cancelled():
                    nonlocal parent_closed
                    if time.monotonic() - start > .2:
                        if reason == "parent_exit" and not parent_closed:
                            os.close(write_fd)
                            parent_closed = True
                        return reason == "cancelled"
                    return False
                try:
                    transport = broker.run_child([str(self.host)], str(self.work),
                        {"PATH": "/usr/bin:/bin", "MODE": "hang_descendant"}, 3,
                        input_exchange=plugin.McpExchange("tools/list", {}),
                        cancelled=cancelled, parent_fd=read_fd, grace_s=.1)
                finally:
                    os.close(read_fd)
                    if not parent_closed:
                        os.close(write_fd)
                self.assertEqual(transport["stopped"], reason, transport)
                self.assertEqual(transport["supervision"]["survivors"], [])
                self.assertFalse(support.alive(int((self.work / "descendant.pid").read_text())))
                self.assertIn("SIGKILL", transport["supervision"]["signals"])


class PluginPolicyTests(unittest.TestCase):
    def test_cgroup_root_missing_files_preserves_parent_limits(self):
        files = {"/proc/self/cgroup": "0::/parent/leaf\n",
                 "/sys/fs/cgroup/parent/leaf/memory.max": "max",
                 "/sys/fs/cgroup/parent/leaf/pids.max": "max",
                 "/sys/fs/cgroup/parent/memory.max": "4294967296",
                 "/sys/fs/cgroup/parent/pids.max": "256"}
        def read(path):
            if str(path) not in files:
                raise FileNotFoundError(str(path))
            return files[str(path)]
        with mock.patch.object(Path, "read_text", read):
            self.assertEqual(plugin.resource_bounds(), {"memory.max": 4294967296, "pids.max": 256})
            files["/sys/fs/cgroup/parent/memory.max"] = "max"
            self.assertEqual(plugin.resource_bounds(), {"memory.max": None, "pids.max": 256})

    def test_source_only_export_boundary(self):
        for name in ("evals/truth.json", "src/answers/truth.py", "src/.env", "README.md",
                     ".orbit/config.yaml", "src/credentials/key.py", "src/data.json"):
            self.assertFalse(plugin.source_path_allowed(name), name)
        for name in ("src/lib.rs", "crates/lib/src/main.rs", "tests/test_parser.py", "Cargo.toml"):
            self.assertTrue(plugin.source_path_allowed(name), name)

    def test_repository_workspace_and_path_escape(self):
        for args in ({"repository": "/etc"}, {"repository": "/eval/repo/../repo"},
                     {"workspace": "live"}, {"selector": "file:../truth.json"},
                     {"selector": "file:.orbit/tasks.json"}, {"scope": "/etc"},
                     {"symbols": ["symbol:src/../../truth#x:function"]},
                     {"selector": "file:.git/config"}):
            with self.subTest(args=args), self.assertRaises(broker.Refusal):
                plugin.validate_arguments("graph_show", args, "/eval/repo")
        for operation in ("orbit_sync", "import", "history_sync", None):
            with self.assertRaises(broker.Refusal):
                plugin.validate_arguments("graph_maintain", {"operation": operation}, "/eval/repo")
        with self.assertRaises(broker.Refusal):
            plugin.validate_arguments("orbit_task_show", {}, "/eval/repo")

    def test_failed_product_evidence_is_not_recovery(self):
        error = {"code": "index_missing", "message": "build the index", "retryable": False}
        good = {"product_reply": {"isError": True, "structuredContent": error,
                                  "content": [{"type": "text", "text": json.dumps(error)}]},
                "transport": {"exit_code": 0, "stopped": None, "error_type": None,
                              "stderr_truncated": False, "supervision": {"signals": [], "survivors": []}}}
        self.assertTrue(plugin.recoverable(good))
        missing = copy.deepcopy(good)
        del missing["product_reply"]["content"]
        self.assertFalse(plugin.recoverable(missing))
        for key, value in (("exit_code", 1), ("stopped", "timeout"), ("stderr_truncated", True),
                           ("supervision", {"signals": [], "survivors": None})):
            altered = copy.deepcopy(good)
            altered["transport"][key] = value
            self.assertFalse(plugin.recoverable(altered))
        for code in ("graph_error", "timeout", "index_incompatible", "capability_denied"):
            altered = copy.deepcopy(good)
            altered["product_reply"]["structuredContent"]["code"] = code
            self.assertFalse(plugin.recoverable(altered))

    def test_bounded_child_input_and_cleanup(self):
        data = b"x" * 100000
        result = broker.run_child(["/usr/bin/cat"], "/", {"PATH": "/usr/bin"}, 5,
                                  input_bytes=data)
        self.assertTrue(plugin.clean(result), result)
        self.assertEqual(result["stdout"], data)
        stalled = broker.run_child(["/usr/bin/sleep", "30"], "/", {"PATH": "/usr/bin"}, .1,
                                   input_bytes=data, grace_s=.1)
        self.assertEqual(stalled["stopped"], "timeout")
        self.assertEqual(stalled["supervision"]["survivors"], [])


@unittest.skipUnless(support.RG and os.environ.get("AGENT_EVAL_ORBIT") and
                     os.environ.get("AGENT_EVAL_ORBIT_GRAPH"),
                     "set AGENT_EVAL_ORBIT and AGENT_EVAL_ORBIT_GRAPH for installed-host tests")
class InstalledPluginTests(EpisodeCase):
    def setUp(self):
        super().setUp()
        # Profile 2 exports code only; the original Git fixture still contains its README.
        self.head.joinpath("README.md").unlink()
        self.base.joinpath("README.md").unlink()
        self.source, self.snapshot = support.snapshot_repo(self.work / "original")
        self.commit = runner.git_run("/usr/bin/git", support.REPO, self.work, "rev-parse", "HEAD")
        self.install_args = ["--plugin-repo", support.REPO, "--orbit", os.environ["AGENT_EVAL_ORBIT"],
                             "--orbit-graph", os.environ["AGENT_EVAL_ORBIT_GRAPH"]]
        code, report, stderr = self.cli("plugin-inspect", *self.install_args, "--plugin-commit", self.commit,
                                        "--out", self.work / "inspect", "--containment", "none")
        self.assertEqual(code, 0, (report, stderr))
        self.pin = report["pin"]
        self.inventory = report["inventory"]

    def plugin_episode(self, steps, arm="graph", containment="none", pin=None, limits=None, env=None, **script):
        request = self.request_file(arm, limits, schema_version=2, profile=plugin.PROFILE,
                                    plugin=pin or self.pin,
                                    source_commits={"head": self.snapshot["head_commit"],
                                                    "base": self.snapshot["base_commit"]},
                                    setup_limits={"wall_ms": 60000, "output_bytes": 8388608},
                                    tools=list(broker.ARM_TOOLS["baseline"] +
                                               (plugin.TOOLS if arm == "graph" else ())))
        return self.episode(steps, arm=arm, request=request, graph=None, env=env, **script,
                            args=[*self.install_args, "--source-repo", self.source,
                                  "--containment", containment, "--auth-file", "none",
                                  "--truth-path", self.truth])

    def test_real_cold_recovery_exact_inventory_skill_and_baseline(self):
        steps = [{"call": "graph_search", "arguments": {"query": "price"}},
                 {"call": "graph_maintain", "arguments": {"operation": "graph_sync"}},
                 {"call": "graph_search", "arguments": {"query": "price"}},
                 {"final": json.dumps(support.ANSWER)}]
        code, report, stderr, out = self.plugin_episode(steps)
        self.assertEqual(code, 0, (report, stderr))
        artifact = self.assert_outcome(out, "ok", None)
        self.assertEqual([c["status"] for c in artifact["calls"]], ["failed", "ok", "ok"])
        self.assertTrue(plugin.recoverable(json.loads(artifact["calls"][0]["output"])))
        product = json.loads(artifact["calls"][2]["output"])["product_reply"]
        self.assertIn("price", json.dumps(product["structuredContent"]))
        self.assertTrue(product["structuredContent"]["index"]["fresh"])
        self.assertEqual(artifact["treatment"]["inventory"], self.inventory)
        self.assertEqual(artifact["treatment"]["skill_files"]["SKILL.md"],
                         (support.REPO / ".orbit-plugin/skills/orbit-graph/SKILL.md").read_text())
        self.assertIn("references/setup.md", artifact["treatment"]["skill_files"])
        self.assertGreater(artifact["timing"]["plugin_install_ms"], 0)
        self.assertGreater(artifact["timing"]["graph_sync_ms"], 0)
        self.assertGreater(artifact["setup_output_bytes"], 0)
        self.assertEqual(artifact["cleanup"]["survivors"], [])
        self.assertEqual(artifact["broker"]["exits"][0]["cleanup"]["survivors"], [])
        self.assertTrue(artifact["isolation"]["cold_start"])
        self.assertGreater(artifact["isolation"]["graph_state_after"]["files"], 0)
        with self.assertRaises(runner.Invalid):
            runner.load_raw_episode(out)  # historical adapter must reject v2
        code, report, stderr, base_out = self.plugin_episode([
            {"call": "read", "arguments": {"path": "src/lib.rs"}},
            {"final": json.dumps(support.ANSWER)}], arm="baseline")
        self.assertEqual(code, 0, (report, stderr))
        baseline = self.assert_outcome(base_out, "ok", None)
        for key in ("prompt", "limits", "setup_limits", "source_commits", "plugin"):
            self.assertEqual(baseline["request"][key], artifact["request"][key])
        self.assertEqual(baseline["model"], artifact["model"])
        self.assertIsNone(baseline["treatment"])
        self.assertEqual(baseline["request"]["tools"], ["read", "rg", "git"])
        self.assertEqual(list((base_out / "state/orbit-home").iterdir()), [])
        evaluator = importlib.util.module_from_spec(SPEC)
        SPEC.loader.exec_module(evaluator)
        self.assertEqual(evaluator.load_episode(out, diagnostic=True)["status"], "ok")
        with self.assertRaises(ValueError):
            evaluator.load_episode(out)  # uncontained diagnostic is not evidence
        plan = {"schema_version": 2, "profile": plugin.PROFILE, "model": artifact["model"],
                "provider_binary_sha256": artifact["provider"]["binary_sha256"], "harness": artifact["harness"],
                "requests": [baseline["request"], artifact["request"]]}
        replay = evaluator.replay(plan, [base_out, out], diagnostic=True)
        self.assertFalse(replay["effectiveness_evidence"])
        self.assertEqual(replay["episodes"], 2)
        plan_path = self.work / "preregistration.json"
        plan_path.write_text(json.dumps(plan))
        cli = broker.run_child([sys.executable, "-B", str(SPEC.origin), "replay",
                                "--preregistration", str(plan_path), "--episode", str(base_out),
                                "--episode", str(out), "--diagnostic"], str(self.work),
                               {"PATH": "/usr/bin:/bin", "HOME": str(self.work)}, 30)
        self.assertTrue(plugin.clean(cli), cli)
        self.assertEqual(json.loads(cli["stdout"])["episodes"], 2)
        for key in ("skill_files", "inventory", "manifest_installed"):
            altered = copy.deepcopy(artifact)
            if key == "skill_files":
                altered["treatment"][key]["SKILL.md"] += "silently rewritten"
            elif key == "inventory":
                altered["treatment"][key][0]["description"] += "changed"
            else:
                altered["treatment"][key] += "# undisclosed edit"
            with self.assertRaises(ValueError):
                evaluator.check_treatment(altered)
        changed = copy.deepcopy(plan)
        changed["requests"][1]["prompt"] += " force graph"
        with self.assertRaises(ValueError):
            evaluator.replay(changed, [base_out, out], diagnostic=True)

    def test_unknown_tool_state_path_and_maintenance_refusals(self):
        marker = self.truth / "orbit.db"
        marker.write_text("HOST-STORE-MUST-NOT-BE-OPENED")
        steps = [{"call": "read", "arguments": {"path": ".orbit"}},
                 {"call": "read", "arguments": {"path": "../state/orbit-home/.orbit"}},
                 {"call": "graph_search", "arguments": {"query": "price", "repository": "/etc"}},
                 {"call": "graph_show", "arguments": {"selector": "file:.git/config"}},
                 {"call": "graph_maintain", "arguments": {"operation": "orbit_sync"}},
                 {"call": "graph_search", "arguments": {"query": "price", "unknown": 1}},
                 {"raw": json.dumps({"jsonrpc": "2.0", "id": 999, "method": "tools/call",
                                       "params": {"name": "orbit_task_show", "arguments": {}}}),
                  "expect_reply": True},
                 {"call": "read", "arguments": {"path": "."}},
                 {"call": "rg", "arguments": {"pattern": "ws_agent|routines", "path": "."}},
                 {"final": json.dumps(support.ANSWER)}]
        code, report, stderr, out = self.plugin_episode(steps, env={
            "ORBIT_ROOT": str(self.truth), "ORBIT_OPERATOR": "1", "ORBIT_WORKSPACE": "live"})
        self.assertEqual(code, 0, (report, stderr))
        self.assertEqual(marker.read_text(), "HOST-STORE-MUST-NOT-BE-OPENED")
        artifact = self.assert_outcome(out, "ok", None)
        self.assertNotIn("HOST-STORE-MUST-NOT-BE-OPENED", json.dumps(artifact))
        self.assertEqual([x["status"] for x in artifact["calls"][:6]], ["failed"] * 6)
        self.assertEqual(artifact["isolation"]["refused_tool_names"],
                         [{"tool": "orbit_task_show", "code": "tool_not_permitted"}])
        self.assertNotIn(".orbit", artifact["calls"][-2]["output"])
        self.assertEqual(json.loads(artifact["calls"][-1]["output"])["output"], "")

    def test_replay_retains_paired_harness_failures(self):
        evaluator = importlib.util.module_from_spec(SPEC)
        SPEC.loader.exec_module(evaluator)
        limits = dict(support.LIMITS, wall_ms=6000)
        read = {"call": "read", "arguments": {"path": "src/lib.rs"}}
        final = {"final": json.dumps(support.ANSWER)}
        code, report, stderr, graph_out = self.plugin_episode([read, final], limits=limits)
        self.assertEqual(code, 0, (report, stderr))
        graph = self.assert_outcome(graph_out, "ok", None)
        cases = [([dict(read, omit_completed=True), final], {}, "failed", "telemetry_mismatch"),
                 ([read, final], {"deny_tool_approvals": True}, "invalid", "provider_tool_approval_required"),
                 ([read, {"sleep": 60}], {}, "timeout", "wall_time_exceeded")]
        for steps, script, status, error in cases:
            with self.subTest(error=error):
                code, report, stderr, out = self.plugin_episode(steps, arm="baseline", limits=limits, **script)
                self.assertEqual(code, 0, (report, stderr))
                failed = self.assert_outcome(out, status, error)
                plan = {"schema_version": 2, "profile": plugin.PROFILE, "model": graph["model"],
                        "provider_binary_sha256": graph["provider"]["binary_sha256"],
                        "harness": graph["harness"], "requests": [failed["request"], graph["request"]]}
                replay = evaluator.replay(plan, [out, graph_out], diagnostic=True)
                self.assertEqual(replay["outcomes"], {status: 1, "ok": 1})
                self.assertEqual(replay["costs"][0]["error"]["code"], error)
                self.assertEqual(replay["costs"][0]["output_bytes"], failed["output_bytes"])
                self.assertFalse(replay["containment_verified"])
                self.assertFalse(replay["costs"][0]["call_telemetry_verified"])
                # A sealed status relabel cannot turn the captured failure into success.
                original = (out / "episode.json").read_bytes()
                forged = dict(failed, status="ok", error=None)
                runner.seal(forged, "artifact_sha256")
                try:
                    (out / "episode.json").write_text(json.dumps(forged))
                    with self.assertRaises(ValueError):
                        evaluator.load_episode(out, diagnostic=True)
                finally:
                    (out / "episode.json").write_bytes(original)

    def test_teardown_replay_uses_prospective_contract_only(self):
        evaluator = importlib.util.module_from_spec(SPEC)
        SPEC.loader.exec_module(evaluator)
        code, _, stderr, out = self.plugin_episode([
            {"call": "graph_maintain", "arguments": {"operation": "graph_sync"}},
            {"call": "graph_search", "arguments": {"query": "price"}},
            {"final": json.dumps(support.ANSWER)}], shutdown="eof_then_sigterm")
        self.assertEqual(code, 0, stderr)
        artifact = self.assert_outcome(out, "ok", None)
        self.assertEqual(evaluator.load_episode(out, diagnostic=True)["status"], "ok")
        self.assertEqual(artifact["broker"]["exits"][0]["stopped"], "cancelled")
        # Only this synthetic fixture is resealed. Historical contract selection
        # must not award the new benefit even with complete new lifecycle facts.
        historical = copy.deepcopy(artifact)
        historical.pop("reply_contract")
        historical.pop("reply_provenance")
        entries = [json.loads(line) for line in (out / "broker-calls.jsonl").read_text().splitlines()]
        for entry in entries:
            entry.pop("reply_contract", None)
            entry.pop("reply_provenance", None)
        (out / "broker-calls.jsonl").write_text("".join(json.dumps(e) + "\n" for e in entries))
        historical["files"]["broker-calls.jsonl"].update(
            sha256=runner.sha256_file(out / "broker-calls.jsonl"),
            bytes=(out / "broker-calls.jsonl").stat().st_size)
        historical["runner_version"] = "2"
        del historical["lifecycle_contract"]
        historical.update(status="failed", answer=None,
                          error={"code": "broker_failed", "message": "historical cancellation"})
        runner.seal(historical, "artifact_sha256")
        (out / "episode.json").write_text(json.dumps(historical))
        self.assertEqual(evaluator.load_episode(out, diagnostic=True)["status"], "failed")
        redacted = dict(historical, redactions=1)
        runner.seal(redacted, "artifact_sha256")
        (out / "episode.json").write_text(json.dumps(redacted))
        with self.assertRaisesRegex(ValueError, "redacted treatment"):
            evaluator.load_episode(out, diagnostic=True)
        for altered in (dict(historical, status="ok", error=None),
                        dict(artifact, lifecycle_contract=None),
                        dict(artifact, runner_version="future")):
            runner.seal(altered, "artifact_sha256")
            (out / "episode.json").write_text(json.dumps(altered))
            with self.assertRaises(ValueError):
                evaluator.load_episode(out, diagnostic=True)

    def test_product_output_bound_stops_and_cleans_up(self):
        code, report, stderr, out = self.plugin_episode([
            {"call": "graph_search", "arguments": {"query": "price"}},
            {"final": json.dumps(support.ANSWER)}], limits=dict(support.LIMITS, call_bytes=1024))
        self.assertEqual(code, 0, (report, stderr))
        artifact = self.assert_outcome(out, "failed", "call_output_truncated")
        self.assertEqual(artifact["cleanup"]["survivors"], [])
        entries = [json.loads(line) for line in (out / "broker-calls.jsonl").read_text().splitlines()]
        full = next(e for e in entries if e["type"] == "plugin_output")
        self.assertEqual(full["reply"]["product_reply"]["structuredContent"]["code"], "index_missing")

    def test_setup_failure_is_a_bounded_refusal(self):
        code, report, stderr = self.cli("plugin-inspect", *self.install_args,
            "--orbit", "/usr/bin/false", "--plugin-commit", self.commit,
            "--out", self.work / "failed-install", "--containment", "none")
        self.assertEqual(code, 3, (report, stderr))
        self.assertFalse(report["provider_started"])
        records = json.loads((self.work / "failed-install/plugin-setup.json").read_text())
        self.assertEqual(records[-1]["role"], "workspace-init")
        self.assertEqual(records[-1]["exit_code"], 1)
        self.assertEqual(records[-1]["supervision"]["survivors"], [])

    def test_inventory_drift_refuses_before_provider(self):
        pin = dict(self.pin, inventory_sha256="0" * 64)
        code, report, stderr, out = self.plugin_episode([], pin=pin)
        self.assertEqual(code, 3, stderr)
        self.assertFalse(report["provider_started"])
        self.assertFalse((out / "provider.jsonl").exists())

    def test_changed_source_is_refused(self):
        self.head.joinpath("src/lib.rs").write_text("pub fn wrong() {}\n")
        code, report, stderr, out = self.plugin_episode([])
        self.assertEqual(code, 1, (report, stderr))
        self.assertIn("source differs from pinned commit", stderr)
        self.assertFalse((out / "provider.jsonl").exists())

    def test_strict_containment_real_installed_plugin(self):
        reason = bwrap_unavailable()
        if reason:
            if os.environ.get("AGENT_EVAL_REQUIRE_BWRAP") == "1":
                self.fail(reason)
            self.skipTest(reason)
        code, report, stderr, out = self.plugin_episode([
            {"call": "graph_search", "arguments": {"query": "price"}},
            {"call": "graph_maintain", "arguments": {"operation": "graph_sync"}},
            {"call": "graph_search", "arguments": {"query": "price"}},
            {"call": "read", "arguments": {"path": ".orbit"}},
            {"final": json.dumps(support.ANSWER)}], containment="bwrap",
            shutdown="eof_then_sigterm")
        self.assertEqual(code, 0, (report, stderr))
        artifact = self.assert_outcome(out, "ok", None)
        self.assertTrue(artifact["isolation"]["contained"])
        self.assertEqual(artifact["isolation"]["probe"]["leaked"], [])
        self.assertEqual(artifact["cleanup"]["survivors"], [])
        probe_script = """import errno
from pathlib import Path
try:
    Path('/eval/repo/src/lib.rs').write_text('must never write source')
except OSError as error:
    assert error.errno in (errno.EROFS, errno.EACCES)
else:
    raise AssertionError('source mount is writable')
Path('/eval/repo/.orbit/private-state-probe').write_text('private fixture state')
"""
        checked = broker.run_child(artifact["isolation"]["sandbox_argv"] +
                                   ["/usr/bin/python3", "-c", probe_script], str(self.work),
                                   {"PATH": "/usr/bin:/bin", "HOME": str(self.work)}, 10)
        self.assertTrue(plugin.clean(checked), checked)
