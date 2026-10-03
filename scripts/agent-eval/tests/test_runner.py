"""End-to-end tests of eval_runner.py driven through its real command line.

The provider is tests/fake_codex.py: it speaks real MCP to the real broker but
never contacts a model. Nothing here is effectiveness evidence.
"""
import copy
import hashlib
import json
import os
import shutil
import signal
import stat
import subprocess
import sys
import time
import unittest

import support
from support import eval_runner

RUNNER = support.TOOL / "eval_runner.py"
MODEL = "fake-model"
SECRET = "agent-eval-test-secret-7f3a9c21b5"
OK_STEPS = [{"call": "read", "arguments": {"path": "src/lib.rs"}},
            {"call": "rg", "arguments": {"pattern": "price", "fixed_strings": True}},
            {"call": "git", "arguments": {"op": "diff", "base": "HEAD^", "head": "HEAD"}},
            {"final": json.dumps(support.ANSWER)}]


REQUIRE_BWRAP = "AGENT_EVAL_REQUIRE_BWRAP"


def bwrap_probe_argv(binary):
    """The runner's own namespaces and system mounts around /usr/bin/true."""
    return eval_runner.system_sandbox_argv(binary) + ["--", "/usr/bin/true"]


def bwrap_unavailable(binary=None, env=None):
    """None when the runner's sandbox works here, else the reason it cannot be created.

    Only bubblewrap's own refusal (no binary, or a `bwrap:` error such as denied
    user namespaces) is a capability gap. A sandbox that starts but cannot run
    /usr/bin/true is a layout defect and raises, so it cannot become a skip.
    """
    binary = binary or shutil.which("bwrap")
    if not binary:
        return "bwrap is not installed"
    result = subprocess.run(bwrap_probe_argv(binary), capture_output=True, timeout=30,
                            check=False, env=env)
    stderr = result.stderr.decode(errors="replace")
    if result.returncode == 0:
        return None
    if stderr.startswith("bwrap:"):
        return f"bwrap cannot create the runner's sandbox here: {stderr.strip()[:300]}"
    raise AssertionError(f"the runner's sandbox started but /usr/bin/true failed "
                         f"(exit {result.returncode}): {stderr[:300]}")


class EpisodeCase(unittest.TestCase):
    """Scratch source trees plus helpers that drive the runner CLI; holds no tests."""

    def setUp(self):
        self.work = support.scratch()
        self.head = support.write_tree(self.work / "head", support.HEAD)
        self.base = support.write_tree(self.work / "base", support.BASE)
        self.truth = self.work / "truth"
        self.truth.mkdir()
        self.counter = 0

    def tearDown(self):
        support.remove(self.work)

    # -- helpers ----------------------------------------------------------
    def cli(self, *argv, env=None, timeout=180):
        """Run the runner CLI; past `timeout` its whole group is killed and reaped (R17/R18)."""
        environment = dict(os.environ)
        environment.update(env or {})
        started = time.monotonic()
        process = subprocess.Popen([sys.executable, "-B", str(RUNNER), *map(str, argv)],
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   env=environment, start_new_session=True)
        try:
            stdout, stderr = process.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.communicate()
            self.fail(f"runner {argv[:1]} exceeded {timeout}s and was killed")
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
        self.elapsed = time.monotonic() - started
        stdout = stdout.decode()
        report = json.loads(stdout) if stdout.strip() else None
        return process.returncode, report, stderr.decode()

    def request_file(self, arm="baseline", limits=None, raw=None, **overrides):
        self.counter += 1
        path = self.work / f"request-{self.counter}.json"
        if raw is None:
            raw = json.dumps(support.make_request(self.head, self.base, arm, limits, **overrides))
        path.write_text(raw)
        return path

    def episode(self, steps=OK_STEPS, arm="baseline", limits=None, request=None, command="run",
                args=(), env=None, graph=support.FAKE_GRAPH, **script):
        """Run the CLI with the fake provider following `steps` (inline, so sandboxes see it)."""
        self.counter += 1
        out = self.work / f"episode-{self.counter}"
        request = request or self.request_file(arm, limits)
        argv = [command, "--request", request, "--head", self.head, "--out", out,
                "--codex", support.FAKE_CODEX, "--model", MODEL, "--rg", support.RG,
                "--containment", "none", "--provider-env", "FAKE_CODEX_SCRIPT_JSON", *args]
        if command == "run":
            argv[5:5] = ["--base", self.base]
        if arm == "graph" and graph is not None:
            argv += ["--orbit-graph", graph]
        inline = json.dumps(dict(script, steps=list(steps)))
        code, report, stderr = self.cli(*argv, env=dict(env or {}, FAKE_CODEX_SCRIPT_JSON=inline))
        return code, report, stderr, out

    def artifact(self, out):
        return json.loads((out / "episode.json").read_text())

    def assert_outcome(self, out, status, code):
        artifact = self.artifact(out)
        self.assertEqual((artifact["status"], (artifact["error"] or {}).get("code")),
                         (status, code), artifact["error"])
        if status != "ok":
            self.assertIsNone(artifact["answer"])
        return artifact

    def graph_recovery(self, args=()):
        steps = [{"call": "search", "arguments": {"query": "price"}},
                 {"call": "graph_sync"}, {"call": "search", "arguments": {"query": "price"}},
                 OK_STEPS[0], OK_STEPS[-1]]
        code, _, stderr, out = self.episode(
            steps, arm="graph", graph=os.environ["AGENT_EVAL_ORBIT_GRAPH"], args=args)
        self.assertEqual(code, 0, stderr)
        artifact = self.artifact(out)
        self.assertEqual([c["status"] for c in artifact["calls"]], ["failed", "ok", "ok", "ok"])
        body = json.loads(artifact["calls"][0]["output"])
        self.assertEqual(body["exit_code"], 1)
        self.assertEqual(json.loads(body["stderr"])["code"], "index_missing")
        self.assertEqual(body["cleanup"], {"signals": [], "survivors": []})
        self.assertIn("price", artifact["calls"][2]["output"])
        log, _ = eval_runner.read_broker_log(out / "broker-calls.jsonl")
        transcript = eval_runner.analyse_transcript((out / "provider.jsonl").read_bytes(),
                                                    False, artifact["request"]["tools"])
        self.assertIsNone(eval_runner.reconcile_calls(transcript, log["calls"]))
        self.assertEqual(artifact["broker"]["exits"][0]["exit_code"], 0)
        self.assertEqual(artifact["broker"]["exits"][0]["cleanup"]["survivors"], [])
        self.assert_outcome(out, "ok", None)
        self.assertEqual(artifact["answer"], support.ANSWER)
        self.assertEqual(artifact["output_bytes"],
                         sum(len(c["output"].encode()) for c in artifact["calls"])
                         + len(artifact["final_output"].encode()))
        self.assertEqual(eval_runner.load_raw_episode(out)["calls"], artifact["calls"])
        return artifact, out

    def forge_contained(self, out):
        """Converter-shape fixture only: mark a test episode contained and re-seal it."""
        self.counter += 1
        copy = self.work / f"forged-{self.counter}"
        shutil.copytree(out, copy, ignore=shutil.ignore_patterns("snapshot", "runtime", "state"))
        artifact = self.artifact(copy)
        artifact["isolation"].update(contained=True, truth_inaccessible=True)
        eval_runner.seal(artifact, "artifact_sha256")
        os.chmod(copy / "episode.json", 0o600)
        (copy / "episode.json").write_text(json.dumps(artifact))
        return copy


