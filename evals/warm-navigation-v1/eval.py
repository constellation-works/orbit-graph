#!/usr/bin/env python3
"""Freeze, replay and score the proposed warm-navigation-v1 profile, offline."""
import argparse
from collections import Counter
import importlib.util
import json
from pathlib import Path
import sys

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts/agent-eval"))
import eval_runner as runner
import warm_batch as warm
import plugin_profile as plugin

SPEC = importlib.util.spec_from_file_location("installed_navigation_replay", ROOT / "evals/plugin-agent-navigation/eval.py")
shared = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(shared)
check = runner.require


def export(spec, lock, output):
    """Seal prospective controls before execution; never derives truth from queries."""
    check("freeze_sha256" not in spec, "spec already sealed")
    rubric = runner.load_json(lock)
    check(len(rubric["expected_items"]) == len(spec["schedule"]), "rubric schedule differs")
    spec = dict(spec, lock_sha256=runner.sha256_file(lock),
                harness={name: runner.sha256_file(runner.HERE / name) for name in warm.HELPERS},
                study_layer={name: runner.sha256_file(warm.STUDY / name) for name in warm.STUDY_FILES})
    runner.seal(spec, "freeze_sha256")
    warm.validate_freeze(spec)
    warm.write(output, spec)
    return spec


