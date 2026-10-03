#!/usr/bin/env python3
"""Offline replay of installed-plugin profile 2. Never calls a provider or changes a corpus."""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import re
import signal
import sys

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts/agent-eval"))
import eval_runner as runner
import plugin_profile as plugin


def require(value, message):
    if not value:
        raise ValueError(message)


def sha(text):
    return hashlib.sha256(text.encode()).hexdigest()


def check_treatment(artifact):
    treatment = artifact["treatment"]
    request = artifact["request"]
    if request["arm"] == "baseline":
        require(treatment is None, "baseline received graph treatment")
        require(artifact["timing"]["plugin_install_ms"] == 0 and artifact["setup_output_bytes"] == 0,
                "baseline has installation costs")
        return
    require(isinstance(treatment, dict), "graph treatment missing")
    pin = request["plugin"]
    require(treatment["pin"] == pin and treatment["profile"] == plugin.PROFILE,
            "treatment pin/profile differs")
    require(treatment["policy"] == plugin.POLICY, "treatment authority policy differs")
    inventory = plugin.plugin_inventory({"tools": treatment["inventory"]})
    require(plugin.digest(inventory) == pin["inventory_sha256"] == treatment["inventory_sha256"],
            "inventory schema/description hash differs")
    original = treatment["manifest_original"]
    require(sha(original) == treatment["source_files"][".orbit-plugin/plugin.yaml"],
            "original manifest hash differs")
    bound = original.replace("  origin: orbit\n", "")
    bound, count = re.subn(r"(?m)^    args: \[--allow-unbound-backend\]$",
                           "    args: [--backend-sha256, " + pin["backend_sha256"] + "]", bound)
    require(count == 1 and treatment["manifest_installed"] == bound,
            "manifest edits exceed declared namespace/backend binding")
    skill_files = treatment["skill_files"]
    expected_files = {name.removeprefix(".orbit-plugin/skills/orbit-graph/"): value
                      for name, value in treatment["source_files"].items()
                      if name.startswith(".orbit-plugin/skills/orbit-graph/")}
    require({name: sha(text) for name, text in skill_files.items()} == expected_files,
            "shipped skill/reference bytes changed or missing")
    require(sha(skill_files["SKILL.md"]) == pin["skill_sha256"], "pinned skill differs")
    context = treatment["binding_context"] + "".join(
        "\n--- shipped file: " + name + " ---\n" + skill_files[name] for name in sorted(skill_files))
    require(context == treatment["developer_context"] and sha(context) == treatment["developer_context_sha256"],
            "treatment context hash differs")
    require("developer_instructions=" + json.dumps(context) in artifact["provider"]["argv"],
            "provider did not receive the declared skill context")
    records = treatment["setup"]
    require([r["role"] for r in records] == ["verify-plugin-commit", "export-plugin", "workspace-init", "plugin-add",
                                            "plugin-enable", "tools-list", "maintenance-authority-negative"],
            "setup provenance sequence differs")
    for record in records:
        require(plugin.clean(record), "setup process failed or evidence truncated")
    listed = [json.loads(line) for line in records[-2]["stdout"].splitlines()]
    require(plugin.plugin_inventory(listed[-1]["result"]) == inventory,
            "served inventory differs from installed host discovery")
    denied = json.loads(records[-1]["stdout"].splitlines()[-1])["result"]
    require(denied["isError"] is True and denied["structuredContent"]["code"] == "capability_denied",
            "actual host did not enforce ordinary maintenance refusal")
    require(treatment["install_ms"] == artifact["timing"]["plugin_install_ms"]
            and 0 <= treatment["install_ms"] <= request["setup_limits"]["wall_ms"], "setup clock differs")
    measured = sum(len(r["stdout"].encode()) + len(r["stderr"].encode()) for r in records)
    require(measured == treatment["setup_output_bytes"] == artifact["setup_output_bytes"]
            and measured <= request["setup_limits"]["output_bytes"], "setup output budget differs")
    require(treatment["index_setup"] == "agent-choice-cold", "index was prewarmed")


