#!/usr/bin/env python3
"""Offline paired navigation study. No providers, product tools or Git are spawned."""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import sys
import unittest

sys.dont_write_bytecode = True
from truth import expected, functions, selector

ROOT = Path(__file__).resolve().parent
MAX_FILE_BYTES = 2 * 1024 * 1024
KINDS = {"discovery", "callers", "callees", "impact_tests", "change", "unsupported"}
ARMS = ("baseline", "graph")
COMMON_TOOLS = ["read", "rg", "git"]
GRAPH_TOOLS = ["graph_sync", "search", "show", "refs", "callees", "impact", "changes"]
LIMITS = {"wall_ms": 120000, "tool_calls": 40, "output_bytes": 131072,
          "answer_items": 32, "call_bytes": 16384}
CONTRACT = (
    '\nReturn only a JSON object with exactly items (unique symbol:path#name:function '
    'strings, or :test for Rust #[test] functions), abstain (boolean), reason (string), evidence (array of objects with '
    'item, file, line, quote). Cite the exact definition line from HEAD for every '
    'item. If evidence cannot establish the complete answer, abstain with empty '
    'items/evidence and a specific reason. Do not treat name similarity as a call. '
    'Use only the supplied repository; do not modify it or invoke other agents.'
)


class Invalid(ValueError):
    pass


def require(condition, message):
    if not condition:
        raise Invalid(message)


def shape(value, fields, label):
    require(isinstance(value, dict) and set(value) == set(fields),
            f"{label}: expected exactly fields {sorted(fields)}")


def integer(value, low, high, label):
    require(type(value) is int and low <= value <= high, f"{label}: invalid integer")


def string(value, label, maximum=4096):
    require(isinstance(value, str) and 0 < len(value.encode()) <= maximum,
            f"{label}: missing or oversized string")


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False).encode()


def digest(value):
    return hashlib.sha256(canonical(value)).hexdigest()


def unique_pairs(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, f"duplicate JSON key {key}")
        result[key] = value
    return result


def load(path):
    with Path(path).open("rb") as stream:
        data = stream.read(MAX_FILE_BYTES + 1)
    require(len(data) <= MAX_FILE_BYTES, f"{path}: exceeds {MAX_FILE_BYTES} bytes")
    return json.loads(data, object_pairs_hook=unique_pairs,
                      parse_constant=lambda value: (_ for _ in ()).throw(Invalid(f"invalid number {value}")))


def source_files(root, mapping):
    require(isinstance(mapping, dict) and mapping, "empty fixture file map")
    result = {}
    for dest, origin in mapping.items():
        for path in (dest, origin):
            require(isinstance(path, str) and not Path(path).is_absolute()
                    and ".." not in Path(path).parts, f"unsafe fixture path {path}")
        physical = (root / origin).resolve(strict=True)
        require(physical.is_relative_to(root.resolve()), f"fixture escapes root: {origin}")
        require(not (root / origin).is_symlink(), f"symlink fixture {origin}")
        require(physical.stat().st_size <= 65536, f"oversized fixture {origin}")
        result[dest] = physical.read_text()
    return result