def replay(directory, diagnostic=False):
    directory = Path(directory)
    check(not directory.is_symlink() and directory.resolve() == directory.absolute(), "unsafe batch directory")
    freeze = warm.validate_freeze(runner.load_json(directory / "freeze.json", 32 * 1024 * 1024))
    if not diagnostic:
        check(freeze["runtime"]["containment"] == "bwrap" and all(
            type(v) is int and v > 0 for v in freeze["runtime"]["resource_limits"].values()), "batch is diagnostic only")
    lock_path = directory / "lock.json"
    check(runner.sha256_file(lock_path) == freeze["lock_sha256"], "original rubric lock differs")
    batch = warm.sealed(runner.load_json(directory / "batch.json", 64 * 1024 * 1024), "batch_sha256")
    check(type(batch["schema_version"]) is int and batch["schema_version"] == 1 and batch["profile"] == warm.PROFILE,
          "unsupported batch schema")
    check(type(batch["batch_index"]) is int and 0 <= batch["batch_index"] < 4, "invalid batch index")
    arm = freeze["batch_order"][batch["batch_index"]]
    check(batch["freeze_sha256"] == freeze["freeze_sha256"] and batch["arm"] == arm
          and batch["state_policy"] == warm.POLICY, "batch freeze binding differs")
    check(batch["admission"] == {"freeze_sha256": freeze["freeze_sha256"], "batch_index": batch["batch_index"],
          "output": str(directory.absolute()), "start_ns": batch["start_ns"]}, "batch admission differs")
    check(type(batch["start_ns"]) is int and type(batch["end_ns"]) is int
          and 0 <= batch["start_ns"] <= batch["end_ns"], "invalid batch wall")
    check(batch["initial_state"] == {"orbit-home": {}, "repo/.orbit": None, "repo/.orbit-graph": None}, "batch did not start empty")
    initial = runner.load_json(directory / "schedule.json")
    check(initial["freeze_sha256"] == freeze["freeze_sha256"]
          and len(batch["slots"]) == len(initial["slots"]) == len(freeze["schedule"]), "dropped scheduled slot")
    probe = batch["identity_probe"]
    check(plugin.clean(probe) and json.loads(probe["stdout"]) == batch["product_identity"] == freeze["product_identity"], "product identity evidence differs")
    artifacts = []
    previous_end = batch["start_ns"]
    previous_state = batch["initial_state"]
    installation = None
    reuse_stopped = False
    for index, (slot, scheduled) in enumerate(zip(batch["slots"], freeze["schedule"])):
        request = scheduled["requests"][arm]
        check(slot["slot"] == index and slot["directory"] == f"slot-{index:03d}"
              and slot["request_digest"] == runner.digest(request), "slot identity differs")
        expected = dict(slot, status="not_run", error=None, start_ns=None, end_ns=None, artifact_sha256=None,
                        state_before=None, state_after=None, state_error=None)
        check(initial["slots"][index] == expected, "original schedule differs")
        if slot["status"] == "not_run":
            check(slot["start_ns"] is None and slot["end_ns"] is None and slot["artifact_sha256"] is None
                  and slot["error"]["code"] in {"batch_interrupted", "state_not_reusable"}, "unexplained unrun slot")
            check(reuse_stopped or batch["interrupted"], "selectively dropped slot")
            reuse_stopped = True
            artifacts.append(None)
            continue
        check(not reuse_stopped, "attempt after reuse stopped")
        check(slot["state_before"] == previous_state, "checkpoint state continuity differs")
        check(type(slot["start_ns"]) is int and type(slot["end_ns"]) is int
              and previous_end <= slot["start_ns"] <= slot["end_ns"] <= batch["end_ns"], "overlapping slot walls")
        previous_end = slot["end_ns"]
        check(runner.load_json(directory / f"checkpoint-{index:03d}.json") == slot, "original checkpoint differs")
        capture = directory / slot["directory"]
        check(not capture.is_symlink() and capture.resolve().is_relative_to(directory.resolve()), "escaped slot capture")
        if slot["status"] == "refused":
            check(slot["artifact_sha256"] is None and isinstance(slot["error"], dict), "refusal fabricated artifact")
            previous_state = slot["state_after"]
            reuse_stopped = True
            artifacts.append(None)
            continue
        artifact = shared.load_episode(capture, diagnostic)
        check(artifact["artifact_sha256"] == slot["artifact_sha256"] and artifact["request"] == request
              and artifact["status"] == slot["status"] and artifact["error"] == slot["error"], "slot capture differs")
        state_id = freeze["freeze_sha256"] + ":batch:" + str(batch["batch_index"])
        check(artifact["isolation"]["repository_id"] == state_id + ":repository"
              and artifact["isolation"]["cache_id"] == state_id + (":graph-state" if arm == "graph" else ":no-graph-state"), "cache identity differs")
        check(artifact["model"] == freeze["model"] and artifact["provider"]["binary_sha256"] == freeze["provider_binary_sha256"]
              and artifact["harness"] == freeze["harness"], "frozen runtime differs")
        evidence = artifact["warm"]
        check(evidence["freeze_sha256"] == freeze["freeze_sha256"] and evidence["slot"] == index
              and evidence["batch_index"] == batch["batch_index"] and evidence["state_policy"] == warm.POLICY,
              "warm state identity differs")
        check(evidence["before"] == previous_state == slot["state_before"]
              and evidence["after"] == slot["state_after"] and slot["state_error"] is None and evidence["cache"] == {
            "outcome": "unknown", "evidence": None,
            "reason": "product replies do not qualify a cache hit; repetition is insufficient"}, "warm state/cache continuity differs")
        previous_state = evidence["after"]
        check(evidence["installation_reused"] is (index > 0 and arm == "graph"), "installation reuse differs")
        if arm == "graph":
            treatment = {k: v for k, v in artifact["treatment"].items() if k not in {"setup", "install_ms", "setup_output_bytes", "binary_staging_bytes"}}
            if installation is None:
                installation = treatment
            check(treatment == installation, "reused shipped treatment differs")
        phases = runner.load_json(capture / "phase-timing.json")
        check(slot["start_ns"] <= phases["preflight"]["outside_wall"]["start_ns"]
              <= phases["wall_start_ns"] <= phases["provider_end_ns"] <= slot["end_ns"], "task clocks outside batch")
        reuse_stopped = artifact["cleanup"]["terminated_by"] is not None or bool(artifact["cleanup"]["survivors"])
        artifacts.append(artifact)
    check(previous_state == batch["final_state"], "final state differs")
    # Original per-field usage and cache semantics are retained verbatim.
    measured = [a["telemetry"]["tools"]["provider_received_output_bytes"] if a else None for a in artifacts]
    wall_ms = (batch["end_ns"] - batch["start_ns"]) / 1_000_000
    setup = sum(a["setup_output_bytes"] for a in artifacts if a)
    identity_bytes = len(probe["stdout"].encode()) + len(probe["stderr"].encode())
    return {"schema_version": 1, "profile": warm.PROFILE, "freeze_sha256": freeze["freeze_sha256"],
            "batch_sha256": batch["batch_sha256"], "batch_index": batch["batch_index"], "arm": arm,
            "outcomes": dict(Counter(s["status"] for s in batch["slots"])), "scheduled": len(artifacts),
            "batch_wall_ms": wall_ms, "amortized_wall_ms": wall_ms / len(artifacts),
            "tool_output_bytes": sum(measured) if all(v is not None for v in measured) else None,
            "observed_partial_tool_output_bytes": sum(v for v in measured if v is not None),
            "usage": [a["telemetry"]["usage"] if a else None for a in artifacts],
            "observed_tools": [a["telemetry"]["tools"] if a else None for a in artifacts],
            "task_overlapping_phases": [a["telemetry"]["timing"] if a else None for a in artifacts],
            "outside_batch_wall": batch["outside_wall"], "installation_output_bytes": setup if all(a is not None for a in artifacts) else None,
            "observed_partial_installation_output_bytes": setup, "identity_probe_output_bytes": identity_bytes,
            "batch_tool_output_bytes": None,
            "observed_partial_batch_tool_output_bytes": sum(v for v in measured if v is not None) + setup + identity_bytes,
            "batch_tool_output_coverage": {"state": "partial",
                "reason": "shared provider preflight and version-probe wire byte totals are unavailable"},
            "cost_usd": None, "effectiveness_evidence": False, "diagnostic": diagnostic,
            "answers": [a["answer"] if a and a["status"] == "ok" else None for a in artifacts]}