def load_episode(directory, diagnostic=False):
    directory = Path(directory)
    artifact = runner.load_json(directory / "episode.json", 64 * 1024 * 1024)
    require(artifact.get("schema_version") == 2 and artifact.get("kind") == plugin.RAW_KIND
            and artifact.get("profile") == plugin.PROFILE, "not an installed-plugin schema-2 capture")
    require((artifact.get("runner_version") == "2" and "lifecycle_contract" not in artifact)
            or (artifact.get("runner_version") in runner.LIFECYCLE_CONTRACTS
                and artifact.get("lifecycle_contract") ==
                runner.LIFECYCLE_CONTRACTS[artifact["runner_version"]]),
            "unsupported runner/lifecycle contract")
    require(artifact.get("artifact_sha256") == runner.digest(
        {k: v for k, v in artifact.items() if k != "artifact_sha256"}), "artifact seal differs")
    request = runner.validate_request(artifact["request"])
    require(artifact["request_digest"] == runner.digest(request), "request digest differs")
    require(artifact["redactions"] == 0, "redacted treatment cannot establish exact provenance")
    required_files = {"provider.jsonl", "provider-stderr.txt", "broker-calls.jsonl", "final-message.txt",
                      "probe.json", "preflight.json"}
    if request["arm"] == "graph":
        required_files.add("plugin-setup.json")
    require(set(artifact["files"]) == required_files, "capture file set differs")
    for name, record in artifact["files"].items():
        require(not (directory / name).is_symlink(), "symlink capture file")
        require(runner.sha256_file(directory / name) == record["sha256"]
                and (directory / name).stat().st_size == record["bytes"], "capture file hash/size differs")
        require(record["redactions"] == 0, "redacted capture cannot establish exact provenance")
    isolation = artifact["isolation"]
    require(isolation["cold_start"] is True, "episode reused graph state")
    if not diagnostic:
        require(all(type(v) is int and v > 0 for v in artifact["resource_limits"].values())
                and set(artifact["resource_limits"]) == {"memory.max", "pids.max"},
                "finite process/memory ceilings were not observed")
        require(isolation["contained"] is True and isolation["truth_inaccessible"] is True
                and isolation["containment"] == "bwrap", "uncontained capture is diagnostic only")
        require(isolation["probe"]["hidden"] is True and isolation["probe"]["leaked"] == []
                and isolation["probe"]["missing"] == [], "containment evidence incomplete")
    require(isolation["sandbox_argv_sha256"] == runner.digest(isolation["sandbox_argv"]), "sandbox digest differs")
    require(all(listed == request["tools"] for listed in isolation["tools_listed"]), "advertised tools differ")
    require(isolation["inventory"]["tool_approvals"]["approved_tools"] == request["tools"],
            "approved tools differ")
    if artifact["status"] == "ok":
        require(artifact["cleanup"]["survivors"] == [] and artifact["cleanup"]["pipes_closed"] is True,
                "successful capture has incomplete process cleanup")
    for role in ("head", "base"):
        source = artifact["source_provenance"][role]
        require(source["commit"] == request["source_commits"][role] and source["selected_blobs"],
                "source commit provenance differs")
        require(all(plugin.source_path_allowed(name) for name in source["selected_blobs"]),
                "source provenance contains a non-source path")
    require(artifact["inputs"]["head"]["content_revision"] == request["source_revision"]
            and artifact["inputs"]["base"]["content_revision"] == request["base_revision"],
            "source view differs")
    check_treatment(artifact)
    if artifact["treatment"]:
        require(runner.load_json(directory / "plugin-setup.json", 32 * 1024 * 1024)
                == artifact["treatment"]["setup"], "installation log differs")
    log, _ = runner.read_broker_log(directory / "broker-calls.jsonl")
    log["truncated"] = log["truncated"] or artifact["files"]["broker-calls.jsonl"]["truncated"]
    require(log["calls"] == artifact["calls"] and artifact["calls_dropped"] == 0, "call evidence differs")
    transcript = runner.analyse_transcript((directory / "provider.jsonl").read_bytes(),
                                           artifact["files"]["provider.jsonl"]["truncated"], request["tools"])
    require(transcript["usage"] == artifact["usage"], "usage differs")
    output_bytes = sum(len(c["output"].encode()) for c in log["calls"]) + len(artifact["final_output"].encode())
    require(output_bytes == artifact["output_bytes"], "tool/answer output costs differ")
    exit_state = artifact["provider"]["exit"]
    supervision = {"terminated_by": artifact["cleanup"]["terminated_by"],
                   **{key: artifact["cleanup"].get(key) for key in
                      ("pipes_closed", "survivors", "signals", "brokers_swept")},
                   "stdout_truncated": artifact["files"]["provider.jsonl"]["truncated"],
                   "stderr": (directory / "provider-stderr.txt").read_bytes(), "exit": exit_state,
                   "returncode": exit_state["code"] if "code" in exit_state else
                                 -signal.Signals[exit_state["signal"]].value}
    final = {"text": (directory / "final-message.txt").read_text(),
             "truncated": artifact["final_output_truncated"]}
    status, code, _ = runner.decide(supervision, transcript, log, final, request["limits"], None,
                                     output_bytes, artifact["timing"]["wall_ms"],
                                     lifecycle_contract=artifact.get("lifecycle_contract"))
    require(status == artifact["status"] and code == (artifact["error"] or {}).get("code"),
            "replayed episode outcome differs")
    if status == "ok":
        require(json.loads(final["text"]) == artifact["answer"], "answer differs")
    return artifact