def corpus(root=ROOT):
    data = load(root / "corpus.json")
    shape(data, ["schema_version", "label", "frozen_at", "limits", "fixtures", "cases"], "corpus")
    require(type(data["schema_version"]) is int and data["schema_version"] == 1, "corpus schema")
    require(data["label"] == "synthetic fixtures", "fixture label")
    require(canonical(data["limits"]) == canonical(LIMITS), "limits drift")
    require(isinstance(data["fixtures"], list) and len(data["fixtures"]) >= 2, "need two fixtures")
    require(isinstance(data["cases"], list) and 8 <= len(data["cases"]) <= 32, "need 8..32 cases")
    trees, ids, languages = {}, set(), set()
    for fixture in data["fixtures"]:
        shape(fixture, ["id", "language", "head", "base", "head_revision", "base_revision"], "fixture")
        string(fixture["id"], "fixture id")
        require(fixture["id"] not in trees, "duplicate fixture id")
        require(fixture["language"] in {"rust", "python"}, "unknown fixture language")
        head = source_files(root, fixture["head"])
        base = source_files(root, fixture["base"])
        require(digest(head) == fixture["head_revision"] and digest(base) == fixture["base_revision"], "source revision drift")
        trees[fixture["id"]] = (fixture, head, base)
        languages.add(fixture["language"])
    require(len(languages) >= 2, "need two language families")
    for case in data["cases"]:
        shape(case, ["id", "fixture", "split", "kind", "target", "prompt", "truth"], "case")
        string(case["id"], "case id")
        require(case["id"] not in ids, "duplicate case id")
        ids.add(case["id"])
        require(case["split"] in {"development", "held-out"}, "unknown split")
        require(case["kind"] in KINDS and case["fixture"] in trees, "invalid case kind/fixture")
        string(case["prompt"], "case prompt")
        shape(case["truth"], ["items", "abstain"], "truth")
        fixture, head, base = trees[case["fixture"]]
        items, abstain = expected(case, fixture["language"], head, base)
        require(case["truth"] == {"items": items, "abstain": abstain}, f"invalid independently checked truth: {case['id']}")
    for split in ("development", "held-out"):
        subset = [case for case in data["cases"] if case["split"] == split]
        require(subset and {case["fixture"] for case in subset} == set(trees), f"incomplete split {split}")
    require({case["kind"] for case in data["cases"]} == KINDS, "missing task category")
    lock = load(root / "corpus.lock.json")
    shape(lock, ["schema_version", "corpus_sha256", "truth_sha256", "split_sha256"], "lock")
    require(type(lock["schema_version"]) is int and lock["schema_version"] == 1 and lock["corpus_sha256"] == digest(data), "frozen corpus drift")
    require(lock["truth_sha256"] == digest({c["id"]: c["truth"] for c in data["cases"]}), "frozen truth drift")
    require(lock["split_sha256"] == digest({c["id"]: c["split"] for c in data["cases"]}), "frozen split drift")
    return data, trees


def requests(data):
    fixtures = {f["id"]: f for f in data["fixtures"]}
    result = []
    for index, case in enumerate(data["cases"]):
        fixture = fixtures[case["fixture"]]
        for arm in (ARMS if index % 2 == 0 else tuple(reversed(ARMS))):
            request = {"schema_version": 1, "case_id": case["id"], "arm": arm,
                       "split": case["split"], "fixture": case["fixture"],
                       "source_revision": fixture["head_revision"],
                       "base_revision": fixture["base_revision"], "corpus_sha256": digest(data),
                       "prompt": case["prompt"] + CONTRACT, "limits": data["limits"],
                       "tools": COMMON_TOOLS + (GRAPH_TOOLS if arm == "graph" else []),
                       "cache_policy": "cold-per-episode-including-graph-setup",
                       "order": len(result)}
            request["request_sha256"] = digest(request)
            result.append(request)
    return result


def answer_check(answer, case, head, language):
    shape(answer, ["items", "abstain", "reason", "evidence"], "answer")
    require(type(answer["abstain"]) is bool and isinstance(answer["reason"], str)
            and len(answer["reason"].encode()) <= 4096, "invalid abstention/reason")
    items, evidence = answer["items"], answer["evidence"]
    require(isinstance(items, list) and len(items) <= LIMITS["answer_items"]
            and all(isinstance(item, str) for item in items) and len(set(items)) == len(items), "invalid/duplicate answer items")
    require(isinstance(evidence, list) and len(evidence) <= LIMITS["answer_items"], "invalid evidence list")
    if answer["abstain"]:
        require(not items and not evidence and answer["reason"].strip(), "abstention must be justified and empty")
    else:
        require(items and evidence, "absent answer evidence; use explicit abstention")
    definitions = {selector(definition, name): definition for name, definition in functions(language, head).items()}
    cited = set()
    for citation in evidence:
        shape(citation, ["item", "file", "line", "quote"], "citation")
        item = citation["item"]
        require(isinstance(item, str) and item in items and item in definitions and item not in cited, "invalid citation item")
        definition = definitions[item]
        require(citation["file"] == definition["path"], "citation file mismatch")
        integer(citation["line"], 1, 10000, "citation line")
        require(citation["line"] == definition["line"], "citation is not definition line")
        require(citation["quote"] == head[definition["path"]].splitlines()[definition["line"] - 1], "citation quote mismatch")
        cited.add(item)
    require(cited == set(items), "every item requires source evidence")
    truth = set(case["truth"]["items"])
    return {"correct": set(items) == truth and answer["abstain"] == case["truth"]["abstain"],
            "false_positives": sorted(set(items) - truth), "missed": sorted(truth - set(items)),
            "abstained": answer["abstain"],
            "correct_abstention": answer["abstain"] and case["truth"]["abstain"]}


