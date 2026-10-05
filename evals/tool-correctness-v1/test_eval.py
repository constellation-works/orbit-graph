"""Independent negative controls and frozen-protocol checks; no product mocks."""
import copy
import importlib.util
import json
from pathlib import Path
import unittest

from scoring import aggregate, constraint, score

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("tool_correctness_eval", HERE / "eval.py")
evaluator = importlib.util.module_from_spec(spec)
spec.loader.exec_module(evaluator)
replay_spec = importlib.util.spec_from_file_location("tool_correctness_replay", HERE / "replay.py")
replayer = importlib.util.module_from_spec(replay_spec)
replay_spec.loader.exec_module(replayer)


class ScoringControls(unittest.TestCase):
    def setUp(self):
        self.case = {"checks": [{"path": "result.callees", "op": "set", "complete": True,
                                "fields": ["target_name", "target_qualified", "confidence"],
                                "value": [["leaf", "leaf", "exact"]]}],
                     "coverage": {"supported": 1, "unsupported": 0, "omitted": 0, "unresolved": 0}}
        self.correct = {"result": {"callees": [{"target_name": "leaf", "target_qualified": "leaf", "confidence": "exact"}]}}

    def test_complete_truth_and_denominators(self):
        result = score(self.case, self.correct)
        self.assertTrue(result["passed"])
        self.assertEqual((result["metrics"][0]["precision"], result["metrics"][0]["recall"]), (1, 1))

    def test_independently_missing_edge_fails(self):
        result = score(self.case, {"result": {"callees": []}})
        self.assertFalse(result["passed"])
        self.assertEqual(result["metrics"][0]["false_negatives"], 1)
        self.assertIsNone(result["metrics"][0]["precision"])
        self.assertEqual(result["metrics"][0]["recall"], 0)

    def test_independently_wrong_identity_fails(self):
        wrong = {"result": {"callees": [{"target_name": "leaf", "target_qualified": "other::leaf", "confidence": "exact"}]}}
        result = score(self.case, wrong)
        self.assertFalse(result["passed"])
        self.assertEqual(result["metrics"][0]["false_positives"], 1)

    def test_extra_and_duplicate_edges_falsify_precision(self):
        wrong = copy.deepcopy(self.correct)
        wrong["result"]["callees"].append(wrong["result"]["callees"][0])
        result = score(self.case, wrong)
        self.assertFalse(result["passed"])
        self.assertEqual(result["metrics"][0]["precision"], 0.5)

    def test_missing_field_is_not_null_or_an_empty_success(self):
        self.assertFalse(score(self.case, {"result": {}})["passed"])
        self.assertFalse(score(self.case, None)["passed"])
        wrong = copy.deepcopy(self.correct)
        del wrong["result"]["callees"][0]["confidence"]
        self.assertFalse(score(self.case, wrong)["passed"])

    def test_overconfidence_and_valid_uncertainty(self):
        uncertainty = {"checks": [{"path": "result.callees", "op": "confidence_ceiling", "value": "fuzzy_name", "names": ["work"]}],
                       "coverage": {"supported": 0, "unsupported": 1, "omitted": 0, "unresolved": 1}}
        uncertain = {"result": {"callees": [{"target_name": "work", "target_qualified": None, "confidence": "fuzzy_name"}]}}
        self.assertTrue(score(uncertainty, uncertain)["passed"])
        self.assertTrue(score(uncertainty, {"result": {"callees": []}})["passed"])
        overconfident = {"result": {"callees": [{"target_name": "work", "target_qualified": "A::work", "confidence": "exact"}]}}
        self.assertFalse(score(uncertainty, overconfident)["passed"])
        self.assertEqual(score(uncertainty, uncertain)["metrics"], [])

    def test_error_or_denial_is_never_coverage(self):
        self.assertFalse(score(self.case, self.correct, transport_ok=False)["passed"])
        self.assertFalse(score(self.case, self.correct, error_code="capability_denied")["passed"])
        refused = {**self.case, "checks": [], "expected_error": "invalid_request"}
        self.assertTrue(score(refused, {}, error_code="invalid_request")["passed"])
        self.assertFalse(score(refused, {}, error_code="invalid_request", transport_ok=False)["passed"])

    def test_recommend_wrapper_preserves_wrong_and_missing_controls(self):
        case = {"tool": "recommend", "checks": [{"path": "resolved_target_revision", "op": "eq", "value": "pinned-target"}],
                "coverage": {"supported": 1, "unsupported": 0, "omitted": 0, "unresolved": 0}}
        self.assertTrue(score(case, {"result": {"resolved_target_revision": "pinned-target"}})["passed"])
        self.assertFalse(score(case, {"result": {"resolved_target_revision": "other-target"}})["passed"])
        self.assertFalse(score(case, {"result": {}})["passed"])

    def test_one_failed_attempt_cannot_be_replaced_by_warm_success(self):
        rows = [{"tool": "callees", "case_id": "control", "surface": "mcp", "output_bytes": 10, "latency_ms": 1,
                 "score": score(self.case, output)} for output in [{"result": {"callees": []}}, self.correct, self.correct, self.correct]]
        report = aggregate(rows, ["callees", "search"])
        self.assertFalse(report["callees"]["passed"])
        self.assertEqual(report["callees"]["failures"], 1)
        self.assertEqual(report["callees"]["samples"], 4)
        self.assertFalse(report["search"]["passed"])

    def test_budget_uncertainty_and_false_completion(self):
        check = {"op": "budget_truth", "path": "coverage"}
        self.assertTrue(constraint(check, {"coverage": {"complete": True, "state": "published"}})[0])
        self.assertTrue(constraint(check, {"coverage": {"complete": False, "state": "budget_exhausted", "phase": "extracting", "note": "discarded"}})[0])
        self.assertFalse(constraint(check, {"coverage": {"complete": True, "state": "budget_exhausted"}})[0])

    def test_null_empty_metrics_and_false_zero(self):
        check = {"op": "null_metrics", "path": "metrics"}
        empty = {"metrics": [{"cases": 0, "recall_at_k": None, "precision_at_k": None, "stale_result_rate": None,
                              "mean_latency_ms": None, "max_latency_ms": None} for _ in range(8)]}
        self.assertTrue(constraint(check, empty)[0])
        empty["metrics"][0]["recall_at_k"] = 0
        self.assertFalse(constraint(check, empty)[0])