def score(directory, truth, diagnostic=False):
    result = replay(directory, diagnostic)
    check(set(truth) == {"freeze_sha256", "expected_items"} and truth["freeze_sha256"] == result["freeze_sha256"]
          and len(truth["expected_items"]) == result["scheduled"], "rubric/schedule binding differs")
    lock = runner.load_json(Path(directory) / "lock.json")
    check(truth["expected_items"] == lock["expected_items"], "rubric changed after output")
    outcomes = []
    for answer, expected in zip(result.pop("answers"), truth["expected_items"]):
        check(isinstance(expected, list) and expected and all(isinstance(i, str) for i in expected), "invalid rubric")
        correct = answer is not None and not answer["abstain"] and set(answer["items"]) == set(expected)
        if correct:
            index = len(outcomes)
            request = runner.load_json(Path(directory) / "freeze.json", 32 * 1024 * 1024)["schedule"][index]["requests"][result["arm"]]
            candidates = [Path(directory) / f"slot-{i:03d}/snapshot/previous-source"
                          for i in range(index + 1, result["scheduled"])] + [Path(directory) / "product/repo"]
            source = None
            for candidate in candidates:
                if candidate.is_dir() and not candidate.is_symlink():
                    entries = runner.tree_entries_with_git(candidate, runner.broker.HIDDEN_NAMES | {".orbit"})
                    if runner.content_revision(entries, "scoring-source")["content_revision"] == request["source_revision"]:
                        source = candidate
                        break
            check(source is not None, "original pinned source view missing for citation scoring")
            supported = set()
            for citation in answer["evidence"]:
                path = source / citation["file"]
                if citation["item"] not in expected or not plugin.source_path_allowed(citation["file"]):
                    correct = False
                    break
                check(not path.is_symlink() and path.resolve().is_relative_to(source.resolve()), "escaped citation")
                lines = path.read_text().splitlines() if path.is_file() else []
                if not citation["quote"].strip() or citation["line"] > len(lines) or citation["quote"] not in lines[citation["line"] - 1]:
                    correct = False
                    break
                supported.add(citation["item"])
            correct = correct and supported == set(expected)
        outcomes.append(correct)
    result.update(correct=outcomes, all_attempt_correct=sum(outcomes),
                  all_attempt_accuracy=sum(outcomes) / result["scheduled"])
    return result


def cohort(directories, diagnostic=False):
    check(len(directories) == 4, "need all four counterbalanced batches")
    results = [replay(d, diagnostic) for d in directories]
    check([r["batch_index"] for r in results] == [0, 1, 2, 3]
          and len({r["freeze_sha256"] for r in results}) == 1, "cohort freeze/order differs")
    raw = [runner.load_json(Path(d) / "batch.json", 64 * 1024 * 1024) for d in directories]
    check(all(a["end_ns"] <= b["start_ns"] for a, b in zip(raw, raw[1:])), "batch walls overlap or order differs")
    return {"profile": warm.PROFILE, "batches": results, "effectiveness_evidence": False}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("export", "replay", "score", "cohort"))
    parser.add_argument("--spec")
    parser.add_argument("--lock")
    parser.add_argument("--output")
    parser.add_argument("--batch", action="append", default=[])
    parser.add_argument("--truth")
    parser.add_argument("--diagnostic", action="store_true")
    args = parser.parse_args()
    try:
        if args.command == "export":
            result = export(runner.load_json(args.spec, 32 * 1024 * 1024), args.lock, args.output)
        elif args.command == "cohort":
            result = cohort(args.batch, args.diagnostic)
        else:
            check(len(args.batch) == 1, "need one batch")
            result = (score(args.batch[0], runner.load_json(args.truth), args.diagnostic) if args.command == "score"
                      else replay(args.batch[0], args.diagnostic))
    except (ValueError, OSError, KeyError, TypeError) as error:
        print(json.dumps({"error": str(error)}), file=sys.stderr)
        return 1
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