def score(data, trees, raw):
    shape(raw, ["schema_version", "study_kind", "episodes"], "episode bundle")
    require(type(raw["schema_version"]) is int and raw["schema_version"] == 1, "episode schema")
    require(raw["study_kind"] in {"scripted-smoke", "agent"}, "unknown study kind")
    require(isinstance(raw["episodes"], list), "episodes must be a list")
    plan = requests(data)
    require(len(raw["episodes"]) == len(plan), "missing/extra episodes: require every paired case, including failures")
    cases = {c["id"]: c for c in data["cases"]}
    rows, run_ids, roots, models, common_versions = [], set(), set(), {}, {}
    cohort_model = None
    graph_version = None
    for request, episode in zip(plan, raw["episodes"]):
        shape(episode, ["request", "run_id", "model", "tool_versions", "isolation", "status", "error",
                        "answer", "final_output", "calls", "wall_ms", "output_bytes", "usage", "record_sha256"], "episode")
        require(canonical(episode["request"]) == canonical(request), "request mismatch/order drift")
        require(episode["record_sha256"] == digest({k: v for k, v in episode.items() if k != "record_sha256"}), "raw episode hash mismatch")
        string(episode["run_id"], "run id")
        require(episode["run_id"] not in run_ids, "duplicate run id")
        run_ids.add(episode["run_id"])
        shape(episode["model"], ["provider", "name", "version", "settings"], "model")
        for key in ("provider", "name", "version"):
            string(episode["model"][key], f"model {key}")
        require(isinstance(episode["model"]["settings"], dict), "model settings required")
        if raw["study_kind"] == "agent":
            require(episode["model"]["provider"] != "scripted", "scripted model cannot be agent evidence")
        if cohort_model is None:
            cohort_model = episode["model"]
        require(canonical(cohort_model) == canonical(episode["model"]), "cohort model/settings drift")
        models.setdefault(request["case_id"], episode["model"])
        require(canonical(models[request["case_id"]]) == canonical(episode["model"]), "paired model/settings mismatch")
        versions = episode["tool_versions"]
        required = COMMON_TOOLS + (["orbit-graph"] if request["arm"] == "graph" else [])
        shape(versions, required, "tool versions")
        for name, version in versions.items():
            string(version, f"{name} version")
        for name in COMMON_TOOLS:
            common_versions.setdefault(name, versions[name])
            require(common_versions[name] == versions[name], f"cohort {name} version drift")
        if request["arm"] == "graph":
            if graph_version is None:
                graph_version = versions["orbit-graph"]
            require(graph_version == versions["orbit-graph"], "graph binary version drift")
        shape(episode["isolation"], ["repository_id", "cache_id", "truth_inaccessible", "cold_start"], "isolation")
        isolation = episode["isolation"]
        require(isolation["truth_inaccessible"] is True and isolation["cold_start"] is True, "missing isolation attestation")
        for key in ("repository_id", "cache_id"):
            string(isolation[key], key)
            require(isolation[key] not in roots, "shared repository/cache between episodes")
            roots.add(isolation[key])
        require(episode["status"] in {"ok", "failed", "timeout", "invalid"}, "unknown episode status")
        integer(episode["wall_ms"], 0, LIMITS["wall_ms"] + 10000, "wall_ms")
        require(episode["status"] == "timeout" or episode["wall_ms"] <= LIMITS["wall_ms"], "wall time over budget")
        calls = episode["calls"]
        require(isinstance(calls, list) and len(calls) <= LIMITS["tool_calls"], "invalid tool call count")
        call_time = 0
        for call in calls:
            shape(call, ["tool", "input", "output", "elapsed_ms", "status"], "call")
            require(call["tool"] in request["tools"], "tool outside arm contract")
            require(isinstance(call["input"], str) and isinstance(call["output"], str), "call I/O must be captured text")
            require(len(call["input"].encode()) <= LIMITS["call_bytes"] and len(call["output"].encode()) <= LIMITS["call_bytes"], "call capture exceeds bound")
            integer(call["elapsed_ms"], 0, LIMITS["wall_ms"] + 10000, "call elapsed_ms")
            call_time += call["elapsed_ms"]
            require(call["status"] in {"ok", "failed", "timeout", "truncated"}, "invalid call status")
        require(call_time <= episode["wall_ms"], "sequential calls exceed wall time")
        if episode["status"] == "ok":
            require(calls and all(call["status"] != "truncated" for call in calls), "ok episode needs nontruncated captured calls")
        output_bytes = sum(len(call["output"].encode()) for call in calls)
        require(isinstance(episode["final_output"], str) and len(episode["final_output"].encode()) <= LIMITS["call_bytes"], "invalid final output capture")
        output_bytes += len(episode["final_output"].encode())
        integer(episode["output_bytes"], 0, LIMITS["output_bytes"], "output_bytes")
        require(episode["output_bytes"] == output_bytes, "output byte accounting mismatch")
        usage = episode["usage"]
        if usage is not None:
            shape(usage, ["input_tokens", "output_tokens", "cost_usd", "source"], "usage")
            string(usage["source"], "usage provenance")
            for field in ("input_tokens", "output_tokens"):
                if usage[field] is not None:
                    integer(usage[field], 0, 10000000, field)
            cost = usage["cost_usd"]
            require(cost is None or (type(cost) in (int, float) and math.isfinite(cost) and 0 <= cost <= 10000), "invalid usage cost")
            require(any(usage[k] is not None for k in ("input_tokens", "output_tokens", "cost_usd")), "empty usage must be null")
        case = cases[request["case_id"]]
        fixture, head, _ = trees[case["fixture"]]
        verdict = {"correct": False, "false_positives": [], "missed": case["truth"]["items"],
                   "abstained": False, "correct_abstention": False}
        answer_error = None
        if episode["status"] == "ok":
            require(episode["error"] is None, "ok episode carries error")
            parsed = json.loads(episode["final_output"], object_pairs_hook=unique_pairs)
            require(canonical(parsed) == canonical(episode["answer"]), "answer differs from captured final output")
            # Capture integrity remains cohort-fatal; answer validity is an episode verdict.
            try:
                verdict = answer_check(episode["answer"], case, head, fixture["language"])
            except Invalid as error:
                answer_error = str(error)
        else:
            shape(episode["error"], ["code", "message"], "failure")
            string(episode["error"]["code"], "error code")
            string(episode["error"]["message"], "error message")
            require(episode["answer"] is None, "failed episode must not supply scored answer")
        rows.append({"case_id": case["id"], "split": case["split"], "arm": request["arm"],
                     "status": episode["status"], "error": episode["error"], "answer_error": answer_error, **verdict,
                     "wall_ms": episode["wall_ms"], "tool_calls": len(calls),
                     "failed_calls": sum(call["status"] != "ok" for call in calls),
                     "output_bytes": output_bytes, "usage": usage})
    aggregates = {}
    for split in ("development", "held-out"):
        aggregates[split] = {}
        for arm in ARMS:
            selected = [r for r in rows if r["arm"] == arm and r["split"] == split]
            aggregates[split][arm] = {"episodes": len(selected), "correct": sum(r["correct"] for r in selected),
                "false_positives": sum(len(r["false_positives"]) for r in selected),
                "abstentions": sum(r["abstained"] for r in selected),
                "correct_abstentions": sum(r["correct_abstention"] for r in selected),
                "failures": sum(r["status"] != "ok" or r["answer_error"] is not None for r in selected),
                "wall_ms_total": sum(r["wall_ms"] for r in selected),
                "tool_calls_total": sum(r["tool_calls"] for r in selected),
                "output_bytes_total": sum(r["output_bytes"] for r in selected),
                "usage_observed": sum(r["usage"] is not None for r in selected)}
    pairs = []
    for case in data["cases"]:
        paired = {r["arm"]: r for r in rows if r["case_id"] == case["id"]}
        pairs.append({"case_id": case["id"], "split": case["split"],
                      "correct_delta_graph_minus_baseline": int(paired["graph"]["correct"]) - int(paired["baseline"]["correct"]),
                      "wall_ms_delta_graph_minus_baseline": paired["graph"]["wall_ms"] - paired["baseline"]["wall_ms"],
                      "calls_delta_graph_minus_baseline": paired["graph"]["tool_calls"] - paired["baseline"]["tool_calls"],
                      "bytes_delta_graph_minus_baseline": paired["graph"]["output_bytes"] - paired["baseline"]["output_bytes"]})
    return {"schema_version": 1, "study_kind": raw["study_kind"],
            "agent_effectiveness_evidence": raw["study_kind"] == "agent",
            "corpus_sha256": digest(data), "episodes_sha256": digest(raw),
            "cases": rows, "pairs": pairs, "aggregate": aggregates,
            "limitations": ["Fixture-only; no production generalization or statistical power.",
                             "Operator telemetry and isolation attestations require independent audit.",
                             "Usage absent is null; no imputed tokens or costs."]}


