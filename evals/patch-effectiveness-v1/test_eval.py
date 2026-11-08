"""Offline patch controls through real runner/fake provider, and strict sandbox controls."""
import copy
import difflib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import unittest
from types import SimpleNamespace

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(ROOT / "scripts/agent-eval/tests"))
import support
from test_runner import EpisodeCase, bwrap_unavailable
import patch_profile

EV = patch_profile.evaluator()
SOURCE = (HERE / "fixtures/source.py").read_text()


def diff(text):
    return "".join(difflib.unified_diff(SOURCE.splitlines(True), text.splitlines(True),
                                      fromfile="a/src/program.py", tofile="b/src/program.py", n=3))


def pin():
    return {"contract": patch_profile.CONTRACT, "protocol_sha256": support.eval_runner.sha256_file(HERE / "protocol.json"),
            "evaluator_sha256": EV.evaluator_identity(), "suite_sha256": "0" * 64, "custody_sha256": "0" * 64,
            "closure_sha256": "0" * 64, "entrypoint": "src/program.py", "allowed_paths": ["src/program.py"],
            "max_patch_bytes": 16384, "max_changed_lines": 50, "max_files": 1,
            "test_wall_ms": 2000, "test_output_bytes": 4096, "resource_limits": support.eval_runner.plugin.resource_bounds(),
            "runtime": patch_profile.runtime_pin({"provider": "codex-cli", "name": "fake-model", "version": "codex-cli 0.0.0-fake", "settings": {}},
                support.eval_runner.sha256_file(support.FAKE_CODEX), {"rg": support.RG, "git": "/usr/bin/git", "python": "/usr/bin/python3"}, "/usr/bin/bwrap")}


