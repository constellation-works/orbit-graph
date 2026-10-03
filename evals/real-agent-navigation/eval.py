#!/usr/bin/env python3
"""Offline frozen real-repository corpus: validate, export, adapt and score.

No provider calls. Explicit local repositories are read through pinned Git
objects. Export creates fresh source-only directories. Score writes nothing.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path, PurePosixPath
import re
import selectors
import signal
import subprocess
import sys
import time

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent
LOCK_SHA256 = "dd1b9d4a789b9bc54102595e13162deb587f3993d0cb975efc5368b381efe448"
MAX_JSON = 64 * 1024 * 1024
MAX_BLOB = 4 * 1024 * 1024
MAX_TREE = 128 * 1024 * 1024
ARMS = ("baseline", "graph")
COMMON = ["read", "rg", "git"]
GRAPH = ["graph_sync", "search", "show", "refs", "callees", "impact", "changes"]
LIMITS = {"wall_ms": 300000, "tool_calls": 60, "output_bytes": 1048576,
          "answer_items": 32, "call_bytes": 65536}
CONTRACT = ('\nReturn only JSON with exactly items (unique symbol:path#qualified_name:kind '
            'strings; kinds function, test, constant, class, or method), abstain (boolean), '
            'reason (string, at most 4096 UTF-8 bytes, explaining behavior and uncertainty), '
            'evidence (array with exactly item,file,line,quote). Cite each item at its exact '
            'HEAD definition line, quoting that whole line without surrounding whitespace. '
            'Use class-qualified Python method names. abstain=true means no supported '
            'locations and requires empty items/evidence and a reason. When dispatch has '
            'multiple possible implementations, return supported locations with abstain=false '
            'and explicitly explain why a unique callee cannot be guaranteed. Do not claim '
            'exhaustiveness from name matching. Use only the supplied source and broker tools; '
            'do not modify source or invoke other agents. The local broker Git history uses two '
            'generated snapshot commits: HEAD and HEAD^ correspond to the supplied original '
            'head and base views. Original commit IDs in the question are provenance IDs, '
            'not local snapshot commit IDs.')


class Invalid(ValueError):
    pass


def require(ok, message):
    if not ok:
        raise Invalid(message)


def shape(value, keys, label):
    require(isinstance(value, dict) and set(value) == set(keys),
            f"{label}: expected exactly {sorted(keys)}")


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False).encode()


def digest(value):
    return hashlib.sha256(canonical(value)).hexdigest()


def seal(value, key):
    value[key] = digest({k: v for k, v in value.items() if k != key})
    return value


def check_seal(value, key):
    require(value.get(key) == digest({k: v for k, v in value.items() if k != key}),
            f"{key}: hash mismatch")


def unique(pairs):
    obj = {}
    for key, value in pairs:
        require(key not in obj, f"duplicate JSON key: {key}")
        obj[key] = value
    return obj


def parse(data):
    return json.loads(data, object_pairs_hook=unique,
                      parse_constant=lambda x: (_ for _ in ()).throw(Invalid(f"number {x}")))


def load(path):
    with Path(path).open("rb") as stream:
        data = stream.read(MAX_JSON + 1)
    require(len(data) <= MAX_JSON, f"oversized JSON: {path}")
    return parse(data)


def text(value, label, bound=4096):
    require(isinstance(value, str) and value.strip() and len(value.encode()) <= bound,
            f"{label}: empty or oversized text")


def integer(value, label, maximum=10**12):
    require(type(value) is int and 0 <= value <= maximum, f"{label}: invalid integer")


def safe_path(value):
    text(value, "path", 1024)
    path = PurePosixPath(value)
    require(not path.is_absolute() and all(p not in ("..", ".", "") for p in value.split("/"))
            and "\\" not in value and not any(ord(c) < 32 for c in value), "unsafe path")
    return path


def included(path):
    """Frozen narrow source view, not a reconstruction of the build environment."""
    parts = safe_path(path).parts
    if any(p.startswith(".") or p.lower() in {
        "docs", "skills", "instructions", "evaluation", "agent-eval", "target",
        "generated", "vendor", "node_modules", "secrets", "credentials"} for p in parts):
        return False
    if path in {"Cargo.toml", "Cargo.lock", "pyproject.toml"}:
        return True
    return (parts[0] in {"src", "crates", "tests"} and
            (path.endswith((".rs", ".py")) or parts[-1] == "Cargo.toml"))


def git(repo, *args, bound=8 * 1024 * 1024):
    env = {"PATH": "/usr/bin:/bin", "HOME": "/nonexistent", "LANG": "C.UTF-8",
           "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": "/dev/null",
           "GIT_OPTIONAL_LOCKS": "0", "GIT_NO_REPLACE_OBJECTS": "1", "GIT_TERMINAL_PROMPT": "0"}
    command = ["/usr/bin/git", "--no-pager", "-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false",
               "-C", str(repo), *args]
    return capture(command, env, bound)


def capture(command, env, bound=8 * 1024 * 1024):
    """Bounded child supervisor, shared by read-only Git and hermetic CLI tests."""
    child = subprocess.Popen(command, env=env, stdout=subprocess.PIPE,
                             stderr=subprocess.PIPE, start_new_session=True)
    out, err, deadline = bytearray(), bytearray(), time.monotonic() + 30
    selector = selectors.DefaultSelector()
    for pipe, data in ((child.stdout, out), (child.stderr, err)):
        os.set_blocking(pipe.fileno(), False)
        selector.register(pipe, selectors.EVENT_READ, data)
    problem = None
    try:
        while True:
            exited = os.waitid(os.P_PID, child.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
            if exited is not None and not selector.get_map():
                break
            if time.monotonic() >= deadline:
                raise Invalid("child read exceeded 30s deadline")
            for key, _ in selector.select(min(0.005, max(0, deadline - time.monotonic()))):
                block = os.read(key.fileobj.fileno(), 65536)
                if not block:
                    selector.unregister(key.fileobj)
                else:
                    key.data.extend(block)
                    require(len(out) <= bound and len(err) <= 16384, "child capture bound exceeded")
    except BaseException as error:
        problem = error
        os.killpg(child.pid, signal.SIGTERM)
        grace = time.monotonic() + 0.5
        while time.monotonic() < grace:
            try:
                os.killpg(child.pid, 0)
            except ProcessLookupError:
                break
            time.sleep(0.01)
    finally:
        # Never poll/wait/reap before signalling the owned group.
        try:
            os.killpg(child.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        selector.close()
        child.stdout.close()
        child.stderr.close()
        code = child.wait(timeout=5)
    if problem is not None:
        raise problem
    require(code == 0, f"child failed ({code}): {err.decode(errors='replace')[:1000]}")
    return bytes(out)


def snapshot(repo, revision):
    require(re.fullmatch(r"[0-9a-f]{40}", revision) is not None, "unpinned Git revision")
    require(git(repo, "rev-parse", revision + "^{commit}").decode().strip() == revision,
            "commit identity mismatch")
    tree = git(repo, "rev-parse", revision + "^{tree}").decode().strip()
    files, total = {}, 0
    for entry in git(repo, "ls-tree", "-rz", "--full-tree", revision).split(b"\0"):
        if not entry:
            continue
        metadata, raw_path = entry.split(b"\t", 1)
        path = raw_path.decode("utf-8")
        mode, kind, oid = metadata.decode().split()
        if not included(path):
            continue
        require(mode in {"100644", "100755"} and kind == "blob", f"nonregular source: {path}")
        size = int(git(repo, "cat-file", "-s", oid))
        require(size <= MAX_BLOB, f"source blob too large: {path}")
        blob = git(repo, "cat-file", "blob", oid, bound=MAX_BLOB)
        total += len(blob)
        require(total <= MAX_TREE and len(files) < 10000, "source tree bound exceeded")
        files[path] = blob.decode("utf-8")
    require(files, "no included source files")
    return {"git_revision": revision, "git_tree": tree, "content_revision": digest(files),
            "files": {p: hashlib.sha256(v.encode()).hexdigest() for p, v in sorted(files.items())}}, files


def corpus(root=ROOT):
    data, lock = load(root / "corpus.json"), load(root / "corpus.lock.json")
    require(digest(lock) == LOCK_SHA256, "frozen lock changed; do not re-freeze after episodes")
    shape(lock, ["schema_version", "corpus_sha256", "truth_sha256", "split_sha256", "request_plan_sha256", "snapshots"], "lock")
    shape(data, ["schema_version", "frozen_at", "label", "limits", "export_policy", "decisions",
                 "repositories", "cases"], "corpus")
    require(data["schema_version"] == lock["schema_version"] == 1, "schema version")
    require(data["limits"] == LIMITS and len(data["cases"]) == 8, "case count/budget drift")
    require(lock["corpus_sha256"] == digest(data), "corpus drift")
    require(lock["truth_sha256"] == digest({c["id"]: c["truth"] for c in data["cases"]}), "truth drift")
    require(lock["split_sha256"] == digest({c["id"]: c["split"] for c in data["cases"]}), "split drift")
    require(lock["request_plan_sha256"] == digest(requests(data, lock)), "request protocol drift")
    return data, lock


def verify(data, lock, repos):
    require(set(repos) == set(data["repositories"]), "pass exactly orbit-graph, Orbit and pulsar paths")
    for name, revision in data["repositories"].items():
        require(repos[name].is_dir() and (repos[name] / ".git").exists(), "repo must be an explicit Git checkout root; parent discovery is refused")
        require(git(repos[name], "rev-parse", revision + "^{commit}").decode().strip() == revision,
                "original repository commit drift")
    for case in data["cases"]:
        if case["kind"] == "commit_change":
            head = lock["snapshots"][case["head_snapshot"]]["git_revision"]
            base = lock["snapshots"][case["base_snapshot"]]["git_revision"]
            require(git(repos[case["repository"]], "rev-parse", head + "^").decode().strip() == base,
                    "change base is not original head parent")
    trees = {}
    for key, manifest in lock["snapshots"].items():
        repo_name = key.split("/", 1)[0]
        actual, files = snapshot(repos[repo_name], manifest["git_revision"])
        require(actual == manifest, f"pinned source manifest drift: {key}")
        trees[key] = files
    for case in data["cases"]:
        for item in case["truth"]["identities"]:
            require(item["file"] in trees[case["head_snapshot"]], "identity outside export")
            lines = trees[case["head_snapshot"]][item["file"]].splitlines()
            require(lines[item["line"] - 1].strip() == item["quote"], "identity source drift")
        for claim in case["truth"]["rubric"]:
            for evidence in claim["evidence"]:
                files = trees[case[evidence["revision"] + "_snapshot"]]
                require(files[evidence["file"]].splitlines()[evidence["line"] - 1].strip()
                        == evidence["quote"], "rubric source drift")
                lines = files[evidence["file"]].splitlines()
                excerpt = "\n".join(lines[evidence["line"] - 1:evidence["end_line"]]) + "\n"
                require(hashlib.sha256(excerpt.encode()).hexdigest() == evidence["excerpt_sha256"], "rubric range drift")
    return trees


def requests(data, lock):
    result = []
    for i, case in enumerate(data["cases"]):
        for arm in ARMS if i % 2 == 0 else reversed(ARMS):
            result.append(seal({"schema_version": 1, "case_id": case["id"], "arm": arm,
                               "split": case["split"], "fixture": case["repository"],
                               "source_revision": lock["snapshots"][case["head_snapshot"]]["content_revision"],
                               "base_revision": lock["snapshots"][case["base_snapshot"]]["content_revision"],
                               "corpus_sha256": digest(data), "prompt": case["prompt"] + CONTRACT,
                               "limits": data["limits"], "tools": COMMON + (GRAPH if arm == "graph" else []),
                               "cache_policy": "cold-per-episode-including-graph-setup", "order": len(result)},
                              "request_sha256"))
    return result


def write_new(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as stream:
        stream.write(value if isinstance(value, str) else json.dumps(value, indent=2, sort_keys=True) + "\n")


def export(data, lock, trees, destination):
    destination = Path(os.path.abspath(destination))
    require(not destination.exists() and not destination.is_symlink(), "export destination already exists")
    require(destination.parent.is_dir() and destination.parent.resolve() == destination.parent,
            "export parent must exist and have no symlink components")
    destination.mkdir(mode=0o700)
    cases = {c["id"]: c for c in data["cases"]}
    for request in requests(data, lock):
        case = cases[request["case_id"]]
        folder = destination / f"{request['order']:02}-{case['id']}-{request['arm']}"
        for name in ("head", "base"):
            for path, content in trees[case[name + "_snapshot"]].items():
                write_new(folder / name / path, content)
        write_new(folder / "request.json", request)
        # Provenance has no identities, rubric, expected answer or limitations.
        manifest = {"schema_version": 1, "request_sha256": request["request_sha256"],
                    "original_source_revision": case["original_source_revision"],
                    "head": lock["snapshots"][case["head_snapshot"]],
                    "base": lock["snapshots"][case["base_snapshot"]]}
        write_new(folder / "source-manifest.json", seal(manifest, "manifest_sha256"))
    write_new(destination / "complete.json", {"schema_version": 1, "corpus_sha256": digest(data),
                                             "requests": requests(data, lock)})
    return {"destination": str(destination), "episodes": 16, "corpus_sha256": digest(data)}


PUBLIC = ["request", "run_id", "model", "tool_versions", "isolation", "status", "error", "answer",
          "final_output", "calls", "wall_ms", "output_bytes", "usage", "record_sha256"]
RAW = ["schema_version", "kind", "runner_version", "run_id", "request", "request_digest",
       "started_at_unix", "model", "provider", "tool_versions", "inputs", "snapshot", "isolation",
       "timing", "status", "error", "answer", "final_output", "final_output_truncated", "final_source",
       "calls", "calls_dropped", "output_bytes", "usage", "usage_raw", "transcript", "broker",
       "cleanup", "files", "redactions", "limitations", "artifact_sha256"]
# Read compatibility for the exact shape accepted by the original adapter.
# A missing aggregate count remains absent in raw evidence; never infer zero.
LEGACY_RAW = [field for field in RAW if field != "redactions"]
CAPTURE_FILES = ["provider.jsonl", "provider-stderr.txt", "broker-calls.jsonl",
                 "final-message.txt", "probe.json", "preflight.json"]


def raw_record(directory):
    directory = Path(directory).resolve(strict=True)
    raw = load(directory / "episode.json")
    shape(raw, RAW if "redactions" in raw else LEGACY_RAW,
          "raw episode; runner contract adaptation must be explicit")
    require(raw["schema_version"] == 1 and raw["kind"] == "agent-eval-raw-episode", "raw version/kind")
    check_seal(raw, "artifact_sha256")
    require(raw["request_digest"] == digest(raw["request"]), "raw request digest")
    if "redactions" in raw:
        # This counts replacements in episode.json, not in the six capture files.
        integer(raw["redactions"], "raw redactions")
    shape(raw["files"], CAPTURE_FILES, "capture file set")
    for name, info in raw["files"].items():
        shape(info, ["bytes", "sha256", "redactions", "truncated"], "capture metadata " + name)
        integer(info["bytes"], "capture bytes " + name, MAX_JSON)
        integer(info["redactions"], "capture redactions " + name)
        require(type(info["truncated"]) is bool, "capture truncated: expected boolean for " + name)
        require(isinstance(info["sha256"], str) and re.fullmatch(r"[0-9a-f]{64}", info["sha256"]) is not None,
                "capture sha256: expected lowercase SHA-256 for " + name)
        path = directory / name
        require(not path.is_symlink() and path.is_file() and path.stat().st_size <= MAX_JSON,
                "unsafe/oversized captured artifact")
        require(hashlib.sha256(path.read_bytes()).hexdigest() == info["sha256"], "capture hash mismatch")
        require(info["bytes"] == path.stat().st_size, "capture byte mismatch")
    shape(raw["inputs"], ["head", "base"], "raw inputs")
    for info in raw["inputs"].values():
        shape(info, ["content_revision", "files", "bytes"], "raw input tree")
    shape(raw["timing"], ["wall_ms", "setup_ms", "provider_ms", "graph_sync_ms", "preflight_ms", "wall_limit_ms"], "raw timing")
    for key, value in raw["timing"].items():
        integer(value, key)
    shape(raw["isolation"], ["containment", "contained", "truth_inaccessible", "cold_start", "repository_id", "cache_id",
                             "probe", "probe_spec_forbidden", "sandbox_argv", "sandbox_argv_sha256", "bwrap", "broker_sha256",
                             "tools_listed", "refused_tool_names", "inventory", "unbrokered", "graph_state_after"], "raw isolation")
    shape(raw["snapshot"], ["base_commit", "head_commit", "base_tree", "head_tree", "commit_count", "commit_epoch", "work_tree_revision"], "raw snapshot")
    shape(raw["provider"], ["binary_sha256", "argv", "env_names", "exit", "thread_id"], "raw provider")
    record = {k: raw[k] for k in PUBLIC if k not in {"isolation", "wall_ms", "record_sha256"}}
    record["isolation"] = {k: raw["isolation"][k] for k in
                           ("repository_id", "cache_id", "truth_inaccessible", "cold_start")}
    record["wall_ms"] = raw["timing"]["wall_ms"]
    return seal(record, "record_sha256"), raw


def adapt(directories, study_kind):
    require(1 <= len(directories) <= 16 and len({str(Path(d).resolve()) for d in directories}) == len(directories),
            "adapt requires 1..16 unique directories")
    episodes, evidence = [], []
    for directory in directories:
        record, raw = raw_record(directory)
        require(study_kind != "agent" or record["model"]["provider"] == "codex-cli",
                "real cohort requires codex-cli raw captures; fixtures must be test-only")
        episodes.append(record)
        evidence.append({"run_id": record["run_id"], "directory": str(Path(directory).resolve()),
                         "artifact_sha256": raw["artifact_sha256"]})
    return {"schema_version": 1, "study_kind": study_kind,
            "episodes": episodes, "evidence": evidence}


def checked_answer(answer, case, trees):
    shape(answer, ["items", "abstain", "reason", "evidence"], "answer")
    require(type(answer["abstain"]) is bool and isinstance(answer["reason"], str)
            and len(answer["reason"].encode()) <= 4096, "answer types/bounds")
    items = answer["items"]
    require(isinstance(items, list) and len(items) <= LIMITS["answer_items"]
            and all(isinstance(i, str) and len(i) <= 1024 for i in items)
            and len(set(items)) == len(items), "answer identities")
    require(isinstance(answer["evidence"], list) and len(answer["evidence"]) <= 32, "citation count")
    require(bool(items) != answer["abstain"], "abstention shape")
    require(not answer["abstain"] or not answer["evidence"], "abstention evidence")
    if answer["abstain"]:
        text(answer["reason"], "abstention reason")
    known = {i["selector"]: i for i in case["truth"]["identities"]}
    cited, valid, identity_valid = set(), 0, set()
    for e in answer["evidence"]:
        shape(e, ["item", "file", "line", "quote"], "citation")
        safe_path(e["file"])
        integer(e["line"], "citation line")
        text(e["quote"], "citation quote")
        require(e["item"] in items, "citation for unreturned item")
        lines = trees[case["head_snapshot"]].get(e["file"], "").splitlines()
        supported = 1 <= e["line"] <= len(lines) and lines[e["line"] - 1].strip() == e["quote"]
        valid += supported
        cited.add(e["item"])
        if e["item"] in known and supported:
            identity = known[e["item"]]
            if all(e[k] == identity[k] for k in ("file", "line", "quote")):
                identity_valid.add(e["item"])
    expected = {i["selector"] for i in case["truth"]["identities"] if i["required"]}
    found = set(items)
    return {"identity_precision": len(found & set(known)) / len(found) if found else 0,
            "identity_recall": len(found & expected) / len(expected),
            "false_positives": sorted(found - set(known)),
            "citation_precision": valid / len(answer["evidence"]) if answer["evidence"] else 0,
            "citation_coverage": len(cited) / len(found) if found else 0,
            "objective_pass": expected <= found <= identity_valid
            and cited == found and valid == len(answer["evidence"])}


def audit_check(audit, record, case, trees):
    shape(audit, ["run_id", "record_sha256", "reviewer", "signed_at", "attestation", "judgments",
                  "attestation_sha256"], "audit")
    check_seal(audit, "attestation_sha256")
    require(audit["run_id"] == record["run_id"] and audit["record_sha256"] == record["record_sha256"],
            "audit not bound to exact record")
    for field in ("reviewer", "signed_at", "attestation"):
        text(audit[field], field)
    require(re.fullmatch(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ", audit["signed_at"]) is not None,
            "audit timestamp must be UTC RFC3339 seconds")
    rubric = {r["id"]: r for r in case["truth"]["rubric"]}
    require(isinstance(audit["judgments"], list) and len(audit["judgments"]) == len(rubric), "incomplete audit")
    seen, passing = set(), True
    for judgment in audit["judgments"]:
        shape(judgment, ["claim_id", "pass", "rationale", "answer_quote", "source_evidence"], "judgment")
        key = judgment["claim_id"]
        require(key in rubric and key not in seen and type(judgment["pass"]) is bool, "judgment identity")
        seen.add(key)
        text(judgment["rationale"], "judgment rationale")
        text(judgment["answer_quote"], "judgment answer_quote")
        reason = record["answer"]["reason"] if record["answer"] is not None else record["final_output"]
        require(judgment["answer_quote"] in reason, "judgment quote absent from answer explanation")
        require(isinstance(judgment["source_evidence"], list) and judgment["source_evidence"], "audit source evidence")
        for e in judgment["source_evidence"]:
            require(e in rubric[key]["evidence"], "audit source is not frozen claim evidence")
        passing &= judgment["pass"]
    return passing


def isolation_check(raw, record):
    iso = raw["isolation"]
    require(iso["containment"] == "bwrap" and iso["contained"] is True
            and iso["truth_inaccessible"] is True and iso["cold_start"] is True, "missing containment evidence")
    require(iso["probe"]["hidden"] is True and not iso["probe"]["leaked"]
            and not iso["probe"]["missing"] and iso["probe_spec_forbidden"], "failed/missing OS probe")
    critical = {"shell_tool", "apps", "plugins", "multi_agent", "browser_use", "computer_use", "view_image", "hooks", "memories"}
    require(not iso["unbrokered"] and iso["inventory"]["mcp_servers"] == ["eval_broker"]
            and critical <= set(iso["inventory"]["disabled_verified"]), "unverified tool isolation")
    require(iso["tools_listed"] == [record["request"]["tools"]], "effective tool inventory mismatch")
    require(iso["sandbox_argv_sha256"] == digest(iso["sandbox_argv"]), "sandbox argv integrity")
    require(not raw["cleanup"]["survivors"] and raw["cleanup"]["pipes_closed"] is True, "incomplete process cleanup")
    require(raw["timing"]["wall_ms"] == raw["timing"]["setup_ms"] + raw["timing"]["provider_ms"], "setup omitted from wall")


def score(data, lock, trees, bundle, audits):
    shape(bundle, ["schema_version", "study_kind", "episodes", "evidence"], "bundle")
    shape(audits, ["schema_version", "audits"], "audits")
    require(bundle["schema_version"] == audits["schema_version"] == 1, "bundle/audit version")
    require(bundle["study_kind"] in {"agent", "test-only"}, "study kind")
    expected = {r["order"]: r for r in requests(data, lock)}
    require(isinstance(bundle["episodes"], list) and len(bundle["episodes"]) == 16, "complete sixteen episodes required")
    evidence = {e["run_id"]: e for e in bundle["evidence"]}
    require(len(evidence) == len(bundle["evidence"]) == 16, "complete unique raw evidence required")
    audit_map = {a["run_id"]: a for a in audits["audits"]}
    require(len(audit_map) == len(audits["audits"]), "duplicate audit")
    cases = {c["id"]: c for c in data["cases"]}
    rows, orders, runs, repositories, caches, models, versions = [], set(), set(), set(), set(), set(), {}
    implementations, threads, chronology = set(), set(), []
    for record in bundle["episodes"]:
        shape(record, PUBLIC, "record")
        check_seal(record, "record_sha256")
        request, run = record["request"], record["run_id"]
        require(request == expected.get(request.get("order")), "request drift or unexpected order")
        require(request["order"] not in orders and run not in runs, "duplicate episode")
        orders.add(request["order"]); runs.add(run)
        text(run, "run id")
        require(bundle["study_kind"] != "agent" or record["model"]["provider"] == "codex-cli",
                "test provider cannot be labeled agent")
        shape(record["model"], ["provider", "name", "version", "settings"], "model")
        for k in ("provider", "name", "version"):
            text(record["model"][k], "model " + k)
        require(isinstance(record["model"]["settings"], dict), "settings")
        models.add(digest(record["model"]))
        shape(record["tool_versions"], COMMON + (["orbit-graph"] if request["arm"] == "graph" else []), "tool versions")
        for tool, version in record["tool_versions"].items():
            text(version, "tool version")
            require(tool not in versions or versions[tool] == version, "tool version drift")
            versions[tool] = version
        shape(record["isolation"], ["repository_id", "cache_id", "truth_inaccessible", "cold_start"], "isolation")
        for key, used in (("repository_id", repositories), ("cache_id", caches)):
            text(record["isolation"][key], key)
            require(record["isolation"][key] not in used, "reused session/source/cache")
            used.add(record["isolation"][key])
        e = evidence.get(run)
        shape(e, ["run_id", "directory", "artifact_sha256"], "raw evidence reference")
        captured, raw = raw_record(e["directory"])
        require(captured == record and e["artifact_sha256"] == raw["artifact_sha256"], "raw differs from record")
        implementations.add(digest({"runner_version": raw["runner_version"], "provider_binary": raw["provider"]["binary_sha256"],
                                    "broker_binary": raw["isolation"]["broker_sha256"], "bwrap": raw["isolation"]["bwrap"]}))
        isolation_error = None
        try:
            isolation_check(raw, record)
        except (Invalid, KeyError, TypeError) as error:
            isolation_error = str(error)
        require(raw["inputs"]["head"]["content_revision"] == request["source_revision"]
                and raw["inputs"]["base"]["content_revision"] == request["base_revision"]
                and raw["snapshot"]["work_tree_revision"] == request["source_revision"], "captured source provenance drift")
        thread = raw["provider"]["thread_id"]
        if thread is not None:
            text(thread, "provider thread id")
            require(thread not in threads, "reused provider session")
            threads.add(thread)
        elif isolation_error is None:
            isolation_error = "provider session identity unavailable"
        started = raw["started_at_unix"]
        require(type(started) in (int, float) and math.isfinite(started) and started >= 0, "captured start time")
        chronology.append((request["order"], started, record["wall_ms"]))
        case = cases[request["case_id"]]
        status = record["status"]
        require(status in {"ok", "failed", "invalid", "timeout"}, "episode status")
        for key in ("wall_ms", "output_bytes"):
            integer(record[key], key)
        require(isinstance(record["calls"], list) and len(record["calls"]) <= 1000, "call capture bound")
        for call in record["calls"]:
            shape(call, ["tool", "input", "output", "elapsed_ms", "status"], "call")
            require(isinstance(call["tool"], str) and isinstance(call["input"], str)
                    and isinstance(call["output"], str) and len(call["input"].encode()) <= LIMITS["call_bytes"]
                    and len(call["output"].encode()) <= LIMITS["call_bytes"], "call capture types/bounds")
            require(call["status"] in {"ok", "failed", "timeout", "truncated"}, "call status")
            integer(call["elapsed_ms"], "elapsed")
        require(isinstance(record["final_output"], str), "final output type")
        counted = sum(len(c["output"].encode()) for c in record["calls"]) + len(record["final_output"].encode())
        require(counted == record["output_bytes"], "output accounting mismatch")
        usage = record["usage"]
        if usage is not None:
            shape(usage, ["input_tokens", "output_tokens", "cost_usd", "source"], "usage")
            for key in ("input_tokens", "output_tokens"):
                if usage[key] is not None:
                    integer(usage[key], key)
            require(usage["cost_usd"] is None or (type(usage["cost_usd"]) in (int, float)
                    and math.isfinite(usage["cost_usd"]) and usage["cost_usd"] >= 0), "cost")
            text(usage["source"], "usage source")
        objective = {"objective_pass": False, "identity_precision": 0, "identity_recall": 0,
                     "citation_precision": 0, "citation_coverage": 0, "false_positives": []}
        semantic, answer_error = False, None
        if status == "ok":
            require(record["error"] is None and record["answer"] is not None and record["calls"], "ok record shape")
            require(parse(record["final_output"]) == record["answer"], "final answer mismatch")
            try:
                objective = checked_answer(record["answer"], case, trees)
            except (Invalid, TypeError, KeyError) as error:
                answer_error = str(error)
            require(run in audit_map, "unaudited successful answer; every rubric judgment required")
            semantic = audit_check(audit_map[run], record, case, trees)
        else:
            shape(record["error"], ["code", "message"], "failure")
            text(record["error"]["code"], "failure code"); text(record["error"]["message"], "failure message")
            require(record["answer"] is None, "failed episode answer must be null")
            if run in audit_map:
                raise Invalid("audit for failed episode; retain failure without semantic judgment")
        within_budget = record["wall_ms"] <= LIMITS["wall_ms"] and len(record["calls"]) <= 60 and counted <= LIMITS["output_bytes"]
        behavior_ok = (status == "ok" and within_budget and isolation_error is None
                       and not raw["final_output_truncated"] and not raw["calls_dropped"]
                       and all(c["tool"] in request["tools"] and c["status"] != "truncated" for c in record["calls"]))
        correct = behavior_ok and objective["objective_pass"] and semantic
        rows.append({"case_id": case["id"], "split": case["split"], "arm": request["arm"], "status": status,
                     **objective, "semantic_pass": semantic, "correct": correct, "answer_error": answer_error,
                     "isolation_error": isolation_error,
                     "error": record["error"], "within_budget": within_budget, "calls": len(record["calls"]),
                     "tool_errors": sum(c["status"] != "ok" for c in record["calls"]),
                     "wall_ms": record["wall_ms"], "setup_ms": raw["timing"]["setup_ms"],
                     "output_bytes": counted, "usage": usage})
    require(len(models) == len(implementations) == 1 and set(evidence) == runs and set(audit_map) <= runs, "cohort/evidence/audit drift")
    chronology.sort()
    require(all(b[1] >= a[1] + a[2] / 1000 - 0.002 for a, b in zip(chronology, chronology[1:])),
            "actual episode timing violates frozen sequential counterbalance order")
    paired = []
    for case in data["cases"]:
        pair = {r["arm"]: r for r in rows if r["case_id"] == case["id"]}
        b, g = pair["baseline"], pair["graph"]
        both = b["correct"] and g["correct"]
        paired.append({"case_id": case["id"], "split": case["split"], "both_correct": both,
                       "accuracy_delta_graph_minus_baseline": int(g["correct"]) - int(b["correct"]),
                       "calls_delta_graph_minus_baseline": g["calls"] - b["calls"],
                       "bytes_delta_graph_minus_baseline": g["output_bytes"] - b["output_bytes"],
                       "tool_errors_delta_graph_minus_baseline": g["tool_errors"] - b["tool_errors"],
                       "false_positives_delta_graph_minus_baseline": len(g["false_positives"]) - len(b["false_positives"]),
                       "usage_delta_graph_minus_baseline": {k: g["usage"][k] - b["usage"][k]
                         if g["usage"] is not None and b["usage"] is not None and g["usage"][k] is not None and b["usage"][k] is not None
                         else None for k in ("input_tokens", "output_tokens", "cost_usd")},
                       "wall_delta_graph_minus_baseline_ms": g["wall_ms"] - b["wall_ms"] if both else None,
                       "speed_ratio_baseline_over_graph": b["wall_ms"] / g["wall_ms"] if both and g["wall_ms"] else None})
    summaries = []
    for split in ("all", "development", "held-out"):
        for arm in ARMS:
            selected = [r for r in rows if r["arm"] == arm and (split == "all" or r["split"] == split)]
            summaries.append({"split": split, "arm": arm, "denominator": len(selected),
                              "accuracy": sum(r["correct"] for r in selected) / len(selected),
                              "false_positives": sum(len(r["false_positives"]) for r in selected),
                              "episode_errors": sum(r["status"] != "ok" or r["answer_error"] is not None or r["isolation_error"] is not None or not r["within_budget"] for r in selected),
                              "tool_errors": sum(r["tool_errors"] for r in selected),
                              "calls_total": sum(r["calls"] for r in selected),
                              "output_bytes_total": sum(r["output_bytes"] for r in selected),
                              "wall_ms_total": sum(r["wall_ms"] for r in selected),
                              "usage_known": sum(r["usage"] is not None for r in selected),
                              "usage_total": {k: sum(r["usage"][k] for r in selected) if all(r["usage"] is not None and r["usage"][k] is not None for r in selected) else None for k in ("input_tokens", "output_tokens", "cost_usd")}})
    heldout = [p for p in paired if p["split"] == "held-out"]
    eligible = (bundle["study_kind"] == "agent" and all(r["isolation_error"] is None for r in rows) and all(p["accuracy_delta_graph_minus_baseline"] >= 0 for p in heldout)
                and sum(p["accuracy_delta_graph_minus_baseline"] for p in heldout) >= 1
                and not any(r["false_positives"] for r in rows if r["arm"] == "graph" and r["split"] == "held-out"))
    return {"schema_version": 1, "study_kind": bundle["study_kind"], "corpus_sha256": digest(data),
            "records_sha256": digest(bundle), "audits_sha256": digest(audits), "episodes": rows,
            "summaries": summaries, "paired": paired, "follow_up_larger_independent_repo_cohort": eligible,
            "effectiveness_claim_permitted": False,
            "limitations": ["Exploratory task holdout within three repositories; no superiority or independent-repository generalization.",
                            "Manual attributed semantics, not automated behavior proof; hashes do not authenticate reviewer identity.",
                            "Speed is descriptive, restricted to both-correct pairs, including cold setup; unknown usage/cost stays null."]}


def main():
    parser = argparse.ArgumentParser(description=__doc__, epilog="Example: python3 eval.py validate --repo orbit-graph=/path/to/repo --repo Orbit=/path/to/orbit --repo pulsar=/path/to/pulsar")
    sub = parser.add_subparsers(dest="command", required=True)
    for command in ("validate", "export", "score"):
        p = sub.add_parser(command, help={"validate": "check frozen corpus and pinned Git source", "export": "create fresh solution-free source bundles", "score": "audit complete paired captured records"}[command])
        p.add_argument("--repo", action="append", required=True, help="repeat NAME=/absolute/local/git/repository")
        if command == "export":
            p.add_argument("--destination", required=True, help="new destination; existing paths refused")
        if command == "score":
            p.add_argument("--records", required=True, help="adapted bounded capture bundle")
            p.add_argument("--audits", required=True, help="attributed rubric judgments")
    p = sub.add_parser("adapt", help="retain raw runner captures as neutral records; no scoring")
    p.add_argument("--raw", action="append", required=True, help="repeat raw runner episode directory")
    p.add_argument("--study-kind", required=True, choices=("agent", "test-only"))
    p.add_argument("--output", required=True, help="new output file; never overwritten")
    args = parser.parse_args()
    def interrupted(signum, frame):
        raise SystemExit(128 + signum)
    signal.signal(signal.SIGTERM, interrupted)
    def expired(signum, frame):
        raise Invalid("offline helper exceeded its 600s command deadline; retain incomplete output and use a fresh destination")
    signal.signal(signal.SIGALRM, expired)
    signal.alarm(600)
    try:
        if args.command == "adapt":
            bundle = adapt(args.raw, args.study_kind)
            write_new(args.output, bundle)
            result = {"output": str(Path(args.output).resolve()), "episodes": len(bundle["episodes"])}
        else:
            repos = {}
            for arg in args.repo:
                name, sep, path = arg.partition("=")
                require(sep and name not in repos and Path(path).is_absolute(), "repo must be unique NAME=/absolute/path")
                repos[name] = Path(path).resolve(strict=True)
            data, lock = corpus()
            trees = verify(data, lock, repos)
            if args.command == "validate":
                result = {"valid": True, "cases": 8, "episodes": 16, "corpus_sha256": digest(data)}
            elif args.command == "export":
                result = export(data, lock, trees, args.destination)
            else:
                result = score(data, lock, trees, load(args.records), load(args.audits))
        print(json.dumps(result, sort_keys=True, allow_nan=False))
        return 0
    except BrokenPipeError:
        return 0
    except (Invalid, OSError, ValueError, KeyError, TypeError, RecursionError) as error:
        print(json.dumps({"code": "invalid_evaluation_input", "error": str(error), "remedy": "Check the frozen corpus, explicit paths and captured contract; use a fresh export destination."}), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
