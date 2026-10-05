"""Prospective accounting via real runner, fake provider, MCP and offline replay."""
import copy
import importlib.util
import json
import os
import signal
import sys
import time
import unittest
from unittest import mock

import support
from support import eval_runner as runner, eval_broker as broker
import plugin_profile as plugin
import telemetry
from test_runner import EpisodeCase, OK_STEPS, bwrap_unavailable

SPEC = importlib.util.spec_from_file_location("prospective_replay", support.REPO / "evals/plugin-agent-navigation/eval.py")
replay = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(replay)
FULL = {"input_tokens": 10, "cached_input_tokens": 2, "output_tokens": 3,
        "reasoning_output_tokens": 1, "total_tokens": 13}


class ProspectiveFixture(EpisodeCase):
    def prospective(self, steps=OK_STEPS, arm="baseline", limits=None, containment="none", extra_args=(), **script):
        request = self.request_file(arm, limits, schema_version=3, profile=plugin.PROSPECTIVE_PROFILE,
            plugin=self.pin, source_commits={"head": self.snapshot["head_commit"], "base": self.snapshot["base_commit"]},
            setup_limits={"wall_ms": 60000, "output_bytes": 8388608},
            tools=list(broker.ARM_TOOLS["baseline"] + (plugin.TOOLS if arm == "graph" else ())))
        return self.episode(steps, arm=arm, request=request, graph=None,
                            args=["--source-repo", self.source, "--auth-file", "none",
                                  "--containment", containment, "--truth-path", self.truth,
                                  *getattr(self, "install_args", []), *extra_args], **script)

    def run_capture(self, **kwargs):
        code, report, stderr, out = self.prospective(**kwargs)
        self.assertEqual(code, 0, (report, stderr))
        artifact = self.artifact(out)
        self.assertEqual(artifact["runner_version"], "6")
        self.assertEqual(replay.load_episode(out, diagnostic=True), artifact)
        return artifact, out

    def reseal(self, out, artifact):
        runner.seal(artifact, "artifact_sha256")
        (out / "episode.json").chmod(0o600)
        (out / "episode.json").write_text(json.dumps(artifact))

    def instrument_broker(self, needle, replacement):
        """Inject a fault into this episode's private broker, through the real runner."""
        wrapper = self.work / "instrumented-runner.py"
        wrapper.write_text(f'''import sys
sys.path.insert(0, {str(support.TOOL)!r})
import eval_runner as runner
original = runner.prepare_layout
def prepare(episode):
    original(episode)
    path = episode.runtime / "eval_broker.py"
    text = path.read_text()
    assert text.count({needle!r}) == 1
    path.chmod(0o700)
    path.write_text(text.replace({needle!r}, {replacement!r}))
    path.chmod(0o500)
runner.prepare_layout = prepare
raise SystemExit(runner.main())
''')
        return mock.patch("test_runner.RUNNER", wrapper)