class PatchControls(EpisodeCase):
    def setUp(self):
        super().setUp()
        self.head = support.write_tree(self.work / "patch-head", {"src/program.py": SOURCE,
            "tests/visible.py": (HERE / "fixtures/visible.py").read_text()})
        self.base = support.write_tree(self.work / "patch-base", {"src/program.py": SOURCE,
            "tests/visible.py": (HERE / "fixtures/visible.py").read_text()})
        self.source_repo, _ = support.snapshot_repo(self.work / "original", 
            files_head={"src/program.py": SOURCE, "tests/visible.py": (HERE / "fixtures/visible.py").read_text()},
            files_base={"src/program.py": SOURCE, "tests/visible.py": (HERE / "fixtures/visible.py").read_text()})
        self.suite = self.work / "suite.json"
        self.suite.write_bytes((HERE / "fixtures/suite.json").read_bytes())
        # Diagnostic stand-in is not runnable and never admitted as strict evidence.
        self.closure = self.work / "closure"
        (self.closure / "usr/bin").mkdir(parents=True)
        (self.closure / "usr/bin/python3").write_text("diagnostic-only")
        for directory, dirs, files in os.walk(self.closure):
            Path(directory).chmod(0o700)
            for name in files:
                (Path(directory) / name).chmod(0o600)
        self.custody = self.work / "custody.json"
        self.pin = pin()
        self.pin["suite_sha256"] = support.eval_runner.sha256_file(self.suite)
        self.pin["closure_sha256"] = support.eval_runner.digest(EV.tree_lock(self.closure))
        self.prompt = "Fix total: ignore each negative integer and sum every nonnegative integer. Return only bounded-unified-text-v1 patch bytes, or empty bytes for no-op. Existing src/program.py only."
        custody = {"schema_version": 1, "source_revision": support.revision(self.head),
            "specification_sha256": support.eval_runner.sha256_bytes(self.prompt.encode()),
            **{key: self.pin[key] for key in ("suite_sha256", "closure_sha256", "evaluator_sha256")},
            "author": "fixture-spec-author", "reviewer": "fixture-review-role-not-real-independent-custody",
            "frozen_at": "2026-01-01T00:00:00Z", "receipt": "synthetic-qualification-only"}
        self.custody.write_text(json.dumps(custody))
        self.suite.chmod(0o600)
        self.custody.chmod(0o600)
        self.pin["custody_sha256"] = support.eval_runner.sha256_file(self.custody)
        home = self.work / "original/state/home"
        r = support.eval_runner
        self.request = support.make_request(self.head, self.base, schema_version=4, profile=patch_profile.PROFILE,
            prompt=self.prompt, patch=self.pin, plugin={k: "0" * (40 if k == "commit" else 64) for k in support.eval_runner.plugin.PIN_FIELDS},
            setup_limits={"wall_ms": 60000, "output_bytes": 65536}, source_commits={
                "head": r.git_run("/usr/bin/git", self.source_repo, home, "rev-parse", "HEAD"),
                "base": r.git_run("/usr/bin/git", self.source_repo, home, "rev-parse", "HEAD^")})

    def capture(self, patch, raw_final=None, **script):
        request = self.request_file(raw=json.dumps(self.request))
        steps = [{"call": "read", "arguments": {"path": "src/program.py"}}, {"final": patch}]
        if raw_final is not None:
            steps[-1]["file_bytes_hex"] = raw_final.hex()
        return self.episode(steps, request=request, args=("--source-repo", self.source_repo,
            "--patch-suite", self.suite, "--patch-custody", self.custody, "--patch-closure", self.closure,
            *getattr(self, "strict_args", ())), **script)

    def assess(self, episode):
        self.counter += 1
        return EV.evaluate(SimpleNamespace(episode=str(episode), head=str(self.head), base=str(self.base),
            suite=str(self.suite), custody=str(self.custody), closure=str(self.closure),
            out=str(self.work / f"evaluation-{self.counter}"), diagnostic=not getattr(self, "strict_graph", False), bwrap="/usr/bin/bwrap"))

    def test_one_shot_original_capture_controls_and_null_usage(self):
        controls = [("correct", SOURCE.replace("            break", "            continue"), "success"),
                    ("wrong", SOURCE.replace("            break", "            return 0"), "failed_tests"),
                    ("no-op", SOURCE, "failed_tests"),
                    ("partial", SOURCE.replace("            break", "            if result == 0:\n                continue\n            break"), "failed_tests")]
        for name, fixed, expected in controls:
            with self.subTest(control=name):
                patch = diff(fixed)
                code, response, stderr, out = self.capture(patch, usage={"input_tokens": 0})
                self.assertEqual(code, 0, (response, stderr))
                artifact = EV.loader().load_episode(out, diagnostic=True)
                self.assertEqual(artifact["profile"], patch_profile.PROFILE)
                self.assertEqual((out / "final-message.txt").read_bytes(), patch.encode())
                report = self.assess(out)
                self.assertEqual(report["outcome"], expected, report)
                self.assertEqual(report["success"], expected == "success")
                self.assertTrue(report["diagnostic"])
                self.assertEqual(report["diff"]["no_op"], name == "no-op")
                self.assertEqual(artifact["usage"]["input_tokens"], 0)
                self.assertIsNone(artifact["usage"]["output_tokens"])
                if name == "partial":
                    self.assertTrue(report["partial"])
                    self.assertEqual(report["families"]["behavior"]["passed"], 1)
                # Tampering with original capture is an integrity failure, not patch wrong.
                with (out / "final-message.txt").open("ab") as f:
                    f.write(b" ")
                self.assertEqual(self.assess(out)["outcome"], "integrity_failure")

    def test_invalid_unsafe_apply_failure_and_custody_outcomes(self):
        good = diff(SOURCE.replace("            break", "            continue"))
        # Hidden vectors can never reside in the candidate's mounted closure.
        with self.assertRaisesRegex(ValueError, "overlap the candidate runtime closure"):
            EV.inputs(self.request, self.suite, self.custody, self.work)
        cases = [("garbage", "invalid_patch"), (good.replace("src/program.py", "../escape.py"), "rejected_path"),
                 (good.replace("src/program.py", "src/other.py"), "rejected_path"),
                 (good.replace("            break", "            missing"), "apply_failure"),
                 (good.replace("@@ -", "@@ --"), "invalid_patch")]
        for patch, outcome in cases:
            with self.subTest(outcome=outcome):
                code, response, stderr, out = self.capture(patch)
                self.assertEqual(code, 0, (response, stderr))
                self.assertEqual(self.assess(out)["outcome"], outcome)
        code, response, stderr, out = self.capture("invalid UTF-8", raw_final=b"\xff\xfe")
        self.assertEqual(code, 0, (response, stderr))
        self.assertEqual((out / "final-message.txt").read_bytes(), b"\xff\xfe")
        self.assertEqual(self.assess(out)["outcome"], "invalid_patch")
        self.suite.write_text("{}")
        code, response, stderr, out = self.capture(good)
        self.assertNotEqual(code, 0)
        refusal = json.loads((out / "refusal.json").read_text())
        self.assertEqual(refusal["request_digest"], support.eval_runner.digest(self.request))
        self.assertFalse((out / "provider.jsonl").exists())

    def test_external_timeout_and_stdout_bound_are_unsuccessful(self):
        for replacement, expected in (("            __import__('time').sleep(5)", "timeout"),
                                      ("            print('x' * 10000)", "failed_tests"),
                                      ("            __import__('sys').stderr.write('x' * 10000)", "failed_tests")):
            code, response, stderr, out = self.capture(diff(SOURCE.replace("            break", replacement)))
            self.assertEqual(code, 0, (response, stderr))
            result = self.assess(out)
            self.assertEqual(result["outcome"], expected, result)
            self.assertFalse(result["success"])

    @unittest.skipUnless(os.environ.get("AGENT_EVAL_ORBIT") and os.environ.get("AGENT_EVAL_ORBIT_GRAPH"),
                         "root must set pinned AGENT_EVAL_ORBIT and AGENT_EVAL_ORBIT_GRAPH for installed schema-4 graph rehearsal")
    def test_installed_graph_profile_uses_same_patch_and_replays_original_capture(self):
        install = ["--plugin-repo", support.REPO, "--orbit", os.environ["AGENT_EVAL_ORBIT"],
                   "--orbit-graph", os.environ["AGENT_EVAL_ORBIT_GRAPH"]]
        commit = support.eval_runner.git_run("/usr/bin/git", support.REPO, self.work, "rev-parse", "HEAD")
        code, inspected, stderr = self.cli("plugin-inspect", *install, "--plugin-commit", commit,
            "--out", self.work / "inspect", "--containment", "none")
        self.assertEqual(code, 0, (inspected, stderr))
        self.strict_graph = os.environ.get("PATCH_EVAL_REQUIRE_STRICT") == "1"
        if self.strict_graph:
            self.assertIn("PATCH_EVAL_CLOSURE", os.environ, "strict original capture needs the pinned real offline closure")
            self.closure = Path(os.environ["PATCH_EVAL_CLOSURE"])
            closure_sha = support.eval_runner.digest(EV.tree_lock(self.closure))
            custody = json.loads(self.custody.read_text())
            custody["closure_sha256"] = closure_sha
            self.custody.write_text(json.dumps(custody))
            self.request["patch"].update(closure_sha256=closure_sha,
                custody_sha256=support.eval_runner.sha256_file(self.custody))
            self.strict_args = ["--containment", "bwrap", "--auth-file", "none", "--truth-path", self.truth]
        self.request["setup_limits"]["output_bytes"] = 8388608
        self.request.update(arm="graph", order=1, plugin=inspected["pin"],
            tools=list(support.eval_broker.ARM_TOOLS["baseline"] + support.eval_runner.plugin.TOOLS))
        self.request.pop("request_sha256")
        self.request["request_sha256"] = support.eval_runner.digest(self.request)
        request = self.request_file(raw=json.dumps(self.request))
        patch = diff(SOURCE.replace("            break", "            continue"))
        steps = [{"call": "read", "arguments": {"path": "src/program.py"}},
                 {"call": "graph_maintain", "arguments": {"operation": "graph_sync"}},
                 {"final": patch}]
        code, response, stderr, out = self.episode(steps, arm="graph", graph=None, request=request,
            args=[*install, "--source-repo", self.source_repo, "--patch-suite", self.suite,
                  "--patch-custody", self.custody, "--patch-closure", self.closure, *getattr(self, "strict_args", ())])
        self.assertEqual(code, 0, (response, stderr))
        artifact = EV.loader().load_episode(out, diagnostic=not self.strict_graph)
        self.assertEqual(artifact["status"], "ok", artifact["error"])
        self.assertIsNotNone(artifact["treatment"])
        self.assertEqual((out / "final-message.txt").read_bytes(), patch.encode())
        graph_report = self.assess(out)
        graph_assessment = self.work / f"evaluation-{self.counter}"
        self.assertEqual(graph_report["outcome"], "success")
        graph_request = copy.deepcopy(self.request)
        self.request.update(arm="baseline", order=0, tools=list(support.eval_broker.ARM_TOOLS["baseline"]))
        self.request.pop("request_sha256")
        self.request["request_sha256"] = support.eval_runner.digest(self.request)
        code, response, stderr, baseline_out = self.capture(patch)
        self.assertEqual(code, 0, (response, stderr))
        baseline = EV.loader().load_episode(baseline_out, diagnostic=not self.strict_graph)
        self.assertIsNone(baseline["treatment"])
        baseline_report = self.assess(baseline_out)
        self.assertEqual(baseline_report["outcome"], "success")
        plan = {"schema_version": 4, "profile": patch_profile.PROFILE, "requests": [self.request, graph_request],
                "model": artifact["model"], "provider_binary_sha256": artifact["provider"]["binary_sha256"],
                "harness": artifact["harness"]}
        replayed = EV.loader().replay(plan, [out, baseline_out], diagnostic=not self.strict_graph)
        self.assertEqual(replayed["episodes"], 2)
        scored = EV.score(plan, [self.work / f"evaluation-{self.counter}", graph_assessment])
        self.assertEqual(scored["successes"], 2)
        if self.strict_graph:
            self.assertEqual(scored["all_attempt_success_rate"], 1)
        else:
            self.assertIsNone(scored["all_attempt_success_rate"], "diagnostic data cannot qualify an effectiveness rate")

    def test_freeze_cli_binds_exact_pair_and_prevents_source_omission(self):
        baseline = copy.deepcopy(self.request)
        baseline.update(schema_version=3, profile=support.eval_runner.plugin.PROSPECTIVE_PROFILE)
        baseline.pop("patch")
        baseline.pop("request_sha256")
        baseline["request_sha256"] = support.eval_runner.digest(baseline)
        graph = copy.deepcopy(baseline)
        graph.update(arm="graph", order=1, tools=list(support.eval_broker.ARM_TOOLS["baseline"] + support.eval_runner.plugin.TOOLS))
        graph.pop("request_sha256")
        graph["request_sha256"] = support.eval_runner.digest(graph)
        seed = self.work / "seed.json"
        seed.write_text(json.dumps({"schema_version": 3, "profile": baseline["profile"], "requests": [baseline, graph],
            "model": {"provider": "codex-cli", "name": "fake-model", "version": "codex-cli 0.0.0-fake", "settings": {}},
            "provider_binary_sha256": "0" * 64, "harness": {}}))
        result = subprocess.run([sys.executable, "-B", str(HERE / "eval.py"), "freeze", "--plan", str(seed),
            "--suite", str(self.suite), "--closure", str(self.closure), "--rg", support.RG, "--author", "author", "--reviewer", "reviewer",
            "--receipt", "synthetic", "--allowed-path", "src/program.py", "--out", str(self.work / "frozen")],
            capture_output=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stderr)
        frozen = json.loads((self.work / "frozen/plan.json").read_text())
        for request in frozen["requests"]:
            support.eval_runner.validate_request(request)
            self.assertEqual(request["patch"]["evaluator_sha256"], EV.evaluator_identity())
        original_harness = dict(self.request["patch"]["runtime"]["harness"])
        self.request["patch"]["runtime"]["harness"]["eval_runner.py"] = "0" * 64
        self.request.pop("request_sha256")
        self.request["request_sha256"] = support.eval_runner.digest(self.request)
        code, response, stderr, out = self.capture("")
        self.assertNotEqual(code, 0, (response, stderr))
        self.assertIn("harness drift before output", response["refusal"]["message"])
        self.assertFalse((out / "provider.jsonl").exists())
        self.request["patch"]["runtime"]["harness"] = original_harness
        (self.head / "tests/visible.py").unlink()
        self.request["source_revision"] = support.revision(self.head)
        custody = json.loads(self.custody.read_text())
        custody["source_revision"] = self.request["source_revision"]
        self.custody.write_text(json.dumps(custody))
        self.request["patch"]["custody_sha256"] = support.eval_runner.sha256_file(self.custody)
        self.request.pop("request_sha256")
        self.request["request_sha256"] = support.eval_runner.digest(self.request)
        # The immutable source commit still contains the omitted visible test.
        code, response, stderr, out = self.capture("")
        self.assertNotEqual(code, 0, (response, stderr))
        self.assertIn("full permitted source universe", response["refusal"]["message"])
        self.assertFalse((out / "provider.jsonl").exists())

    def test_schedule_keeps_refusal_slot_and_detects_missing_or_drift(self):
        good = diff(SOURCE.replace("            break", "            continue"))
        code, response, stderr, out = self.capture(good)
        self.assertEqual(code, 0, (response, stderr))
        report = self.assess(out)
        artifact = EV.loader().load_episode(out, diagnostic=True)
        graph = copy.deepcopy(self.request)
        graph.update(arm="graph", order=1, tools=list(support.eval_broker.ARM_TOOLS["baseline"] + support.eval_runner.plugin.TOOLS))
        graph.pop("request_sha256")
        graph["request_sha256"] = support.eval_runner.digest(graph)
        refusal = self.work / "refused"
        refusal.mkdir()
        (refusal / "refusal.json").write_text(json.dumps({"schema_version": 4, "profile": patch_profile.PROFILE,
            "status": "refused", "provider_started": False, "stage": "admission",
            "request_digest": support.eval_runner.digest(graph), "request_identity": support.eval_runner.digest(graph),
            "refusal": {"code": "closure_missing", "message": "synthetic", "evidence": None}}))
        plan = {"schema_version": 4, "profile": patch_profile.PROFILE, "requests": [self.request, graph],
                "model": artifact["model"], "provider_binary_sha256": artifact["provider"]["binary_sha256"],
                "harness": artifact["harness"]}
        scored = EV.score(plan, [self.work / f"evaluation-{self.counter}", refusal])
        self.assertEqual(scored["scheduled"], 2)
        self.assertEqual(scored["outcomes"], {"success": 1, "setup_refusal": 1})
        self.assertIsNone(scored["all_attempt_success_rate"])
        with self.assertRaisesRegex(ValueError, "every scheduled"):
            EV.score(plan, [refusal])


