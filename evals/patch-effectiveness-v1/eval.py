#!/usr/bin/env python3
"""External one-shot patch qualification, evaluation and all-slot reporting (offline)."""
import argparse
from collections import Counter
from datetime import datetime, timezone
import importlib.util
import json
import os
from pathlib import Path
import shutil
import stat
import sys
import time

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(ROOT / "scripts/agent-eval"))
import eval_runner as runner
import eval_broker as broker
import patch_profile as profile
import plugin_profile as plugin

spec = importlib.util.spec_from_file_location("patch_apply", HERE / "apply.py")
patcher = importlib.util.module_from_spec(spec)
spec.loader.exec_module(patcher)
check = profile.check
VERSION = "patch-evaluator-v1"
FILES = ("eval.py", "apply.py", "protocol.json", "build_closure.py")
ENV = {"PATH": "/usr/bin", "HOME": "/nonexistent", "LANG": "C", "LC_ALL": "C"}
LAUNCH = '''import resource,runpy,sys
resource.setrlimit(resource.RLIMIT_CPU, (10, 10))
resource.setrlimit(resource.RLIMIT_AS, (268435456, 268435456))
resource.setrlimit(resource.RLIMIT_FSIZE, (1048576, 1048576))
resource.setrlimit(resource.RLIMIT_NOFILE, (32, 32))
runpy.run_path(sys.argv[1], run_name="__main__")
'''
CANARY = '''import json,os,resource,socket,sys
assert not any(k in os.environ for k in ("CODEX_HOME", "OPENAI_API_KEY", "ORBIT_ROOT"))
assert not os.path.exists("/eval/state") and not os.path.exists("/home/daniel")
s = socket.socket()
try:
 s.connect(("1.1.1.1", 443))
 raise AssertionError("network accessible")
except OSError:
 pass
try:
 open("/source/.write-probe", "w").close()
 raise AssertionError("source writable")
except OSError:
 pass
print(json.dumps({"offline":True,"python":list(sys.version_info[:3])}))
'''


def evaluator_identity():
    return runner.digest({name: runner.sha256_file(HERE / name) for name in FILES})


def private_json(path, value):
    return runner.write_private(Path(path), json.dumps(value, sort_keys=True, indent=2) + "\n", runner.Redactor())


def disk_gate(parent):
    usage = shutil.disk_usage(parent)
    check(usage.used / usage.total < .8, "disk usage is at least 80%; no new build fixture")


def tree_lock(root):
    """Exact regular-file/mode closure; refuses links, special nodes and writable inputs."""
    root = Path(root)
    check(root.is_dir() and not root.is_symlink(), "missing/symlink closure")
    info = root.stat()
    check(info.st_uid == os.getuid() and not info.st_mode & 0o022, "unsafe closure owner/mode")
    records, size = {}, 0
    for directory, dirs, files in os.walk(root, followlinks=False):
        check(len(records) <= 10000, "closure has too many files")
        for name in dirs + files:
            path = Path(directory) / name
            info = path.lstat()
            check(not stat.S_ISLNK(info.st_mode), "symlink in closure")
            check(stat.S_ISDIR(info.st_mode) or stat.S_ISREG(info.st_mode), "special closure input")
            check(info.st_uid == os.getuid() and not info.st_mode & 0o022, "closure owner/mode unsafe")
            if path.is_file():
                check(info.st_size <= 32 * 1024 * 1024, "closure file too large")
                size += info.st_size
                check(size <= 512 * 1024 * 1024, "closure too large")
                records[str(path.relative_to(root))] = {"sha256": runner.sha256_file(path),
                                                       "mode": stat.S_IMODE(info.st_mode)}
    check(records and "usr/bin/python3" in records, "offline Python closure missing")
    return records