def replay(plan, directories, diagnostic=False):
    require(set(plan) == {"schema_version", "profile", "model", "provider_binary_sha256", "harness", "requests"}
            and plan["schema_version"] == 2 and plan["profile"] == plugin.PROFILE,
            "unsupported preregistration contract")
    episodes = [load_episode(directory, diagnostic) for directory in directories]
    episodes.sort(key=lambda a: a["request"]["order"])
    require([a["request"] for a in episodes] == plan["requests"], "captures differ from preregistration")
    require([a["request"]["order"] for a in episodes] == list(range(len(episodes))), "orders are incomplete")
    require(len({a["run_id"] for a in episodes}) == len(episodes), "reused run")
    for key in ("repository_id", "cache_id"):
        require(len({a["isolation"][key] for a in episodes}) == len(episodes), "reused episode state")
    require(len({runner.canonical({k: a["tool_versions"][k] for k in ("read", "rg", "git")})
                 for a in episodes}) == 1, "baseline tool version drift")
    require(len({runner.canonical(a["resource_limits"]) for a in episodes}) == 1,
            "process/memory ceiling drift")
    pairs = {}
    for episode in episodes:
        require(episode["harness"] == plan["harness"], "harness revision drift")
        require(episode["model"] == plan["model"] and
                episode["provider"]["binary_sha256"] == plan["provider_binary_sha256"], "model/settings drift")
        request = episode["request"]
        pairs.setdefault(request["case_id"], []).append(request)
    for pair in pairs.values():
        require(len(pair) == 2 and {r["arm"] for r in pair} == {"baseline", "graph"}, "missing paired arm")
        omit = {"arm", "tools", "order", "request_sha256"}
        require({k: v for k, v in pair[0].items() if k not in omit} ==
                {k: v for k, v in pair[1].items() if k not in omit}, "paired question/view/budget/pin drift")
    require(episodes, "empty cohort")
    return {"schema_version": 2, "profile": plugin.PROFILE, "episodes": len(episodes),
            "preregistration_sha256": runner.digest(plan), "effectiveness_evidence": False,
            "containment_verified": not diagnostic and all(a["status"] == "ok" for a in episodes),
            "outcomes": dict(Counter(a["status"] for a in episodes)),
            "costs": [{"case_id": a["request"]["case_id"], "arm": a["request"]["arm"],
                       "timing": a["timing"], "setup_output_bytes": a["setup_output_bytes"],
                       "status": a["status"], "error": a["error"],
                       "call_telemetry_verified": a["status"] == "ok",
                       "tool_calls": len(a["calls"]), "output_bytes": a["output_bytes"],
                       "usage": a["usage"]} for a in episodes],
            "limitation": "Provenance and behavior replay only; an independent frozen corpus/rubric is still required for effectiveness scoring."}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=["replay"])
    parser.add_argument("--preregistration", required=True)
    parser.add_argument("--episode", action="append", required=True)
    parser.add_argument("--diagnostic", action="store_true", help="allow uncontained fixtures; never agent evidence")
    args = parser.parse_args()
    try:
        result = replay(runner.load_json(args.preregistration, 32 * 1024 * 1024), args.episode, args.diagnostic)
    except (ValueError, OSError, KeyError, TypeError) as error:
        print(json.dumps({"error": str(error)}), file=sys.stderr)
        return 1
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