@unittest.skipUnless(support.RG, "ripgrep (rg) not on PATH; runner episodes need it")
class RunnerTests(EpisodeCase):
    # -- successful episodes and telemetry -------------------------------
    def test_baseline_episode_captures_bounded_hashed_telemetry(self):
        code, report, stderr, out = self.episode()
        self.assertEqual(code, 0, stderr)
        self.assertEqual(report["status"], "ok")
        artifact = self.assert_outcome(out, "ok", None)
        self.assertEqual([call["tool"] for call in artifact["calls"]], ["read", "rg", "git"])
        self.assertTrue(all(call["status"] == "ok" for call in artifact["calls"]))
        self.assertIn("helper(v) * 3", artifact["calls"][2]["output"])
        self.assertIn("helper(v) * 2", artifact["calls"][2]["output"])
        self.assertEqual(artifact["answer"], support.ANSWER)
        self.assertEqual(artifact["final_source"], "output_last_message")
        accounted = sum(len(call["output"].encode()) for call in artifact["calls"]) + \
            len(artifact["final_output"].encode())
        self.assertEqual(artifact["output_bytes"], accounted)
        self.assertEqual(artifact["usage"], {"input_tokens": 1200, "output_tokens": 80,
                                             "cost_usd": None,
                                             "source": eval_runner.USAGE_SOURCE})
        self.assertEqual(artifact["usage_raw"]["cached_input_tokens"], 100)
        self.assertEqual(artifact["model"], {"provider": "codex-cli", "name": MODEL,
                                             "version": "codex-cli 0.0.0-fake",
                                             "settings": {}})
        self.assertEqual(set(artifact["tool_versions"]), {"read", "rg", "git"})
        self.assertEqual(artifact["snapshot"]["commit_count"], 2)
        self.assertEqual(artifact["inputs"]["head"]["content_revision"],
                         artifact["request"]["source_revision"])
        self.assertEqual(artifact["snapshot"]["work_tree_revision"],
                         artifact["request"]["source_revision"])
        # The escape hatch is never recorded as containment.
        isolation = artifact["isolation"]
        self.assertEqual((isolation["containment"], isolation["contained"],
                          isolation["truth_inaccessible"]), ("none", False, False))
        self.assertEqual(isolation["inventory"]["mcp_servers"], ["eval_broker"])
        self.assertEqual(isolation["inventory"]["disabled_verified"],
                         list(eval_runner.DISABLED_FEATURES))
        self.assertEqual(isolation["inventory"]["tool_approvals"]["approved_tools"],
                         ["read", "rg", "git"])
        self.assertEqual(isolation["tools_listed"], [list(support.eval_broker.ARM_TOOLS
                                                          ["baseline"])])
        self.assertLessEqual(artifact["timing"]["setup_ms"], artifact["timing"]["wall_ms"])
        self.assertEqual(artifact["provider"]["env_names"],
                         sorted(["CODEX_HOME", "FAKE_CODEX_SCRIPT_JSON", "HOME", "LANG", "LC_ALL",
                                 "NO_COLOR", "PATH", "TERM", "TMPDIR"]))
        self.assertIsNone(artifact["cleanup"]["terminated_by"])
        self.assertEqual(artifact["cleanup"]["survivors"], [])
        for name, record in artifact["files"].items():
            data = (out / name).read_bytes()
            self.assertEqual(hashlib.sha256(data).hexdigest(), record["sha256"], name)
            self.assertEqual(stat.S_IMODE(os.stat(out / name).st_mode), 0o600, name)
        self.assertEqual(stat.S_IMODE(os.stat(out).st_mode), 0o700)
        self.assertEqual(eval_runner.load_raw_episode(out)["run_id"], report["run_id"])

    def test_broker_exit_diagnostics_preserve_raw_compatibility_and_fail_closed_audit(self):
        _, _, _, out = self.episode()
        artifact = self.assert_outcome(out, "ok", None)
        self.assertEqual(artifact["broker"]["exits"][0]["exit_code"], 0)
        self.assertEqual(artifact["broker"]["exits"][0]["cleanup"]["survivors"], [])
        log, _ = eval_runner.read_broker_log(out / "broker-calls.jsonl")
        transcript = eval_runner.analyse_transcript((out / "provider.jsonl").read_bytes(),
                                                    False, artifact["request"]["tools"])
        supervision = {"terminated_by": None, "stdout_truncated": False, "stderr": b"",
                       "returncode": 0, "exit": {"code": 0}}

        def decide():
            return eval_runner.decide(supervision, transcript, log,
                                      {"text": json.dumps(support.ANSWER), "truncated": False},
                                      support.LIMITS, None, artifact["output_bytes"], 1)[1]
        log["exits"][0]["exit_code"] = -signal.SIGKILL
        self.assertEqual(decide(), "broker_failed")
        log["exits"] = []
        self.assertEqual(decide(), "broker_exit_missing")
        log["calls"].pop()
        self.assertEqual(decide(), "telemetry_mismatch")
        # Older sealed episodes lack supervision entries; their schema still reads.
        old_log = self.work / "old-broker.jsonl"
        old_log.write_text("\n".join(line for line in (out / "broker-calls.jsonl").read_text().splitlines()
                                     if json.loads(line)["type"] not in ("supervisor_start", "broker_exit")) + "\n")
        log, _ = eval_runner.read_broker_log(old_log)
        self.assertIsNone(decide())
        diagnostic = {"type": "broker_exit", "stderr": SECRET + " source-diagnostic",
                      "exit_code": 1, "stopped": None, "error_type": None,
                      "stderr_truncated": False, "cleanup": {"survivors": []}}
        redactor = eval_runner.Redactor([SECRET])
        path = self.work / "redacted-broker.jsonl"
        eval_runner.write_private(path, json.dumps(diagnostic) + "\n", redactor)
        projected, _ = eval_runner.read_broker_log(path)
        self.assertNotIn(SECRET, path.read_text())
        self.assertIn("[REDACTED]", projected["exits"][0]["stderr"])
        malformed = self.work / "malformed-broker.jsonl"
        malformed.write_text(json.dumps(dict(diagnostic, cleanup=["not-an-object"])) + "\n")
        projected, _ = eval_runner.read_broker_log(malformed)
        self.assertEqual(projected["malformed_lines"], 1)
        self.assertEqual(projected["exits"], [])

    def test_graph_execution_failure_cannot_be_hidden_by_a_final_answer(self):
        graph = self.work / "failing-graph"
        graph.write_text("#!/usr/bin/python3\nimport sys\n"
                         "if sys.argv[1] == 'version':\n"
                         "    print('{\"crate_version\":\"test\"}'); sys.exit(0)\n"
                         "sys.stderr.write('bounded graph failure\\n'); sys.exit(7)\n")
        graph.chmod(0o700)
        code, _, stderr, out = self.episode(
            [{"call": "graph_sync"}, OK_STEPS[0], OK_STEPS[-1]], arm="graph", graph=str(graph))
        self.assertEqual(code, 0, stderr)
        artifact = self.assert_outcome(out, "failed", "tool_execution_failed")
        self.assertEqual([c["status"] for c in artifact["calls"]], ["failed", "ok"])
        body = json.loads(artifact["calls"][0]["output"])
        self.assertEqual(body["exit_code"], 7)
        self.assertEqual(body["cleanup"]["survivors"], [])
        self.assertIn("bounded graph failure", body["stderr"])

    def test_final_answer_falls_back_to_jsonl_agent_message(self):
        steps = OK_STEPS[:-1] + [{"final": json.dumps(support.ANSWER), "jsonl_only": True}]
        code, _, stderr, out = self.episode(steps)
        self.assertEqual(code, 0, stderr)
        artifact = self.assert_outcome(out, "ok", None)
        self.assertEqual(artifact["final_source"], "jsonl_agent_message")

    def test_graph_episode_counts_sync_inside_the_episode(self):
        steps = [{"call": "graph_sync"}, {"call": "search", "arguments": {"query": "price"}},
                 {"final": json.dumps(support.ANSWER)}]
        code, _, stderr, out = self.episode(steps, arm="graph")
        self.assertEqual(code, 0, stderr)
        artifact = self.assert_outcome(out, "ok", None)
        calls = artifact["calls"]
        self.assertEqual([call["tool"] for call in calls], ["graph_sync", "search"])
        self.assertEqual(artifact["timing"]["graph_sync_ms"], calls[0]["elapsed_ms"])
        self.assertEqual(json.loads(json.loads(calls[1]["output"])["output"])["argv"],
                         ["search", "--json", "--", "price"])
        isolation = artifact["isolation"]
        self.assertTrue(isolation["cold_start"])
        self.assertEqual(isolation["graph_state_after"]["files"], 1)
        self.assertTrue(isolation["cache_id"].endswith(":graph-state"))
        self.assertIn("orbit-graph", artifact["tool_versions"])
        self.assertIn("0.0.0-fake", artifact["tool_versions"]["orbit-graph"])

    def test_only_the_arms_broker_tools_are_approved_under_never(self):
        for arm, steps in (("baseline", OK_STEPS),
                           ("graph", [{"call": "graph_sync"}, OK_STEPS[0], OK_STEPS[-1]])):
            with self.subTest(arm=arm):
                code, _, stderr, out = self.episode(steps, arm=arm)
                self.assertEqual(code, 0, stderr)
                artifact = self.assert_outcome(out, "ok", None)
                tools = list(support.eval_broker.ARM_TOOLS[arm])
                server = f"mcp_servers.{eval_runner.BROKER_SERVER}"
                overrides = [value for flag, value in zip(artifact["provider"]["argv"],
                                                          artifact["provider"]["argv"][1:])
                             if flag == "-c"]
                approvals = sorted(value for value in overrides if "approval_mode" in value)
                self.assertEqual(approvals, sorted(
                    [f'{server}.default_tools_approval_mode="prompt"'] +
                    [f'{server}.tools.{tool}.approval_mode="approve"' for tool in tools]))
                self.assertIn('approval_policy="never"', overrides)
                self.assertIn(f"{server}.enabled_tools={json.dumps(tools)}", overrides)
                self.assertIn(f"{server}.required=true", overrides)
                self.assertFalse([value for value in overrides if value.startswith(
                    "mcp_servers.") and not value.startswith(server + ".")])
                inventory = artifact["isolation"]["inventory"]["tool_approvals"]
                self.assertEqual(inventory, {
                    "approval_policy": "never", "default_tools_approval_mode": "prompt",
                    "approval_mode": "approve", "approved_tools": tools, "enabled_tools": tools,
                    "per_tool_key_validated":
                        f"{server}.tools.{tools[0]}.approval_mode",
                    "per_tool_readback": None})
                preflight = json.loads((out / "preflight.json").read_text())
                self.assertEqual(preflight["mcp_server"]["enabled_tools"], tools)

    def test_missing_usage_is_null_not_imputed(self):
        code, _, stderr, out = self.episode(usage=None)
        self.assertEqual(code, 0, stderr)
        artifact = self.assert_outcome(out, "ok", None)
        self.assertIsNone(artifact["usage"])
        self.assertIsNone(artifact["usage_raw"])

    def test_provider_broker_parallel_completions_preserve_measurements(self):
        cases = [("graph", [OK_STEPS[1], OK_STEPS[2], OK_STEPS[0],
                            {"call": "search", "arguments": {"query": "price"}}], [0, 2, 1, 3]),
                 ("baseline", [OK_STEPS[0], {"call": "read", "arguments": {"path": "README.md"}},
                               OK_STEPS[0]], [2, 1, 0]),
                 ("baseline", [OK_STEPS[0], {"call": "read", "arguments": {"path": "absent"}}],
                  [1, 0])]
        for arm, calls, order in cases:
            with self.subTest(arm=arm, calls=calls):
                _, _, _, sequential = self.episode(calls + OK_STEPS[-1:], arm=arm)
                reference = self.assert_outcome(sequential, "ok", None)
                code, report, stderr, out = self.episode(
                    [{"parallel": calls, "completion_order": order}, OK_STEPS[-1]], arm=arm)
                self.assertEqual(code, 0, stderr)
                artifact = self.assert_outcome(out, "ok", None)
                self.assertEqual(report["tool_calls"], len(calls))
                self.assertEqual(artifact["calls_dropped"], 0)

                def measured(episode, directory):
                    return [{k: c[k].replace(str(directory / "snapshot" / "repo"), "<repository>")
                             for k in ("tool", "input", "output", "status")}
                            for c in episode["calls"]]

                self.assertEqual(measured(artifact, out), measured(reference, sequential))
                broker_log, _ = eval_runner.read_broker_log(out / "broker-calls.jsonl")
                self.assertEqual(artifact["calls"], broker_log["calls"])
                self.assertEqual(artifact["output_bytes"], reference["output_bytes"])
                self.assertEqual(artifact["answer"], reference["answer"])
                self.assertEqual(artifact["transcript"]["mcp_calls"],
                                 [{"tool": calls[i]["call"],
                                   "status": "completed" if artifact["calls"][i]["status"] == "ok"
                                             else "failed"} for i in order])
                # Sealed/raw and public schemas stay readable with the new audit.
                record = eval_runner.public_record(eval_runner.load_raw_episode(
                    self.forge_contained(out)))
                eval_runner.check_public_record(record)
                self.assertEqual(record["calls"], artifact["calls"])

    def test_provider_broker_mismatched_call_reports_fail_closed(self):
        cases = [("missing", {"omit_completed": True}, "incomplete"),
                 ("duplicate", {"duplicate_completed": True}, "duplicate"),
                 ("arguments", {"completed": {"arguments": {"path": "README.md"}}}, "arguments"),
                 ("tool", {"completed": {"tool": "rg"}}, "tool"),
                 ("id", {"completed": {"id": "unexpected"}}, "unexpected"),
                 ("no id", {"omit_fields": ["id"]}, "id"),
                 ("long id", {"completed": {"id": "x" * 129}}, "id"),
                 ("invalid id", {"completed": {"id": "\ud800"}}, "id"),
                 ("no arguments", {"omit_fields": ["arguments"]}, "arguments"),
                 ("no result", {"omit_fields": ["result"]}, "result"),
                 ("invalid output", {"completed": {"result": {
                     "content": [{"type": "text", "text": "\ud800"}]}}}, "UTF-8"),
                 ("extra content", {"completed": {"result": {
                     "content": [{"type": "text", "text": "a"},
                                 {"type": "text", "text": "b"}]}}}, "result"),
                 ("structured result", {"completed": {"result": {
                     "content": [{"type": "text", "text": "a"}],
                     "structured_content": {"unexpected": True}}}}, "structured"),
                 ("isError", {"completed": {"result": {
                     "content": [{"type": "text", "text": "a"}], "isError": True}}}, "isError"),
                 ("altered output", {"completed": {"result": {
                     "content": [{"type": "text", "text": "altered"}]}}}, "unmatched"),
                 ("altered status", {"completed": {"status": "failed"}}, "unmatched"),
                 ("invalid status", {"completed": {"status": "in_progress"}}, "status"),
                 ("error", {"completed": {"error": {"message": "transport failed"}}}, "error"),
                 ("no start", {"omit_started": True}, "unexpected"),
                 ("different start", {"started": {"arguments": {"path": "README.md"}}},
                  "arguments"),
                 ("broker arguments", {"started": {"arguments": {"path": "README.md"}},
                                       "completed": {"arguments": {"path": "README.md"}}},
                  "unmatched"),
                 ("argument types", {"arguments": {"path": "src/lib.rs", "start_line": 1},
                                     "started": {"arguments": {"path": "src/lib.rs",
                                                                 "start_line": True}},
                                     "completed": {"arguments": {"path": "src/lib.rs",
                                                                   "start_line": True}}},
                  "unmatched")]
        for name, overrides, diagnostic in cases:
            with self.subTest(name=name):
                code, _, stderr, out = self.episode([dict(OK_STEPS[0], **overrides),
                                                     OK_STEPS[-1]])
                self.assertEqual(code, 0, stderr)
                artifact = self.assert_outcome(out, "failed", "telemetry_mismatch")
                self.assertIn(diagnostic, artifact["error"]["message"])

    def test_provider_broker_duplicate_names_do_not_hide_substitutions(self):
        calls = [OK_STEPS[0], {"call": "read", "arguments": {"path": "README.md"}}]
        cases = [[calls[0], dict(calls[1], completed={"arguments": calls[0]["arguments"]})],
                 [dict(calls[0], result_from=1), dict(calls[1], result_from=0)],
                 [calls[0], dict(calls[1], started={"id": "item_1"},
                                completed={"id": "item_1"})],
                 [dict(calls[0], duplicate_completed=True), dict(calls[1], omit_completed=True)]]
        for corrupted in cases:
            with self.subTest(calls=corrupted):
                code, _, stderr, out = self.episode(
                    [{"parallel": corrupted, "completion_order": [1, 0]}, OK_STEPS[-1]])
                self.assertEqual(code, 0, stderr)
                self.assert_outcome(out, "failed", "telemetry_mismatch")

    def test_provider_broker_unexpected_completion_is_rejected(self):
        # The old subsequence check accepted this extra report of an allowed tool.
        extra = {"id": "extra", "type": "mcp_tool_call", "server": "eval_broker", "tool": "read",
                 "arguments": {"path": "README.md"}, "status": "in_progress"}
        steps = [OK_STEPS[0], {"emit": {"type": "item.started", "item": extra}},
                 {"emit": {"type": "item.completed", "item": dict(extra, status="completed",
                      result={"content": [{"type": "text", "text": "invented"}]})}}, OK_STEPS[-1]]
        code, _, stderr, out = self.episode(steps)
        self.assertEqual(code, 0, stderr)
        artifact = self.assert_outcome(out, "failed", "telemetry_mismatch")
        self.assertIn("unexpected", artifact["error"]["message"])

    def test_provider_broker_malformed_and_partial_transcripts_fail_closed(self):
        for raw, partial in (("{not json", False), ('{"type":"a","type":"b"}', False),
                             ("[]", False), ('{"value":NaN}', False), ('{"type":"turn.completed"}',
                                                                       True)):
            with self.subTest(raw=raw, partial=partial):
                code, _, stderr, out = self.episode(
                    OK_STEPS + [{"emit_raw": raw, "newline": not partial}],
                    no_turn_completed=partial)
                self.assertEqual(code, 0, stderr)
                self.assert_outcome(out, "failed", "provider_output_malformed")

    # -- adapter -----------------------------------------------------------
    def test_adapter_refuses_uncontained_and_tampered_episodes(self):
        _, _, _, out = self.episode()
        code, _, stderr = self.cli("adapt", "--episode", out, "--output", self.work / "b.json")
        self.assertEqual(code, 1)
        self.assertIn("uncontained", stderr)
        self.assertFalse((self.work / "b.json").exists())

        forged = self.forge_contained(out)
        with open(forged / "provider.jsonl", "ab") as stream:
            stream.write(b"\n")
        code, _, stderr = self.cli("adapt", "--episode", forged, "--output", self.work / "c.json")
        self.assertEqual(code, 1)
        self.assertIn("does not match its recorded hash", stderr)

        forged = self.forge_contained(out)
        artifact = self.artifact(forged)
        artifact["output_bytes"] += 1
        (forged / "episode.json").write_text(json.dumps(artifact))
        code, _, stderr = self.cli("adapt", "--episode", forged, "--output", self.work / "d.json")
        self.assertEqual(code, 1)
        self.assertIn("artifact hash mismatch", stderr)

    def test_adapter_emits_the_public_paired_contract(self):
        _, _, _, baseline = self.episode()
        graph_steps = [{"call": "graph_sync"}, {"call": "show", "arguments":
                                                {"selector": "symbol:src/lib.rs#price:function"}},
                       {"final": json.dumps(support.ANSWER)}]
        _, _, _, graph = self.episode(graph_steps, arm="graph")
        _, _, _, failed = self.episode(OK_STEPS[:-1])  # final answer missing
        bundle_path = self.work / "bundle.json"
        episodes = [self.forge_contained(graph), self.forge_contained(baseline)]
        code, report, stderr = self.cli("adapt", "--episode", episodes[0], "--episode",
                                        episodes[1], "--output", bundle_path)
        self.assertEqual(code, 0, stderr)
        bundle = json.loads(bundle_path.read_text())
        self.assertEqual(report["bundle_sha256"], eval_runner.digest(bundle))
        self.assertEqual(stat.S_IMODE(os.stat(bundle_path).st_mode), 0o600)
        self.assertEqual((bundle["schema_version"], bundle["study_kind"]), (1, "agent"))
        self.assertEqual([e["request"]["arm"] for e in bundle["episodes"]], ["baseline", "graph"])
        for record in bundle["episodes"]:
            self.assertEqual(tuple(sorted(record)), tuple(sorted(eval_runner.PUBLIC_FIELDS)))
            body = {key: value for key, value in record.items() if key != "record_sha256"}
            self.assertEqual(record["record_sha256"], eval_runner.digest(body))
            eval_runner.check_public_record(record)
        self.assertNotEqual(bundle["episodes"][0]["isolation"]["cache_id"],
                            bundle["episodes"][1]["isolation"]["cache_id"])
        # Never overwritten; orders must be complete; failures convert too.
        code, _, stderr = self.cli("adapt", "--episode", episodes[0], "--episode", episodes[1],
                                   "--output", bundle_path)
        self.assertEqual(code, 1)
        code, _, stderr = self.cli("adapt", "--episode", episodes[0], "--output",
                                   self.work / "partial.json")
        self.assertEqual(code, 1)
        self.assertIn("orders must be 0..n-1", stderr)
        record = eval_runner.public_record(eval_runner.load_raw_episode(
            self.forge_contained(failed)))
        self.assertEqual((record["status"], record["error"]["code"], record["answer"]),
                         ("failed", "final_answer_missing", None))

    # -- failure classification ---------------------------------------------
    def test_incomplete_provider_completion_fails(self):
        for extra in ({"no_turn_completed": True}, {"exit_code": 1},
                      {"turn_failed": "upstream error"}):
            with self.subTest(extra=extra):
                code, report, stderr, out = self.episode(**extra)
                self.assertEqual(code, 0, stderr)
                self.assertEqual(report["status"], "failed")
                self.assert_outcome(out, "failed", "provider_incomplete")

    def test_missing_or_malformed_final_answers(self):
        cases = [(OK_STEPS[:-1], "failed", "final_answer_missing"),
                 (OK_STEPS[:-1] + [{"final": "the answer is price"}], "invalid",
                  "answer_malformed"),
                 (OK_STEPS[:-1] + [{"final": json.dumps(dict(support.ANSWER, extra=1))}],
                  "invalid", "answer_malformed"),
                 (OK_STEPS[:-1] + [{"final": '{"items":[],"items":[]}'}], "invalid",
                  "answer_malformed"),
                 ([{"final": json.dumps(support.ANSWER)}], "invalid", "no_tool_calls")]
        for steps, status, error in cases:
            with self.subTest(error=error, final=steps[-1]):
                code, _, stderr, out = self.episode(steps)
                self.assertEqual(code, 0, stderr)
                self.assert_outcome(out, status, error)

    def test_non_broker_tool_use_invalidates_the_episode(self):
        # Codex 0.160.0 reports its native MCP resource tools as mcp_tool_call items:
        # list_mcp_resources under server "codex", read_mcp_resource under the
        # server it targets, so the broker's server name alone is not proof.
        for step in ({"unbrokered": "command_execution"}, {"unbrokered": "web_search"},
                     {"unbrokered": "mcp_tool_call", "server": "other"},
                     {"unbrokered": "mcp_tool_call", "server": "codex",
                      "tool": "list_mcp_resources"},
                     {"unbrokered": "mcp_tool_call", "server": "eval_broker",
                      "tool": "read_mcp_resource"},
                     {"unbrokered": "mcp_tool_call", "server": "eval_broker",
                      "tool": "graph_sync"}):
            with self.subTest(step=step):
                code, _, stderr, out = self.episode([OK_STEPS[0], step, OK_STEPS[-1]])
                self.assertEqual(code, 0, stderr)
                artifact = self.assert_outcome(out, "invalid", "unbrokered_tool_use")
                self.assertEqual(artifact["isolation"]["unbrokered"][0]["type"],
                                 step["unbrokered"])

    def test_malformed_or_unattributed_provider_telemetry_fails(self):
        _, _, _, out = self.episode([OK_STEPS[0], {"emit_raw": "{not json"}, OK_STEPS[-1]])
        self.assert_outcome(out, "failed", "provider_output_malformed")
        hidden_call = json.dumps({"jsonrpc": "2.0", "id": 900, "method": "tools/call",
                                  "params": {"name": "read", "arguments": {"path": "README.md"}}})
        _, _, _, out = self.episode([OK_STEPS[0], {"raw": hidden_call, "expect_reply": True},
                                     OK_STEPS[-1]])
        self.assert_outcome(out, "failed", "telemetry_mismatch")

    def test_approval_gate_refusals_are_a_distinct_harness_failure(self):
        code, report, stderr, out = self.episode(deny_tool_approvals=True)
        self.assertEqual(code, 0, stderr)
        self.assertEqual(report["error"]["code"], "provider_tool_approval_required")
        artifact = self.assert_outcome(out, "invalid", "provider_tool_approval_required")
        self.assertEqual(artifact["calls"], [])
        self.assertEqual([denial["tool"] for denial in artifact["transcript"]["approval_denied"]],
                         ["read", "rg", "git"])
        self.assertIn(eval_runner.APPROVAL_DENIED,
                      artifact["transcript"]["approval_denied"][0]["message"])
        self.assertIn("approval_policy=never", artifact["error"]["message"])
        code, _, stderr = self.cli("adapt", "--episode", self.forge_contained(out), "--output",
                                   self.work / "harness.json")
        self.assertEqual(code, 1)
        self.assertIn("measures the harness", stderr)
        # One refused call taints an episode whose other calls reached the broker.
        steps = OK_STEPS[:1] + [{"emit": {"type": "item.completed", "item": {
            "id": "denied", "type": "mcp_tool_call", "server": "eval_broker", "tool": "rg",
            "status": "failed", "error": {"message": "MCP tool call requires approval, but "
                                                     "approval policy is never"}}}}] + OK_STEPS[1:]
        code, _, stderr, out = self.episode(steps)
        self.assertEqual(code, 0, stderr)
        artifact = self.assert_outcome(out, "invalid", "provider_tool_approval_required")
        self.assertEqual([call["tool"] for call in artifact["calls"]], ["read", "rg", "git"])

    def test_tool_call_budget_stops_the_episode(self):
        limits = dict(support.LIMITS, tool_calls=2)
        code, _, stderr, out = self.episode(OK_STEPS + [{"sleep": 30}], limits=limits)
        self.assertEqual(code, 0, stderr)
        artifact = self.assert_outcome(out, "failed", "tool_call_budget_exceeded")
        self.assertEqual(len(artifact["calls"]), 2)
        self.assertEqual(artifact["cleanup"]["terminated_by"], "budget")
        self.assertLess(self.elapsed, 25)

    def test_output_budget_is_enforced_by_the_broker(self):
        big = {"src/big.txt": "".join(f"line {n} price\n" for n in range(400))}
        self.head = support.write_tree(self.work / "head2", dict(support.HEAD, **big))
        self.base = support.write_tree(self.work / "base2", dict(support.BASE, **big))
        limits = dict(support.LIMITS, output_bytes=2048, call_bytes=1024)
        steps = [{"call": "read", "arguments": {"path": "src/big.txt", "max_lines": 40}}] * 4
        code, _, stderr, out = self.episode(steps + OK_STEPS[-1:], limits=limits)
        self.assertEqual(code, 0, stderr)
        artifact = self.assert_outcome(out, "failed", "output_budget_exceeded")
        self.assertLessEqual(artifact["output_bytes"] - len(artifact["final_output"].encode()),
                             limits["output_bytes"])

    def test_provider_stdout_flood_is_bounded_and_stopped(self):
        code, _, stderr, out = self.episode(
            [OK_STEPS[0], {"spam_stdout": eval_runner.MAX_PROVIDER_STDOUT + 2 ** 20},
             {"sleep": 30}])
        self.assertEqual(code, 0, stderr)
        artifact = self.assert_outcome(out, "failed", "provider_output_truncated")
        self.assertTrue(artifact["files"]["provider.jsonl"]["truncated"])
        self.assertLessEqual(artifact["files"]["provider.jsonl"]["bytes"],
                             eval_runner.MAX_PROVIDER_STDOUT * 3)
        self.assertLess(self.elapsed, 25)

    def test_wall_timeout_kills_the_whole_provider_group(self):
        sleeper = self.work / "sleeper.pid"
        limits = dict(support.LIMITS, wall_ms=3000)
        code, _, stderr, out = self.episode([OK_STEPS[0], {"spawn_sleeper": str(sleeper)},
                                             {"sleep": 120}], limits=limits)
        self.assertEqual(code, 0, stderr)
        artifact = self.assert_outcome(out, "timeout", "wall_time_exceeded")
        self.assertEqual(artifact["cleanup"]["terminated_by"], "timeout")
        self.assertTrue(artifact["cleanup"]["pipes_closed"])
        self.assertLess(self.elapsed, 3 + eval_runner.TERMINATION_GRACE_S + 20)
        self.assertFalse(support.alive(int(sleeper.read_text())))
        starts = [json.loads(line)["identity"]["pid"] for line in
                  (out / "broker-calls.jsonl").read_text().splitlines()
                  if json.loads(line)["type"] == "start"]
        self.assertTrue(starts)
        self.assertFalse(any(support.alive(pid) for pid in starts))

    def test_secret_provider_env_values_never_reach_episode_files(self):
        steps = [OK_STEPS[0], {"echo_env": "FAKE_API_KEY"}, OK_STEPS[-1]]
        code, report, stderr, out = self.episode(steps, args=["--provider-env", "FAKE_API_KEY"],
                                                 env={"FAKE_API_KEY": SECRET})
        self.assertEqual(code, 0, stderr)
        self.assertNotIn(SECRET, json.dumps(report) + stderr)
        artifact = self.assert_outcome(out, "ok", None)
        self.assertGreaterEqual(artifact["files"]["provider.jsonl"]["redactions"], 1)
        self.assertGreaterEqual(artifact["files"]["provider-stderr.txt"]["redactions"], 1)
        for path in out.rglob("*"):
            if path.is_file() and not path.is_symlink() and os.access(path, os.R_OK):
                self.assertNotIn(SECRET.encode(), path.read_bytes(), path)
        self.assertEqual(eval_runner.load_raw_episode(out)["artifact_sha256"],
                         artifact["artifact_sha256"])

    def test_code_mode_models_fail_closed_unless_explicitly_allowed(self):
        code, _, stderr, out = self.episode(requires_code_mode=True)
        self.assertEqual(code, 0, stderr)
        artifact = self.assert_outcome(out, "invalid", "provider_code_mode_required")
        self.assertEqual(artifact["calls"], [])
        self.assertFalse(artifact["isolation"]["inventory"]["code_mode_allowed"])
        code, _, stderr = self.cli("adapt", "--episode", self.forge_contained(out), "--output",
                                   self.work / "harness.json")
        self.assertEqual(code, 1)
        self.assertIn("measures the harness", stderr)

        # The opt-in needs the host binary next to codex; copies keep realpath there.
        release = self.work / "release" / "bin"
        release.mkdir(parents=True)
        shutil.copy2(support.FAKE_CODEX, release / "codex")
        args = ["--codex", release / "codex", "--allow-code-mode"]
        self.assert_refused(self.episode(args=args, exec_marker=str(self.work / "m")),
                            "expected_tool_unavailable", self.work / "m")
        host = release / "codex-code-mode-host"
        host.write_text("#!/bin/sh\nexit 0\n")
        host.chmod(0o755)
        code, _, stderr, out = self.episode(args=args, requires_code_mode=True)
        self.assertEqual(code, 0, stderr)
        artifact = self.assert_outcome(out, "ok", None)
        inventory = artifact["isolation"]["inventory"]
        self.assertTrue(inventory["code_mode_allowed"])
        self.assertNotIn("code_mode_host", inventory["disabled_verified"])
        self.assertEqual(artifact["model"]["settings"], {"code_mode_host": "enabled"})
        self.assertNotIn("code_mode_host", artifact["provider"]["argv"])

    # -- capability refusals: the provider never starts ---------------------
    def assert_refused(self, result, code, marker):
        exit_code, report, stderr, out = result
        self.assertEqual(exit_code, eval_runner.EXIT_REFUSED, stderr)
        self.assertEqual(report["refusal"]["code"], code, report)
        self.assertFalse(report["provider_started"])
        self.assertFalse(marker.exists(), "the provider episode must not start")
        self.assertFalse((out / "episode.json").exists())
        if out.exists():
            refusal = json.loads((out / "refusal.json").read_text())
            self.assertEqual(refusal["refusal"]["code"], code)

    def test_provider_surface_that_cannot_be_verified_is_refused(self):
        cases = [({"sticky": ["shell_tool"]}, "provider_feature_not_disableable"),
                 ({"drop_features": ["hooks"]}, "provider_feature_unknown"),
                 ({"extra_mcp": True}, "mcp_inventory_mismatch"),
                 ({"drop_enabled_tools": True}, "mcp_tool_inventory_mismatch"),
                 ({"drop_default_approval": True}, "provider_tool_approval_unverified"),
                 ({"unvalidated_tool_approvals": True}, "provider_tool_approval_unsupported")]
        for extra, code in cases:
            for command in ("preflight", "run"):
                with self.subTest(code=code, command=command):
                    marker = self.work / f"exec-{self.counter}"
                    self.assert_refused(self.episode(command=command, exec_marker=str(marker),
                                                     **extra), code, marker)

    def test_unavailable_bubblewrap_is_a_capability_refusal(self):
        marker = self.work / "exec-marker"
        args = ["--containment", "bwrap", "--bwrap", support.FAKE_BWRAP, "--auth-file", "none",
                "--truth-path", self.truth, "--provider-env", "FAKE_BWRAP_MODE"]
        result = self.episode(args=args, env={"FAKE_BWRAP_MODE": "denied"},
                              exec_marker=str(marker))
        self.assert_refused(result, "containment_unavailable", marker)
        self.assertIn("No permissions", result[1]["refusal"]["evidence"]["stderr"])

    def test_capability_probe_uses_the_runner_sandbox_layout(self):
        # A probe without the /lib links cannot start a dynamic /usr/bin/true and
        # used to skip the live test on hosts where the runner's sandbox works.
        system = eval_runner.system_sandbox_argv(str(support.FAKE_BWRAP))
        probe = bwrap_probe_argv(str(support.FAKE_BWRAP))
        self.assertEqual(probe, system + ["--", "/usr/bin/true"])
        for name in ("bin", "lib", "lib64"):
            host = f"/{name}"
            if os.path.islink(host):
                self.assertIn(["--symlink", os.readlink(host), host],
                              [probe[i:i + 3] for i in range(len(probe))], host)
        args = ["--containment", "bwrap", "--bwrap", support.FAKE_BWRAP, "--auth-file", "none",
                "--truth-path", self.truth, "--provider-env", "FAKE_BWRAP_MODE"]
        code, report, stderr, out = self.episode(command="preflight", args=args,
                                                 env={"FAKE_BWRAP_MODE": "passthrough"})
        self.assertEqual(code, 0, stderr)
        self.assertEqual(report["status"], "ready")
        recorded = json.loads((out / "preflight.json").read_text())["sandbox_argv"]
        self.assertEqual(recorded[:len(system)], system)

    def test_capability_probe_skips_only_for_bubblewrap_refusals(self):
        denied = bwrap_unavailable(str(support.FAKE_BWRAP),
                                   env={"PATH": "/usr/bin:/bin", "FAKE_BWRAP_MODE": "denied"})
        self.assertIn("No permissions to create new namespace", denied)
        self.assertIsNone(bwrap_unavailable(str(support.FAKE_BWRAP),
                                            env={"PATH": "/usr/bin:/bin"}))
        broken = self.work / "loaderless-bwrap"
        broken.write_text("#!/bin/sh\necho 'true: error while loading shared libraries' >&2\n"
                          "exit 127\n")
        broken.chmod(0o700)
        with self.assertRaises(AssertionError):
            bwrap_unavailable(str(broken), env={"PATH": "/usr/bin:/bin"})

    def test_ineffective_sandbox_never_starts_the_provider(self):
        marker = self.work / "exec-marker"
        args = ["--containment", "bwrap", "--bwrap", support.FAKE_BWRAP, "--auth-file", "none",
                "--truth-path", self.truth, "--provider-env", "FAKE_BWRAP_MODE"]
        exit_code, report, stderr, out = self.episode(args=args,
                                                      env={"FAKE_BWRAP_MODE": "passthrough"},
                                                      exec_marker=str(marker))
        self.assertEqual(exit_code, eval_runner.EXIT_REFUSED, stderr)
        self.assertIn(report["refusal"]["code"],
                      {"containment_ineffective", "containment_probe_missing"})
        self.assertFalse(marker.exists())
        self.assertFalse((out / "episode.json").exists())

    def test_absent_expected_tools_are_refused(self):
        marker = self.work / "exec-marker"
        missing = self.work / "no-such-binary"
        not_executable = self.head / "README.md"
        cases = [(["--rg", missing], "baseline", None),
                 ([], "graph", None),
                 ([], "graph", not_executable),
                 (["--codex", missing], "baseline", None)]
        for args, arm, graph in cases:
            with self.subTest(args=args, arm=arm, graph=graph):
                self.assert_refused(self.episode(arm=arm, args=args, graph=graph,
                                                 exec_marker=str(marker)),
                                    "expected_tool_unavailable", marker)

    # -- invalid input: exit 1, nothing runs -----------------------------------
    def test_malformed_requests_and_inputs_are_rejected(self):
        marker = self.work / "exec-marker"
        good = support.make_request(self.head, self.base)
        tampered = dict(good, prompt="different")
        cases = {
            "unknown field": self.request_file(raw=json.dumps(dict(good, extra=1))),
            "graph tools in baseline": self.request_file(tools=list(
                support.eval_broker.ARM_TOOLS["graph"])),
            "request hash": self.request_file(raw=json.dumps(tampered)),
            "duplicate keys": self.request_file(raw=json.dumps(good)[:-1] + ', "order": 0}'),
            "bad limits": self.request_file(limits=dict(support.LIMITS, wall_ms=10)),
            "revision": self.request_file(source_revision="1" * 64),
            "not json": self.request_file(raw="{"),
        }
        for label, request in cases.items():
            with self.subTest(label=label):
                code, _, stderr, _ = self.episode(request=request, exec_marker=str(marker))
                self.assertEqual(code, eval_runner.EXIT_INVALID, stderr)
                self.assertIn('"invalid"', stderr)
                self.assertFalse(marker.exists())

    def test_unsafe_trees_and_options_are_rejected(self):
        marker = self.work / "exec-marker"
        request = self.request_file()
        (self.head / "escape").symlink_to(self.work / "truth")
        code, _, stderr, _ = self.episode(request=request, exec_marker=str(marker))
        self.assertEqual(code, eval_runner.EXIT_INVALID)
        self.assertIn("symlink refused", stderr)
        (self.head / "escape").unlink()
        os.makedirs(self.head / "src" / ".git")
        code, _, stderr, _ = self.episode(request=request, exec_marker=str(marker))
        self.assertEqual(code, eval_runner.EXIT_INVALID)
        self.assertIn("reserved", stderr)
        os.rmdir(self.head / "src" / ".git")
        for args, text in ((["--provider-env", "ORBIT_TOKEN"], "privileged"),
                           (["--setting", "sandbox_mode=danger-full-access"], "--setting"),
                           (["--containment", "bwrap", "--bwrap", support.FAKE_BWRAP],
                            "--truth-path")):
            with self.subTest(args=args):
                code, _, stderr, _ = self.episode(args=args, exec_marker=str(marker),
                                                  env={"ORBIT_TOKEN": "x"})
                self.assertEqual(code, eval_runner.EXIT_INVALID, stderr)
                self.assertIn(text, stderr)
        existing = self.work / "existing"
        existing.mkdir()
        code, _, stderr = self.cli("run", "--request", self.request_file(), "--head", self.head,
                                   "--base", self.base, "--out", existing, "--codex",
                                   support.FAKE_CODEX, "--model", MODEL, "--rg", support.RG,
                                   "--containment", "none")
        self.assertEqual(code, eval_runner.EXIT_INVALID)
        self.assertIn("output already exists", stderr)
        self.assertFalse(marker.exists())

    def test_writable_state_files_are_parsed_defensively(self):
        for raw in (b"", b"{", b"[1]", b'{"a":1,"a":2}', b"NaN"):
            self.assertIsNone(eval_runner.state_object(raw, "probe"), raw)
        log = self.work / "log.jsonl"
        log.write_bytes(b'[1]\n7\n{"type":"call","tool":"read","status":"ok"}\nnot json\n')
        parsed, _ = eval_runner.read_broker_log(log)
        self.assertEqual((parsed["malformed_lines"], len(parsed["calls"])), (3, 1))

    # -- content identity and snapshot ----------------------------------------
    def test_revision_matches_the_public_canonical_digest(self):
        code, report, stderr = self.cli("revision", "--tree", self.head)
        self.assertEqual(code, 0, stderr)
        expected = hashlib.sha256(json.dumps(support.HEAD, sort_keys=True,
                                             separators=(",", ":")).encode()).hexdigest()
        self.assertEqual(report["content_revision"], expected)
        self.assertEqual(report["files"], len(support.HEAD))

    def test_snapshot_is_two_deterministic_commits(self):
        first, snapshot = support.snapshot_repo(self.work / "one")
        _, again = support.snapshot_repo(self.work / "two")
        self.assertEqual(snapshot["commit_count"], 2)
        for key in ("head_commit", "base_commit", "head_tree", "base_tree"):
            self.assertEqual(snapshot[key], again[key], key)
        shown = subprocess.run(["/usr/bin/git", "-C", str(first), "show", "HEAD^:src/lib.rs"],
                               capture_output=True, check=True, env={"PATH": "/usr/bin:/bin"})
        self.assertEqual(shown.stdout.decode(), support.BASE["src/lib.rs"])
        self.assertEqual(snapshot["work_tree_revision"], support.revision(self.head))
        self.assertEqual(snapshot["base"]["content_revision"], support.revision(self.base))


