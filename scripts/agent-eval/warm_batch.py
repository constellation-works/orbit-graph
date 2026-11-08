"""Prospective warm batch lifecycle; shared runner owns every provider/tool process.

No implicit resume. A batch directory is single-use. All scheduled slots are
written before launch, and a stopped batch retains unattempted slots. Hash seals
are drift checks, not authentication against a same-user evidence editor.
"""
import copy
import secrets
import importlib.util
import json
import os
from pathlib import Path
import shutil
import signal
import stat
import time

import plugin_profile as plugin

PROFILE = plugin.WARM_PROFILE
CACHE_POLICY = "empty-start-declared-private-state-fixed-batch-v1"
POLICY = {
    "version": 1, "reuse": ["installation", "orbit-home/.orbit", "orbit-home/.agents/skills", "orbit-home/.claude/skills", "repo/.orbit", "repo/.orbit-graph"],
    "fresh": ["model-session", "home", "codex-home", "tmp", "logs", "runtime", "source-git-view"],
    "cache_hit": "unknown-unless-product-explicitly-reports-hit-or-miss",
    "source": "pinned-code-files-two-commit-view", "retry": "none",
    "clock": "batch-admission-through-last-slot-finalization-including-all-setup",
    "os_cache": "uncontrolled-recorded-no-selective-warmup-counterbalanced-batch-order",
}
HARNESS = ("eval_runner.py", "eval_broker.py", "plugin_profile.py", "reply_provenance.py", "telemetry.py", "warm_batch.py")


def runner_module():
    import eval_runner
    return eval_runner


def check(value, message):
    if not value:
        raise ValueError("warm batch: " + message)


def write(path, value):
    """Publish an immutable record atomically; existing evidence is never replaced."""
    r = runner_module()
    path = Path(path)
    pending = path.with_name('.pending-' + path.name + '-' + secrets.token_hex(8))
    try:
        info = r.write_private(pending, json.dumps(value, indent=2, sort_keys=True) + "\n", r.Redactor())
        check(info["redactions"] == 0, "evidence requires exact non-secret bytes")
        with pending.open('rb') as stream:
            os.fsync(stream.fileno())
        os.link(pending, path, follow_symlinks=False)  # exclusive, atomic publication
        pending.unlink()
        fd = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)
    finally:
        if pending.exists():
            pending.unlink()


def sealed(value, key):
    return runner_module().seal(value, key)


def verify_seal(value, key):
    check(value.get(key) == plugin.digest({k: v for k, v in value.items() if k != key}), key + " differs")


def manifest(root):
    """Bounded lstat walk; never follow a product-created link or special file."""
    r = runner_module()
    result, size = {}, 0
    if not os.path.lexists(root):
        return result
    root_info = root.lstat()
    check(stat.S_ISDIR(root_info.st_mode) and root_info.st_uid == os.getuid() and not root_info.st_mode & 0o022, "unsafe state root")
    for directory, dirs, files in os.walk(root, followlinks=False):
        for name in sorted(dirs + files):
            check(len(result) < 20000, "state manifest entry bound exceeded")
            path = Path(directory) / name
            info = path.lstat()
            check(info.st_uid == os.getuid() and (stat.S_ISLNK(info.st_mode) or not info.st_mode & 0o022), "unsafe state ownership/mode: " + str(path) + " " + oct(info.st_mode))
            relative = str(path.relative_to(root))
            parts = Path(relative).parts
            if parts[0] in {".agents", ".claude"}:
                check((len(parts) == 1 or (len(parts) == 2 and parts[1] == "skills")) and stat.S_ISDIR(info.st_mode)
                      or stat.S_ISLNK(info.st_mode), "undeclared installation projection state")
            if stat.S_ISLNK(info.st_mode):
                parts = Path(relative).parts
                check(len(parts) == 3 and parts[0] in {".agents", ".claude"} and parts[1] == "skills"
                      and parts[2] in {"orbit", "orbit-setup", "orbit-orchestrate", "graph-orbit-graph"}, "undeclared state symlink")
                target = str(path.readlink())
                prefix = "/eval/product-home/" if target.startswith("/eval/product-home/") else str(root) + "/"
                check(target.startswith(prefix), "projection escapes private home")
                selected = target.removeprefix(prefix)
                expected = ".orbit/skills/" + parts[2]
                selected_parts = Path(selected).parts
                graph_target = (len(selected_parts) == 6 and selected_parts[:3] == (".orbit", "plugins", "graph")
                                and selected_parts[3] not in {".", ".."} and selected_parts[4:] == ("skills", "orbit-graph"))
                check(selected == expected or (parts[2] == "graph-orbit-graph" and graph_target), "projection target differs")
                check((root / selected).resolve(strict=True).is_relative_to(root.resolve()), "projection target escapes")
                result[relative] = {"kind": "installation-projection", "target": target}
            elif stat.S_ISDIR(info.st_mode):
                result[relative] = {"kind": "directory"}
            else:
                check(stat.S_ISREG(info.st_mode) and info.st_nlink == 1, "state link or special file")
                size += info.st_size
                check(size <= 2 * 1024**3 and len(result) < 20000, "state manifest bound exceeded")
                result[relative] = {"kind": "file", "bytes": info.st_size, "sha256": r.sha256_file(path)}
    return result


