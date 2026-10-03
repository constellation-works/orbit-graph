"""Behavior tests for the operator entry point and its fail-closed contract."""
import copy
from contextlib import redirect_stderr, redirect_stdout
import io
import json
from pathlib import Path
import shutil
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

    def test_absent_and_invalid_evidence_fail_even_if_truth_answer_is_correct(self):
        for change in ("missing", "wrong_quote", "wrong_line", "unknown_symbol", "duplicate", "wrong_file"):
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
                episode["final_output"] = study.canonical(answer).decode()
                episode["output_bytes"] = sum(len(c["output"].encode()) for c in episode["calls"]) + len(episode["final_output"].encode())
                study.seal(episode)
                code, out, _ = self.entry(json.dumps(raw))
                self.assertEqual((code, out), (1, ""))

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
