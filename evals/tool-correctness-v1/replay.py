#!/usr/bin/env python3
"""Re-score immutable captured outputs; never execute a candidate or replace a capture."""
import argparse
import copy
import json
from pathlib import Path

import eval as evaluator
from scoring import aggregate, canonical, score


def payload(record, surface, tool, action=False):
    if surface == "mcp":
        frames = [json.loads(line) for line in record["stdout"].splitlines()]
        replies = [frame["result"] for frame in frames if frame.get("id") == 2]
        evaluator.require(len(replies) == 1, "missing/duplicate MCP result")
        reply = replies[0]
        value = reply.get("structuredContent")
        return value, (value or {}).get("code", "product_error") if reply.get("isError") else None
    if surface == "plugin-cli":
        return evaluator.plugin_cli_reply(record)
    value = json.loads(record["stdout"] if record["exit_code"] == 0 else record["stderr"])
    if record["exit_code"] != 0:
        return value, value.get("code", "product_error")
    if action:
        return value, None
    if tool in ("search", "show", "refs", "callees", "impact", "trace", "deps", "overview", "changes"):
        if tool == "callees" and isinstance(value, list):
            value = {"callees": value}
        value = {"result": value}
    elif tool == "status":
        value = {"status": value}
    return value, None


def replay(directory):
    directory = Path(directory).resolve(strict=True)
    original = json.loads((directory / "report.json").read_text())
    corpus = json.loads((directory / "corpus.json").read_text())
    evaluator.require(evaluator.sha(directory / "corpus.json") == original["identities"]["corpus_sha256"], "capture corpus drift")
    by_id = {case["id"]: case for case in corpus["cases"]}
    rows = []
    for measured in original["attempts"]:
        row = copy.deepcopy(measured)
        row["measurement_score"] = row["score"]
        if row.get("sample") is None:
            rows.append(row)
            continue
        case = copy.deepcopy(by_id[row["case_id"]])
        revisions = row["source_revisions"]
        substitutions = {"base": revisions[0], "head": revisions[-1], "target": revisions[2] if case["setup"] == "history" else revisions[-1]}
        def resolve(value):
            if isinstance(value, str) and value.startswith("$"):
                return substitutions.get(value[1:], value)
            if isinstance(value, dict):
                return {k: resolve(v) for k, v in value.items()}
            if isinstance(value, list):
                return [resolve(v) for v in value]
            return value
        case = resolve(case)
        if row["surface"] == "cli" and "cli_checks" in case:
            case["checks"] = case["cli_checks"]
        record = json.loads((directory / row["raw_artifact"]).read_text())
        try:
            value, error = payload(record, row["surface"], case["tool"], bool(case.get("action")))
            verdict = score(case, value, error, evaluator.transport_ok(record, bool(case.get("expected_error"))))
        except (ValueError, KeyError, TypeError) as error:
            verdict = {"passed": False, "findings": ["replay decode failed: " + str(error)], "metrics": [], "declared_coverage": case["coverage"]}
        # Capture-time state/parity checks are evidence; replay cannot silently drop them.
        for finding in row["measurement_score"]["findings"]:
            if finding.startswith(("same-input", "maintain index", "status freshness")):
                verdict["findings"].append(finding)
                verdict["passed"] = False
        row["score"] = verdict
        rows.append(row)
    per_tool = aggregate(rows, corpus["tools"])
    return {"schema_version": 1, "kind": "tool-correctness-capture-replay-v1", "identities": original["identities"],
            "scorer_sha256": evaluator.sha(Path(__file__).parent / "scoring.py"),
            "capture_report_sha256": evaluator.sha(directory / "report.json"),
            "qualified": original["qualified"] and all(tool["passed"] for tool in per_tool.values()),
            "original_qualified": original["qualified"], "per_tool": per_tool, "attempts": rows,
            "coverage": original["coverage"], "setup": original["setup"],
            "missing_case_surfaces": original["missing_case_surfaces"],
            "note": "Original capture/scores retained. A failed capture qualification cannot become passing through replay."}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("capture")
    parser.add_argument("--output", required=True)
    args = parser.parse_args()
    output = Path(args.output)
    evaluator.require(output.parent.resolve().is_relative_to((evaluator.ROOT / ".orbit/tmp").resolve()), "replay output must be owned scratch")
    result = replay(args.capture)
    evaluator.write_json(output, result)
    print(canonical({"qualified": result["qualified"], "output": str(output)}))


if __name__ == "__main__":
    main()