class FrozenProtocol(unittest.TestCase):
    def test_all_tools_languages_and_variants_have_source_truth(self):
        corpus = evaluator.frozen()
        self.assertEqual(len(corpus["cases"]), 95)
        self.assertGreaterEqual(len(corpus["cases"]), 30)
        for language in corpus["code_extractors"]:
            for suffix in ("declarations", "direct-call", "import"):
                self.assertIn(language + "-" + suffix, {c["id"] for c in corpus["cases"]})
        self.assertIn("jsx-direct-call", {c["id"] for c in corpus["cases"]})
        self.assertIn("tsx-direct-call", {c["id"] for c in corpus["cases"]})
        for surface in ("plugin-cli", "mcp", "cli"):
            self.assertEqual({c["tool"] for c in corpus["cases"] if surface in c["surfaces"]}, set(corpus["tools"]))

    def test_only_named_volatile_fields_are_removed(self):
        a = {"repository": "/one", "index": {"synced_at": "first", "files": 3}, "result": {"callees": []}}
        b = {"repository": "/two", "index": {"synced_at": "later", "files": 3}, "result": {"callees": []}}
        self.assertEqual(evaluator.semantic(a, ["repository", "index.synced_at"]), evaluator.semantic(b, ["repository", "index.synced_at"]))
        b["index"]["files"] = 4
        self.assertNotEqual(evaluator.semantic(a, ["repository", "index.synced_at"]), evaluator.semantic(b, ["repository", "index.synced_at"]))

    def test_envelope_strict_chronology_is_independent_of_candidate(self):
        task = evaluator.synthetic_task("target", 400)
        fixture = {"commits": ["0" * 40, "1" * 40, "2" * 40, "3" * 40]}
        truth = evaluator.envelope(fixture, 2, 3, task, 600)
        self.assertEqual(truth["before_revision"], "2" * 40)
        self.assertEqual(truth["delivered_at"]["timestamp"], "unix:600")
        self.assertEqual(task["snapshot_available_at"]["timestamp"], "unix:400")
        self.assertNotIn("changed_files", task)
        self.assertEqual(truth["source"]["system"], "reviewed_synthetic_public_envelope")

    def test_child_environment_excludes_host_authority(self):
        env = evaluator.environment(Path("/owned/home"), Path("/owned/tmp"), Path("/owned/bin"))
        self.assertNotIn("ORBIT_ROOT", env)
        self.assertNotIn("ORBIT_OPERATOR", env)
        self.assertEqual(env["HOME"], "/owned/home")
        self.assertEqual(env["TMPDIR"], "/owned/tmp")
        self.assertEqual(env["GIT_CONFIG_GLOBAL"], "/dev/null")

    def test_host_cli_error_after_warning_retains_structured_code(self):
        record = {"exit_code": 1, "stdout": "", "stderr":
                  'host diagnostic\n{"code":"index_missing","message":"build history first"}\n'}
        value, code = evaluator.plugin_cli_reply(record)
        self.assertEqual(code, "index_missing")
        self.assertEqual(value["message"], "build history first")
        record["stderr"] += '{"code":"other","message":"second error"}\n'
        with self.assertRaises(ValueError):
            evaluator.plugin_cli_reply(record)

    def test_non_utf8_transport_cannot_pass(self):
        record = {"exit_code": 0, "stopped": None, "error_type": None, "stderr_truncated": False,
                  "supervision": {"survivors": [], "signals": []}, "streams_utf8": False}
        self.assertFalse(evaluator.transport_ok(record))

    def test_fixture_path_is_not_ambiguity_disclosure(self):
        case = {"checks": [], "challenge": "ambiguous_show", "coverage": {"supported": 1, "unsupported": 0, "omitted": 0, "unresolved": 0}}
        output = {"repository": "/owned/show-ambiguous/repo", "result": {"metadata": {"qualified": "A::work"}}}
        self.assertFalse(score(case, output)["passed"])
        output["result"]["ambiguous"] = True
        self.assertTrue(score(case, output)["passed"])

    def test_replay_decodes_real_mcp_shape_without_querying(self):
        wire = ('{"jsonrpc":"2.0","id":1,"result":{"capabilities":{"tools":{}}}}\n'
                '{"jsonrpc":"2.0","id":2,"result":{"structuredContent":{"result":null},"isError":false}}\n')
        result, error = replayer.payload({"stdout": wire}, "mcp", "show")
        self.assertEqual(result, {"result": None})
        self.assertIsNone(error)
        with self.assertRaises(ValueError):
            replayer.payload({"stdout": wire + wire.splitlines()[1] + "\n"}, "mcp", "show")