@unittest.skipUnless(support.RG, "runner requires rg")
class ProspectiveUsageTests(ProspectiveFixture):
    def setUp(self):
        super().setUp()
        self.head.joinpath("README.md").unlink()
        self.base.joinpath("README.md").unlink()
        self.source, self.snapshot = support.snapshot_repo(self.work / "source")
        self.pin = {key: "0" * (40 if key == "commit" else 64) for key in plugin.PIN_FIELDS}

    def test_two_turn_partial_usage_is_not_session_total(self):
        steps = [OK_STEPS[0], {"emit": {"type": "turn.completed", "turn_id": "first", "usage": FULL}},
                 {"emit": {"type": "turn.started", "turn_id": "second"}}, OK_STEPS[-1]]
        artifact, out = self.run_capture(steps=steps, usage={"output_tokens": 0})
        usage = artifact["telemetry"]["usage"]
        self.assertIsNone(artifact["usage"])
        self.assertIsNone(artifact["usage_raw"])
        field = usage["fields"]["input_tokens"]
        self.assertEqual((field["expected_turns"], field["reported_turns"], field["observed_sum"],
                          field["coverage"], field["total"]), (2, 1, 10, "partial", None))
        output = usage["fields"]["output_tokens"]
        self.assertEqual((output["coverage"], output["observed_sum"], output["total_state"], output["total"]),
                         ("complete", 3, "unknown_semantics", None))
        self.assertEqual([t["turn_id"] for t in usage["turns"]], ["first", "second"])
        self.assertEqual([t["session_id"] for t in usage["turns"]], ["fake-thread"] * 2)
        raw = (out / "provider.jsonl").read_bytes()
        for turn in usage["turns"]:
            for observation in turn["observations"]:
                loc = observation["event"]
                self.assertEqual(telemetry.sha(raw[loc["byte_offset"]:loc["byte_offset"] + loc["bytes"]]), loc["sha256"])
        self.assertEqual(usage["turns"][1]["observations"][0]["reported_fields"], ["output_tokens"])

    def test_absent_zero_and_complete_single_turn(self):
        for counters, state in [(None, "partial"), ({k: 0 for k in FULL}, "complete"), (FULL, "complete")]:
            with self.subTest(counters=counters):
                artifact, _ = self.run_capture(usage=counters)
                fields = artifact["telemetry"]["usage"]["fields"]
                self.assertEqual(fields["input_tokens"]["coverage"], state)
                self.assertEqual(fields["input_tokens"]["total"], None if counters is None else counters["input_tokens"])
                self.assertIsNone(artifact["telemetry"]["usage"]["derived"]["uncached_input_tokens"])
                self.assertIsNone(artifact["telemetry"]["usage"]["cost_usd"])
                self.assertIsNone(artifact["telemetry"]["usage"]["per_tool_tokens"])

    def test_invalid_bool_negative_cache_and_duplicate_reasoning(self):
        for changes in [{"input_tokens": True}, {"output_tokens": -1}, {"cached_input_tokens": 11},
                        {"reasoning_tokens": 1}]:
            with self.subTest(changes=changes):
                artifact, _ = self.run_capture(usage=dict(FULL, **changes))
                fields = artifact["telemetry"]["usage"]["fields"]
                self.assertTrue(all(f["coverage"] == "inconsistent" and f["total"] is None for f in fields.values()))
                self.assertIsNone(artifact["usage"])

    def test_supplied_invalid_and_cross_session_identities_cannot_qualify(self):
        original = support.FAKE_CODEX.read_text()
        scenarios = [
            ({"thread_id": "another-session", "turn_id": "t"}, "cross_session_identity"),
            ({"thread_id": {"invalid": True}, "turn_id": ["t"]}, "invalid_identity"),
            ({"thread_id": False, "turn_id": 1}, "invalid_identity"),
            ({"thread_id": "", "turn_id": "t"}, "invalid_identity"),
        ]
        for ids, issue in scenarios:
            with self.subTest(ids=ids):
                provider = self.work / "identity-provider.py"
                start = dict(type="turn.started", **ids)
                end = dict(type="turn.completed", **ids)
                text = original.replace('emit({"type": "turn.started"})', f"emit({start!r})")
                text = text.replace('completed = {"type": "turn.completed"}', f"completed = {end!r}")
                provider.write_text(text)
                provider.chmod(0o700)
                with mock.patch("support.FAKE_CODEX", provider):
                    artifact, out = self.run_capture(usage=FULL)
                observed = artifact["telemetry"]["usage"]
                self.assertFalse(observed["session_complete"])
                self.assertIn(issue, [item["code"] for item in observed["issues"]])
                self.assertTrue(all(f["coverage"] == "inconsistent" and f["total"] is None
                                    for f in observed["fields"].values()))
                self.assertIsNone(artifact["usage"])
                # Re-sealing an invented complete ledger still fails byte replay.
                altered = copy.deepcopy(artifact)
                altered["telemetry"]["usage"]["fields"]["input_tokens"].update(
                    coverage="complete", total=10, total_state="observed_single_turn")
                self.reseal(out, altered)
                with self.assertRaisesRegex(ValueError, "ledger/coverage"):
                    replay.load_episode(out, diagnostic=True)

    def test_duplicates_failed_missing_and_truncated_turns(self):
        scenarios = [
            {"steps": [OK_STEPS[0], {"emit": {"type": "turn.completed", "usage": FULL}}, OK_STEPS[-1]]},
            {"steps": [OK_STEPS[0], {"emit": {"type": "turn.failed", "usage": FULL}},
                       {"emit": {"type": "turn.started"}}, OK_STEPS[-1]]},
            {"steps": [OK_STEPS[0], {"emit": {"type": "turn.cancelled", "usage": FULL}},
                       {"emit": {"type": "turn.started"}}, OK_STEPS[-1]]},
            {"no_turn_completed": True},
            {"steps": [*OK_STEPS, {"emit_raw": '{"type":"turn.completed","usage":{"input_tokens":1,"input_tokens":2}}'}],
             "no_turn_completed": True},
            {"steps": [*OK_STEPS, {"emit_raw": '{"type":"turn.started"', "newline": False}]},
            {"exit_code": 7},
        ]
        for scenario in scenarios:
            with self.subTest(scenario=scenario):
                artifact, _ = self.run_capture(**scenario)
                observed = artifact["telemetry"]["usage"]
                self.assertFalse(observed["session_complete"])
                self.assertTrue(all(f["total"] is None for f in observed["fields"].values()))
                self.assertIsNone(artifact["usage"])
        # Explicit bounded-stream evidence, independent of how the process stopped.
        raw = b'{"type":"turn.started"}\n{"type":"turn.completed","usage":{"input_tokens":0}}\n'
        self.assertIsNone(telemetry.usage(raw, True, True)["fields"]["input_tokens"]["total"])

    def test_replay_rejects_resealed_ledger_coverage_bytes_phase_and_version_tampering(self):
        original, out = self.run_capture(usage=FULL)
        mutations = []
        for area, key, value in [("usage", "cost_usd", 0), ("tools", "attempts", 999)]:
            altered = copy.deepcopy(original)
            altered["telemetry"][area][key] = value
            mutations.append(altered)
        altered = copy.deepcopy(original)
        altered["telemetry"]["usage"]["fields"]["input_tokens"]["total"] += 1
        mutations.append(altered)
        altered = copy.deepcopy(original)
        altered["telemetry"]["usage"]["fields"]["input_tokens"]["expected_turns"] = True
        mutations.append(altered)
        altered = copy.deepcopy(original)
        altered["telemetry"]["tools"]["graph_attempts"] = False
        mutations.append(altered)
        for key, value in [("output_bytes", 1), ("input_sha256", "0" * 64), ("elapsed_ns", True),
                           ("attempt_id", "request-999"), ("successful", False)]:
            altered = copy.deepcopy(original)
            altered["telemetry"]["attempts"][0][key] = value
            mutations.append(altered)
        altered = copy.deepcopy(original)
        altered["timing"]["provider_ms"] += 1
        mutations.append(altered)
        mutations += [dict(original, runner_version="5"), dict(original, schema_version=2),
                      dict(original, profile=plugin.PROFILE)]
        altered = copy.deepcopy(original)
        del altered["telemetry"]
        mutations.append(altered)
        for altered in mutations:
            with self.subTest(altered=altered.get("runner_version")):
                self.reseal(out, altered)
                with self.assertRaises(ValueError):
                    replay.load_episode(out, diagnostic=True)
        self.reseal(out, original)
        self.assertEqual(replay.load_episode(out, diagnostic=True), original)
        with self.assertRaises(runner.Invalid):
            runner.load_raw_episode(out)

    def test_duplicate_raw_attempt_and_historical_downgrade_cannot_fall_back(self):
        artifact, out = self.run_capture(usage=FULL)
        broker_path = out / "broker-calls.jsonl"
        raw = broker_path.read_bytes()
        broker_path.chmod(0o600)
        for kind in ("attempt_start", "request", "reply"):
            with self.subTest(kind=kind):
                first = next(line for line in raw.splitlines(keepends=True)
                             if json.loads(line)["type"] == kind)
                broker_path.write_bytes(raw + first)
                altered = copy.deepcopy(artifact)
                altered["files"]["broker-calls.jsonl"].update(
                    sha256=runner.sha256_file(broker_path), bytes=broker_path.stat().st_size)
                self.reseal(out, altered)
                with self.assertRaisesRegex(ValueError, "duplicate"):
                    replay.load_episode(out, diagnostic=True)
        broker_path.write_bytes(raw)
        # Even removing the normalized object and metadata cannot hide a
        # prospective broker under the historical capture validator.
        altered = copy.deepcopy(artifact)
        altered.update(schema_version=2, profile=plugin.PROFILE, runner_version="5")
        del altered["telemetry"]
        del altered["files"]["phase-timing.json"]
        del altered["harness"]["telemetry.py"]
        request = altered["request"]
        request.update(schema_version=2, profile=plugin.PROFILE)
        request.pop("request_sha256")
        request["request_sha256"] = runner.digest(request)
        altered["request_digest"] = runner.digest(request)
        self.reseal(out, altered)
        with self.assertRaisesRegex(ValueError, "prospective broker evidence"):
            replay.load_episode(out, diagnostic=True)

    def test_real_runner_rejects_duplicate_request_and_reply_sequences(self):
        needle = '        os.write(self.log, (canonical(entry) + "\\n").encode())'
        for kind in ("request", "reply"):
            with self.subTest(kind=kind):
                replacement = needle + f'\n        if entry["type"] == {kind!r}:\n    ' + needle
                with self.instrument_broker(needle, replacement):
                    code, report, stderr, _ = self.prospective()
                self.assertNotEqual(code, 0, report)
                self.assertIn("duplicate/invalid " + kind + " sequence", stderr)

    def test_cut_broker_tail_retains_prefix_with_unknown_attempt_coverage(self):
        needle = "        logger.record(entry)\n        return 0 if orderly_broker_exit(entry) else 1"
        replacement = ('        logger.record(entry)\n'
                       '        os.write(logger.log, b\'{"type":"attempt_sta\')\n'
                       '        return 0 if orderly_broker_exit(entry) else 1')
        refused = {"raw": json.dumps({"jsonrpc": "2.0", "id": 999, "method": "tools/call",
                   "params": {"name": "graph_search", "arguments": {}}}), "expect_reply": True}
        with self.instrument_broker(needle, replacement):
            artifact, out = self.run_capture(steps=[refused, OK_STEPS[0], OK_STEPS[-1]])
        self.assertEqual(artifact["error"]["code"], "broker_output_malformed")
        metrics = artifact["telemetry"]
        self.assertEqual((metrics["tools"]["attempts"], metrics["tools"]["completed"]), (2, 1))
        self.assertEqual(metrics["attempts"][0]["observed_outcome"], "refused")
        self.assertEqual(metrics["attempts"][1]["observed_outcome"], "ok")
        self.assertEqual(metrics["tools"]["coverage"]["state"], "partial")
        self.assertIsNone(metrics["tools"]["coverage"]["expected_attempts"])
        self.assertIsNone(metrics["tools"]["all_attempt_denominator"])
        self.assertEqual(metrics["tools"]["successful"], 0)
        self.assertFalse(any(a["successful"] for a in metrics["attempts"]))
        self.assertFalse(metrics["reconciliation"]["matched"])
        self.assertIsNone(metrics["tools"]["provider_received_output_bytes"])
        raw = (out / "broker-calls.jsonl").read_bytes()
        event = metrics["tools"]["coverage"]["issues"][0]["event"]
        self.assertEqual(telemetry.sha(raw[event["byte_offset"]:]), event["sha256"])

    def test_historical_writer_retains_partial_sum_and_old_replay(self):
        steps = [OK_STEPS[0], {"emit": {"type": "turn.completed", "usage": FULL}},
                 {"emit": {"type": "turn.started"}}, OK_STEPS[-1]]
        request = self.request_file(schema_version=2, profile=plugin.PROFILE, plugin=self.pin,
            source_commits={"head": self.snapshot["head_commit"], "base": self.snapshot["base_commit"]},
            setup_limits={"wall_ms": 60000, "output_bytes": 8388608})
        code, report, stderr, out = self.episode(steps, request=request, usage={"output_tokens": 0},
            args=["--source-repo", self.source, "--auth-file", "none"])
        self.assertEqual(code, 0, (report, stderr))
        historical = replay.load_episode(out, diagnostic=True)
        self.assertEqual(historical["runner_version"], "5")
        self.assertEqual(historical["usage"]["input_tokens"], 10)
        self.assertNotIn("telemetry", historical)

    def test_admission_and_setup_refusals_have_request_identity_without_provider(self):
        marker = self.work / "provider-started"
        code, report, stderr, out = self.prospective(sticky=["shell_tool"], exec_marker=str(marker))
        self.assertEqual(code, 3, (report, stderr))
        self.assertFalse(marker.exists())
        self.assertFalse(report["provider_started"])
        self.assertEqual(report["stage"], "admission")
        self.assertEqual(report["request_digest"], report["request_identity"])
        self.assertEqual(json.loads((out / "refusal.json").read_text()), report)
        code, report, stderr, _ = self.prospective(extra_args=["--provider-env", "ORBIT_FORBIDDEN"],
                                                  exec_marker=str(marker))
        self.assertEqual(code, 1, (report, stderr))
        self.assertEqual(report["stage"], "admission")
        self.assertFalse(report["provider_started"])
        self.assertEqual(report["request_digest"], report["request_identity"])
        self.assertFalse(marker.exists())
        # Pinned source mismatch happens in setup, before the launch probe/provider.
        self.head.joinpath("src/lib.rs").write_text("changed after source pin\n")
        code, report, stderr, out = self.prospective(exec_marker=str(marker))
        self.assertEqual(code, 1, (report, stderr))
        self.assertEqual(report["stage"], "setup")
        self.assertFalse(report["provider_started"])
        self.assertFalse(marker.exists())

    def test_final_reply_teardown_and_pending_request_controls(self):
        artifact, _ = self.run_capture(shutdown="eof_then_sigterm")
        self.assertEqual(artifact["status"], "ok")
        self.assertTrue(artifact["telemetry"]["reconciliation"]["matched"])
        failed, _ = self.run_capture(shutdown="eof_with_pending_request")
        self.assertEqual(failed["error"]["code"], "broker_failed")

    def test_budget_and_uncorroborated_refusal_captures_replay_without_qualifying(self):
        artifact, _ = self.run_capture(steps=[OK_STEPS[0], OK_STEPS[0], OK_STEPS[-1]],
                                      limits=dict(support.LIMITS, tool_calls=1))
        self.assertEqual(artifact["error"]["code"], "tool_call_budget_exceeded")
        metrics = artifact["telemetry"]
        self.assertEqual((metrics["tools"]["attempts"], metrics["tools"]["completed"]), (2, 1))
        self.assertEqual(metrics["attempts"][1]["error"]["code"], "tool_call_budget_exceeded")
        refused, _ = self.run_capture(steps=[OK_STEPS[0], {"raw": json.dumps({"jsonrpc": "2.0", "id": 999,
            "method": "tools/call", "params": {"name": "graph_search", "arguments": {}}}),
            "expect_reply": True}, OK_STEPS[-1]])
        self.assertEqual(refused["error"]["code"], "telemetry_mismatch")
        self.assertFalse(refused["telemetry"]["reconciliation"]["matched"])
        self.assertIsNone(refused["telemetry"]["tools"]["provider_received_output_bytes"])
        self.assertEqual(refused["telemetry"]["tools"]["graph_attempts"], 1)


