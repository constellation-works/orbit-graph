#!/usr/bin/env python3
"""Prospective development preparation and original-capture scoring. JSON stdout; JSON errors on stderr."""
import argparse
from collections import Counter
import ctypes
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import secrets
import signal
import sys
import tempfile

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]


def module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    result = importlib.util.module_from_spec(spec)
    sys.modules[name] = result
    spec.loader.exec_module(result)
    return result


# Imported primitives remain frozen and read-only. There is no provider runner here.
legacy = module("effectiveness_source", REPO / "evals/plugin-navigation-study-v1/eval.py")
source, profile = legacy.source, legacy.profile
runner, plugin = profile.runner, profile.plugin
require, digest, seal, load = source.require, source.digest, source.seal, source.load
sys.path.insert(0, str(REPO / "evals/source-identity-v1"))
import source_identity as identity
sys.path.insert(0, str(HERE))
import scoring
import analysis
import runtime

FAMILIES = ("localization", "control_flow", "cross_module", "change_impact", "before_after", "uncertainty")
SCRIPT_PATHS = ()
PROSPECTIVE_PROFILE = "installed-plugin-skill-v3"


class JsonParser(argparse.ArgumentParser):
    def error(self, message):
        self.exit(2, json.dumps({"schema_version":1,"error":{"code":"usage_error","message":message}}) + "\n")


def sha(data):
    return hashlib.sha256(data).hexdigest()


def snapshot(repo, commit, extra=()):
    """Use the unchanged established full source boundary."""
    repo = Path(repo).resolve(strict=True)
    require(Path(source.git(repo, "rev-parse", "--show-toplevel").decode().strip()).resolve() == repo,
            "explicit Git root required")
    manifest, files = source.snapshot(repo, commit)
    blobs = {}
    entries = source.git(repo, "ls-tree", "-rz", "--full-tree", commit).split(b"\0")
    for entry in entries:
        if not entry:
            continue
        meta, raw_path = entry.split(b"\t", 1)
        path = raw_path.decode()
        mode, kind, oid = meta.decode().split()
        if path in extra:
            require(path in SCRIPT_PATHS and mode in ("100644", "100755") and kind == "blob",
                    "unreviewed script or nonregular source")
            files[path] = source.git(repo, "cat-file", "blob", oid, bound=source.MAX_BLOB).decode("utf-8")
        if path in files:
            blobs[path] = {"mode": mode, "oid": oid}
    require(set(extra) <= set(files), "missing reviewed script")
    require(all(source.included(p) or p in extra for p in files), "non-source export")
    manifest.update(content_revision=digest(files), files={p: sha(v.encode()) for p, v in sorted(files.items())},
                    blobs=blobs, script_allowlist=list(extra))
    return manifest, files


def documents():
    corpus, protocol, lock = [load(HERE / p) for p in ("corpus.json", "protocol.json", "corpus.lock.json")]
    require(corpus["schema_version"] == protocol["schema_version"] == lock["schema_version"] == 1,
            "unsupported study schema")
    require(lock["corpus_sha256"] == digest(corpus) and lock["protocol_sha256"] == digest(protocol),
            "corpus/protocol changed; prospective version required")
    for p, expected in lock["dependencies"].items():
        require(sha((REPO / p).read_bytes()) == expected, "dependency drift: " + p)
    require(lock["python_ast_version"] == f"{sys.version_info.major}.{sys.version_info.minor}",
            "Python AST runtime differs; adopt a new prospective pin")
    require(lock["runtime_version"] == runtime.VERSION and lock["profile_contract"] == PROSPECTIVE_PROFILE and
            lock["runner_version"] == runner.PROSPECTIVE_RUNNER_VERSION == "6" and
            lock["broker_version"] == runner.broker.PROSPECTIVE_BROKER_VERSION == "5", "prospective runtime version drift")
    return corpus, protocol, lock


def evidence(e, views):
    source.shape(e, ["view", "file", "start_line", "end_line", "quote", "sha256"], "source evidence")
    require(e["view"] in views and e["file"] in views[e["view"]], "evidence outside frozen universe")
    start, end = e["start_line"], e["end_line"]
    lines = views[e["view"]][e["file"]].splitlines()
    require(type(start) is int and type(end) is int and 1 <= start <= end <= len(lines), "evidence range")
    quote = "\n".join(lines[start - 1:end])
    require(e["quote"] == quote and e["sha256"] == sha((quote + "\n").encode()), "source evidence differs")