class OperatorReviewControls(unittest.TestCase):
    """Controls requested by the independent source review, including its exact counterexamples."""
    def setUp(self):
        corpus = evaluator.frozen()
        substitutions = {"base": "a" * 40, "head": "b" * 40, "target": "c" * 40}
        def resolve(value):
            if isinstance(value, str) and value.startswith("$"):
                return substitutions.get(value[1:], value)
            if isinstance(value, list):
                return [resolve(v) for v in value]
            if isinstance(value, dict):
                return {k: resolve(v) for k, v in value.items()}
            return value
        self.cases = {case["id"]: resolve(case) for case in corpus["cases"]}
        self.comparison = {"base": {"commit_sha": "a" * 40}, "head": {"commit_sha": "b" * 40}}

    def test_original_operator_missing_changes_controls_fail(self):
        invalid = {"result": {"complete": True, "comparison": {
            "base": {"commit_sha": "invalid"}, "head": {"commit_sha": "also-invalid"}},
            "symbols": [{"status": "added"}]}}
        for name in ("changes-rename-edit", "changes-ambiguous-body"):
            self.assertFalse(score(self.cases[name], invalid)["passed"])
            invalid["result"]["comparison"] = self.comparison
            self.assertFalse(score(self.cases[name], invalid)["passed"])

    def uncertainty(self, name):
        check = next(c for c in self.cases[name]["checks"] if c["op"] == "uncertain_pairing")
        candidates = [{"snapshot": side, "selector": selector, "commit_sha": revision,
                       "reason": "The source does not establish pair correspondence"}
                      for side, selector, revision in check["value"]]
        return {"result": {"complete": True, "comparison": copy.deepcopy(self.comparison), "symbols": [{
            "status": "uncertain", "pairing": "uncertain", "base": None, "head": None,
            "uncertain_candidates": candidates, "note": "Every source identity is retained; none is chosen"}]}}

    def test_valid_uncertainty_covers_exact_sides_and_provenance(self):
        for name in ("changes-rename-edit", "changes-ambiguous-body"):
            valid = self.uncertainty(name)
            self.assertTrue(score(self.cases[name], valid)["passed"])
            for mutation in ("missing", "duplicate", "invented", "wrong_side", "wrong_revision", "overconfident"):
                wrong = copy.deepcopy(valid)
                row = wrong["result"]["symbols"][0]
                candidates = row["uncertain_candidates"]
                if mutation == "missing": candidates.pop()
                elif mutation == "duplicate": candidates.append(copy.deepcopy(candidates[0]))
                elif mutation == "invented": candidates[0]["selector"] = "symbol:src/lib.rs#invented:function"
                elif mutation == "wrong_side": candidates[0]["snapshot"] = "head"
                elif mutation == "wrong_revision": candidates[0]["commit_sha"] = "b" * 40
                else: row["pairing"] = "renamed"
                self.assertFalse(score(self.cases[name], wrong)["passed"], mutation)
            wrong = copy.deepcopy(valid)
            wrong["result"]["comparison"]["base"]["commit_sha"] = "b" * 40
            self.assertFalse(score(self.cases[name], wrong)["passed"])

    def test_complete_unpaired_sides_are_valid_without_a_rename_claim(self):
        valid = self.uncertainty("changes-rename-edit")
        candidates = valid["result"]["symbols"][0]["uncertain_candidates"]
        valid["result"]["symbols"] = [{"status": "removed" if c["snapshot"] == "base" else "added",
            "pairing": "same_selector", "base": c if c["snapshot"] == "base" else None,
            "head": c if c["snapshot"] == "head" else None, "uncertain_candidates": []} for c in candidates]
        self.assertTrue(score(self.cases["changes-rename-edit"], valid)["passed"])

    def ranking(self):
        fields = ("variant", "level", "k", "cases", "relevant", "true_positives", "returned")
        rows = [{**dict(zip(fields, row)), "recall_at_k": 1.0, "precision_at_k": 0.1,
                 "stale_result_rate": 0.0, "mean_latency_ms": 1.0, "max_latency_ms": 1.0}
                for row in next(c for c in self.cases["recommend-strict-evaluation"]["checks"]
                                if c["op"] == "ranking_metrics")["value"]]
        return {"metrics": rows, "coverage": {"isolated_indexes": True}, "cases": [{
            "evaluated": True, "target_revision": "c" * 40, "truth_coverage": {
            "file_changes_total": 2, "file_truth_eligible": 1, "file_truth_omitted": {"new": 1},
            "symbol_truth_eligible": 1, "symbol_truth_omitted": {"new": 1}}}]}

    def test_operator_invalid_metrics_and_inconsistent_counts_fail(self):
        valid = self.ranking()
        case = self.cases["recommend-strict-evaluation"]
        self.assertTrue(score(case, valid)["passed"])
        for key, value in (("recall_at_k", -1), ("precision_at_k", 9), ("precision_at_k", 1),
                           ("true_positives", 0), ("returned", 2), ("k", 1),
                           ("mean_latency_ms", float("nan")), ("recall_at_k", None)):
            wrong = copy.deepcopy(valid)
            wrong["metrics"][0][key] = value
            self.assertFalse(score(case, wrong)["passed"], (key, value))
        original = copy.deepcopy(valid)
        for row in original["metrics"]:
            row["recall_at_k"], row["precision_at_k"] = -1, 9
        self.assertFalse(score(case, original)["passed"])
        wrong = copy.deepcopy(valid)
        wrong["metrics"][1] = copy.deepcopy(wrong["metrics"][0])
        self.assertFalse(score(case, wrong)["passed"])

    def snapshot(self):
        # Independent literal head truth: keep -> added, moved at the new path.
        snapshot = {"overview": {"total_files": 2, "total_symbols": 3, "files": [
            {"path": "src/lib.rs", "symbols": [{"name": n, "qualified": n, "kind": "function"} for n in ("keep", "added")]},
            {"path": "src/new.rs", "symbols": [{"name": "moved", "qualified": "moved", "kind": "function"}]}]}}
        for name in ("keep", "remove", "added", "moved"):
            matches = [] if name == "remove" else [{"path": "src/new.rs" if name == "moved" else "src/lib.rs", "name": name}]
            snapshot["search-" + name] = {"matches": matches, "truncated": False}
        for path, name in (("src/lib.rs", "keep"), ("src/lib.rs", "remove"), ("src/lib.rs", "added"),
                           ("src/old.rs", "moved"), ("src/new.rs", "moved")):
            key = path.replace("/", "_").replace(".", "_") + "-" + name
            snapshot["callees-" + key] = {"callees": [{"target_name": "added", "target_qualified": "added", "confidence": "exact"}] if name == "keep" else []}
            snapshot["refs-" + key] = {"refs": [{"from_selector": "symbol:src/lib.rs#keep:function", "kind": "call", "confidence": "exact"}] if name == "added" else []}
        return snapshot

    def test_lifecycle_same_counts_stale_identity_or_edge_cannot_pass(self):
        case = self.cases["maintain-incremental"]
        valid = self.snapshot()
        self.assertTrue(evaluator.lifecycle_verdict(case, valid, valid, "head")["passed"])
        for mutate in ("identity", "edge", "empty_search"):
            wrong = copy.deepcopy(valid)
            if mutate == "identity": wrong["overview"]["files"][1]["path"] = "src/old.rs"
            elif mutate == "edge": wrong["callees-src_lib_rs-keep"]["callees"][0]["target_qualified"] = "remove"
            else: wrong["search-added"]["matches"] = []
            self.assertFalse(evaluator.lifecycle_verdict(case, wrong, wrong, "head")["passed"], mutate)
            self.assertFalse(evaluator.lifecycle_verdict(case, wrong, valid, "head")["passed"], mutate)

    def test_budget_fixture_has_distinct_source_trees_and_preserves_all_bounds(self):
        corpus = evaluator.frozen()
        case = self.cases["changes-budget"]
        trees = corpus["repositories"][case["repository"]]["trees"]
        self.assertEqual(len(trees), 2)
        self.assertNotEqual((HERE / trees[0]["src/lib.rs"]).read_bytes(), (HERE / trees[1]["src/lib.rs"]).read_bytes())
        valid = {"result": {"complete": False, "comparison": None, "symbols": [],
            "incomplete": {"phase": "indexing", "bound": "budget_ms", "value": 1000, "message": "Stopped before comparison"},
            "query": {"bounds": {"budget_ms": 1000, "query_budget_ms": 100, "node_cap": 1}},
            "truncated": True, "truncation": [{"what": "comparison", "bound": "budget_ms", "value": 1000}]}}
        self.assertTrue(score(case, valid)["passed"])
        for key in ("budget_ms", "query_budget_ms", "node_cap"):
            wrong = copy.deepcopy(valid)
            del wrong["result"]["query"]["bounds"][key]
            self.assertFalse(score(case, wrong)["passed"])
        wrong = copy.deepcopy(valid)
        wrong["result"]["complete"] = True
        self.assertFalse(score(case, wrong)["passed"])


if __name__ == "__main__":
    unittest.main()