class ProspectiveBrokerTests(unittest.TestCase):
    def setUp(self):
        self.work = support.scratch()
        self.addCleanup(support.remove, self.work)
        self.repo, self.snapshot = support.snapshot_repo(self.work)

    def client(self, limits=None, rg=None):
        config_path, state = support.broker_config(self.work, self.repo, self.snapshot, limits=limits)
        config = json.loads(config_path.read_text())
        config.update(schema_version=3, plugin=None)
        if rg:
            config["binaries"]["rg"] = str(rg)
        config_path.write_text(json.dumps(config))
        client = support.McpClient(config_path)
        self.addCleanup(client.terminate)
        result = client.request("initialize")
        self.assertEqual(result["result"]["serverInfo"]["version"], "5")
        return client, state

    def ledger(self, state):
        log, raw = runner.read_broker_log(state / "calls.jsonl")
        return telemetry.attempts(raw, log["calls"])

    def test_actual_unknown_tool_budget_protocol_and_success_attempts(self):
        client, state = self.client(dict(support.LIMITS, tool_calls=1))
        refused, error = client.call("graph_search", {})
        self.assertTrue(error)
        success, error = client.call("read", {"path": "README.md"})
        self.assertFalse(error)
        budget, error = client.call("read", {"path": "src/lib.rs"})
        self.assertTrue(error)
        stopped, error = client.call("read", {})
        self.assertTrue(error)
        malformed = client.request("tools/call", {"name": False})
        self.assertIn("error", malformed)
        self.assertEqual(client.close(), 0)
        ledger, totals = self.ledger(state)
        self.assertEqual([r["attempt_id"] for r in ledger], ["request-" + str(i) for i in range(1, 6)])
        self.assertEqual([r["error"]["code"] for r in ledger if r["error"] is not None],
                         ["tool_not_permitted", "tool_call_budget_exceeded", "episode_stopped", -32602])
        self.assertEqual((totals["attempts"], totals["completed"], totals["successful"], totals["responses"]), (5, 1, 1, 5))
        self.assertEqual((totals["graph_attempts"], totals["graph_completed"], totals["graph_successful"]), (1, 0, 0))
        self.assertEqual(totals["output_bytes"], sum(r["output_bytes"] or 0 for r in ledger))
        self.assertTrue(all(r["reply_published"] and r["elapsed_ns"] >= 0 for r in ledger))
        self.assertEqual(ledger[1]["input_bytes"], len(broker.canonical({"path": "README.md"}).encode()))
        self.assertEqual(json.loads(ledger[1]["output"]), success)

    def test_pending_then_interrupted_call_has_no_success_or_output(self):
        ready = self.work / "tool-ready"
        tool = self.work / "blocked-rg"
        tool.write_text("#!/usr/bin/python3\nimport os, signal\n"
                        f"with open({str(ready) + '.pending'!r}, 'w') as stream: stream.write(str(os.getpid()))\n"
                        f"os.replace({str(ready) + '.pending'!r}, {str(ready)!r})\n"
                        "signal.pause()\n")
        tool.chmod(0o700)
        client, state = self.client(rg=tool)
        client.send_raw(json.dumps({"jsonrpc": "2.0", "id": 22, "method": "tools/call",
                                   "params": {"name": "rg", "arguments": {"pattern": "price"}}}) + "\n")
        deadline = time.monotonic() + 10
        while not ready.exists():
            self.assertLess(time.monotonic(), deadline)
            time.sleep(.01)
        ledger, totals = self.ledger(state)
        self.assertEqual(ledger[0]["outcome"], "pending")
        self.assertEqual(totals["attempts"], 1)
        client.process.send_signal(signal.SIGTERM)
        self.assertTrue(broker.wait_leader(client.process.pid, 30))
        self.assertEqual(client.close(), 1)
        ledger, totals = self.ledger(state)
        self.assertEqual(ledger[0]["outcome"], "interrupted")
        self.assertIsNotNone(ledger[0]["interruption_event"])
        self.assertIsNone(ledger[0]["elapsed_ns"])
        self.assertIsNone(ledger[0]["output_bytes"])
        self.assertFalse(ledger[0]["successful"])
        self.assertEqual(totals["completed"], 0)

    def test_measured_or_prepared_reply_interrupted_before_publication_is_not_success(self):
        source = (support.TOOL / "eval_broker.py").read_text()
        anchor = "            self.end_attempt(result=result)"
        self.assertEqual(source.count(anchor), 1)
        for phase in ("before_return", "after_return"):
            with self.subTest(phase=phase):
                case_root = self.work / phase
                case_root.mkdir()
                config_path, state = support.broker_config(case_root, self.repo, self.snapshot)
                config = json.loads(config_path.read_text())
                config.update(schema_version=3, plugin=None)
                config_path.write_text(json.dumps(config))
                runtime = case_root / "interrupted-broker.py"
                pause = "            if self.attempt is not None: signal.pause()"
                replacement = pause + "\n" + anchor if phase == "before_return" else anchor + "\n" + pause
                runtime.write_text(source.replace(anchor, replacement))
                (case_root / "reply_provenance.py").write_bytes((support.TOOL / "reply_provenance.py").read_bytes())
                client = support.McpClient(argv=[sys.executable, "-B", str(runtime), "--config", str(config_path)])
                self.addCleanup(client.terminate)
                client.request("initialize")
                client.send_raw(json.dumps({"jsonrpc": "2.0", "id": 42, "method": "tools/call",
                                           "params": {"name": "read", "arguments": {"path": "src/lib.rs"}}}) + "\n")
                deadline = time.monotonic() + 10
                observed = "call" if phase == "before_return" else "attempt_end"
                while not any(r["type"] == observed for r in support.log_entries(state)):
                    self.assertLess(time.monotonic(), deadline)
                    time.sleep(.01)
                client.process.stdin.close()
                client.process.send_signal(signal.SIGTERM)
                self.assertTrue(broker.wait_leader(client.process.pid, 30))
                self.assertEqual(client.close(), 1)
                ledger, totals = self.ledger(state)
                self.assertEqual(ledger[0]["outcome"], "interrupted")
                self.assertTrue(ledger[0]["call_completed"])
                self.assertEqual(ledger[0]["completed"], phase == "after_return")
                self.assertFalse(ledger[0]["successful"])
                self.assertFalse(ledger[0]["reply_published"])
                self.assertIsNotNone(ledger[0]["interruption_event"])
                if phase == "before_return":
                    self.assertIsNone(ledger[0]["output"])
                else:
                    self.assertEqual(ledger[0]["observed_outcome"], "ok")
                    self.assertIsInstance(ledger[0]["output"], str)
                self.assertGreater(ledger[0]["measured_output_bytes"], 0)
                self.assertEqual((totals["attempts"], totals["completed"], totals["successful"]), (1, 1, 0))