def cache_observations(calls):
    """Keep product facts and attempt positions; index freshness is not a cache hit."""
    result = []
    for position, call in enumerate(calls):
        if not call["tool"].startswith("graph_"):
            continue
        body = json.loads(call["output"])
        product = body.get("product_reply", {}).get("structuredContent")
        result.append({"call": position, "tool": call["tool"], "status": call["status"],
                       "cache": "unknown", "evidence": product,
                       "sync_attempt": call["tool"] == "graph_maintain"})
    return result


class Context:
    def __init__(self, root, arm):
        self.root, self.arm = root, arm
        self.repo, self.home, self.installation = root / "repo", root / "orbit-home", root / "installation"
        self.held = root / "held"
        for path in (root, self.home, self.installation, self.held):
            runner_module().make_private_dir(path)
        self.treatment = None
        self.expected = self.state()
        self.install_manifest = None
        self.installed_home = None
        self.install_slot = None
        self.cache_id = "batch:" + root.parent.name + ":" + arm
        self.slot = 0
        self.cancelled = False

    def state(self):
        check(set(os.listdir(self.root)) <= {"repo", "orbit-home", "installation", "held"}, "undeclared product state")
        check(set(os.listdir(self.home)) <= {".orbit", ".agents", ".claude"}, "undeclared private HOME state: " + repr(sorted(os.listdir(self.home))))
        check(set(os.listdir(self.held)) <= {".orbit", ".orbit-graph"}, "undeclared held state")
        return {"installation": manifest(self.installation), "orbit-home": manifest(self.home),
                "workspace": manifest(self.held)}

    def immutable_home(self, state):
        return {p: v for p, v in state["orbit-home"].items() if p.startswith((".orbit/plugins/", ".orbit/skills/", ".orbit/resources/", ".agents/", ".claude/"))}

    def before(self):
        check(not self.repo.exists(), "previous source view not finalized")
        actual = self.state()
        check(actual == self.expected, "state changed between scheduled slots")
        if self.install_manifest is not None:
            check(actual["installation"] == self.install_manifest and self.immutable_home(actual) == self.installed_home, "installed bytes changed")
        return actual

    def restore(self, episode):
        for name in (".orbit", ".orbit-graph"):
            path = self.held / name
            if path.exists():
                path.rename(self.repo / name)
        if self.treatment:
            with (self.repo / ".git/info/exclude").open("a") as stream:
                stream.write(".orbit/\n")

    def reuse(self, episode):
        for name in ("orbit", "orbit_graph"):
            episode.binaries[name] = str(self.installation / "bin" / name.replace("_", "-"))
        write(episode.out / "plugin-setup.json", self.treatment["setup"])
        return copy.deepcopy(self.treatment)

    def pin_installation(self, episode):
        # Capture immediately after installation, before the first provider task.
        self.treatment = copy.deepcopy(episode.treatment)
        self.install_manifest = manifest(self.installation)
        self.installed_home = self.immutable_home({"orbit-home": manifest(self.home)})
        self.install_slot = self.slot

    def evidence(self, episode, artifact):
        reused = self.install_slot is not None and self.slot != self.install_slot
        if reused:
            artifact["timing"]["plugin_install_ms"] = 0
            artifact["setup_output_bytes"] = 0
        return {"profile": PROFILE, "slot": self.slot, "cache_id": self.cache_id,
                "reused_installation": reused, "state_policy": POLICY,
                "identity": {"source_revision": episode.request["source_revision"], "source_commits": episode.request["source_commits"],
                             "plugin": episode.request["plugin"], "cache_id": self.cache_id},
                "cache_observations": cache_observations(artifact["calls"])}

    def finish(self, out):
        # Archive source/Git only; only the four declared state roots survive.
        if self.repo.exists():
            for name in (".orbit", ".orbit-graph"):
                path = self.repo / name
                if path.exists():
                    path.rename(self.held / name)
            archive = out / "snapshot/repo"
            archive.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            self.repo.rename(archive)
        self.expected = self.state()
        if self.install_manifest is not None:
            check(self.expected["installation"] == self.install_manifest and self.immutable_home(self.expected) == self.installed_home, "installed bytes changed during task")
        return self.expected