def written_declaration(selector, text):
    leaf = selector["name"].split("::")[-1].split(".")[-1]
    token = re.escape(leaf)
    kind = selector["kind"]
    if selector["language"] == "rust":
        pattern = r"\b" + re.escape(kind) + r"\s+" + token + r"\b"
    elif kind == "assignment":
        pattern = r"^\s*" + token + r"\s*(?::[^=]+)?="
    else:
        pattern = r"\b" + ("def" if kind == "function" else "class") + r"\s+" + token + r"\b"
    return re.search(pattern, text) is not None


def validate_truth(corpus, views):
    ids = [c["id"] for c in corpus["cases"]]
    require(len(ids) >= 24 and len(set(ids)) == len(ids), "24 distinct development cases required")
    counts = Counter(c["family"] for c in corpus["cases"])
    require(set(counts) == set(FAMILIES) and min(counts.values()) >= 4, "six families, four cases each")
    require(len({c["repository"] for c in corpus["cases"]}) >= 4, "four repositories required")
    langs = Counter(c["language"] for c in corpus["cases"])
    require(langs["rust"] >= 8 and langs["python"] >= 8, "language coverage")
    for c in corpus["cases"]:
        require(c["split"] == "development" and c["prompt"] and c["distinctness"], "case metadata")
        require(isinstance(c.get("component"), str) and c["component"], "declared component dependence required")
        require(c["head_view"] in views and c["base_view"] in views, "case source views")
        truth = c["truth"]
        require(truth["status"] == "source-authored" and truth["author"] and truth["method"] == "pinned-source-first",
                "truth attribution")
        require(truth["claims"] and len({x["id"] for x in truth["claims"]}) == len(truth["claims"]), "claims")
        for claim in truth["claims"]:
            require(claim["expected"] and claim["evidence"] and claim["id"] in c["prompt"], "prompt must ask each claim")
            for e in claim["evidence"]:
                evidence(e, views)
        selectors = set()
        for item in truth["identities"]:
            require(set(item) == {"selector", "view", "owner", "module", "trait", "context"}, "truth identity shape")
            s = item["selector"]
            require(s["language"] == c["language"] and item["view"] == c["head_view"], "identity view/language")
            require(set(s) == {"language", "name", "file", "line", "kind", "citation"}, "selector shape")
            identity.canonical_path(s["file"])
            require(s["kind"] in ("fn", "struct", "enum", "trait", "function", "class", "assignment"), "identity kind")
            key = digest(s)
            require(key not in selectors, "duplicate required truth")
            selectors.add(key)
            require(item["context"], "declaration/module/owner source context required")
            for e in item["context"]:
                evidence(e, views)
            lines = views[item["view"]][s["file"]].splitlines()
            ct = s["citation"]
            require(type(s["line"]) is int and 1 <= s["line"] <= len(lines), "truth line")
            require(ct["start_line"] == ct["end_line"] == s["line"] and ct["quote"] == lines[s["line"] - 1],
                    "invalid required citation")
            require(written_declaration(s, ct["quote"]), "truth declaration spelling/kind")
            require(c["truth"]["resolution"] == "qualified", "ambiguous required truth blocks admission")
        if c["family"] == "before_after":
            require(c["head_view"] != c["base_view"] and c.get("diff_review"), "accessible actual change required")
            require(c["diff_review"]["status"] == "author-reviewed" and c["diff_review"]["independent_status"] == "pending",
                    "independent diff review must be honest")
        if "absence_search" in truth:
            absent = truth["absence_search"]
            require(absent["needle"] and absent["matches"] == 0 and
                    not any(absent["needle"] in text for text in views[c["head_view"]].values()) and
                    absent["universe_sha256"] == digest({p:sha(text.encode()) for p,text in sorted(views[c["head_view"]].items())}),
                    "absence claim contradicted by full source universe")
    return counts


def verify(corpus, lock, repos):
    require(set(repos) == set(corpus["repositories"]), "supply every explicit repository root")
    views = {}
    for name, spec in corpus["views"].items():
        manifest, files = snapshot(repos[spec["repository"]], spec["commit"], spec["scripts"])
        require(manifest == lock["snapshots"][name], "source hash/blob/tree mismatch: " + name)
        views[name] = files
    validate_truth(corpus, views)
    for c in corpus["cases"]:
        if "diff_review" in c:
            review = c["diff_review"]
            base, head = (corpus["views"][c[v]]["commit"] for v in ("base_view", "head_view"))
            raw = source.git(repos[c["repository"]], "diff", "--no-ext-diff", "--no-textconv", base, head,
                             "--", *review["paths"])
            require(sha(raw) == review["diff_sha256"], "historical diff changed")
    return views