class ParserSafety(unittest.TestCase):
    def test_rejects_extensions_paths_counts_size_and_symlinks_before_write(self):
        work = support.scratch()
        self.addCleanup(support.remove, work)
        source = work / "src"
        source.mkdir()
        path = source / "program.py"
        good = diff(SOURCE.replace("            break", "            continue")).encode()
        for data, bounds in [(b"GIT binary patch\n", pin()), (b"diff --git a/a b/a\n" + good, pin()),
                            (good.replace(b"src/program.py", b"/absolute.py"), pin()),
                            (good.replace(b"src/program.py", b"src//program.py"), pin()),
                            (good.replace(b"src/program.py", b"src/./program.py"), pin()),
                            (good.replace(b"src/program.py", b"src/a\tprogram.py"), pin()),
                            (good + b"new file mode 120000\n", pin()), (good + good, pin()),
                            (good, dict(pin(), max_patch_bytes=1)), (good, dict(pin(), max_changed_lines=1)),
                            (good.replace(b"@@ -", b"@@ -999"), pin()), (b"\x00", pin())]:
            path.write_text(SOURCE)
            with self.subTest(data=data[:40]):
                with self.assertRaises(EV.patcher.Rejected):
                    EV.patcher.apply(work, data, bounds)
                self.assertEqual(path.read_text(), SOURCE)
        path.unlink()
        outside = work / "outside"
        outside.write_text(SOURCE)
        path.symlink_to(outside)
        with self.assertRaises(EV.patcher.Rejected):
            EV.patcher.apply(work, good, pin())
        self.assertEqual(outside.read_text(), SOURCE)
        path.unlink()
        source.rmdir()
        source.symlink_to(work)
        with self.assertRaises(EV.patcher.Rejected):
            EV.patcher.apply(work, good, pin())