def validate_plan(plan):
    r = runner_module()
    check(set(plan) == {"schema_version", "profile", "study_kind", "batch_id", "arm", "runtime", "policy", "slots", "study_files", "freeze_sha256"}, "freeze fields differ")
    verify_seal(plan, "freeze_sha256")
    check(type(plan["schema_version"]) is int and plan["schema_version"] == 1 and plan["profile"] == PROFILE and plan["policy"] == POLICY, "profile/policy differs")
    check(plan["study_kind"] in {"test-only", "agent"}, "study kind")
    check(isinstance(plan["batch_id"], str) and r.CASE_ID.fullmatch(plan["batch_id"]), "batch id")
    check(plan["arm"] in r.ARMS and 3 <= len(plan["slots"]) <= 100, "batch needs at least three fixed slots")
    study_root = r.HERE.parent.parent / "evals/warm-navigation-v1"
    check(plan["study_files"] == {n: r.sha256_file(study_root / n) for n in ("eval.py", "protocol.json", "corpus.lock.json")}, "frozen study/lock drift")
    runtime = plan["runtime"]
    check(set(runtime) == {"model", "provider_binary_sha256", "harness", "binaries", "settings", "containment", "allow_code_mode", "provider_env", "approval_reference", "resource_limits"}, "runtime fields")
    check(runtime["harness"] == {n: r.sha256_file(r.HERE / n) for n in HARNESS}, "frozen helper drift")
    check(set(runtime["binaries"]) == {"codex", "orbit", "orbit_graph", "python", "git", "rg", "bwrap"}, "binary pin fields")
    check(runtime["provider_binary_sha256"] == runtime["binaries"]["codex"], "provider hash differs")
    check(runtime["model"]["provider"] == "codex-cli" and runtime["model"]["settings"] == runtime["settings"], "model/settings contract differs")
    check(runtime["approval_reference"], "approval reference required (pending is offline only)")
    if plan["study_kind"] == "agent":
        check(set(runtime["resource_limits"]) == {"memory.max", "pids.max"} and
              all(type(v) is int and v > 0 for v in runtime["resource_limits"].values()), "finite resource pins required")
        check(runtime["containment"] == "bwrap" and "fake" not in runtime["model"]["version"].lower(), "agent freeze needs strict real client")
        check(not runtime["approval_reference"].startswith("pending"), "provider approval pending")
    for i, slot in enumerate(plan["slots"]):
        check(set(slot) == {"request", "head_view", "base_view"}, "slot fields")
        for view in (slot["head_view"], slot["base_view"]):
            check(isinstance(view, str) and r.CASE_ID.fullmatch(view), "unsafe export view")
        q = r.validate_request(slot["request"])
        check(q["schema_version"] == 5 and q["profile"] == PROFILE and q["arm"] == plan["arm"]
              and q["order"] == i and q["cache_policy"] == CACHE_POLICY, "ordered request/profile differs")
        check(q["plugin"] == plan["slots"][0]["request"]["plugin"], "plugin changed during batch")
        check(q["limits"] == plan["slots"][0]["request"]["limits"] and q["setup_limits"] == plan["slots"][0]["request"]["setup_limits"], "budget changed during batch")
    revisions = [s["request"]["source_revision"] for s in plan["slots"]]
    check(revisions[0] == revisions[1] and revisions[-1] != revisions[0], "qualification needs unchanged tasks followed by pinned edit")
    return plan