def export(corpus, lock, views, destination):
    destination = legacy.fresh(destination)
    libc = ctypes.CDLL(None, use_errno=True)
    rename = getattr(libc, "renameat2", None)
    require(rename is not None, "atomic source export requires Linux renameat2")
    rename.argtypes = [ctypes.c_int,ctypes.c_char_p,ctypes.c_int,ctypes.c_char_p,ctypes.c_uint]
    rename.restype = ctypes.c_int
    staged = Path(tempfile.mkdtemp(prefix=".source-stage-", dir=destination.parent))
    for name, files in views.items():
        for path, value in files.items():
            source.write_new(staged / "views" / name / path, value)
    # Outside every mountable view: provenance, truth and reviews never go in source roots.
    source.write_new(staged / "source-manifest.json", lock["snapshots"])
    source.write_new(staged / "complete.json", seal(dict(schema_version=1, corpus_sha256=digest(corpus),
                                                            snapshots_sha256=digest(lock["snapshots"])), "export_sha256"))
    directories=[staged]
    for entry in staged.rglob("*"):
        if entry.is_dir():
            directories.append(entry)
        else:
            fd=os.open(entry,os.O_RDONLY | os.O_NOFOLLOW)
            try:
                os.fsync(fd)
            finally:
                os.close(fd)
    for directory in sorted(directories,key=lambda p:len(p.parts),reverse=True):
        sync_directory(directory)
    # Linux's no-replace rename publishes the complete directory in one step,
    # and cannot replace even an empty concurrently created destination.
    if rename(-100,os.fsencode(staged),-100,os.fsencode(destination),1) != 0:
        error=ctypes.get_errno()
        raise OSError(error,os.strerror(error),str(destination))
    sync_directory(destination.parent)
    return {"destination": str(destination), "views": len(views), "source_only": True}


def check_export(corpus, lock, directory):
    directory = Path(directory).resolve(strict=True)
    marker = load(directory / "complete.json")
    source.check_seal(marker, "export_sha256")
    require(marker["corpus_sha256"] == digest(corpus) and marker["snapshots_sha256"] == digest(lock["snapshots"]), "export pin")
    require(load(directory / "source-manifest.json") == lock["snapshots"], "export manifest")
    views = {}
    for name, manifest in lock["snapshots"].items():
        entries = runner.tree_entries(directory / "views" / name, name)
        require({p: sha(Path(f).read_bytes()) for p, f in entries} == manifest["files"], "export file set/bytes")
        require(runner.content_revision(entries, name)["content_revision"] == manifest["content_revision"], "export revision")
        views[name] = {p: Path(f).read_text(encoding="utf-8") for p, f in entries}
    validate_truth(corpus, views)
    return views