@unittest.skipUnless(support.RG, "ripgrep (rg) not on PATH")
class LiveContainmentTests(EpisodeCase):
    """Real bubblewrap with the fake provider, in the runner's own sandbox layout.

    Skipped, with bubblewrap's reason, only where it cannot create namespaces;
    AGENT_EVAL_REQUIRE_BWRAP=1 turns that skip into a failure on the episode host.
    """

    def setUp(self):
        reason = bwrap_unavailable()
        if reason and os.environ.get(REQUIRE_BWRAP) == "1":
            self.fail(f"{REQUIRE_BWRAP}=1, but {reason}")
        if reason:
            self.skipTest(reason)
        super().setUp()

    def test_contained_episode_hides_truth_and_converts(self):
        args = ["--containment", "bwrap", "--auth-file", "none", "--truth-path", self.truth]
        code, _, stderr, out = self.episode(args=args)
        self.assertEqual(code, 0, stderr)
        artifact = self.assert_outcome(out, "ok", None)
        probe = artifact["isolation"]["probe"]
        self.assertTrue(probe["hidden"], probe)
        self.assertEqual(probe["leaked"], [])
        self.assertTrue(artifact["isolation"]["contained"])
        self.assertIn(str(self.truth), probe["forbidden"])
        self.assertEqual(artifact["isolation"]["inventory"]["tool_approvals"]["approved_tools"],
                         ["read", "rg", "git"])
        self.assertEqual([call["tool"] for call in artifact["calls"]], ["read", "rg", "git"])
        record = eval_runner.public_record(eval_runner.load_raw_episode(out))
        eval_runner.check_public_record(record)

    @unittest.skipUnless(os.environ.get("AGENT_EVAL_ORBIT_GRAPH"), "set AGENT_EVAL_ORBIT_GRAPH")
    def test_contained_real_graph_broker(self):
        steps = [{"call": "graph_sync"}, {"call": "search", "arguments": {"query": "price"}},
                 OK_STEPS[0], OK_STEPS[-1]]
        code, _, stderr, out = self.episode(
            steps, arm="graph", graph=os.environ["AGENT_EVAL_ORBIT_GRAPH"],
            args=["--containment", "bwrap", "--auth-file", "none", "--truth-path", self.truth])
        self.assertEqual(code, 0, stderr)
        artifact = self.assert_outcome(out, "ok", None)
        self.assertTrue(artifact["isolation"]["probe"]["hidden"])
        self.assertEqual({c["status"] for c in artifact["calls"]}, {"ok"})
        self.assertEqual(artifact["broker"]["exits"][0]["exit_code"], 0)

    @unittest.skipUnless(os.environ.get("AGENT_EVAL_ORBIT_GRAPH"), "set AGENT_EVAL_ORBIT_GRAPH")
    def test_contained_real_graph_cold_search_recovery(self):
        artifact, out = self.graph_recovery(
            args=["--containment", "bwrap", "--auth-file", "none", "--truth-path", self.truth])
        self.assertTrue(artifact["isolation"]["probe"]["hidden"])
        record = eval_runner.public_record(eval_runner.load_raw_episode(out))
        eval_runner.check_public_record(record)
        self.assertEqual(record["calls"][0]["status"], "failed")