def paired(left, right):
    """Validate equality of ordered controls, except the declared graph treatment."""
    validate_plan(left); validate_plan(right)
    check({left["arm"], right["arm"]} == {"baseline", "graph"}, "paired arms required")
    check(left["runtime"] == right["runtime"] and left["study_kind"] == right["study_kind"], "paired runtime differs")
    def controls(plan):
        return [dict(s, request={k: v for k, v in s["request"].items()
                                 if k not in {"arm", "tools", "request_sha256"}}) for s in plan["slots"]]
    check(controls(left) == controls(right), "paired schedule/source/control differs")


def run(args):
    previous_umask = os.umask(0o077)
    try:
        return _run(args)
    finally:
        os.umask(previous_umask)


def _run(args):
    r = runner_module()
    start = time.monotonic_ns()
    plan = validate_plan(r.load_json(args.plan))
    out = Path(args.out).absolute()
    check(not os.path.lexists(out) and out.parent.is_dir(), "batch output must be new")
    check(shutil.disk_usage(out.parent).used / shutil.disk_usage(out.parent).total < .8, "disk usage is at least 80%")
    # The only batch wall includes frozen-plan admission above. Per-episode
    # setup/provider spans are overlapping detail.
    r.make_private_dir(out)
    write(out / "freeze.json", plan)
    context = Context(out / "product", plan["arm"])
    context.cache_id = "batch:" + plan["batch_id"] + ":" + secrets.token_hex(8)
    rows = [{"slot": i, "status": "not_run", "reason": "batch_incomplete", "request_digest": plugin.digest(s["request"])}
            for i, s in enumerate(plan["slots"])]
    record = {"schema_version": 1, "profile": PROFILE, "freeze_sha256": plan["freeze_sha256"],
              "slots": rows, "cache_id": context.cache_id, "clock_id": Path("/proc/sys/kernel/random/boot_id").read_text().strip(), "start_ns": start, "end_ns": None, "complete": False}
    def checkpoint():
        record["end_ns"] = time.monotonic_ns()
        write(out / ("checkpoint-%03d.json" % len(list(out.glob('checkpoint-*.json')))), sealed(copy.deepcopy(record), "batch_sha256"))
    checkpoint()  # persisted schedule before any provider process
    stopped = False
    previous_handlers = {}
    def cancel(*_):
        context.cancelled = True
    for sig in (signal.SIGINT, signal.SIGTERM):
        previous_handlers[sig] = signal.signal(sig, cancel)
    try:
        for i, slot in enumerate(plan["slots"]):
            if stopped or context.cancelled:
                break
            context.slot = i
            row = rows[i]
            row.update(start_ns=time.monotonic_ns(), before=context.before())
            episode_out = out / ("slot-%03d" % i)
            request_path = out / ("request-%03d.json" % i)
            write(request_path, slot["request"])
            local = copy.copy(args)
            local.request, local.out = str(request_path), str(episode_out)
            local.head = str(Path(args.export) / slot["head_view"])
            local.base = str(Path(args.export) / slot["base_view"])
            local.batch_context = context
            try:
                episode = r.Episode(local, slot["request"])
                runtime = plan["runtime"]
                check(episode.recorded_settings() == runtime["settings"] and args.model == runtime["model"]["name"], "runtime settings/model drift")
                check(args.containment == runtime["containment"] and bool(args.allow_code_mode) == runtime["allow_code_mode"]
                      and args.provider_env == runtime["provider_env"], "runtime execution policy drift")
                check({n: r.sha256_file(r.resolve_binary(getattr(args, n), n)) if getattr(args, n, None) and (n != "bwrap" or episode.contained) else None
                       for n in runtime["binaries"]} == runtime["binaries"], "runtime binary drift")
                check(r.probe_version([episode.binaries["codex"], "--version"], episode.env()) == runtime["model"]["version"], "provider version drift")
                check(episode.resource_limits == runtime["resource_limits"], "resource limit drift")
                report = r.command_run(local)
                artifact = r.load_json(episode_out / "episode.json", 64 * 1024**2)
                check(artifact["model"] == runtime["model"], "captured model drift")
                row.update(status=report["status"], artifact_sha256=report["artifact_sha256"], directory=episode_out.name)
                stopped = bool(artifact["cleanup"]["survivors"]) or artifact["status"] == "invalid"
            except (ValueError, OSError, KeyError, TypeError, r.CapabilityRefusal) as error:
                row.update(status="refused", reason=str(error), directory=episode_out.name)
                refusal = {"status": "refused", "request_digest": row["request_digest"], "error": str(error),
                           "code": getattr(error, "code", "setup_invalid"),
                           "evidence": getattr(error, "evidence", {}),
                           "files": {p.name: {"sha256": r.sha256_file(p), "bytes": p.stat().st_size}
                                     for p in [episode_out / "plugin-setup.json"] if p.is_file()}}
                row["refusal_sha256"] = plugin.digest(refusal)
                write(out / ("refusal-%03d.json" % i), refusal)
                stopped = True
            try:
                row["after"] = context.finish(episode_out)
            except (ValueError, OSError) as error:
                row.update(state_error=str(error))
                stopped = True
            row["end_ns"] = time.monotonic_ns()
            checkpoint()
        record["complete"] = all(row["status"] != "not_run" for row in rows)
        record["cancelled"] = context.cancelled
        record["end_ns"] = time.monotonic_ns()
        write(out / "batch.json", sealed(record, "batch_sha256"))
    finally:
        for sig, handler in previous_handlers.items():
            signal.signal(sig, handler)
    return {"batch": str(out / "batch.json"), "complete": record["complete"], "scheduled": len(rows), "outcomes": [row["status"] for row in rows]}


