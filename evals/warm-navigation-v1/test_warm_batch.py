"""Source-only test qualification, real private installed plugin, fake provider only."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import shutil
import sys
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts/agent-eval/tests"))
import support
from support import eval_runner as runner, eval_broker as broker
import plugin_profile as plugin
import warm_batch as warm

SPEC = importlib.util.spec_from_file_location("warm_evaluator", Path(__file__).with_name("eval.py"))
ev = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ev)
FIXTURES = Path(__file__).with_name("fixtures")
LOCK = Path(__file__).with_name("fixtures.lock.json")
ANSWER = support.ANSWER


class WarmBoundaryTests(unittest.TestCase):
    def setUp(self):
        self.work = support.scratch()
        self.addCleanup(support.remove, self.work)
        self.orbit = os.environ.get("AGENT_EVAL_ORBIT")
        self.graph = os.environ.get("AGENT_EVAL_ORBIT_GRAPH")
        self.assertTrue(self.orbit and self.graph and support.RG,
                        "qualification requires AGENT_EVAL_ORBIT, AGENT_EVAL_ORBIT_GRAPH and rg; no vacuous skip")
        for name, expected in json.loads(LOCK.read_text())["files"].items():
            self.assertEqual(runner.sha256_file(LOCK.parent / name), expected)
        self.source = self.work / "source"
        self.source.mkdir()
        self.home = self.work / "git-home"
        self.home.mkdir()
        runner.git_run("/usr/bin/git", self.source, self.home, "init", "--quiet", "-b", "main")
        self.commits = {}
        for view in ("initial", "edited"):
            support.write_tree(self.source, {"src/lib.rs": (FIXTURES / view / "src/lib.rs").read_text()})
            runner.git_run("/usr/bin/git", self.source, self.home, "add", "src/lib.rs")
            runner.git_run("/usr/bin/git", self.source, self.home, "commit", "--quiet", "-m", view)
            self.commits[view] = runner.git_run("/usr/bin/git", self.source, self.home, "rev-parse", "HEAD")
        self.commit = runner.git_run("/usr/bin/git", ROOT, self.home, "rev-parse", "HEAD")
        inspected = self.cli("plugin-inspect", "--plugin-repo", ROOT, "--plugin-commit", self.commit,
                             "--orbit", self.orbit, "--orbit-graph", self.graph,
                             "--out", self.work / "inspect", "--containment", "none")
        self.assertEqual(inspected["exit_code"], 0, inspected)
        self.pin = json.loads(inspected["stdout"])["pin"]
        self.count = 0

    def cli(self, *args, script=None, timeout=90):
        env = {"PATH": "/usr/bin:/bin", "HOME": str(self.work)}
        if script is not None:
            env["FAKE_CODEX_SCRIPT_JSON"] = json.dumps(script)
        return broker.run_child([sys.executable, "-B", str(runner.HERE / "eval_runner.py"), *map(str, args)],
                                str(self.work), env, timeout, capture_limit=2 * 1024 * 1024)

    def freeze(self, edits=None, containment="none"):
        schedule = []
        for index, view in enumerate(("initial", "initial", "edited")):
            requests = {}
            for arm in runner.ARMS:
                request = {"schema_version": 5, "case_id": "find-price", "arm": arm, "split": "test-only",
                           "source_revision": support.revision(FIXTURES / view),
                           "base_revision": support.revision(FIXTURES / "initial"),
                           "prompt": f"Find price. Test-only slot {index}.", "limits": dict(support.LIMITS, call_bytes=131072, output_bytes=1048576),
                           "tools": list(broker.ARM_TOOLS["baseline"] + (plugin.TOOLS if arm == "graph" else ())),
                           "order": index, "profile": warm.PROFILE, "cache_policy": warm.CACHE_POLICY,
                           "plugin": copy.deepcopy(self.pin), "source_commits": {"head": self.commits[view], "base": self.commits["initial"]},
                           "setup_limits": {"wall_ms": 60000, "output_bytes": 8388608}}
                requests[arm] = request
            schedule.append({"view": view, "requests": requests})
        spec = {"schema_version": 1, "profile": warm.PROFILE, "state_policy": warm.POLICY,
                "model": {"provider": "codex-cli", "name": "fake-test-only", "version": "codex-cli 0.0.0-fake", "settings": {}},
                "provider_binary_sha256": runner.sha256_file(support.FAKE_CODEX),
                "product_identity": json.loads(broker.run_child([self.graph, "version", "--json"], str(self.work),
                    {"PATH": "/usr/bin:/bin"}, 30)["stdout"]),
                "runtime": {"baseline_binaries": {n: runner.sha256_file(p) for n, p in
                    (("git", "/usr/bin/git"), ("python", "/usr/bin/python3"), ("rg", support.RG))},
                    "resource_limits": plugin.resource_bounds(), "containment": containment, "bwrap_sha256": runner.sha256_file(shutil.which("bwrap")) if containment == "bwrap" else None},
                "schedule": schedule, "batch_order": ["baseline", "graph", "graph", "baseline"]}
        if edits:
            edits(spec)
        # Base views keyed by the same schedule name, always the initial bytes.
        bases = self.work / "bases"
        for view in ("initial", "edited"):
            support.write_tree(bases / view, {"src/lib.rs": (FIXTURES / "initial/src/lib.rs").read_text()})
        self.count += 1
        path = self.work / f"freeze-{self.count}.json"
        lock = self.work / f"lock-{self.count}.json"
        lock.write_text(json.dumps({"schema_version": 1, "purpose": "test-only",
            "source_fixture_lock_sha256": runner.sha256_file(LOCK),
            "expected_items": [ANSWER["items"]] * len(spec["schedule"])}))
        return ev.export(spec, lock, path), path

    def run_batch(self, index=1, scripts=None, edits=None, containment="none", frozen=None, suffix="", expected_exit=0):
        freeze, path = frozen or self.freeze(edits, containment)
        out = self.work / f"batch-{self.count}-{index}{suffix}"
        calls = [{"call": "graph_maintain", "arguments": {"operation": "graph_sync"}},
                 {"call": "graph_search", "arguments": {"query": "price"}},
                 {"final": json.dumps(ANSWER)}]
        script = {"slot_scripts": {slot["requests"]["graph"]["prompt"]:
                   {"steps": calls if index in (1, 2) else [{"call": "read", "arguments": {"path": "src/lib.rs"}}, {"final": json.dumps(ANSWER)}]}
                   for slot in freeze["schedule"]}}
        if scripts:
            for slot, change in scripts.items():
                script["slot_scripts"][freeze["schedule"][slot]["requests"]["graph"]["prompt"]].update(change)
        response = self.cli("run-batch", "--request", path, "--batch-index", index, "--lock", self.work / f"lock-{self.count}.json",
                    "--head", FIXTURES, "--base", self.work / "bases", "--out", out,
                    "--codex", support.FAKE_CODEX, "--model", "fake-test-only", "--rg", support.RG,
                    "--orbit", self.orbit, "--orbit-graph", self.graph, "--plugin-repo", ROOT,
                    "--source-repo", self.source, "--containment", containment, "--auth-file", "none", "--truth-path", LOCK,
                    "--provider-env", "FAKE_CODEX_SCRIPT_JSON", script=script)
        self.assertEqual(response["exit_code"], expected_exit, response)
        if expected_exit:
            self.assertFalse(out.exists(), "repeat admission must fail before creating a replacement batch")
            return response
        return out, freeze, runner.load_json(out / "batch.json", 64 * 1024 * 1024)

    def artifact(self, out, index):
        return runner.load_json(out / f"slot-{index:03d}/episode.json", 64 * 1024 * 1024)

    def test_real_reuse_edit_invalidation_parity_and_all_attempt_costs(self):
        unchanged = [{"call": "graph_search", "arguments": {"query": "price"}}, {"final": json.dumps(ANSWER)}]
        edit = [{"call": "graph_search", "arguments": {"query": "price"}},
                {"call": "graph_maintain", "arguments": {"operation": "graph_sync"}},
                {"call": "graph_search", "arguments": {"query": "discount"}}, {"final": json.dumps(ANSWER)}]
        out, freeze, batch = self.run_batch(scripts={1: {"steps": unchanged, "usage": None}, 2: {"steps": edit}})
        self.assertEqual([s["status"] for s in batch["slots"]], ["ok"] * 3, batch["slots"])
        first, second, third = [self.artifact(out, i) for i in range(3)]
        self.assertTrue(first["isolation"]["cold_start"])
        self.assertFalse(second["isolation"]["cold_start"])
        self.assertTrue(second["warm"]["installation_reused"])
        self.assertEqual(first["warm"]["after"], second["warm"]["before"])
        self.assertNotEqual(first["provider"]["argv"], second["provider"]["argv"])
        self.assertEqual([a["treatment"]["setup"] == [] for a in (first, second, third)], [False, True, True])
        stale = json.loads(third["calls"][0]["output"])["product_reply"]["structuredContent"]
        self.assertFalse(stale["index"]["fresh"])
        rebuilt = json.loads(third["calls"][2]["output"])["product_reply"]["structuredContent"]
        self.assertTrue(rebuilt["index"]["fresh"])
        self.assertIn("discount", json.dumps(rebuilt))
        # Independent empty-state batch starts at the edited source, then compare
        # semantic search results (index IDs/timing may legitimately differ).
        def edited_only(spec):
            spec["schedule"] = [spec["schedule"][2]]
            for req in spec["schedule"][0]["requests"].values():
                req["order"] = 0
        fresh_calls = [{"call": "graph_maintain", "arguments": {"operation": "graph_sync"}},
                       {"call": "graph_search", "arguments": {"query": "discount"}}, {"final": json.dumps(ANSWER)}]
        fresh, _, fresh_batch = self.run_batch(edits=edited_only, scripts={0: {"steps": fresh_calls}})
        self.assertEqual(fresh_batch["slots"][0]["status"], "ok")
        result = json.loads(self.artifact(fresh, 0)["calls"][1]["output"])["product_reply"]["structuredContent"]
        self.assertEqual(rebuilt["result"], result["result"])
        replay = ev.replay(out, diagnostic=True)
        self.assertEqual(replay["scheduled"], 3)
        self.assertEqual(replay["amortized_wall_ms"], replay["batch_wall_ms"] / 3)
        self.assertIsNone(replay["usage"][1]["fields"]["input_tokens"]["total"])
        self.assertIsNone(replay["usage"][0]["fields"]["total_tokens"]["total"])
        self.assertIsNone(replay["cost_usd"])
        self.assertIsNone(replay["batch_tool_output_bytes"])
        self.assertEqual(replay["batch_tool_output_coverage"]["state"], "partial")
        scored = ev.score(out, {"freeze_sha256": freeze["freeze_sha256"], "expected_items": [ANSWER["items"]] * 3}, True)
        self.assertEqual(scored["all_attempt_correct"], 3)
        with self.assertRaises(ValueError):
            ev.replay(out)  # diagnostic never qualifies strict containment

    def test_failures_refusals_interruption_keep_scheduled_denominator(self):
        for fault in ("failure", "source", "interrupt"):
            with self.subTest(fault=fault):
                scripts, edits = {}, None
                if fault == "failure":
                    scripts = {1: {"exit_code": 9}}
                elif fault == "source":
                    def edits(spec):
                        for req in spec["schedule"][1]["requests"].values():
                            req["source_commits"]["head"] = "0" * 40
                else:
                    scripts = {1: {"steps": [{"signal_parent": True}, {"sleep": 2}]}}
                out, freeze, batch = self.run_batch(scripts=scripts, edits=edits)
                self.assertEqual(batch["slots"][0]["status"], "ok", batch["slots"])
                self.assertNotEqual(batch["slots"][1]["status"], "ok")
                result = ev.score(out, {"freeze_sha256": freeze["freeze_sha256"], "expected_items": [ANSWER["items"]] * 3}, True)
                self.assertEqual(result["scheduled"], 3)
                self.assertLess(result["all_attempt_accuracy"], 1)
                if fault != "failure":
                    self.assertEqual(batch["slots"][2]["status"], "not_run")

    def test_state_and_provenance_tampering_refuses(self):
        out, _, batch = self.run_batch(scripts={0: {"steps": [
            {"write_state": {"path": ".orbit/plugins/graph/0.10.0/skills/orbit-graph/SKILL.md", "text": "altered"}},
            {"call": "read", "arguments": {"path": "src/lib.rs"}}, {"final": json.dumps(ANSWER)}]}})
        self.assertEqual(batch["slots"][0]["status"], "refused", batch["slots"])
        self.assertEqual(batch["slots"][1]["status"], "not_run")
        out, _, batch = self.run_batch()
        # Frozen rubric cannot be replaced by a post-output answer key.
        with self.assertRaises(ValueError):
            ev.score(out, {"freeze_sha256": batch["freeze_sha256"], "expected_items": [["wrong"]] * 3}, True)
        raw = self.artifact(out, 1)
        raw["reply_provenance"][0]["delivered_bytes"] += 1
        runner.seal(raw, "artifact_sha256")
        (out / "slot-001/episode.json").write_text(json.dumps(raw))
        with self.assertRaises(ValueError):
            ev.replay(out, True)
        # Undeclared roots and symlinks refuse even without a provider.
        product = out / "product"
        (product / "undeclared").mkdir()
        custody = object.__new__(warm.Custody)
        custody.product, custody.home, custody.repo = product, product / "orbit-home", product / "repo"
        with self.assertRaises(ValueError):
            custody.state()
        root = self.work / "linked"
        root.symlink_to(product, target_is_directory=True)
        with self.assertRaises(ValueError):
            warm.manifest(root)

    def test_counterbalanced_cohort_rejects_replacement_and_resealed_drift(self):
        frozen = self.freeze()
        outputs = [self.run_batch(index=i, frozen=frozen)[0] for i in range(4)]
        self.assertEqual(len(ev.cohort(outputs, True)["batches"]), 4)
        self.run_batch(index=0, frozen=frozen, suffix="-replacement", expected_exit=1)
        result = broker.run_child([sys.executable, "-B", str(SPEC.origin), "replay", "--batch", str(outputs[0]), "--diagnostic"],
            str(self.work), {"PATH": "/usr/bin:/bin"}, 30)
        self.assertEqual(result["exit_code"], 0, result)
        self.assertEqual(json.loads(result["stdout"])["scheduled"], 3)
        path = outputs[0] / "batch.json"
        original = runner.load_json(path, 64 * 1024 * 1024)
        for field, value in (("schema_version", True), ("end_ns", original["start_ns"] - 1),
                             ("slots", original["slots"][:-1])):
            with self.subTest(field=field):
                changed = dict(original, **{field: value})
                runner.seal(changed, "batch_sha256")
                path.write_text(json.dumps(changed))
                with self.assertRaises(ValueError):
                    ev.replay(outputs[0], True)
        path.write_text(json.dumps(original))
        with self.assertRaises(ValueError):
            ev.cohort(list(reversed(outputs)), True)
        freeze = copy.deepcopy(frozen[0])
        for view in (".", ".."):
            freeze["schedule"][0]["view"] = view
            runner.seal(freeze, "freeze_sha256")
            with self.assertRaises(ValueError):
                warm.validate_freeze(freeze)

    def test_strict_private_plugin_state_reuse(self):
        from test_runner import bwrap_unavailable
        reason = bwrap_unavailable()
        bounds = plugin.resource_bounds()
        if reason or not all(bounds.values()):
            message = f"strict warm qualification unavailable: {reason}; bounds={bounds}"
            if os.environ.get("WARM_REQUIRE_STRICT") == "1":
                self.fail(message)
            self.skipTest(message)
        out, _, batch = self.run_batch(containment="bwrap")
        self.assertEqual([s["status"] for s in batch["slots"]], ["ok"] * 3, batch["slots"])
        self.assertEqual(ev.replay(out)["scheduled"], 3)

    def test_baseline_is_fresh_and_wrong_binary_refuses_before_provider(self):
        out, _, batch = self.run_batch(index=0)
        self.assertEqual([s["status"] for s in batch["slots"]], ["ok"] * 3, batch["slots"])
        ev.replay(out, True)
        self.assertEqual(batch["final_state"]["orbit-home"], {})
        def bad_pin(spec):
            for slot in spec["schedule"]:
                for req in slot["requests"].values():
                    req["plugin"]["backend_sha256"] = "0" * 64
        out, _, batch = self.run_batch(edits=bad_pin)
        self.assertEqual([s["status"] for s in batch["slots"]], ["refused", "not_run", "not_run"])
        self.assertFalse((out / "slot-000/provider.jsonl").exists())


if __name__ == "__main__":
    unittest.main()