def seal(episode):
    episode.pop("record_sha256", None)
    episode["record_sha256"] = digest(episode)
    return episode


def smoke(data, trees):
    cases = {c["id"]: c for c in data["cases"]}
    episodes = []
    for request in requests(data):
        case = cases[request["case_id"]]
        fixture, head, _ = trees[case["fixture"]]
        defs = {selector(d, name): d for name, d in functions(fixture["language"], head).items()}
        answer = {"items": list(case["truth"]["items"]), "abstain": case["truth"]["abstain"], "reason": "Unsupported runtime identity is absent." if case["truth"]["abstain"] else "scripted oracle answer",
                  "evidence": [{"item": item, "file": defs[item]["path"], "line": defs[item]["line"],
                                "quote": head[defs[item]["path"]].splitlines()[defs[item]["line"] - 1]}
                               for item in case["truth"]["items"]]}
        # Invented deterministic numbers are permissible only in this labelled smoke.
        calls = [{"tool": "read", "input": "scripted fixture read", "output": "scripted source capture", "elapsed_ms": 1, "status": "ok"}]
        status, error = "ok", None
        if request["order"] == 3:
            status, error, answer = "failed", {"code": "scripted_failure", "message": "Intentional smoke failure"}, None
        if request["order"] == 9:
            status, error, answer = "timeout", {"code": "scripted_timeout", "message": "Intentional smoke timeout"}, None
        if request["order"] == 15:
            status, error, answer = "invalid", {"code": "scripted_invalid", "message": "Intentional invalid agent output"}, None
        if request["order"] == 0:
            answer = {"items": [], "abstain": True, "reason": "Intentional smoke incorrect abstention", "evidence": []}
        if request["order"] == 2:
            item = next(item for item in defs if "#price_preview:" in item)
            definition = defs[item]
            answer["items"].append(item)
            answer["evidence"].append({"item": item, "file": definition["path"], "line": definition["line"], "quote": head[definition["path"]].splitlines()[definition["line"] - 1]})
        final_output = canonical(answer).decode() if answer is not None else ("not JSON" if status == "invalid" else "")
        episode = {"request": request, "run_id": f"scripted-{request['order']:02}",
                   "model": {"provider": "scripted", "name": "oracle-smoke", "version": "1", "settings": {}},
                   "tool_versions": {name: "scripted-not-executed" for name in COMMON_TOOLS + (["orbit-graph"] if request["arm"] == "graph" else [])},
                   "isolation": {"repository_id": f"scripted-repo-{request['order']}", "cache_id": f"scripted-cache-{request['order']}",
                                 "truth_inaccessible": True, "cold_start": True},
                   "status": status, "error": error, "answer": answer, "final_output": final_output, "calls": calls,
                   "wall_ms": LIMITS["wall_ms"] if status == "timeout" else 2,
                   "output_bytes": sum(len(c["output"].encode()) for c in calls) + len(final_output.encode()), "usage": None}
        episodes.append(seal(episode))
    return {"schema_version": 1, "study_kind": "scripted-smoke", "episodes": episodes}