def schedule(corpus, protocol, study_id, repetitions, treatments, cache):
    require(re.fullmatch(r"[A-Za-z0-9._-]{1,64}", study_id) is not None, "study_id")
    require(type(repetitions) is int and 1 <= repetitions <= 10, "declare 1..10 repetitions")
    require(cache == "cold", "warm/cache-amortization runtime unsupported; separate study required")
    require(treatments == ["baseline", "product"], "experimental ablation unavailable until separately qualified")
    # Balance both repository and family margins, then reverse each repetition.
    repo_total = Counter(c["repository"] for c in corpus["cases"])
    family_total = Counter(c["family"] for c in corpus["cases"])
    require(all(n % 2 == 0 for n in [*repo_total.values(), *family_total.values()]), "counterbalance requires even margins")
    chosen, rused, fused = [], Counter(), Counter()
    def assign(n):
        if n == len(corpus["cases"]):
            return all(rused[k] == v//2 for k,v in repo_total.items()) and all(fused[k] == v//2 for k,v in family_total.items())
        c = corpus["cases"][n]
        for first in (n % 2, 1-n % 2):
            if first and (rused[c["repository"]] >= repo_total[c["repository"]]//2 or fused[c["family"]] >= family_total[c["family"]]//2):
                continue
            chosen.append(first)
            rused[c["repository"]] += first
            fused[c["family"]] += first
            if assign(n+1):
                return True
            chosen.pop()
            rused[c["repository"]] -= first
            fused[c["family"]] -= first
        return False
    require(assign(0), "cannot balance repository/family order")
    slots = []
    for repeat in range(1, repetitions + 1):
        for n, case in enumerate(corpus["cases"]):
            arms = ("baseline", "graph") if (chosen[n] + repeat) % 2 else ("graph", "baseline")
            for arm in arms:
                slots.append(dict(order=len(slots), case_id=case["id"], repetition=repeat, arm=arm,
                                  pair_id=f"{case['id']}--r{repeat:02}",
                                  attempt_id=f"{study_id}.{case['id']}.r{repeat:02}.{arm}"))
    return seal(dict(schema_version=1, study_id=study_id, corpus_sha256=digest(corpus), protocol_sha256=digest(protocol),
                     repetitions=repetitions, treatments=treatments, cache=cache, slots=slots,
                     runtime_admitted=False, lock_sha256=digest(load(HERE / "corpus.lock.json")),
                     runtime_version=runtime.VERSION,
                     implementation_sha256={p:sha((HERE / p).read_bytes()) for p in ("eval.py","scoring.py","analysis.py","runtime.py")},
                     admission_pending=protocol["admission"]["pending"]), "plan_sha256")


def checked_plan(plan, corpus, protocol):
    source.check_seal(plan, "plan_sha256")
    require(plan == schedule(corpus, protocol, plan["study_id"], plan["repetitions"], plan["treatments"], plan["cache"]), "plan differs")


def requests(corpus, protocol, lock, plan, pin):
    checked_plan(plan, corpus, protocol)
    plugin.validate_pin(pin)
    cases = {c["id"]: c for c in corpus["cases"]}
    result = []
    for slot in plan["slots"]:
        c = cases[slot["case_id"]]
        head, base = (lock["snapshots"][c[v]] for v in ("head_view", "base_view"))
        request = dict(schema_version=3, profile=PROSPECTIVE_PROFILE, case_id=slot["pair_id"], order=slot["order"],
                       arm=slot["arm"], split="development", fixture=c["repository"],
                       source_revision=head["content_revision"], base_revision=base["content_revision"],
                       corpus_sha256=digest(corpus), source_commits={"head": head["git_revision"], "base": base["git_revision"]},
                       prompt=c["prompt"] + source.CONTRACT,
                       limits=protocol["limits"], setup_limits=protocol["setup_limits"], plugin=pin,
                       tools=source.COMMON + (list(plugin.TOOLS) if slot["arm"] == "graph" else []), cache_policy=runner.CACHE_POLICY)
        result.append(runner.validate_request(seal(request, "request_sha256")))
    return result


def adapt(plan, request_plan, directories, refusals, lock, corpus, kind, freeze=None, export_root=None, views=None):
    """Replay real raw directories with the existing profile; never accept copied episode dictionaries."""
    require(kind in ("test-only", "agent"), "capture kind")
    protocol = load(HERE / "protocol.json")
    ev = sys.modules[__name__]
    expected_requests = runtime.request_plan(ev, request_plan, plan, corpus, protocol, lock)
    if kind == "agent":
        runtime.checked_freeze(ev, freeze, plan, request_plan, corpus, protocol, lock, export_root, views)
    else:
        require(freeze is None, "test-only captures cannot use a live operational freeze")
    # Each original is independently replayed, including incomplete pairs and
    # failed episodes. Test-only permits uncontained fixtures, never live evidence.
    episodes = [profile.load_episode(p, diagnostic=kind == "test-only") for p in directories]
    require(len({e["request"]["order"] for e in episodes}) == len(episodes), "duplicate captured attempt")
    require(len({e["run_id"] for e in episodes}) == len(episodes), "reused capture run")
    require(len({e["isolation"]["repository_id"] for e in episodes}) == len(episodes) and
            len({e["isolation"]["cache_id"] for e in episodes}) == len(episodes), "reused state")
    if episodes:
        require(len({digest({k:e["tool_versions"][k] for k in ("read","rg","git")}) for e in episodes}) == 1,
                "baseline tool version drift in partial cohort")
        require(len({digest(e["resource_limits"]) for e in episodes}) == 1,
                "process/memory ceiling drift in partial cohort")
    cases = {c["id"]: c for c in corpus["cases"]}
    for e in episodes:
        require(e["schema_version"] == 3 and e["profile"] == PROSPECTIVE_PROFILE, "schema2 relabel cannot qualify")
        order = e["request"]["order"]
        require(0 <= order < len(plan["slots"]) and e["request"] == expected_requests[order], "unexpected capture")
        require(e["model"] == request_plan["model"] and e["harness"] == request_plan["harness"] and
                e["provider"]["binary_sha256"] == request_plan["provider_binary_sha256"], "model/harness drift")
        require(e["runner_version"] == "6" and e["reply_contract"]["version"] == profile.replies.CONTRACT,
                "prospective lifecycle/safe-text capture required")
        require(e["reply_contract"]["policy"] == profile.replies.Redactor().policy,
                "host-value masking policy not prospectively admitted")
        if kind == "agent":
            require(type(e["started_at_unix"]) in (int, float) and
                    e["started_at_unix"] >= runtime.timestamp(freeze["custody"]["recorded_at"]).timestamp(),
                    "capture predates the operational freeze")
            qualification = freeze["qualification"]
            require({k:e["tool_versions"][k] for k in ("read", "rg", "git")} == qualification["tool_versions"] and
                    e["resource_limits"] == qualification["resource_limits"] and
                    e["isolation"]["bwrap"] == qualification["bwrap"], "qualified binary/confinement/resource pins differ")
        c = cases[plan["slots"][order]["case_id"]]
        for role in ("head", "base"):
            require(e["source_provenance"][role]["selected_blobs"] ==
                    {p: b["oid"] for p, b in lock["snapshots"][c[role + "_view"]]["blobs"].items()}, "source universe drift")
    if episodes and not refusals:
        profile.replay(request_plan, directories, diagnostic=kind == "test-only")
    records = []
    for e, path in zip(episodes, directories):
        records.append(dict(order=e["request"]["order"], capture=str(Path(path).resolve()), artifact_sha256=e["artifact_sha256"], episode=e))
    for refusal in refusals:
        source.shape(refusal, ["order", "status", "error", "operator", "evidence", "elapsed_ms"], "setup refusal")
        require(refusal["status"] == "setup_refused" and refusal["error"] and refusal["operator"] and refusal["evidence"], "attributed refusal required")
        require(type(refusal["order"]) is int and 0 <= refusal["order"] < len(plan["slots"]), "refusal order")
        require(isinstance(refusal["error"],dict) and refusal["error"].get("code") and refusal["error"].get("message"), "refusal error code/message")
        require(type(refusal["elapsed_ms"]) is int and refusal["elapsed_ms"] >= 0, "refusal time")
        records.append(refusal)
    require(sorted(r["order"] for r in records) == list(range(len(plan["slots"]))), "missing/duplicated scheduled attempt")
    return seal(dict(schema_version=1, runtime_version=runtime.VERSION, study_kind=kind, plan_sha256=plan["plan_sha256"],
                     request_plan=request_plan, freeze=freeze, records=sorted(records, key=lambda r:r["order"])), "bundle_sha256")


def checked_bundle(bundle, plan, corpus, lock, export_root=None, views=None):
    source.check_seal(bundle, "bundle_sha256")
    require(bundle["plan_sha256"] == plan["plan_sha256"], "bundle plan differs")
    require(bundle["study_kind"] in ("test-only", "agent") and bundle.get("runtime_version") == runtime.VERSION,
            "fabricated/live evidence refused: unsupported bundle runtime")
    records = bundle["records"]
    require([r["order"] for r in records] == list(range(len(plan["slots"]))), "missing/duplicate attempts")
    # Replay again at scoring/packet time, binding originals rather than trusting a seal.
    captures = [r for r in records if "episode" in r]
    rebuilt = adapt(plan, bundle["request_plan"], [r["capture"] for r in captures],
                    [r for r in records if "episode" not in r], lock, corpus, bundle["study_kind"], bundle["freeze"], export_root, views)
    require(rebuilt == bundle, "capture changed since adaptation")


def write(path, value):
    path = legacy.fresh(path)
    durable_new(path, value)


def sync_directory(path):
    fd=os.open(path,os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def durable_new(path,value):
    """Reuse the source writer, then publish an immutable file without overwrite."""
    path=Path(path)
    path.parent.mkdir(parents=True,exist_ok=True,mode=0o700)
    staged=path.parent / (".file-stage-"+secrets.token_hex(16))
    created=False
    try:
        source.write_new(staged,value)
        created=True
        fd=os.open(staged,os.O_RDONLY | os.O_NOFOLLOW)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)
        os.link(staged,path,follow_symlinks=False)
        sync_directory(path.parent)
    finally:
        if created and staged.exists():
            staged.unlink()
            sync_directory(path.parent)


def diagnostic_fixture(value, plan, protocol):
    """Explicit synthetic scorer input. Never an episode capture or live admission."""
    source.shape(value, ["schema_version", "kind", "study_kind", "plan_sha256", "records"], "offline fixture")
    require(value["schema_version"] == 1 and value["kind"] == "offline-scoring-fixture-v1" and
            value["study_kind"] == "test-only" and value["plan_sha256"] == plan["plan_sha256"],
            "fabricated evidence refused: only explicitly test-only offline fixtures allowed")
    records = value["records"]
    require(isinstance(records, list) and [r["order"] for r in records] == list(range(len(plan["slots"]))),
            "missing/duplicated fixture attempt")
    for record in records:
        source.shape(record, ["order", "episode"], "fixture record")
        require(type(record["order"]) is int, "fixture order must be integer")
        e = record["episode"]
        source.shape(e, ["status", "error", "answer", "calls", "timing", "usage", "usage_raw", "output_bytes", "setup_output_bytes"] +
                     (["telemetry"] if "telemetry" in e else []), "fixture outcome")
        require(e["status"] in ("ok", "failed", "invalid", "timeout"), "fixture status")
        if e["status"] == "ok":
            require(e["error"] is None, "ok fixture error")
            runner.check_answer(e["answer"], protocol["limits"])
        else:
            require(e["answer"] is None and isinstance(e["error"], dict) and e["error"].get("code") and e["error"].get("message"), "failed fixture must retain error")
        require(isinstance(e["calls"], list), "fixture calls")
        for call in e["calls"]:
            source.shape(call, ["tool", "status"], "fixture call")
            require(call["tool"] in [*source.COMMON, *plugin.TOOLS] and call["status"] in ("ok", "failed", "timeout"), "fixture call values")
        source.shape(e["timing"], ["wall_ms", "setup_ms", "provider_ms", "graph_sync_ms", "plugin_install_ms"], "fixture timing")
        for key, number in e["timing"].items():
            require(number is None or type(number) is int and number >= 0, "fixture clock value")
        require(e["timing"]["wall_ms"] is not None and e["timing"]["setup_ms"] is not None and
                e["timing"]["provider_ms"] is not None and e["timing"]["wall_ms"] ==
                e["timing"]["setup_ms"] + e["timing"]["provider_ms"], "fixture total/setup/provider double counting")
        for key in ("usage", "usage_raw"):
            require(e[key] is None or isinstance(e[key], dict) and
                    set(e[key]) <= {"input_tokens", "cached_input_tokens", "output_tokens", "total_tokens", "cost_usd"} and
                    all(v is None or type(v) is int and v >= 0 for v in e[key].values()), "fixture telemetry")
        require(all(type(e[k]) is int and e[k] >= 0 for k in ("output_bytes", "setup_output_bytes")), "fixture byte costs")
        if "telemetry" in e:
            t=e["telemetry"]
            require(t["contract"] == "prospective-accounting-v1", "fixture prospective telemetry contract")
            for value in t["usage"]["fields"].values():
                require(value["coverage"] in ("complete","partial","inconsistent") and
                        type(value["expected_turns"]) is int and type(value["reported_turns"]) is int and
                        0 <= value["reported_turns"] <= value["expected_turns"], "fixture field coverage")
                require(all(value[k] is None or type(value[k]) is int and value[k] >= 0 for k in ("observed_sum","total")),
                        "fixture telemetry counter")
                require(value["total"] is None or value["coverage"] == "complete" and value["expected_turns"] == 1,
                        "unqualified multi-turn/partial fixture totals")
            require(t["tools"]["attempts"] == len(t["attempts"]) and
                    t["tools"]["graph_attempts"] == sum(a["tool"].startswith("graph_") for a in t["attempts"]),
                    "fixture all-attempt accounting")
    return dict(schema_version=1, study_kind="test-only", plan_sha256=plan["plan_sha256"], records=records,
                provenance="offline-scoring-fixture-v1; no raw replay or agent evidence")


def main():
    parser = JsonParser(description=__doc__)
    parser.add_argument("--json", action="store_true", help="Emit structured JSON (the default); help remains text.")
    nouns = parser.add_subparsers(dest="noun", required=True)
    study = nouns.add_parser("study", help="Prepare and analyze the offline development study.")
    commands = study.add_subparsers(dest="command", required=True)
    definitions = {
        "repo": dict(action="append", default=[], metavar="NAME=ROOT", help="Explicit pinned repository root; repeat for all four repositories."),
        "destination": dict(type=Path, help="Fresh output path with an existing physical parent; never overwrite."),
        "export": dict(type=Path, help="Previously verified source-only export directory."),
        "plan": dict(type=Path, help="Frozen offline schedule JSON."),
        "pin": dict(type=Path, help="Existing installed-plugin treatment pin JSON; does not install or qualify it."),
        "model-pin": dict(type=Path, help="Optional exact model and provider_binary_sha256 document; emits a complete request_plan without running a provider."),
        "request-plan": dict(type=Path, help="Schema-3 profile plan with exact requests, model/settings, provider binary and all five harness pins."),
        "freeze": dict(type=Path, help="Operational freeze with externally attested qualification, independent truth and operator custody; required for agent admission."),
        "qualification": dict(type=Path, help="External root strict namespace/client qualification document with exact runtime pins and evidence file hashes."),
        "custody": dict(type=Path, help="External operator custody document attesting serial schedule, independent reviewer identity and freeze before output."),
        "raw": dict(action="append", default=[], help="Original capture directory; replayed offline, repeat for every captured attempt."),
        "refusals": dict(type=Path, help="Attributed setup-refusal JSON array, one record for each refused schedule slot."),
        "bundle": dict(type=Path, help="Adapted bundle JSON retaining original capture references."),
        "reviews": dict(type=Path, help="Attributed answer identity/semantic review document."),
        "review-packets": dict(type=Path, help="Frozen operator packet registry from study review; keep bindings outside reviewer view."),
        "truth-reviews": dict(type=Path, help="Independent frozen truth identity/semantic reviews and adjudications."),
        "study-kind": dict(default="test-only", choices=["test-only", "agent"], help="Evidence class; default test-only, agent requires a qualified operational freeze and strict original replay."),
        "study-id": dict(default="graph-effectiveness-dev-v1", help="Stable declared study identifier."),
        "repetitions": dict(type=int, default=1, help="Declared paired sessions per case/arm, 1 through 10."),
        "treatments": dict(default="baseline,product", help="Declared treatments; only baseline,product currently qualified."),
        "cache": dict(default="cold", help="Declared cache study; warm is refused until separately qualified."),
        "report": dict(type=Path, help="Scored development report for power/precision planning."),
        "fixture": dict(type=Path, help="Explicit offline-scoring-fixture-v1 test data; never original captures or agent evidence.")
    }
    surfaces = {
        "validate": ("Verify corpus/source hashes and expose independent-review admission coverage.", ["repo", "export", "truth-reviews"]),
        "export": ("Export full frozen source universes with manifests outside mountable views.", ["repo", "destination"]),
        "plan": ("Prepare a counterbalanced all-attempt schedule, without live admission.", ["study-id", "repetitions", "treatments", "cache", "destination"]),
        "requests": ("Prepare requests validated by the delivered schema-3 runner; does not grant runtime admission.", ["plan", "pin", "model-pin", "destination"]),
        "freeze": ("Bind a prospective operational freeze to independent truth, external qualification and custody before dispatch.", ["plan", "request-plan", "export", "truth-reviews", "qualification", "custody", "destination"]),
        "truth-packets": ("Prepare source-only independent truth review packets.", ["repo", "export", "destination"]),
        "review": ("Prepare arm-blinded answer packets from replayed captures or explicit test fixtures.", ["plan", "export", "bundle", "fixture", "destination"]),
        "adapt": ("Replay every original installed-plugin capture and retain setup refusals.", ["plan", "request-plan", "raw", "refusals", "study-kind", "freeze", "export", "destination"]),
        "score": ("Score every scheduled attempt with explicit pending review/missingness.", ["plan", "export", "bundle", "fixture", "reviews", "review-packets", "truth-reviews", "destination"]),
        "precision": ("Compute prospective sample/precision diagnostics from completed development judgments.", ["report", "destination"])
    }
    for name, (help_text, options) in surfaces.items():
        command = commands.add_parser(name, help=help_text, description=help_text)
        for option in options:
            command.add_argument("--" + option, **definitions[option])
    parser.set_defaults(**{key.replace("-", "_"): value.get("default") for key,value in definitions.items()})
    # Declare the global once and accept it on either side of the command.
    arguments=sys.argv[1:]
    if "--json" in arguments:
        arguments=["--json",*[value for value in arguments if value != "--json"]]
    args = parser.parse_args(arguments)
    try:
        corpus, protocol, lock = documents()
        repos = {}
        for value in args.repo or []:
            name, sep, path = value.partition("=")
            require(sep and name not in repos, "duplicate/malformed explicit repo root")
            repos[name] = Path(path).resolve(strict=True)
        views = verify(corpus, lock, repos) if repos else check_export(corpus, lock, args.export) if args.export else None
        if args.command == "validate":
            require(views is not None, "validation requires pinned repositories or export")
            result = dict(schema_version=1, cases=len(corpus["cases"]), families=dict(Counter(c["family"] for c in corpus["cases"])),
                          source_verified=True, offline_ready=True, runtime_admitted=False,
                          truth_admission=scoring.truth_admission(corpus, args.export, load(args.truth_reviews) if args.truth_reviews else None, views))
        elif args.command == "export":
            require(views is not None and args.destination is not None, "export inputs")
            result = export(corpus, lock, views, args.destination)
        elif args.command == "plan":
            result = schedule(corpus, protocol, args.study_id, args.repetitions, args.treatments.split(","), args.cache)
        elif args.command == "requests":
            require(args.plan and args.pin, "plan and qualified treatment pin required; no fabricated pin")
            result = dict(schema_version=1, runtime_admitted=False, requests=requests(corpus, protocol, lock, load(args.plan), load(args.pin)))
            if args.model_pin:
                model_pin = load(args.model_pin)
                source.shape(model_pin, ["model", "provider_binary_sha256"], "model pin")
                rp = dict(schema_version=3, profile=PROSPECTIVE_PROFILE, **model_pin,
                          harness={p:sha((REPO / "scripts/agent-eval" / p).read_bytes()) for p in runtime.HARNESS},
                          requests=result["requests"])
                runtime.request_plan(sys.modules[__name__], rp, load(args.plan), corpus, protocol, lock)
                result["request_plan"] = rp
        elif args.command == "truth-packets":
            require(views is not None, "source views required")
            result = scoring.truth_packets(corpus, lock)
        elif args.command == "freeze":
            require(args.plan and args.request_plan and args.truth_reviews and args.qualification and args.custody and args.export,
                    "freeze requires plan, request-plan, full export, truth-reviews, qualification and custody")
            result = runtime.freeze(sys.modules[__name__], load(args.plan), load(args.request_plan), corpus, protocol, lock,
                                    args.export, views, load(args.truth_reviews), load(args.qualification), load(args.custody))
        elif args.command in ("adapt", "review", "score"):
            require(args.plan, "frozen offline plan required")
            plan = load(args.plan)
            checked_plan(plan, corpus, protocol)
            if args.command == "adapt":
                require(args.request_plan, "profile request plan required")
                result = adapt(plan, load(args.request_plan), args.raw or [], load(args.refusals) if args.refusals else [], lock, corpus,
                               args.study_kind, load(args.freeze) if args.freeze else None, args.export, views)
            else:
                require(bool(args.bundle) != bool(args.fixture) and views is not None and args.export,
                        "exactly one --bundle or explicit test-only --fixture and verified --export required")
                if args.bundle:
                    bundle = load(args.bundle)
                    checked_bundle(bundle, plan, corpus, lock, args.export, views)
                else:
                    bundle = diagnostic_fixture(load(args.fixture), plan, protocol)
                if args.command == "review":
                    result = scoring.review_packets(corpus, plan, bundle)
                else:
                    if bundle["study_kind"] == "agent" and args.truth_reviews:
                        require(load(args.truth_reviews) == bundle["freeze"]["truth_reviews"], "truth reviews differ from operational freeze")
                    result = scoring.score(corpus, protocol, plan, bundle, args.export, views,
                                           load(args.reviews) if args.reviews else {"schema_version":1,"reviews":[],"adjudications":[]},
                                           load(args.truth_reviews) if args.truth_reviews else (bundle.get("freeze") or {}).get("truth_reviews"),
                                           load(args.review_packets) if args.review_packets else None)
                    result["provenance"] = bundle.get("provenance", "original schema3 installed-plugin capture replay (" + bundle["study_kind"] + ")")
                    result["runtime_freeze"] = bundle.get("freeze")
        else:
            require(args.report, "precision requires --report with completed development judgments")
            result = analysis.precision(load(args.report), protocol)
        if args.destination:
            if args.command != "export":
                write(args.destination, result)
            print(json.dumps({"destination": str(args.destination), "complete": True}))
        else:
            print(json.dumps(result, sort_keys=True, allow_nan=False))
    except BrokenPipeError:
        return 0
    except (ValueError, OSError, KeyError, TypeError, IndexError) as error:
        print(json.dumps({"schema_version":1,"error":{"code":"study_refused","message":str(error)}}), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    def expired(*_):
        raise source.Invalid("offline command exceeded 600-second deadline")
    signal.signal(signal.SIGALRM, expired)
    signal.alarm(600)
    sys.exit(main())