@unittest.skipUnless(support.RG and os.environ.get("AGENT_EVAL_ORBIT_GRAPH"),
                     "set AGENT_EVAL_ORBIT_GRAPH=<orbit-graph binary> for the real graph check")
class RealGraphTests(EpisodeCase):
    """The real orbit-graph binary behind the broker's fixed argv (fake provider)."""

    def test_real_graph_cold_search_recovery(self):
        self.graph_recovery()

    def test_typed_error_cannot_hide_infrastructure_failure(self):
        artifact, out = self.graph_recovery()
        original_log, _ = eval_runner.read_broker_log(out / "broker-calls.jsonl")
        original_transcript = eval_runner.analyse_transcript(
            (out / "provider.jsonl").read_bytes(), False, artifact["request"]["tools"])
        original_body = json.loads(original_log["calls"][0]["output"])
        supervision = {"terminated_by": None, "stdout_truncated": False, "stderr": b"",
                       "returncode": 0, "exit": {"code": 0}}
        cases = [("killed", {"exit_code": -9}), ("unknown exit", {"exit_code": 7}),
                 ("no exit", {"exit_code": None}), ("boolean exit", {"exit_code": True}),
                 ("timeout", {"stopped": "timeout"}),
                 ("spawn", {"stopped": "spawn_error", "error_type": "OSError"}),
                 ("supervision", {"stopped": "exception", "error_type": "OSError"}),
                 ("exception", {"error": {"code": "tool_exception"}}),
                 ("survivor", {"cleanup": {"signals": [], "survivors": [123]}}),
                 ("unknown cleanup", {"cleanup": {"signals": [], "survivors": None}}),
                 ("killed descendant", {"cleanup": {"signals": ["SIGKILL"], "survivors": []}}),
                 ("stderr truncated", {"stderr_truncated": True}),
                 ("partial output", {"output": "partial", "total_chars": 7, "next_offset": 1}),
                 ("wrong tool", {"tool": "git"}), ("wrong status", {"status": "ok"}),
                 ("non JSON", {"stderr": "index_missing"}),
                 ("not object", {"stderr": "[]"}),
                 ("wrong error type", {"stderr": '{"code":"index_missing","error":{}}'}),
                 ("duplicate keys", {"stderr": '{"code":"graph_error","code":"index_missing",'
                                               '"error":"fixture"}'})]
        for code in ("graph_error", "timeout", "index_incompatible", "version_mismatch",
                     "unsafe_state_path", "orbit_refused", "unknown"):
            cases.append((code, {"stderr": json.dumps({"code": code, "error": "fixture"})}))
        for field in ("exit_code", "stopped", "error_type", "cleanup", "stderr_truncated",
                      "stderr", "output", "offset", "total_chars", "next_offset"):
            cases.append(("missing " + field, {field: None}))
        for label, changes in cases:
            with self.subTest(label=label):
                body = dict(original_body, **changes)
                if label.startswith("missing "):
                    body.pop(label.removeprefix("missing "))
                log, transcript = copy.deepcopy(original_log), copy.deepcopy(original_transcript)
                text = json.dumps(body)
                log["calls"][0]["output"] = text
                # Keep the provider/broker audit coherent so execution classification
                # itself must reject the fault, rather than an output-hash mismatch.
                audit = transcript["mcp_call_audit"][0]
                transcript["mcp_call_audit"][0] = (*audit[:-1], eval_runner.sha256_bytes(text.encode()))
                self.assertIsNone(eval_runner.reconcile_calls(transcript, log["calls"]))
                result = eval_runner.decide(
                    supervision, transcript, log,
                    {"text": artifact["final_output"], "truncated": False},
                    support.LIMITS, None, artifact["output_bytes"], 1)
                self.assertEqual(result[:2], ("failed", "tool_execution_failed"))

    def test_graph_tools_answer_from_a_fresh_index(self):
        selector = "symbol:src/lib.rs#price:function"
        steps = [{"call": "graph_sync"}, {"call": "search", "arguments": {"query": "price"}},
                 {"call": "show", "arguments": {"selector": selector}},
                 {"call": "callees", "arguments": {"selector": selector}},
                 {"call": "refs", "arguments": {"selector": "symbol:src/lib.rs#helper:function"}},
                 {"call": "impact", "arguments": {"selector": selector}},
                 {"call": "changes"}, {"final": json.dumps(support.ANSWER)}]
        code, _, stderr, out = self.episode(steps, arm="graph",
                                            graph=os.environ["AGENT_EVAL_ORBIT_GRAPH"])
        self.assertEqual(code, 0, stderr)
        artifact = self.assert_outcome(out, "ok", None)
        self.assertEqual({call["status"] for call in artifact["calls"]}, {"ok"}, artifact["calls"])
        self.assertIn("price", artifact["calls"][1]["output"])
        self.assertIn("helper", artifact["calls"][3]["output"])
        self.assertIn("price", artifact["calls"][6]["output"])
        self.assertGreater(artifact["isolation"]["graph_state_after"]["files"], 0)


if __name__ == "__main__":
    unittest.main()