class StrictSandbox(unittest.TestCase):
    def test_offline_closure_process_output_and_path_controls(self):
        reason = bwrap_unavailable()
        if reason:
            if os.environ.get("PATCH_EVAL_REQUIRE_STRICT") == "1":
                self.fail(reason)
            self.skipTest(reason + "; root must run PATCH_EVAL_REQUIRE_STRICT=1")
        closure = os.environ.get("PATCH_EVAL_CLOSURE")
        if not closure:
            if os.environ.get("PATCH_EVAL_REQUIRE_STRICT") == "1":
                self.fail("PATCH_EVAL_CLOSURE required")
            self.skipTest("PATCH_EVAL_CLOSURE missing; root qualification required")
        work = support.scratch()
        self.addCleanup(support.remove, work)
        EV.tree_lock(closure)
        EV.canary("/usr/bin/bwrap", closure, work)
        self.assertTrue(all(support.eval_runner.plugin.resource_bounds().values()), "finite memory/pids ceilings required")
        source = support.write_tree(work / "source", {"src/program.py": SOURCE.replace("            break", "            continue")})
        suite = EV.suite_load(HERE / "fixtures/suite.json")
        for replacement, expected in ((None, "passed"), ("import time; time.sleep(5)", "timeout"),
                ("import os; os.write(1, b'x' * 20000)", "failed_tests"),
                ("import os,time; child=os.fork()\nif child == 0:\n os.setsid(); time.sleep(5)\nelse:\n os._exit(0)", "failed_tests"),
                ("import os; os.setsid(); os._exit(0)", "failed_tests")):
            if replacement:
                (source / "src/program.py").write_text(replacement)
            rows = EV.test_vectors(source, suite, pin(), "/usr/bin/bwrap", closure)
            self.assertTrue(all(row["outcome"] == expected for row in rows), rows)
            self.assertFalse(any((row.get("transport") or {}).get("supervision", {}).get("survivors") for row in rows))


if __name__ == "__main__":
    unittest.main()