def write_new(path, value):
    # Output roots are caller-selected, physically resolved, and never overwritten.
    require(not path.is_symlink(), f"symlink output {path}")
    path = path.resolve()
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "w") as stream:
        stream.write(value)


def export(data, trees, destination, crew):
    require(not destination.is_symlink(), f"symlink export target {destination}")
    destination = destination.resolve()
    require(not destination.exists(), f"export target already exists: {destination}")
    destination.mkdir(parents=True, mode=0o700)
    plan = requests(data)
    invocations = []
    for request in plan:
        episode_root = destination / f"{request['order']:02}-{request['case_id']}-{request['arm']}"
        episode_root.mkdir(mode=0o700)
        _, head, base = trees[request["fixture"]]
        for prefix, files in (("repository", head), ("before", base)):
            for relative, source in files.items():
                write_new(episode_root / prefix / relative, source)
        write_new(episode_root / "prompt.txt", request["prompt"])
        write_new(episode_root / "request.json", json.dumps(request, indent=2) + "\n")
        invocations.append({"request": request, "invoke_input": {
            "prompt": request["prompt"], "cwd": str(episode_root / "repository"),
            "crew": crew, "timeout_seconds": LIMITS["wall_ms"] // 1000,
            "provider_sandbox": "read-only",
            "idempotency_key": f"navigation-v1-{destination.name}-{request['request_sha256']}"}})
    write_new(destination / "plan.json", json.dumps({"schema_version": 1, "episodes": invocations}, indent=2) + "\n")
    return {"exported_to": str(destination), "episode_count": len(plan), "corpus_sha256": digest(data)}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("check", help="validate corpus, negative tests and smoke drift; tests use ephemeral scratch")
    sub.add_parser("validate", help="validate frozen corpus and independent truth (read-only)")
    export_parser = sub.add_parser("export", help="create solution-free paired source bundles at a new path")
    export_parser.add_argument("--output", type=Path, required=True)
    export_parser.add_argument("--crew", required=True, help="operator-approved fixed provider/model crew")
    score_parser = sub.add_parser("score", help="score a complete bounded episode JSON file; JSON to stdout")
    score_parser.add_argument("--input", type=Path, required=True)
    sub.add_parser("smoke", help="emit synthetic/scripted episodes to stdout; never effectiveness evidence")
    args = parser.parse_args(argv)
    try:
        data, trees = corpus()
        if args.command == "validate":
            result = {"valid": True, "case_count": len(data["cases"]), "corpus_sha256": digest(data), "episode_count": len(requests(data))}
        elif args.command == "export":
            string(args.crew, "crew")
            result = export(data, trees, args.output, args.crew)
        elif args.command == "score":
            result = score(data, trees, load(args.input))
        elif args.command == "smoke":
            result = smoke(data, trees)
        elif args.command == "check":
            raw = smoke(data, trees)
            require(raw == load(ROOT / "smoke-episodes.json"), "scripted episode drift; regenerate and review")
            require(score(data, trees, raw) == load(ROOT / "smoke-result.json"), "scripted result drift; regenerate and review")
            suite = unittest.defaultTestLoader.discover(str(ROOT), pattern="test_eval.py")
            require(suite.countTestCases() > 0, "empty negative-test gate")
            tests = unittest.TextTestRunner(stream=sys.stderr, verbosity=1).run(suite)
            require(tests.wasSuccessful(), "negative-test gate failed")
            result = {"valid": True, "tests": tests.testsRun, "cases": len(data["cases"]), "scripted_only": True}
        print(json.dumps(result, indent=2, allow_nan=False))
        return 0
    except (Invalid, ValueError, OSError, KeyError, TypeError, RecursionError) as error:
        print(json.dumps({"error": {"code": "invalid_evaluation", "message": str(error)}}), file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