def suite_load(path):
    value = runner.load_json(path)
    check(set(value) == {"schema_version", "purpose", "tests"} and value["schema_version"] == 1
          and value["purpose"] in {"qualification-only", "independent-development"}, "invalid test suite")
    rows = value["tests"]
    check(isinstance(rows, list) and 2 <= len(rows) <= 128, "invalid test count")
    check({r.get("family") for r in rows} == {"behavior", "regression"}, "both test families required")
    ids = set()
    for row in rows:
        check(set(row) == {"id", "family", "input", "expected"} and profile.safe_path(row["id"])
              and row["id"] not in ids, "invalid or duplicate test")
        ids.add(row["id"])
        check(len(runner.canonical(row)) <= 32768, "test vector too large")
    return value


def custody_load(path, request):
    value = runner.load_json(path)
    check(set(value) == {"schema_version", "source_revision", "specification_sha256", "suite_sha256",
          "closure_sha256", "evaluator_sha256", "author", "reviewer", "frozen_at", "receipt"},
          "invalid custody fields")
    check(value["schema_version"] == 1, "unknown custody schema")
    for key in ("suite_sha256", "closure_sha256", "evaluator_sha256"):
        check(value[key] == request["patch"][key], "custody pin differs: " + key)
    check(value["source_revision"] == request["source_revision"]
          and value["specification_sha256"] == runner.sha256_bytes(request["prompt"].encode()),
          "custody source/specification differs")
    check(all(isinstance(value[k], str) and 0 < len(value[k]) <= 1024
              for k in ("author", "reviewer", "receipt")) and value["author"] != value["reviewer"],
          "independent author/reviewer receipt required")
    frozen = datetime.strptime(value["frozen_at"], "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc).timestamp()
    check(frozen <= time.time(), "custody freeze is in the future")
    return value, frozen


def inputs(request, suite, custody, closure):
    pin = request["patch"]
    closure_root = Path(closure).resolve()
    for hidden in (Path(suite).resolve(), Path(custody).resolve(), HERE.resolve()):
        check(not hidden.is_relative_to(closure_root) and not closure_root.is_relative_to(hidden),
              "hidden inputs/evaluator overlap the candidate runtime closure")
    check(pin["protocol_sha256"] == runner.sha256_file(HERE / "protocol.json")
          and pin["evaluator_sha256"] == evaluator_identity(), "evaluator/protocol drift")
    for path, key in ((suite, "suite_sha256"), (custody, "custody_sha256")):
        check(path and Path(path).is_file() and not Path(path).is_symlink(), "private input missing/symlink")
        info = Path(path).stat()
        check(info.st_uid == os.getuid() and not info.st_mode & 0o077, "private input must be owned 0600")
        check(runner.sha256_file(path) == pin[key], "private input drift: " + key)
    suite_value = suite_load(suite)
    custody_value, frozen = custody_load(custody, request)
    closure_value = tree_lock(closure)
    check(runner.digest(closure_value) == pin["closure_sha256"], "offline closure drift")
    return suite_value, custody_value, frozen, closure_value


def sandbox_argv(bwrap, closure, source):
    return [bwrap, "--die-with-parent", "--new-session", "--unshare-all", "--cap-drop", "ALL",
            "--ro-bind", str(Path(closure).resolve()), "/", "--proc", "/proc", "--dev", "/dev",
            "--tmpfs", "/tmp", "--ro-bind", str(Path(source).resolve()), "/source",
            "--chdir", "/source", "--clearenv", "--setenv", "PATH", "/usr/bin",
            "--setenv", "HOME", "/nonexistent", "--setenv", "LANG", "C", "--"]


def canary(bwrap, closure, source):
    result = broker.run_child(sandbox_argv(bwrap, closure, source) +
        ["/usr/bin/python3", "-I", "-S", "-B", "-c", CANARY], str(source), ENV, 10, capture_limit=4096)
    check(plugin.clean(result) and result["supervision"]["signals"] == [],
          "offline closure/namespace canary unavailable: " + result["stderr"].decode(errors="replace")[:500])
    value = runner.parse_json(result["stdout"], "canary")
    check(value.get("offline") is True, "offline canary failed")
    return {"argv_sha256": runner.digest(sandbox_argv(bwrap, closure, source)),
            "runtime": value, "elapsed_ms": result["elapsed_ms"]}


def admit(episode):
    args, request = episode.args, episode.request
    profile.verify_runtime(episode)
    suite, custody, closure = args.patch_suite, args.patch_custody, args.patch_closure
    check(all((suite, custody, closure)), "patch suite/custody/offline closure required before provider")
    _, _, frozen, closure_value = inputs(request, suite, custody, closure)
    # Provider receives system mounts, its installation, runtime, state and source;
    # hidden data must be physically disjoint from every one, in both directions.
    mounted = ["/usr", "/bin", "/lib", "/lib64", *runner.ETC_ENTRIES,
               str(episode.codex_root), str(episode.out), args.head, args.base,
               str(ROOT / "scripts/agent-eval"), *[p for p in episode.binaries.values() if p],
               *([episode.auth_file] if episode.auth_file else [])]
    private = [Path(suite).resolve(), Path(custody).resolve(), Path(closure).resolve(), HERE.resolve()]
    for secret in private:
        for mount in mounted:
            root = Path(mount).resolve()
            check(not secret.is_relative_to(root) and not root.is_relative_to(secret),
                  "hidden evaluator/custody/closure overlaps an agent mount")
    args.truth_path = list(args.truth_path) + [str(p) for p in private]
    mode = "strict" if episode.contained else "diagnostic"
    evidence = None
    if episode.contained:
        check(request["patch"]["resource_limits"] == episode.resource_limits
              and all(episode.resource_limits.values()), "frozen resource ceilings differ/unavailable")
        evidence = canary(episode.binaries["bwrap"], closure, args.head)
    return {"version": VERSION, "mode": mode, "frozen_at_unix": frozen,
            "verified_at_unix": time.time(), "pins": request["patch"],
            "closure_files": len(closure_value), "canary": evidence}


def verify_patch_capture(artifact, directory):
    check(artifact["schema_version"] == 4 and artifact["profile"] == profile.PROFILE, "wrong patch profile")
    data = (Path(directory) / "final-message.txt").read_bytes()
    check(artifact["patch_identity"] == {"sha256": runner.sha256_bytes(data), "bytes": len(data)},
          "original patch bytes lost/changed (including redaction)")
    runtime = artifact["request"]["patch"]["runtime"]
    check(artifact["harness"] == runtime["harness"] and artifact["model"] == runtime["model"]
          and artifact["provider"]["binary_sha256"] == runtime["provider_binary_sha256"],
          "capture runtime differs from pre-output freeze")
    admission = artifact["patch_admission"]
    check(admission["version"] == VERSION and admission["pins"] == artifact["request"]["patch"]
          and admission["frozen_at_unix"] <= admission["verified_at_unix"] <= artifact["started_at_unix"],
          "pre-output admission/custody evidence invalid")
    if artifact["isolation"]["contained"]:
        check(admission["mode"] == "strict" and admission["canary"], "strict admission missing")
    # A truncated final cannot silently be treated as the complete original patch.
    check(not artifact["files"]["final-message.txt"]["truncated"], "patch capture truncated")


def loader():
    spec = importlib.util.spec_from_file_location("patch_replay", ROOT / "evals/plugin-agent-navigation/eval.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_vectors(repo, suite, pin, bwrap, closure, diagnostic=False):
    """Host retains all expected results. Candidate sees one input, never test truth."""
    results, remaining = [], pin["test_wall_ms"] / 1000
    output_remaining = pin["test_output_bytes"]
    for row in suite["tests"]:
        if remaining <= 0 or output_remaining <= 0:
            results.append({"id": row["id"], "family": row["family"], "outcome": "timeout" if remaining <= 0 else "failed_tests", "transport": None})
            continue
        argv = ([sys.executable, "-I", "-S", "-B", "-c", LAUNCH, str(repo / pin["entrypoint"])]
                if diagnostic else sandbox_argv(bwrap, closure, repo) +
                ["/usr/bin/python3", "-I", "-S", "-B", "-c", LAUNCH, "/source/" + pin["entrypoint"]])
        transport = broker.run_child(argv, str(repo), ENV, remaining,
            capture_limit=output_remaining, total_capture_limit=output_remaining, input_bytes=(runner.canonical(row["input"]) + b"\n"))
        remaining -= transport["elapsed_ms"] / 1000
        output_remaining -= len(transport["stdout"]) + len(transport["stderr"])
        outcome = "failed_tests"
        if transport["stopped"] == "timeout":
            outcome = "timeout"
        elif transport["error_type"] or transport["supervision"]["survivors"]:
            outcome = "integrity_failure"
        elif plugin.clean(transport) and transport["supervision"]["signals"] == []:
            try:
                actual = runner.parse_json(transport["stdout"], "candidate stdout")
                if runner.canonical(actual) == runner.canonical(row["expected"]):
                    outcome = "passed"
            except ValueError:
                pass
        results.append({"id": row["id"], "family": row["family"], "outcome": outcome,
                        "transport": plugin.evidence(transport)})
    return results


def evaluate(args):
    """Only sealed original captures; always a new evaluator clone after provider exit."""
    out = Path(args.out)
    check(not out.exists() and out.parent.is_dir(), "evaluation output must be new")
    runner.make_private_dir(out)
    started = time.monotonic()
    report = {"schema_version": 1, "version": VERSION, "outcome": "integrity_failure", "success": False,
              "diagnostic": args.diagnostic, "episode": str(Path(args.episode).resolve()), "tests": []}
    try:
        artifact = loader().load_episode(args.episode, args.diagnostic)
        request, pin = artifact["request"], artifact["request"]["patch"]
        report.update(request_digest=runner.digest(request), capture_sha256=artifact["artifact_sha256"],
                      patch_identity=artifact["patch_identity"], pins=pin, usage=artifact["telemetry"]["usage"],
                      timing=artifact["telemetry"]["timing"], model=artifact["model"],
                      provider_sha256=artifact["provider"]["binary_sha256"], harness=artifact["harness"])
        suite, _, frozen, _ = inputs(request, args.suite, args.custody, args.closure)
        check(frozen <= artifact["started_at_unix"], "truth frozen after output")
        # Runtime failures are not evidence of an incorrect patch.
        runtime_errors = {"process_cleanup_failed", "telemetry_mismatch", "broker_failed", "broker_lifecycle_incomplete",
                          "unbrokered_tool_use", "provider_code_mode_required", "provider_tool_approval_required",
                          "broker_output_malformed", "broker_not_initialized", "tool_inventory_mismatch",
                          "broker_exit_missing", "tool_execution_failed"}
        code = (artifact["error"] or {}).get("code")
        if code in runtime_errors or artifact["redactions"]:
            raise ValueError("runtime provenance invalid: " + str(code))
        if artifact["status"] != "ok":
            report["outcome"] = ("timeout" if artifact["status"] == "timeout" else
                                 "invalid_patch" if code in {"answer_malformed", "final_output_oversized"} else "failed_submission")
        else:
            disk_gate(out)
            for role, path in (("head", args.head), ("base", args.base)):
                revision = runner.content_revision(runner.tree_entries(path, role), role)
                check(revision == artifact["inputs"][role], "fresh source reconstruction differs")
            state = out / "state"
            runner.make_private_dir(state)
            runner.make_private_dir(state / "home")
            repo = out / "repo"
            snapshot = runner.materialize_snapshot(repo, state, "/usr/bin/git",
                runner.tree_entries(args.base, "base"), runner.tree_entries(args.head, "head"))
            check(snapshot["head_commit"] == artifact["snapshot"]["head_commit"], "immutable snapshot differs")
            if not args.diagnostic:
                bounds = plugin.resource_bounds()
                check(all(bounds.values()) and bounds == pin["resource_limits"], "frozen memory/pids ceilings differ/unavailable")
                report["canary"] = canary(args.bwrap, args.closure, repo)
                report["resource_limits"] = bounds
            data = (Path(args.episode) / "final-message.txt").read_bytes()
            report["diff"] = patcher.apply(repo, data, pin)
            # Candidate has no history or mutable graph/config state in its mount.
            source = out / "source"
            runner.make_private_dir(source)
            for name, path in runner.tree_entries_with_git(repo):
                target = source / name
                target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
                with os.fdopen(os.open(target, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600), "wb") as f:
                    f.write(Path(path).read_bytes())
            report["tests"] = test_vectors(source, suite, pin, args.bwrap, args.closure, args.diagnostic)
            states = {r["outcome"] for r in report["tests"]}
            report["outcome"] = next((s for s in ("integrity_failure", "timeout", "failed_tests") if s in states), "success")
            report["families"] = {family: {"passed": sum(r["outcome"] == "passed" for r in report["tests"] if r["family"] == family),
                                          "total": sum(r["family"] == family for r in report["tests"])}
                                  for family in ("behavior", "regression")}
            report["partial"] = "passed" in states and states != {"passed"}
        # Detect closure/custody drift over evaluation; no success on a changed validator.
        inputs(request, args.suite, args.custody, args.closure)
        report["success"] = report["outcome"] == "success"
    except patcher.Rejected as error:
        report.update(outcome=error.outcome, reason=str(error))
    except (ValueError, OSError, KeyError, TypeError) as error:
        report.update(outcome="integrity_failure", reason=str(error)[:1000])
    report["evaluation_ms"] = int((time.monotonic() - started) * 1000)
    report, redactions = runner.Redactor().value(report)
    if redactions:
        report.update(outcome="integrity_failure", success=False, reason="evaluation report redacted")
    runner.seal(report, "report_sha256")
    private_json(out / "report.json", report)
    return report


def score(plan, slots):
    check(set(plan) == {"schema_version", "profile", "requests", "model", "provider_binary_sha256", "harness"}
          and plan["schema_version"] == 4 and plan["profile"] == profile.PROFILE, "patch schedule required")
    requests = plan["requests"]
    check(requests and [r["order"] for r in requests] == list(range(len(requests))), "non-contiguous schedule")
    pairs = {}
    for request in requests:
        runner.validate_request(request)
        pairs.setdefault(request["case_id"], []).append(request)
    omit = {"arm", "tools", "order", "request_sha256"}
    for pair in pairs.values():
        check(len(pair) == 2 and {r["arm"] for r in pair} == {"baseline", "graph"}, "unpaired schedule")
        check({k: v for k, v in pair[0].items() if k not in omit} ==
              {k: v for k, v in pair[1].items() if k not in omit}, "paired source/spec/pins/bounds drift")
    check(len(slots) == len(requests), "every scheduled slot needs a result/refusal")
    outcomes, costs, integrity = Counter(), [], False
    seen = set()
    for request, directory in zip(requests, slots):
        directory = Path(directory)
        refusal = directory / "refusal.json"
        report_path = directory / "report.json"
        if refusal.exists():
            record = runner.load_json(refusal)
            check(record.get("schema_version") == 4 and record.get("profile") == profile.PROFILE
                  and record.get("status") == "refused" and record.get("provider_started") is False
                  and record.get("request_digest") == runner.digest(request)
                  and record.get("request_identity") == runner.digest(request)
                  and isinstance(record.get("stage"), str) and isinstance(record.get("refusal"), dict)
                  and bool(record["refusal"].get("code")), "refusal not bound to slot")
            outcome = "setup_refusal"
            integrity = True  # admission failed; retained but cannot score patch correctness
            costs.append({"order": request["order"], "usage": None, "timing": None, "outcome": outcome})
        else:
            record = runner.load_json(report_path, 32 * 1024 * 1024)
            check(record["report_sha256"] == runner.digest({k: v for k, v in record.items() if k != "report_sha256"}),
                  "evaluation report drift")
            check(record["request_digest"] == runner.digest(request), "report not bound to slot")
            capture = loader().load_episode(record["episode"], record["diagnostic"])
            check(capture["artifact_sha256"] == record["capture_sha256"] and capture["run_id"] not in seen,
                  "capture changed/reused")
            seen.add(capture["run_id"])
            check(record["model"] == plan["model"] and record["harness"] == plan["harness"]
                  and record["provider_sha256"] == plan["provider_binary_sha256"], "runtime/model drift")
            outcome = record["outcome"]
            integrity |= outcome == "integrity_failure" or record["diagnostic"]
            costs.append({"order": request["order"], "outcome": outcome,
                          **{key: record.get(key) for key in ("usage", "timing", "evaluation_ms", "diff", "families", "partial")}})
        outcomes[outcome] += 1
    return {"schema_version": 1, "profile": profile.PROFILE, "scheduled": len(requests),
            "outcomes": dict(outcomes), "successes": outcomes["success"],
            "all_attempt_success_rate": None if integrity else outcomes["success"] / len(requests),
            "integrity_valid": not integrity, "effectiveness_evidence": False,
            "costs": costs, "limitation": "Qualification only; independent root runtime/custody and real-task admission remain required."}



def freeze(args):
    """Prepare a one-case pair and exact pins, with a separately reviewed custody receipt."""
    plan = runner.load_json(args.plan)
    check(set(plan) == {"schema_version", "profile", "model", "provider_binary_sha256", "harness", "requests"}
          and plan["schema_version"] == 3 and plan["profile"] == plugin.PROSPECTIVE_PROFILE,
          "independently prepared schema-3 seed plan required")
    requests = plan["requests"]
    check(len(requests) == 2 and {r["arm"] for r in requests} == {"baseline", "graph"}
          and [r["order"] for r in requests] == [0, 1], "one paired task required")
    omit = {"arm", "tools", "order", "request_sha256"}
    check({k: v for k, v in requests[0].items() if k not in omit} ==
          {k: v for k, v in requests[1].items() if k not in omit}, "paired task/source/budget drift")
    for request in requests:
        runner.validate_request(request)
    suite_load(args.suite)
    out = Path(args.out)
    check(not out.exists() and out.parent.is_dir(), "freeze output must be new")
    check(args.author != args.reviewer, "independent reviewer required")
    runner.make_private_dir(out)
    pin = {"contract": profile.CONTRACT, "protocol_sha256": runner.sha256_file(HERE / "protocol.json"),
           "evaluator_sha256": evaluator_identity(), "suite_sha256": runner.sha256_file(args.suite),
           "closure_sha256": runner.digest(tree_lock(args.closure)), "entrypoint": args.entrypoint,
           "allowed_paths": args.allowed_path, "max_patch_bytes": min(65536, requests[0]["limits"]["call_bytes"]),
           "max_changed_lines": 1000, "max_files": len(args.allowed_path),
           "test_wall_ms": 10000, "test_output_bytes": 65536, "resource_limits": plugin.resource_bounds(),
           "runtime": profile.runtime_pin(plan["model"], plan["provider_binary_sha256"],
               {k: runner.resolve_binary(getattr(args, k), k) for k in ("rg", "git", "python")},
               runner.resolve_binary(args.bwrap, "bwrap"))}
    prompt = requests[0]["prompt"] + (
        "\nOne-shot patch submission. Use the same read-only navigation tools; no build/debug or hidden feedback. "
        "Return only UTF-8 LF unified patch bytes, or empty bytes for no-op. "
        "Use --- a/path, +++ b/path and bare @@ -start,count +start,count @@ headers. "
        "No Git preamble, timestamps, modes, renames, additions, deletions or binary data. "
        "Patch bounds: " + json.dumps({k: pin[k] for k in ("allowed_paths", "max_patch_bytes", "max_changed_lines", "max_files")}, sort_keys=True))
    custody = {"schema_version": 1, "source_revision": requests[0]["source_revision"],
               "specification_sha256": runner.sha256_bytes(prompt.encode()),
               **{k: pin[k] for k in ("suite_sha256", "closure_sha256", "evaluator_sha256")},
               "author": args.author, "reviewer": args.reviewer, "receipt": args.receipt,
               "frozen_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")}
    private_json(out / "custody.json", custody)
    pin["custody_sha256"] = runner.sha256_file(out / "custody.json")
    for i, request in enumerate(requests):
        request.update(schema_version=4, profile=profile.PROFILE, patch=pin, prompt=prompt)
        request.pop("request_sha256", None)
        request["request_sha256"] = runner.digest(request)
        runner.validate_request(request)
        private_json(out / f"request-{i}.json", request)
    plan.update(schema_version=4, profile=profile.PROFILE, harness={name: runner.sha256_file(runner.HERE / name)
                for name in ("eval_runner.py", "eval_broker.py", "plugin_profile.py", "reply_provenance.py", "telemetry.py", "patch_profile.py")})
    private_json(out / "plan.json", plan)
    return {"plan": str(out / "plan.json"), "custody": str(out / "custody.json"),
            "plan_sha256": runner.digest(plan), "provider_started": False,
            "admission": "proposed; external root review and strict synthetic qualification required"}

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    identity = sub.add_parser("identity", help="exact evaluator and protocol pins")
    identity.add_argument("--closure", required=True)
    freeze_cmd = sub.add_parser("freeze", help="freeze one independently prepared paired task; no provider")
    for name in ("plan", "suite", "closure", "author", "reviewer", "receipt", "out"):
        freeze_cmd.add_argument("--" + name, required=True)
    for name, default in (("rg", "rg"), ("git", "/usr/bin/git"), ("python", "/usr/bin/python3"), ("bwrap", "/usr/bin/bwrap")):
        freeze_cmd.add_argument("--" + name, default=default)
    freeze_cmd.add_argument("--entrypoint", default="src/program.py")
    freeze_cmd.add_argument("--allowed-path", action="append", required=True)
    ev = sub.add_parser("evaluate")
    for name in ("episode", "head", "base", "suite", "custody", "closure", "out"):
        ev.add_argument("--" + name, required=True)
    ev.add_argument("--bwrap", default="/usr/bin/bwrap")
    ev.add_argument("--diagnostic", action="store_true", help="UNSAFE trusted controls only; no evidence")
    report = sub.add_parser("score")
    report.add_argument("--plan", required=True)
    report.add_argument("--slot", action="append", required=True, help="evaluation/refusal dirs in schedule order")
    args = parser.parse_args()
    try:
        if args.command == "evaluate":
            result = evaluate(args)
        elif args.command == "freeze":
            result = freeze(args)
        elif args.command == "identity":
            result = {"evaluator_sha256": evaluator_identity(), "protocol_sha256": runner.sha256_file(HERE / "protocol.json"),
                      "closure_sha256": runner.digest(tree_lock(args.closure))}
        else:
            result = score(runner.load_json(args.plan), args.slot)
        print(json.dumps(result, indent=2, sort_keys=True))
        return 0
    except (ValueError, OSError, KeyError, TypeError, runner.CapabilityRefusal) as error:
        print(json.dumps({"error": str(error)[:1000]}), file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
