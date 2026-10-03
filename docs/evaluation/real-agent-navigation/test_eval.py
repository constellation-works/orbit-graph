"""Hermetic behavioral tests; test artifacts are never effectiveness evidence."""
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("real_evaluation", ROOT / "eval.py")
e = importlib.util.module_from_spec(spec)
spec.loader.exec_module(e)


class EvaluationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        scratch = Path(os.environ.get("ORBIT_SCRATCH_DIR", Path.cwd() / ".orbit/tmp"))
        scratch.mkdir(parents=True, exist_ok=True)
        cls.tmp = tempfile.TemporaryDirectory(prefix="real-eval-tests-", dir=scratch)
        cls.root = Path(cls.tmp.name)
        cls.repo = cls.root / "source"
        cls.repo.mkdir()
        e.git(cls.repo, "init", "--initial-branch=main")
        e.write_new(cls.repo / "src/lib.py", "def target():\n    return 1\n\ndef other():\n    return 2\n")
        e.write_new(cls.repo / "docs/answers.py", "DO_NOT_EXPORT = 'rubric answer'\n")
        e.write_new(cls.repo / "src/.secret.py", "DO_NOT_EXPORT = 'secret'\n")
        e.write_new(cls.repo / "skills/instructions.py", "DO_NOT_EXPORT = 'instruction'\n")
        e.git(cls.repo, "add", ".")
        e.git(cls.repo, "-c", "user.name=fixture", "-c", "user.email=fixture@example.invalid", "-c", "commit.gpgSign=false", "commit", "-m", "test source")
        revision = e.git(cls.repo, "rev-parse", "HEAD").decode().strip()
        manifest, files = e.snapshot(cls.repo, revision)
        original, _ = e.corpus()
        cls.data = copy.deepcopy(original)
        cls.data["label"] = "test-only synthetic source view"
        cls.data["repositories"] = {k: revision for k in original["repositories"]}
        for case in cls.data["cases"]:
            case["original_source_revision"] = revision
            case["kind"] = "test-only"
            case["truth"] = {
                "identities": [{"selector": "symbol:src/lib.py#target:function", "file": "src/lib.py", "line": 1, "quote": "def target():", "required": True},
                               {"selector": "symbol:src/lib.py#other:function", "file": "src/lib.py", "line": 4, "quote": "def other():", "required": False}],
                "rubric": [{"id": "behavior", "required_behavior": "target returns one; no exhaustive graph claim",
                            "evidence": [{"revision": "head", "file": "src/lib.py", "line": 2, "end_line": 2,
                                          "quote": "return 1", "excerpt_sha256": hashlib.sha256(b"    return 1\n").hexdigest()}]}],
                "accepted_alternatives": ["equivalent explanation"], "limitations": ["test only"]}
        keys = {c[k] for c in cls.data["cases"] for k in ("head_snapshot", "base_snapshot")}
        cls.lock = {"schema_version": 1, "corpus_sha256": e.digest(cls.data), "truth_sha256": e.digest({c["id"]: c["truth"] for c in cls.data["cases"]}),
                    "split_sha256": e.digest({c["id"]: c["split"] for c in cls.data["cases"]}), "snapshots": {k: manifest for k in keys}}
        cls.lock["request_plan_sha256"] = e.digest(e.requests(cls.data, cls.lock))
        cls.trees = {k: files for k in keys}
        cls.tool = cls.root / "helper"
        cls.tool.mkdir()
        source = (ROOT / "eval.py").read_text().replace(e.LOCK_SHA256, e.digest(cls.lock))
        e.write_new(cls.tool / "eval.py", source)
        e.write_new(cls.tool / "corpus.json", cls.data)
        e.write_new(cls.tool / "corpus.lock.json", cls.lock)
        cls.repos = [arg for k in cls.data["repositories"] for arg in ("--repo", f"{k}={cls.repo}")]
        cls.fixture_index = 0

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def cli(self, *args, ok=True, helper=None):
        command = [sys.executable, str((helper or self.tool) / "eval.py"), *map(str, args)]
        env = {"PATH": "/usr/bin:/bin", "HOME": str(self.root), "LANG": "C.UTF-8", "PYTHONDONTWRITEBYTECODE": "1"}
        if ok:
            return e.parse(e.capture(command, env, bound=e.MAX_JSON))
        with self.assertRaises(e.Invalid):
            e.capture(command, env, bound=e.MAX_JSON)

    def cohort(self):
        type(self).fixture_index += 1
        root = self.root / f"cohort-{self.fixture_index}"
        cases = {c["id"]: c for c in self.data["cases"]}
        audits, directories = [], []
        for request in e.requests(self.data, self.lock):
            run = f"fixture-{request['order']}"
            directory = root / run
            directory.mkdir(parents=True)
            answer = {"items": ["symbol:src/lib.py#target:function"], "abstain": False,
                      "reason": "target returns one; the source view is not exhaustive",
                      "evidence": [{"item": "symbol:src/lib.py#target:function", "file": "src/lib.py", "line": 1, "quote": "def target():"}]}
            final = json.dumps(answer)
            call = {"tool": "read", "input": '{"path":"src/lib.py"}', "output": self.trees[next(iter(self.trees))]["src/lib.py"], "elapsed_ms": 2, "status": "ok"}
            raw = {k: None for k in e.RAW if k != "artifact_sha256"}
            raw.update(schema_version=1, kind="agent-eval-raw-episode", runner_version="1", run_id=run,
                       request=request, request_digest=e.digest(request), started_at_unix=1790994000 + request["order"],
                       model={"provider": "fake-test-provider", "name": "fake", "version": "test-only", "settings": {"reasoning": "fixed"}},
                       provider={"binary_sha256": "a" * 64, "argv": [], "env_names": [], "exit": 0, "thread_id": run},
                       tool_versions={k: "fixture-v1" for k in e.COMMON + (["orbit-graph"] if request["arm"] == "graph" else [])},
                       inputs={"head": {"content_revision": request["source_revision"], "files": 1, "bytes": 54}, "base": {"content_revision": request["base_revision"], "files": 1, "bytes": 54}},
                       snapshot={"base_commit": "b" * 40, "head_commit": "c" * 40, "base_tree": "d" * 40, "head_tree": "d" * 40,
                                 "commit_count": 2, "commit_epoch": 1790985600, "work_tree_revision": request["source_revision"]},
                       isolation={"containment": "bwrap", "contained": True, "truth_inaccessible": True, "cold_start": True,
                                  "repository_id": run + ":repo", "cache_id": run + ":cache",
                                  "probe": {"forbidden": ["/fixture/truth"], "leaked": [], "missing": [], "mount_count": 3, "identity": {}, "hidden": True},
                                  "probe_spec_forbidden": ["/fixture/truth"], "sandbox_argv": ["fake-bwrap-test-only"],
                                  "sandbox_argv_sha256": e.digest(["fake-bwrap-test-only"]), "bwrap": "fixture", "broker_sha256": "f" * 64,
                                  "tools_listed": [request["tools"]], "refused_tool_names": [],
                                  "inventory": {"disabled_verified": ["shell_tool", "apps", "plugins", "multi_agent", "browser_use", "computer_use", "view_image", "hooks", "memories"], "enabled_features": [], "shell_gated": [], "mcp_servers": ["eval_broker"]},
                                  "unbrokered": [], "graph_state_after": {}},
                       timing={"wall_ms": 100 + request["order"], "setup_ms": 10, "provider_ms": 90 + request["order"],
                               "graph_sync_ms": 0, "preflight_ms": 1, "wall_limit_ms": 300000}, status="ok", error=None,
                       answer=answer, final_output=final, final_output_truncated=False, final_source="fixture",
                       calls=[call], calls_dropped=0, output_bytes=len(call["output"].encode()) + len(final.encode()), usage=None,
                       usage_raw=None, transcript={}, broker={}, cleanup={"terminated_by": None, "signals": [], "survivors": [], "pipes_closed": True, "brokers_swept": []},
                       files={}, limitations=["FAKE deterministic test; no live effectiveness"])
            for name in ("provider.jsonl", "provider-stderr.txt", "broker-calls.jsonl", "final-message.txt", "probe.json", "preflight.json"):
                payload = (final if name == "final-message.txt" else "fake test only\n").encode()
                (directory / name).write_bytes(payload)
                raw["files"][name] = {"bytes": len(payload), "sha256": hashlib.sha256(payload).hexdigest(), "redactions": 0, "truncated": False}
            e.write_new(directory / "episode.json", e.seal(raw, "artifact_sha256"))
            record, _ = e.raw_record(directory)
            audits.append(e.seal({"run_id": run, "record_sha256": record["record_sha256"], "reviewer": "fixture reviewer (not human evidence)",
                                  "signed_at": "2026-10-03T02:20:00Z", "attestation": "FAKE test judgment only",
                                  "judgments": [{"claim_id": "behavior", "pass": True, "rationale": "fixture rubric",
                                                 "answer_quote": "target returns one", "source_evidence": cases[request["case_id"]]["truth"]["rubric"][0]["evidence"]}]}, "attestation_sha256"))
            directories.append(directory)
        bundle = e.adapt(directories, "test-only")
        return bundle, {"schema_version": 1, "audits": audits}, directories

    def score_cli(self, bundle, audits, ok=True):
        type(self).fixture_index += 1
        folder = self.root / f"inputs-{self.fixture_index}"
        e.write_new(folder / "records.json", bundle)
        e.write_new(folder / "audits.json", audits)
        return self.cli("score", *self.repos, "--records", folder / "records.json", "--audits", folder / "audits.json", ok=ok)

    def update_raw(self, bundle, directories, index, mutate, audits=None):
        raw = e.load(directories[index] / "episode.json")
        mutate(raw)
        raw["request_digest"] = e.digest(raw["request"])
        e.seal(raw, "artifact_sha256")
        (directories[index] / "episode.json").write_text(json.dumps(raw))
        record, _ = e.raw_record(directories[index])
        bundle["episodes"][index] = record
        bundle["evidence"][index]["artifact_sha256"] = raw["artifact_sha256"]
        if audits is not None:
            audit = audits["audits"][index]
            audit["record_sha256"] = record["record_sha256"]
            e.seal(audit, "attestation_sha256")

    def test_actual_cli_validate_export_adapt_score_and_no_answer_leak(self):
        self.assertTrue(self.cli("validate", *self.repos)["valid"])
        destination = self.root / "export"
        self.assertEqual(self.cli("export", *self.repos, "--destination", destination)["episodes"], 16)
        all_files = [p for p in destination.rglob("*") if p.is_file()]
        self.assertFalse(any("DO_NOT_EXPORT" in p.read_text() for p in all_files))
        self.assertFalse(any(p.name in {"corpus.json", "corpus.lock.json", "eval.py"} for p in all_files))
        request = e.load(destination / "00-graph-inbound-identity-baseline/request.json")
        self.assertNotIn("truth", request)
        self.assertEqual(request["limits"], e.LIMITS)
        self.cli("export", *self.repos, "--destination", destination, ok=False)
        bundle, audits, dirs = self.cohort()
        path = self.root / "adapted.json"
        self.cli("adapt", *[arg for d in dirs for arg in ("--raw", d)], "--study-kind", "test-only", "--output", path)
        self.assertEqual(e.load(path), bundle)
        result = self.score_cli(bundle, audits)
        self.assertEqual(result["summaries"][0]["accuracy"], 1)
        self.assertFalse(result["effectiveness_claim_permitted"])
        self.assertFalse(result["follow_up_larger_independent_repo_cohort"])
        self.assertIsNone(result["summaries"][0]["usage_total"]["input_tokens"])

    def test_truth_corpus_lock_and_source_drift_refused(self):
        for name in ("corpus.json", "corpus.lock.json"):
            folder = self.root / ("mutated-" + name)
            folder.mkdir()
            for filename in ("eval.py", "corpus.json", "corpus.lock.json"):
                (folder / filename).write_bytes((self.tool / filename).read_bytes())
            value = e.load(folder / name)
            if name == "corpus.json":
                value["cases"][0]["truth"]["identities"][0]["quote"] = "wrong truth"
            else:
                value["snapshots"][next(iter(value["snapshots"]))]["git_tree"] = "f" * 40
            (folder / name).write_text(json.dumps(value))
            self.cli("validate", *self.repos, helper=folder, ok=False)
        mutated = copy.deepcopy(self.lock)
        mutated["snapshots"][next(iter(mutated["snapshots"]))]["files"]["src/lib.py"] = "0" * 64
        with self.assertRaises(e.Invalid):
            e.verify(self.data, mutated, {k: self.repo for k in self.data["repositories"]})

    def test_missing_duplicate_malformed_unknown_and_unaudited_pairs_refused(self):
        for mutation in (lambda b, a: b["episodes"].pop(), lambda b, a: b["episodes"].append(b["episodes"][0]),
                         lambda b, a: b.update(unrecognized=True), lambda b, a: a["audits"].pop(),
                         lambda b, a: b["episodes"][0].update(extra=True)):
            bundle, audits, _ = self.cohort()
            mutation(bundle, audits)
            self.score_cli(bundle, audits, ok=False)
        with self.assertRaises(e.Invalid):
            e.parse('{"a":1,"a":2}')
        with self.assertRaises(e.Invalid):
            e.parse('{"a":NaN}')

    def test_invalid_citation_and_wrong_identity_valid_source_fail_objective(self):
        for wrong_identity in (False, True):
            bundle, audits, dirs = self.cohort()
            def mutate(raw):
                if wrong_identity:
                    item = "symbol:src/lib.py#invented:function"
                    raw["answer"]["items"] = [item]
                    raw["answer"]["evidence"][0]["item"] = item
                else:
                    raw["answer"]["evidence"][0]["quote"] = "def absent():"
                raw["final_output"] = json.dumps(raw["answer"])
                raw["output_bytes"] = len(raw["calls"][0]["output"].encode()) + len(raw["final_output"].encode())
            self.update_raw(bundle, dirs, 0, mutate, audits)
            report = self.score_cli(bundle, audits)
            self.assertFalse(report["episodes"][0]["correct"])
            self.assertTrue(report["episodes"][0]["semantic_pass"])
            if wrong_identity:
                self.assertEqual(len(report["episodes"][0]["false_positives"]), 1)
                self.assertEqual(report["episodes"][0]["citation_precision"], 1)

    def test_failure_denominator_unknown_usage_and_speed_filter(self):
        for status in ("failed", "invalid", "timeout"):
            bundle, audits, dirs = self.cohort()
            def mutate(raw):
                raw["status"] = status
                raw["answer"] = None
                raw["error"] = {"code": status, "message": "fake test failure"}
            self.update_raw(bundle, dirs, 0, mutate)
            audits["audits"].pop(0)
            report = self.score_cli(bundle, audits)
            self.assertEqual(report["summaries"][0]["denominator"], 8)
            self.assertEqual(report["summaries"][0]["accuracy"], 7 / 8)
            self.assertEqual(report["episodes"][0]["status"], status)
            self.assertIsNone(report["paired"][0]["wall_delta_graph_minus_baseline_ms"])

    def test_required_and_optional_identity_citations_must_match_declarations(self):
        required, optional = self.data["cases"][0]["truth"]["identities"]
        for required_match, optional_match in ((True, False), (True, True), (False, True), (True, None)):
            with self.subTest(required_match=required_match, optional_match=optional_match):
                bundle, audits, dirs = self.cohort()
                def mutate(raw):
                    raw["answer"]["items"].append(optional["selector"])
                    citation = required if required_match else optional
                    raw["answer"]["evidence"][0].update({k: citation[k] for k in ("file", "line", "quote")})
                    if optional_match is not None:
                        citation = optional if optional_match else required
                        raw["answer"]["evidence"].append({"item": optional["selector"],
                            **{k: citation[k] for k in ("file", "line", "quote")}})
                    raw["final_output"] = json.dumps(raw["answer"])
                    raw["output_bytes"] = len(raw["calls"][0]["output"].encode()) + len(raw["final_output"].encode())
                self.update_raw(bundle, dirs, 0, mutate, audits)
                row = self.score_cli(bundle, audits)["episodes"][0]
                self.assertEqual(row["objective_pass"], required_match and optional_match is True)
                self.assertEqual(row["correct"], required_match and optional_match is True)
                self.assertTrue(row["semantic_pass"])
                self.assertEqual(row["identity_precision"], 1)
                self.assertEqual(row["identity_recall"], 1)
                self.assertEqual(row["citation_precision"], 1)
                self.assertEqual(row["citation_coverage"], 0.5 if optional_match is None else 1)
                self.assertEqual(row["false_positives"], [])
                self.assertIsNone(row["answer_error"])
                self.assertIsNone(row["isolation_error"])

    def test_partial_captured_usage_preserves_fields_totals_and_paired_deltas(self):
        bundle, audits, dirs = self.cohort()
        development = [case["id"] for case in self.data["cases"] if case["split"] == "development"]
        heldout = [case["id"] for case in self.data["cases"] if case["split"] == "held-out"]
        expected_usage = []
        for index, record in enumerate(bundle["episodes"]):
            arm, case_id = record["request"]["arm"], record["request"]["case_id"]
            usage = {"input_tokens": 100 if arm == "baseline" else 80,
                     "output_tokens": 10 if arm == "baseline" else 12,
                     "cost_usd": 0.25 if arm == "baseline" else 0.125,
                     "source": "fixture captured usage"}
            if case_id == development[0]:
                usage["output_tokens" if arm == "baseline" else "input_tokens"] = None
                if arm == "baseline":
                    usage["cost_usd"] = None
            elif case_id == development[1]:
                usage["input_tokens" if arm == "baseline" else "output_tokens"] = None
                if arm == "graph":
                    usage["cost_usd"] = None
            elif case_id == development[2]:
                usage = None if arm == "baseline" else {
                    "input_tokens": 0, "output_tokens": 0, "cost_usd": 0, "source": "fixture captured usage"}
            elif case_id == heldout[0] and arm == "graph":
                usage.update(input_tokens=None, output_tokens=None, cost_usd=0)
            def mutate(raw):
                raw["usage"] = usage
                raw["usage_raw"] = None if usage is None else {
                    k: usage[k] for k in ("input_tokens", "output_tokens") if usage[k] is not None}
            self.update_raw(bundle, dirs, index, mutate, audits)
            expected_usage.append(usage)
        self.assertEqual(e.adapt(dirs, "test-only"), bundle)
        report = self.score_cli(bundle, audits)
        self.assertEqual([r["usage"] for r in report["episodes"]], expected_usage)
        self.assertTrue(all(r["correct"] for r in report["episodes"]))
        for summary in report["summaries"]:
            with self.subTest(split=summary["split"], arm=summary["arm"]):
                expected = {"input_tokens": None, "output_tokens": None, "cost_usd": None}
                if summary["split"] == "held-out":
                    expected = ({"input_tokens": 500, "output_tokens": 50, "cost_usd": 1.25}
                                if summary["arm"] == "baseline" else
                                {"input_tokens": None, "output_tokens": None, "cost_usd": 0.5})
                self.assertEqual(summary["usage_total"], expected)
                missing = int(summary["arm"] == "baseline" and summary["split"] != "held-out")
                self.assertEqual(summary["usage_known"], summary["denominator"] - missing)
        for pair in report["paired"]:
            if pair["case_id"] in development:
                expected = {"input_tokens": None, "output_tokens": None, "cost_usd": None}
            elif pair["case_id"] == heldout[0]:
                expected = {"input_tokens": None, "output_tokens": None, "cost_usd": -0.25}
            else:
                expected = {"input_tokens": -20, "output_tokens": 2, "cost_usd": -0.125}
            self.assertEqual(pair["usage_delta_graph_minus_baseline"], expected)

    def test_non_null_captured_token_usage_still_requires_bounded_integers(self):
        for key in ("input_tokens", "output_tokens"):
            for value in (-1, True, 1.5, "1", 10**12 + 1):
                with self.subTest(key=key, value=value):
                    bundle, audits, dirs = self.cohort()
                    def mutate(raw):
                        raw["usage"] = {"input_tokens": None, "output_tokens": None,
                                        "cost_usd": None, "source": "fixture captured usage", key: value}
                    self.update_raw(bundle, dirs, 0, mutate, audits)
                    self.score_cli(bundle, audits, ok=False)

    def test_model_tool_request_capture_audit_and_isolation_integrity(self):
        mutations = [lambda r: r["model"].update(name="other-model"),
                     lambda r: r["tool_versions"].update(read="different"),
                     lambda r: r["request"].update(prompt="wrong request"),
                     lambda r: r["inputs"]["head"].update(content_revision="0" * 64)]
        for mutate in mutations:
            bundle, audits, dirs = self.cohort()
            self.update_raw(bundle, dirs, 0, mutate, audits)
            self.score_cli(bundle, audits, ok=False)
        bundle, audits, dirs = self.cohort()
        self.update_raw(bundle, dirs, 0, lambda r: r["isolation"]["probe"].update(hidden=False), audits)
        report = self.score_cli(bundle, audits)
        self.assertFalse(report["episodes"][0]["correct"])
        self.assertIsNotNone(report["episodes"][0]["isolation_error"])
        audits["audits"][0]["judgments"][0]["answer_quote"] = "not in the answer"
        e.seal(audits["audits"][0], "attestation_sha256")
        self.score_cli(bundle, audits, ok=False)
        (dirs[1] / "provider.jsonl").write_text("tampered capture")
        with self.assertRaises(e.Invalid):
            e.adapt(dirs, "test-only")

    def test_dynamic_candidates_and_manual_fail_are_separate(self):
        bundle, audits, dirs = self.cohort()
        # The answer can cite candidates with abstain=false while explicitly
        # denying uniqueness. Identity presence alone cannot pass the rubric.
        audits["audits"][0]["judgments"][0]["pass"] = False
        e.seal(audits["audits"][0], "attestation_sha256")
        report = self.score_cli(bundle, audits)
        self.assertTrue(report["episodes"][0]["objective_pass"])
        self.assertFalse(report["episodes"][0]["semantic_pass"])
        self.assertFalse(report["episodes"][0]["correct"])

    def test_real_dynamic_rubric_permits_cited_candidates_with_uncertainty(self):
        data, _ = e.corpus()
        case = next(c for c in data["cases"] if c["id"] == "pulsar-channel-dispatch-abstention")
        files = {}
        identities = [i for i in case["truth"]["identities"] if i["required"]]
        for identity in identities:
            lines = files.setdefault(identity["file"], [])
            while len(lines) < identity["line"]:
                lines.append("")
            lines[identity["line"] - 1] = identity["quote"]
        trees = {case["head_snapshot"]: {p: "\n".join(lines) for p, lines in files.items()}}
        answer = {"items": [i["selector"] for i in identities], "abstain": False,
                  "reason": "The call uses an injected Channel. XChannel and FakeChannel are candidates; a unique implementation for every invocation cannot be guaranteed.",
                  "evidence": [{"item": i["selector"], **{k: i[k] for k in ("file", "line", "quote")}} for i in identities]}
        self.assertTrue(e.checked_answer(answer, case, trees)["objective_pass"])
        answer["abstain"] = True
        with self.assertRaises(e.Invalid):
            e.checked_answer(answer, case, trees)

    def test_mock_captures_cannot_be_labeled_real_and_unknown_nested_fields_refused(self):
        bundle, audits, dirs = self.cohort()
        bundle["study_kind"] = "agent"
        self.score_cli(bundle, audits, ok=False)
        with self.assertRaises(e.Invalid):
            e.adapt(dirs, "agent")
        raw = e.load(dirs[0] / "episode.json")
        raw["timing"]["unknown"] = 0
        e.seal(raw, "artifact_sha256")
        (dirs[0] / "episode.json").write_text(json.dumps(raw))
        with self.assertRaises(e.Invalid):
            e.adapt(dirs, "test-only")

    def test_signed_judgment_evidence_and_nullable_paired_usage(self):
        bundle, audits, dirs = self.cohort()
        report = self.score_cli(bundle, audits)
        self.assertIsNone(report["paired"][0]["usage_delta_graph_minus_baseline"]["input_tokens"])
        audits["audits"][0]["judgments"][0]["source_evidence"] = []
        e.seal(audits["audits"][0], "attestation_sha256")
        self.score_cli(bundle, audits, ok=False)
        audits["audits"][0]["reviewer"] = ""
        e.seal(audits["audits"][0], "attestation_sha256")
        self.score_cli(bundle, audits, ok=False)

    def test_actual_session_reuse_and_order_drift_refused(self):
        for mutation in (lambda r: r["provider"].update(thread_id="fixture-1"),
                         lambda r: r.update(started_at_unix=1790994005)):
            bundle, audits, dirs = self.cohort()
            self.update_raw(bundle, dirs, 0, mutation, audits)
            self.score_cli(bundle, audits, ok=False)
        bundle, audits, dirs = self.cohort()
        self.update_raw(bundle, dirs, 0, lambda r: r["isolation"]["inventory"].update(disabled_verified=[]), audits)
        report = self.score_cli(bundle, audits)
        self.assertFalse(report["episodes"][0]["correct"])

    def test_repository_override_does_not_discover_parent_checkout(self):
        repos = [arg for k in self.data["repositories"] for arg in ("--repo", f"{k}={self.repo / 'src'}")]
        self.cli("validate", *repos, ok=False)

    def test_help_and_usage_surface(self):
        env = {"PATH": "/usr/bin:/bin", "HOME": str(self.root)}
        help_text = e.capture([sys.executable, str(self.tool / "eval.py"), "--help"], env).decode()
        for command in ("validate", "export", "adapt", "score"):
            self.assertIn(command, help_text)
        self.cli("validate", "--unexpected", ok=False)


if __name__ == "__main__":
    unittest.main()