@unittest.skipUnless(os.environ.get("AGENT_EVAL_ORBIT") and os.environ.get("AGENT_EVAL_ORBIT_GRAPH"),
                     "requires private installed-plugin binaries")
class ProspectiveInstalledTests(ProspectiveFixture):
    def setUp(self):
        from test_plugin_profile import InstalledPluginTests
        helper = InstalledPluginTests()
        helper.setUp()
        for name in ("work", "head", "base", "truth", "counter", "source", "snapshot",
                     "commit", "install_args", "pin", "inventory"):
            setattr(self, name, getattr(helper, name))

    def test_real_installed_plugin_pair_and_replay_entry_point(self):
        steps = [{"call": "graph_search", "arguments": {"query": "price"}},
                 {"call": "graph_maintain", "arguments": {"operation": "graph_sync"}},
                 {"call": "graph_search", "arguments": {"query": "price"}}, OK_STEPS[-1]]
        graph, graph_out = self.run_capture(steps=steps, arm="graph")
        baseline, base_out = self.run_capture(steps=[OK_STEPS[0], OK_STEPS[-1]])
        self.assertEqual(graph["status"], "ok")
        metrics = graph["telemetry"]
        self.assertEqual((metrics["tools"]["graph_attempts"], metrics["tools"]["graph_completed"],
                          metrics["tools"]["graph_successful"]), (3, 3, 2))
        self.assertEqual(metrics["tools"]["output_bytes"], sum(len(c["output"].encode()) for c in graph["calls"]))
        self.assertEqual(metrics["timing"]["wall_ms"], metrics["timing"]["setup_ms"] + metrics["timing"]["provider_ms"])
        self.assertGreater(metrics["timing"]["overlapping"]["plugin_install_ms"], 0)
        self.assertGreater(metrics["timing"]["outside_wall"]["preflight_ms"], 0)
        plan = {"schema_version": 3, "profile": plugin.PROSPECTIVE_PROFILE,
                "model": graph["model"], "provider_binary_sha256": graph["provider"]["binary_sha256"],
                "harness": graph["harness"], "requests": [baseline["request"], graph["request"]]}
        path = self.work / "plan.json"
        path.write_text(json.dumps(plan))
        result = broker.run_child([sys.executable, "-B", SPEC.origin, "replay", "--preregistration", str(path),
                                  "--episode", str(base_out), "--episode", str(graph_out), "--diagnostic"],
                                 str(self.work), {"PATH": "/usr/bin:/bin", "HOME": str(self.work)}, 30)
        self.assertTrue(plugin.clean(result), result)
        costs = json.loads(result["stdout"])["costs"]
        self.assertEqual(costs[1]["telemetry"], metrics)
        self.assertIsNone(graph["telemetry"]["usage"]["derived"]["uncached_input_tokens"])

    def test_strict_prospective_host_qualification(self):
        reason = bwrap_unavailable()
        if reason:
            if os.environ.get("AGENT_EVAL_REQUIRE_BWRAP") == "1":
                self.fail(reason)
            self.skipTest(reason)
        graph, _ = self.run_capture(steps=[{"call": "graph_version"}, OK_STEPS[-1]], arm="graph", containment="bwrap")
        self.assertTrue(graph["isolation"]["contained"])

    def test_safe_reply_proofs_and_byte_accounting_remain_lossless_in_schema_three(self):
        from test_reply_provenance import SafeReplyTests
        SafeReplyTests.fixture(self)
        for arm in ("baseline", "graph"):
            steps = [{"call": "read", "arguments": {"path": "src/lib.rs"}},
                     {"call": "rg", "arguments": {"pattern": "fixture"}}]
            if arm == "graph":
                steps += [{"call": "graph_maintain", "arguments": {"operation": "graph_sync"}},
                          {"call": "graph_show", "arguments": {"selector": "file:src/lib.rs"}}]
            artifact, out = self.run_capture(steps=[*steps, OK_STEPS[-1]], arm=arm,
                                            env={"FAKE_API_KEY": self.host_value})
            self.assertEqual(artifact["status"], "ok")
            self.assertEqual(artifact["redactions"], 0)
            metrics = artifact["telemetry"]
            self.assertTrue(metrics["reconciliation"]["matched"])
            self.assertEqual(metrics["tools"]["provider_received_output_bytes"],
                             sum(p["delivered_bytes"] for p in artifact["reply_provenance"]))
            captured = (out / "provider.jsonl").read_text() + (out / "broker-calls.jsonl").read_text()
            self.assertNotIn(self.key, captured)
            self.assertNotIn(self.host_value, captured)
