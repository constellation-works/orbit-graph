"""Behavior tests for the operator entry point and its fail-closed contract."""
import copy
from contextlib import redirect_stderr, redirect_stdout
import io
import json
import os
from pathlib import Path
import signal
import shutil
import subprocess
import sys
import tempfile
import unittest

import eval as study


class EvaluationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.data, cls.trees = study.corpus()
        cls.raw = study.smoke(cls.data, cls.trees)
        cls.scratch = study.ROOT.parents[1] / ".orbit/tmp/agent-navigation-tests"
        cls.scratch.mkdir(parents=True, exist_ok=True)

    def bundle(self):
        return copy.deepcopy(self.raw)

    def evaluate(self, raw):
        return study.score(self.data, self.trees, raw)

    def entry(self, raw_text):
        with tempfile.TemporaryDirectory(dir=self.scratch) as directory:
            path = Path(directory) / "episodes.json"
            path.write_text(raw_text)
            out, err = io.StringIO(), io.StringIO()
            with redirect_stdout(out), redirect_stderr(err):
                code = study.main(["score", "--input", str(path)])
            return code, out.getvalue(), err.getvalue()

    def cli_entry(self, raw_text):
        with tempfile.TemporaryDirectory(dir=self.scratch) as directory:
            path = Path(directory) / "episodes.json"
            path.write_text(raw_text)
            with subprocess.Popen(
                [sys.executable, "-B", str(study.ROOT / "eval.py"), "score", "--input", str(path)],
                cwd=directory, env={"PATH": os.defpath, "HOME": directory},
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, start_new_session=True,
            ) as process:
                try:
                    out, err = process.communicate(timeout=10)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGTERM)
                    try:
                        process.communicate(timeout=1)
                    except subprocess.TimeoutExpired:
                        os.killpg(process.pid, signal.SIGKILL)
                        process.communicate()
                    raise
                self.assertEqual(path.read_text(), raw_text, "scoring must preserve captured input bytes")
                return process.returncode, out, err

    def capture_answer(self, episode, answer):
        episode["answer"] = answer
        episode["final_output"] = study.canonical(answer).decode()
        episode["output_bytes"] = sum(len(c["output"].encode()) for c in episode["calls"]) + len(episode["final_output"].encode())
        study.seal(episode)

    def test_cli_retains_duplicate_citation_failure_and_complete_cohort(self):
        raw = self.bundle()
        episode = raw["episodes"][1]
        episode["answer"]["evidence"] *= 2
        self.capture_answer(episode, episode["answer"])
        code, out, err = self.cli_entry(json.dumps(raw))
        self.assertEqual((code, err), (0, ""))
        report = json.loads(out)
        row = report["cases"][1]
        self.assertEqual((row["status"], row["error"]), ("ok", None))
        self.assertEqual(row["answer_error"], "invalid citation item")
        self.assertFalse(row["correct"])
        self.assertFalse(row["correct_abstention"])
        self.assertEqual(row["missed"], self.data["cases"][0]["truth"]["items"])
        self.assertEqual(len(report["cases"]), 24)
        self.assertEqual(len(report["pairs"]), 12)
        self.assertTrue(report["cases"][-1]["correct"])
        self.assertEqual(report["pairs"][0]["correct_delta_graph_minus_baseline"], 0)
        aggregate = report["aggregate"]["development"]["graph"]
        self.assertEqual((aggregate["episodes"], aggregate["correct"], aggregate["failures"]), (4, 3, 1))
        self.assertEqual(sum(arm["episodes"] for split in report["aggregate"].values() for arm in split.values()), 24)
        self.assertEqual(row["wall_ms"], episode["wall_ms"])
        self.assertEqual(row["tool_calls"], len(episode["calls"]))
        self.assertEqual(row["output_bytes"], episode["output_bytes"])

    def test_entry_point_accepts_complete_bundle(self):
        code, out, err = self.entry(json.dumps(self.bundle()))
        self.assertEqual((code, err), (0, ""))
        report = json.loads(out)
        self.assertEqual(len(report["cases"]), 24)
        self.assertFalse(report["agent_effectiveness_evidence"])
        self.assertEqual(report["study_kind"], "scripted-smoke")

    def test_entry_point_refuses_missing_and_extra_pairs(self):
        for count in (0, 1, 23, 25):
            with self.subTest(count=count):
                raw = self.bundle()
                raw["episodes"] = (raw["episodes"] * 2)[:count]
                code, out, err = self.entry(json.dumps(raw))
                self.assertEqual((code, out), (1, ""))
                self.assertEqual(json.loads(err)["error"]["code"], "invalid_evaluation")

    def test_bad_json_duplicate_fields_and_nonfinite_values_fail(self):
        for text in ('{}', '{"episodes":[],"episodes":[]}', '{"x":NaN}', '[', 'x' * (study.MAX_FILE_BYTES + 1)):
            with self.subTest(text=text[:40]):
                code, out, err = self.entry(text)
                self.assertEqual((code, out), (1, ""))
                self.assertTrue(json.loads(err)["error"]["message"])

    def test_absent_and_invalid_evidence_never_earn_correct_grade(self):
        for change in ("missing", "wrong_quote", "wrong_line", "unknown_symbol", "duplicate", "wrong_file",
                       "duplicate_citation", "unsupported_citation", "missing_selector", "missing_item_evidence"):
            with self.subTest(change=change):
                raw = self.bundle()
                episode = raw["episodes"][1]
                answer = episode["answer"]
                if change == "missing":
                    answer["evidence"] = []
                elif change == "wrong_quote":
                    answer["evidence"][0]["quote"] = "invented source"
                elif change == "wrong_line":
                    answer["evidence"][0]["line"] += 1
                elif change == "wrong_file":
                    answer["evidence"][0]["file"] = "../secret"
                elif change == "unknown_symbol":
                    answer["items"][0] = "symbol:src/lib.rs#invented:function"
                elif change == "duplicate":
                    answer["items"] *= 2
                elif change == "duplicate_citation":
                    answer["evidence"] *= 2
                elif change == "unsupported_citation":
                    answer["items"][0] = answer["evidence"][0]["item"] = "symbol:src/lib.rs#invented:function"
                elif change == "missing_selector":
                    answer["evidence"][0].pop("item")
                elif change == "missing_item_evidence":
                    answer["items"].append("symbol:src/lib.rs#invoice:function")
                self.capture_answer(episode, answer)
                code, out, err = self.cli_entry(json.dumps(raw))
                self.assertEqual((code, err), (0, ""))
                report = json.loads(out)
                self.assertEqual(len(report["cases"]), 24)
                self.assertFalse(report["cases"][1]["correct"])
                self.assertTrue(report["cases"][1]["answer_error"])
                self.assertEqual(report["aggregate"]["development"]["graph"]["failures"], 1)

    def test_structurally_captured_invalid_answers_are_episode_failures(self):
        for answer in (None, [], {}, {"items": [], "abstain": "yes", "reason": "", "evidence": []},
                       {"items": [], "abstain": False, "reason": "", "evidence": []},
                       {"items": [], "abstain": True, "reason": "", "evidence": []}):
            with self.subTest(answer=answer):
                raw = self.bundle()
                self.capture_answer(raw["episodes"][1], answer)
                code, out, err = self.cli_entry(json.dumps(raw))
                self.assertEqual((code, err), (0, ""))
                row = json.loads(out)["cases"][1]
                self.assertTrue(row["answer_error"])
                self.assertFalse(row["correct"])
                self.assertFalse(row["correct_abstention"])

    def test_invalid_abstention_never_earns_correct_abstention(self):
        raw = self.bundle()
        episode = raw["episodes"][10]
        episode["answer"]["reason"] = " "
        self.capture_answer(episode, episode["answer"])
        code, out, err = self.cli_entry(json.dumps(raw))
        self.assertEqual((code, err), (0, ""))
        report = json.loads(out)
        self.assertTrue(report["cases"][10]["answer_error"])
        self.assertFalse(report["cases"][10]["correct"])
        self.assertFalse(report["cases"][10]["correct_abstention"])
        self.assertEqual(report["aggregate"]["held-out"]["graph"]["episodes"], 8)
        self.assertEqual(report["aggregate"]["held-out"]["graph"]["correct_abstentions"], 1)

    def test_invalid_answer_does_not_hide_later_collection_rejection(self):
        modifications = [
            ("hash", lambda e: e.update(record_sha256="0" * 64)),
            ("request", lambda e: e["request"].update(prompt="different prompt")),
            ("model", lambda e: e["model"].update(name="another model")),
            ("truth", lambda e: e["isolation"].update(truth_inaccessible=False)),
            ("warm", lambda e: e["isolation"].update(cold_start=False)),
            ("shared", lambda e: e["isolation"].update(cache_id="scripted-cache-0")),
            ("version", lambda e: e["tool_versions"].update(rg="another-version")),
            ("run_id", lambda e: e.update(run_id="scripted-00")),
            ("tool", lambda e: e["calls"][0].update(tool="outside-contract")),
            ("truncated", lambda e: e["calls"][0].update(status="truncated")),
            ("wall", lambda e: e.update(wall_ms=study.LIMITS["wall_ms"] + 1)),
            ("accounting", lambda e: e.update(output_bytes=e["output_bytes"] + 1)),
            ("calls", lambda e: e.update(calls=e["calls"] * 41)),
            ("capture", lambda e: e["calls"][0].update(output="x" * 16385)),
            ("answer_capture", lambda e: e.update(answer={})),
            ("duplicate_json_key", lambda e: e.update(final_output='{"items":[],"items":[]}')),
            ("malformed_final", lambda e: e.update(final_output="not JSON")),
            ("unknown_field", lambda e: e.update(unexpected=True)),
        ]
        for label, modify in modifications + [("order", None)]:
            with self.subTest(label=label):
                raw = self.bundle()
                raw["episodes"][1]["answer"]["evidence"] *= 2
                self.capture_answer(raw["episodes"][1], raw["episodes"][1]["answer"])
                if label == "order":
                    raw["episodes"][4], raw["episodes"][5] = raw["episodes"][5], raw["episodes"][4]
                else:
                    episode = raw["episodes"][5]
                    modify(episode)
                    if label in {"duplicate_json_key", "malformed_final"}:
                        episode["output_bytes"] = sum(len(c["output"].encode()) for c in episode["calls"]) + len(episode["final_output"].encode())
                    if label != "hash":
                        study.seal(episode)
                code, out, err = self.cli_entry(json.dumps(raw))
                self.assertEqual((code, out), (1, ""))
                self.assertEqual(json.loads(err)["error"]["code"], "invalid_evaluation")

    def test_answer_must_match_raw_capture(self):
        raw = self.bundle()
        raw["episodes"][1]["final_output"] = "{}"
        study.seal(raw["episodes"][1])
        with self.assertRaises(study.Invalid):
            self.evaluate(raw)

    def test_wrong_but_cited_answer_is_scored_as_false_positive(self):
        report = self.evaluate(self.bundle())
        row = report["cases"][2]
        self.assertFalse(row["correct"])
        self.assertEqual(row["false_positives"], ["symbol:src/lib.rs#price_preview:function"])

    def test_abstention_is_distinguished_from_correct_abstention(self):
        rows = self.evaluate(self.bundle())["cases"]
        self.assertTrue(rows[0]["abstained"])
        self.assertFalse(rows[0]["correct_abstention"])
        self.assertFalse(rows[0]["correct"])
        supported = [row for row in rows if row["case_id"].endswith("06")]
        self.assertTrue(all(row["correct_abstention"] for row in supported))

    def test_failures_timeouts_and_invalid_output_remain_in_denominator(self):
        report = self.evaluate(self.bundle())
        rows = report["cases"]
        for order, status in ((3, "failed"), (9, "timeout"), (15, "invalid")):
            self.assertEqual(rows[order]["status"], status)
            self.assertFalse(rows[order]["correct"])
            self.assertTrue(rows[order]["error"]["code"])
        self.assertEqual(sum(arm["episodes"] for split in report["aggregate"].values() for arm in split.values()), 24)
        self.assertEqual(sum(arm["failures"] for split in report["aggregate"].values() for arm in split.values()), 3)

    def test_unscored_failures_and_failure_answers_are_rejected(self):
        for field, value in (("error", None), ("answer", {}), ("status", "ok")):
            with self.subTest(field=field):
                raw = self.bundle()
                raw["episodes"][3][field] = value
                study.seal(raw["episodes"][3])
                with self.assertRaises(study.Invalid):
                    self.evaluate(raw)

    def test_absent_usage_stays_null_and_available_usage_is_preserved(self):
        raw = self.bundle()
        self.assertTrue(all(row["usage"] is None for row in self.evaluate(raw)["cases"]))
        usage = {"input_tokens": 42, "output_tokens": None, "cost_usd": None, "source": "operator provider response usage"}
        raw["episodes"][1]["usage"] = usage
        study.seal(raw["episodes"][1])
        report = self.evaluate(raw)
        self.assertEqual(report["cases"][1]["usage"], usage)
        self.assertEqual(report["aggregate"]["development"]["graph"]["usage_observed"], 1)

    def test_invalid_usage_is_refused(self):
        for usage in ({}, {"input_tokens": -1, "output_tokens": None, "cost_usd": None, "source": "provider"},
                      {"input_tokens": None, "output_tokens": None, "cost_usd": None, "source": "provider"},
                      {"input_tokens": True, "output_tokens": 1, "cost_usd": None, "source": "provider"}):
            with self.subTest(usage=usage):
                raw = self.bundle()
                raw["episodes"][1]["usage"] = usage
                study.seal(raw["episodes"][1])
                with self.assertRaises(study.Invalid):
                    self.evaluate(raw)

    def test_hashed_contract_model_and_isolation_changes_fail(self):
        modifications = [("hash", lambda e: e.update(record_sha256="0" * 64)),
                         ("request", lambda e: e["request"].update(prompt="different prompt")),
                         ("model", lambda e: e["model"].update(name="another model")),
                         ("truth", lambda e: e["isolation"].update(truth_inaccessible=False)),
                         ("warm", lambda e: e["isolation"].update(cold_start=False)),
                         ("shared", lambda e: e["isolation"].update(cache_id="scripted-cache-0")),
                         ("version", lambda e: e["tool_versions"].pop("orbit-graph")),
                         ("version_drift", lambda e: e["tool_versions"].update(rg="another-version")),
                         ("typed_request", lambda e: e["request"].update(order=True)),
                         ("no_calls", lambda e: e.update(calls=[])),
                         ("truncated", lambda e: e["calls"][0].update(status="truncated"))]
        for label, modify in modifications:
            with self.subTest(label=label):
                raw = self.bundle()
                modify(raw["episodes"][1])
                if label != "hash":
                    study.seal(raw["episodes"][1])
                with self.assertRaises(study.Invalid):
                    self.evaluate(raw)

    def test_episode_order_and_tool_allowlist_are_enforced(self):
        raw = self.bundle()
        raw["episodes"][0], raw["episodes"][1] = raw["episodes"][1], raw["episodes"][0]
        with self.assertRaises(study.Invalid):
            self.evaluate(raw)
        raw = self.bundle()
        raw["episodes"][0]["calls"][0]["tool"] = "refs"
        study.seal(raw["episodes"][0])
        with self.assertRaises(study.Invalid):
            self.evaluate(raw)

    def test_bounds_and_accounting_are_enforced(self):
        modifications = [lambda e: e.update(wall_ms=study.LIMITS["wall_ms"] + 1),
                         lambda e: e.update(wall_ms=0),
                         lambda e: e.update(output_bytes=e["output_bytes"] + 1),
                         lambda e: e.update(calls=e["calls"] * 41),
                         lambda e: e["calls"][0].update(output="x" * 16385),
                         lambda e: e.update(wall_ms=True)]
        for index, modify in enumerate(modifications):
            with self.subTest(index=index):
                raw = self.bundle()
                modify(raw["episodes"][1])
                study.seal(raw["episodes"][1])
                with self.assertRaises(study.Invalid):
                    self.evaluate(raw)

    def test_empty_cases_invalid_truth_and_source_drift_fail(self):
        for modification in ("empty", "truth", "split", "source"):
            with self.subTest(modification=modification), tempfile.TemporaryDirectory(dir=self.scratch) as directory:
                root = Path(directory) / "corpus"
                shutil.copytree(study.ROOT / "fixtures", root / "fixtures")
                data = copy.deepcopy(self.data)
                if modification == "empty":
                    data["cases"] = []
                elif modification == "truth":
                    data["cases"][0]["truth"]["items"] = []
                elif modification == "split":
                    data["cases"][0]["split"] = "held-out"
                else:
                    (root / "fixtures/python/shop.py").write_text("def bogus(): pass\n")
                (root / "corpus.json").write_text(json.dumps(data))
                (root / "corpus.lock.json").write_text((study.ROOT / "corpus.lock.json").read_text())
                with self.assertRaises(study.Invalid):
                    study.corpus(root)

    def test_rust_test_identities_use_declared_test_kind(self):
        fixture, head, base = self.trees["rust"]
        case = next(case for case in self.data["cases"] if case["id"] == "rust-04")
        items, abstain = study.expected(case, fixture["language"], head, base)
        self.assertFalse(abstain)
        self.assertEqual(items, ["symbol:src/lib.rs#test_invoice:test", "symbol:src/lib.rs#test_preview:test"])

    def test_requests_are_paired_balanced_and_solution_free(self):
        plan = study.requests(self.data)
        for index in range(0, len(plan), 2):
            first, second = plan[index:index + 2]
            self.assertEqual(first["prompt"], second["prompt"])
            self.assertEqual(first["source_revision"], second["source_revision"])
            self.assertEqual(first["limits"], second["limits"])
            self.assertNotIn("truth", first)
        for split in ("development", "held-out"):
            firsts = [r for r in plan[::2] if r["split"] == split]
            self.assertEqual(sum(r["arm"] == "baseline" for r in firsts), len(firsts) // 2)

    def test_export_contains_only_requests_prompts_and_pinned_source(self):
        with tempfile.TemporaryDirectory(dir=self.scratch) as directory:
            destination = Path(directory) / "export"
            report = study.export(self.data, self.trees, destination, "operator-fixed-crew")
            self.assertEqual(report["episode_count"], 24)
            exported = study.load(destination / "plan.json")
            for planned in exported["episodes"]:
                request = planned["request"]
                folder = destination / f"{request['order']:02}-{request['case_id']}-{request['arm']}"
                head = {str(path.relative_to(folder / "repository")): path.read_text()
                        for path in (folder / "repository").rglob("*") if path.is_file()}
                self.assertEqual(study.digest(head), request["source_revision"])
                self.assertEqual((folder / "prompt.txt").read_text(), request["prompt"])
                self.assertEqual(planned["invoke_input"]["provider_sandbox"], "read-only")
            with self.assertRaises(study.Invalid):
                study.export(self.data, self.trees, destination, "operator-fixed-crew")
            names = {path.name for path in destination.rglob("*")}
            self.assertTrue(names.isdisjoint({"truth.py", "corpus.json", "corpus.lock.json", "smoke-episodes.json", "smoke-result.json"}))

    def test_scripted_bundle_cannot_be_relabelled_agent_evidence(self):
        raw = self.bundle()
        raw["study_kind"] = "agent"
        with self.assertRaises(study.Invalid):
            self.evaluate(raw)


if __name__ == "__main__":
    unittest.main()
