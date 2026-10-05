#!/usr/bin/env python3
"""Build a diagnostic dashboard while retaining the entire failed attempt series."""
import argparse
import json
from pathlib import Path

import eval as evaluator
from scoring import aggregate


def dashboard(series, current_path):
    series = Path(series)
    current_path = Path(current_path)
    current = json.loads(current_path.read_text())
    rows = [{**row, "capture_id": current_path.parent.name} for row in current["attempts"]]
    corpus = evaluator.frozen()
    evaluator.require(current["identities"]["corpus_sha256"] == evaluator.sha(evaluator.HERE / "corpus.json"),
                      "current report has a different frozen corpus")
    evaluator.require(current["identities"]["harness_files"]["evals/tool-correctness-v1/scoring.py"] == evaluator.sha(evaluator.HERE / "scoring.py"),
                      "current report was scored with different bytes")
    expected = {(case["id"], surface) for case in corpus["cases"] for surface in case["surfaces"]}
    actual = {(row["case_id"], row["surface"]) for row in rows}
    evaluator.require(actual == expected, "dashboard coverage differs from current protocol")
    pins = ("source_commit", "plugin_commit", "candidate_sha256", "orbit_sha256", "plugin_manifest_sha256")
    attempts = []
    for folder in sorted(series.glob("attempt-*")):
        report = folder / "report.json"
        if not report.exists():
            continue
        data = json.loads(report.read_text())
        evaluator.require(all(current["identities"][key] == data["identities"][key] for key in pins),
                          "candidate/source/runtime drift across attempt series")
        attempts.append({"id": folder.name, "report_sha256": evaluator.sha(report),
                         "corpus_sha256": data["identities"]["corpus_sha256"], "qualified": data["qualified"],
                         "recorded_attempts": len(data["attempts"]), "fatal_error": data["fatal_error"]})
    tools = aggregate(rows, corpus["tools"])
    for tool in tools.values():
        tool["cold_samples"] = sum(item["sample"] == 0 for item in tool["cost_samples"])
        tool["warm_samples"] = sum(item["sample"] in (1, 2, 3) for item in tool["cost_samples"])
        tool["setup_failures"] = sum(item["sample"] is None for item in tool["cost_samples"])
    qualified = current["qualified"] and all(attempt["qualified"] for attempt in attempts)
    return {"schema_version": 1, "kind": "tool-correctness-diagnostic-dashboard-v1", "qualified": qualified,
            "current_qualified": current["qualified"],
            "reason": "Every failed attempt is retained; qualification requires all attempted invariants to pass.",
            "identities": {key: current["identities"][key] for key in pins},
            "current_corpus_sha256": evaluator.sha(evaluator.HERE / "corpus.json"),
            "current_manifest_sha256": evaluator.sha(evaluator.HERE / "manifest.json"),
            "scorer_sha256": evaluator.sha(evaluator.HERE / "scoring.py"),
            "semantic_scenarios": len(corpus["cases"]), "per_tool": tools,
            "coverage": [{"case_id": case["id"], **case["coverage"]} for case in corpus["cases"]],
            "attempt_series": attempts, "current_diagnostic_sources": [str(current_path)],
            "protocol_errata": "capture-review.md; all original measurements and scores retained",
            "failures": [{"case_id": row["case_id"], "surface": row["surface"], "sample": row["sample"],
                          "capture_id": row["capture_id"], "findings": row["score"]["findings"]}
                         for row in rows if not row["score"]["passed"]],
            "setup_process_costs": {"capture_id": current_path.parent.name, "bytes": current["setup_output_bytes"],
                                    "latency_ms": current["setup_latency_ms"]},
            "effectiveness_or_generalization_claim": False, "diagnostic_only": True,
            "outer_runtime_limits": current["identities"]["runtime_limits"],
            "independent_root_review": {"source_truth": "operator/source-truth-20261005T0057; findings reconciled in capture-review.md",
                                        "corrected_final_approval": "not attested by executor"}}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("series")
    parser.add_argument("--current", required=True)
    parser.add_argument("--output", required=True)
    args = parser.parse_args()
    evaluator.write_json(args.output, dashboard(args.series, args.current))


if __name__ == "__main__":
    main()