def replay(directory, diagnostic=False):
    """Original capture replay; partial batches stay partial, never become smaller batches."""
    r = runner_module()
    directory = Path(directory)
    plan = validate_plan(r.load_json(directory / "freeze.json"))
    record = r.load_json(directory / "batch.json", 64 * 1024**2)
    verify_seal(record, "batch_sha256")
    check(type(record.get("schema_version")) is int and record["schema_version"] == 1, "unsupported batch schema")
    check(record["freeze_sha256"] == plan["freeze_sha256"] and len(record["slots"]) == len(plan["slots"]), "batch schedule drift")
    path = r.HERE.parent.parent / "evals/plugin-agent-navigation/eval.py"
    spec = importlib.util.spec_from_file_location("warm_shared_replay", path)
    adapter = importlib.util.module_from_spec(spec); spec.loader.exec_module(adapter)
    check(record["profile"] == PROFILE and type(record["start_ns"]) is int and type(record["end_ns"]) is int
          and record["start_ns"] <= record["end_ns"], "batch clock/profile invalid")
    check(record["complete"] == all(row["status"] != "not_run" for row in record["slots"]), "completion differs")
    prior, boundary, episodes = None, record["start_ns"], []
    saw_unattempted = False
    installed_state = None
    initial_treatment = None
    for i, row in enumerate(record["slots"]):
        check(row["slot"] == i and row["request_digest"] == plugin.digest(plan["slots"][i]["request"]), "slot identity differs")
        if row["status"] == "not_run":
            saw_unattempted = True
            check(not record["complete"], "complete batch has unattempted slot")
            episodes.append(None)
            continue
        check(not saw_unattempted, "dropped scheduled slot")
        check(boundary <= row["start_ns"] <= row["end_ns"] <= record["end_ns"], "overlapping batch slot boundaries")
        boundary = row["end_ns"]
        check(prior is None or row["before"] == prior, "state handoff drift")
        if prior is None:
            check(row["before"] == {"installation": {}, "orbit-home": {}, "workspace": {}}, "batch did not start empty")
        check(not row.get("state_error"), "invalid state handoff")
        prior = row["after"]
        if installed_state is not None:
            check(prior["installation"] == installed_state["installation"] and
                  Context.immutable_home(None, prior) == Context.immutable_home(None, installed_state), "installation state drift")
        elif prior["installation"]:
            installed_state = prior
        check(row["directory"] == "slot-%03d" % i, "slot directory differs")
        if row["status"] == "refused":
            refusal = r.load_json(directory / ("refusal-%03d.json" % i))
            check(plugin.digest(refusal) == row["refusal_sha256"] and refusal["request_digest"] == row["request_digest"]
                  and refusal["error"] == row["reason"], "refusal provenance differs")
            for name, facts in refusal["files"].items():
                check(name == "plugin-setup.json", "unknown refusal file")
                path = directory / row["directory"] / name
                check(r.sha256_file(path) == facts["sha256"] and path.stat().st_size == facts["bytes"], "refusal log drift")
            episodes.append(None)
            continue
        a = adapter.load_episode(directory / row["directory"], diagnostic=diagnostic, warm=True)
        phases = r.load_json(directory / row["directory"] / "phase-timing.json")
        check(row["start_ns"] <= phases["preflight"]["outside_wall"]["start_ns"]
              and phases["provider_end_ns"] <= row["end_ns"], "episode phase crosses batch slot boundary")
        check(a["request"] == plan["slots"][i]["request"] and a["artifact_sha256"] == row["artifact_sha256"]
              and a["status"] == row["status"], "original episode differs")
        check(a["harness"] == plan["runtime"]["harness"] and a["model"] == plan["runtime"]["model"]
              and a["provider"]["binary_sha256"] == plan["runtime"]["provider_binary_sha256"], "frozen execution differs")
        check(a["batch"]["slot"] == i and a["batch"]["state_policy"] == POLICY
              and a["batch"]["cache_id"] == record["cache_id"] == a["isolation"]["cache_id"]
              and a["batch"]["identity"] == {"source_revision": a["request"]["source_revision"],
                  "source_commits": a["request"]["source_commits"], "plugin": a["request"]["plugin"], "cache_id": record["cache_id"]}
              and a["batch"]["cache_observations"] == cache_observations(a["calls"]), "cache evidence drift")
        check("--ephemeral" in a["provider"]["argv"], "provider session was persistent")
        check(a["batch"]["reused_installation"] == (i > 0 and plan["arm"] == "graph"), "installation reuse differs")
        if a["treatment"]:
            check(initial_treatment is None or initial_treatment == a["treatment"], "shipped treatment changed")
            initial_treatment = a["treatment"]
        episodes.append(a)
    # Compare persisted final state to its observed manifest; no silent stale/wrong cache.
    product = directory / "product"
    if prior is not None:
        check(set(os.listdir(product)) == {"installation", "orbit-home", "held"}, "undeclared final state")
        check(prior == {"installation": manifest(product / "installation"), "orbit-home": manifest(product / "orbit-home"),
                        "workspace": manifest(product / "held")}, "final product state was tampered")
    return plan, record, episodes
