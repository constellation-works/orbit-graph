"""Prospective warm batch custody. Shared runner still owns each provider and broker.

Schema 5 / runner 8 deliberately differs from cold navigation and patch schema 4.
Seals detect drift; they are not authentication against a same-user producer.
"""
import copy
from collections import Counter
import shutil
import json
import os
from pathlib import Path
import signal
import stat
import time
from types import SimpleNamespace

import eval_runner as runner
import plugin_profile as plugin

PROFILE = "installed-plugin-warm-batch-v1"
CACHE_POLICY = "declared-private-state-per-batch-v1"
POLICY = {"version": 1, "retained": ["repo/.orbit", "repo/.orbit-graph", "orbit-home"],
          "fresh": ["model-session", "runtime", "logs", "HOME", "CODEX_HOME", "TMPDIR", "source-and-git-view"],
          "installation_links": "only-generated-agents-and-claude-skills-to-private-installed-skills",
          "cache": "unknown-unless-product-evidenced", "index": "agent-choice",
          "failure": "retain-all-slots-stop-reuse-on-integrity-or-interruption",
          "os_cache": "uncontrolled-counterbalanced-order-no-selective-warmup"}
STUDY = Path(__file__).resolve().parents[2] / "evals/warm-navigation-v1"
STUDY_FILES = ("eval.py", "PROTOCOL.md")
HELPERS = ("eval_runner.py", "eval_broker.py", "plugin_profile.py", "reply_provenance.py", "telemetry.py", "warm_batch.py")
check = runner.require


def sealed(value, field):
    check(isinstance(value, dict) and value.get(field) == runner.digest(
        {k: v for k, v in value.items() if k != field}), field + " differs")
    return value


def manifest(root, install_home=None):
    """Bounded no-link private state inventory, including empty directories/modes."""
    root = Path(root)
    check(not root.is_symlink() and root.is_dir(), f"unsafe state root: {root}")
    info = root.stat()
    check(info.st_uid == os.getuid() and not info.st_mode & 0o022, f"unsafe state root ownership/mode: {root}")
    files, total = {}, 0
    pending = [root]
    while pending:
        current = pending.pop()
        for path in sorted(current.iterdir()):
            info = path.lstat()
            check(info.st_uid == os.getuid() and (stat.S_ISLNK(info.st_mode) or not info.st_mode & 0o022),
                  f"unsafe state ownership/mode: {path}")
            relative = str(path.relative_to(root))
            if stat.S_ISLNK(info.st_mode):
                parts = Path(relative).parts
                check(install_home is not None and len(parts) == 3
                      and parts[0] in {".agents", ".claude"} and parts[1] == "skills"
                      and parts[2] in {"orbit", "orbit-setup", "orbit-orchestrate", "graph-orbit-graph"},
                      "undeclared installation symlink")
                target = os.readlink(path)
                if target.startswith("/eval/state/orbit-home/"):
                    physical = Path(install_home) / target.removeprefix("/eval/state/orbit-home/")
                else:
                    physical = path.parent / target
                resolved = physical.resolve(strict=True)
                check(resolved.is_relative_to(Path(install_home).resolve() / ".orbit")
                      and resolved.is_dir(), "installation symlink escapes private root")
                files[relative] = {"kind": "installation-link", "target": str(resolved.relative_to(install_home))}
            elif stat.S_ISDIR(info.st_mode):
                files[relative] = {"kind": "dir", "mode": stat.S_IMODE(info.st_mode)}
                pending.append(path)
            else:
                check(stat.S_ISREG(info.st_mode), f"state link/special file refused: {path}")
                total += info.st_size
                check(info.st_size <= 512 * 1024 * 1024 and total <= 2 * 1024**3,
                      "state inventory exceeds bound")
                files[relative] = {"kind": "file", "mode": stat.S_IMODE(info.st_mode),
                                   "bytes": info.st_size, "sha256": runner.sha256_file(path)}
            check(len(files) <= 100000, "too many state entries")
    return files


def validate_freeze(freeze):
    check(isinstance(freeze, dict) and set(freeze) == {
        "schema_version", "profile", "state_policy", "lock_sha256", "model", "provider_binary_sha256",
        "harness", "study_layer", "runtime", "product_identity", "schedule", "batch_order", "freeze_sha256"}, "freeze fields differ")
    check(type(freeze["schema_version"]) is int and freeze["schema_version"] == 1
          and freeze["profile"] == PROFILE and freeze["state_policy"] == POLICY, "freeze profile/policy differs")
    sealed(freeze, "freeze_sha256")
    for name in ("lock_sha256", "provider_binary_sha256"):
        check(isinstance(freeze[name], str) and runner.HEX64.fullmatch(freeze[name]), "invalid " + name)
    check(isinstance(freeze["model"], dict) and set(freeze["model"]) == {"provider", "name", "version", "settings"}
          and freeze["model"]["provider"] == "codex-cli", "invalid model identity")
    check(set(freeze["harness"]) == set(HELPERS) and all(
        isinstance(v, str) and runner.HEX64.fullmatch(v) for v in freeze["harness"].values()), "helper identity differs")
    check(freeze["batch_order"] in (["baseline", "graph", "graph", "baseline"],
                                    ["graph", "baseline", "baseline", "graph"]), "need ABBA or BAAB batch order")
    check(set(freeze["study_layer"]) == set(STUDY_FILES) and all(isinstance(v, str)
          and runner.HEX64.fullmatch(v) for v in freeze["study_layer"].values()), "study layer pins differ")
    runtime = freeze["runtime"]
    check(set(runtime) == {"baseline_binaries", "bwrap_sha256", "resource_limits", "containment"}
          and set(runtime["baseline_binaries"]) == {"git", "rg", "python"}
          and all(isinstance(v, str) and runner.HEX64.fullmatch(v) for v in runtime["baseline_binaries"].values())
          and runtime["containment"] in {"none", "bwrap"}, "invalid runtime pin")
    check(set(runtime["resource_limits"]) == {"memory.max", "pids.max"}
          and all(v is None or type(v) is int and v > 0 for v in runtime["resource_limits"].values()), "invalid resource pin")
    check(runtime["bwrap_sha256"] is None if runtime["containment"] == "none" else
          isinstance(runtime["bwrap_sha256"], str) and runner.HEX64.fullmatch(runtime["bwrap_sha256"]), "invalid sandbox pin")
    check(isinstance(freeze["product_identity"], dict) and set(freeze["product_identity"]) == {
        "crate_version", "extractor_version", "history_schema_version", "plugin_schema_version", "store_schema_version"}, "product version fields differ")
    schedule = freeze["schedule"]
    check(isinstance(schedule, list) and 1 <= len(schedule) <= 100, "schedule must have 1..100 slots")
    for index, slot in enumerate(schedule):
        check(set(slot) == {"view", "requests"} and isinstance(slot["view"], str)
              and runner.CASE_ID.fullmatch(slot["view"]) and slot["view"] not in {".", ".."}, "invalid source view")
        check(set(slot["requests"]) == set(runner.ARMS), "missing paired request")
        for arm, request in slot["requests"].items():
            runner.validate_request(request)
            check(request["schema_version"] == 5 and request["arm"] == arm
                  and request["order"] == index and request.get("cache_policy") == CACHE_POLICY,
                  "warm slot identity differs")
        omit = {"arm", "tools", "request_sha256"}
        a, b = [slot["requests"][arm] for arm in runner.ARMS]
        check({k: v for k, v in a.items() if k not in omit} ==
              {k: v for k, v in b.items() if k not in omit}, "paired task/source/control drift")
    pins = [s["requests"]["graph"]["plugin"] for s in schedule]
    check(all(pin == pins[0] for pin in pins), "plugin pin changes within batch")
    return freeze


class Custody:
    def __init__(self, root, freeze, batch_index):
        self.root, self.freeze, self.batch_index = Path(root), freeze, batch_index
        self.product = self.root / "product"
        runner.make_private_dir(self.product)
        self.home = self.product / "orbit-home"
        runner.make_private_dir(self.home)
        self.repo = self.product / "repo"
        self.treatment = None
        self.expected = self.state()
        self.immutable = None
        self.previous = None
        self.slot = 0

    def state(self):
        check(set(p.name for p in self.product.iterdir()) <= {"repo", "orbit-home"}, "undeclared product state")
        check(set(p.name for p in self.home.iterdir()) <= {".orbit", ".agents", ".claude"}, "undeclared private home state")
        result = {"orbit-home": manifest(self.home, self.home)}
        for name in (".orbit", ".orbit-graph"):
            path = self.repo / name
            result["repo/" + name] = manifest(path) if os.path.lexists(path) else None
        return result

    def verify_runtime(self, episode, provider):
        check(provider["version"] == self.freeze["model"]["version"]
              and episode.recorded_settings() == self.freeze["model"]["settings"], "provider version/settings drift before output")

    def before_snapshot(self, episode):
        check(self.state() == self.expected, "state changed between scheduled slots")
        self.before = self.state()
        if self.repo.exists():
            self.previous = episode.out / "snapshot" / "previous-source"
            self.repo.rename(self.previous)
        episode.repo = self.repo

    def after_snapshot(self, episode):
        if self.previous:
            for name in (".orbit", ".orbit-graph"):
                path = self.previous / name
                if os.path.lexists(path):
                    path.rename(self.repo / name)
        if (self.repo / ".orbit").exists():
            with (self.repo / ".git/info/exclude").open("a") as stream:
                stream.write(".orbit/\n")

    def observe(self):
        try:
            return self.state(), None
        except (ValueError, OSError) as error:
            return None, str(error)[:2000]

    def installation(self):
        # Installed assets are immutable; runtime graph/Orbit DBs are manifested
        # separately and may change only inside the explicitly declared roots.
        return {name: record for name, record in manifest(self.home, self.home).items()
                if record["kind"] == "installation-link" or record["kind"] == "file" and (
                    name == ".orbit/config.toml" or name.startswith((".orbit/plugins/", ".orbit/skills/", ".orbit/resources/")))}

    def install(self, episode, deadline):
        pin = episode.request["plugin"]
        for name, key in (("orbit", "orbit_sha256"), ("orbit_graph", "backend_sha256")):
            check(runner.sha256_file(episode.binaries[name]) == pin[key], "warm binary pin drift: " + name)
        if self.treatment is None:
            self.treatment = plugin.install(episode, deadline)
            self.immutable = self.installation()
            check(self.immutable, "installed immutable assets missing")
            result = copy.deepcopy(self.treatment)
        else:
            check(self.installation() == self.immutable, "installed assets changed")
            result = copy.deepcopy(self.treatment)
            result.update(install_ms=0, setup_output_bytes=0, binary_staging_bytes=0, setup=[])
            runner.write_private(episode.out / "plugin-setup.json", "[]\n", episode.redactor)
        result["index_setup"] = "agent-choice-batch"
        return result

    def finish_episode(self, episode):
        after = self.state()
        if episode.arm == "graph":
            check(self.installation() == self.immutable, "installed assets changed during task")
        # Read-only source is a security boundary under bwrap; none is diagnostic.
        check(runner.content_revision(runner.tree_entries_with_git(self.repo, runner.broker.HIDDEN_NAMES | {".orbit"}), "after")["content_revision"]
              == episode.request["source_revision"], "source mutated during task")
        self.expected = after
        return {"freeze_sha256": self.freeze["freeze_sha256"], "batch_index": self.batch_index,
                "slot": self.slot, "before": self.before, "after": after,
                "state_policy": POLICY, "cache": {"outcome": "unknown", "evidence": None,
                    "reason": "product replies do not qualify a cache hit; repetition is insufficient"},
                "installation_reused": self.slot > 0 and episode.arm == "graph"}


def write(path, value):
    runner.write_private(path, json.dumps(value, indent=2, sort_keys=True) + "\n", runner.Redactor([]))


def run(args):
    start = time.monotonic_ns()
    freeze = validate_freeze(runner.load_json(args.request, 32 * 1024 * 1024))
    check(type(args.batch_index) is int and 0 <= args.batch_index < 4, "batch index must be 0..3")
    check(freeze["study_layer"] == {name: runner.sha256_file(STUDY / name) for name in STUDY_FILES}, "study layer drift")
    check(freeze["harness"] == {n: runner.sha256_file(runner.HERE / n) for n in HELPERS}, "frozen helper drift")
    check(runner.sha256_file(args.codex) == freeze["provider_binary_sha256"], "frozen provider drift")
    check(args.model == freeze["model"]["name"], "frozen model drift")
    lock = runner.load_json(args.lock)
    check(runner.sha256_file(args.lock) == freeze["lock_sha256"], "study lock drift")
    check(set(lock) == {"schema_version", "purpose", "source_fixture_lock_sha256", "expected_items"}
          and type(lock["schema_version"]) is int and lock["schema_version"] == 1
          and len(lock["expected_items"]) == len(freeze["schedule"])
          and all(isinstance(items, list) and items and all(isinstance(i, str) for i in items) for items in lock["expected_items"]),
          "study lock/rubric differs")
    runtime = freeze["runtime"]
    check(args.containment == runtime["containment"] and plugin.resource_bounds() == runtime["resource_limits"], "runtime bounds drift")
    check(runtime["baseline_binaries"] == {name: runner.sha256_file(runner.resolve_binary(getattr(args, name), name))
          for name in ("git", "rg", "python")}, "baseline binary drift")
    if args.containment == "bwrap":
        check(runner.sha256_file(runner.resolve_binary(args.bwrap, "bwrap")) == runtime["bwrap_sha256"], "sandbox drift")
    identity = runner.broker.run_child([runner.resolve_binary(args.orbit_graph, "orbit-graph"), "version", "--json"],
        "/", {"PATH": "/usr/bin:/bin", "HOME": "/nonexistent"}, 30, capture_limit=65536)
    check(plugin.clean(identity) and runner.parse_json(identity["stdout"], "product version") == freeze["product_identity"], "product version drift")
    # Validate all exported views before admitting any provider output.
    for slot in freeze["schedule"]:
        for role, root, field in (("head", args.head, "source_revision"), ("base", args.base, "base_revision")):
            revision = runner.content_revision(runner.tree_entries(Path(root) / slot["view"], role), role)
            check(revision["content_revision"] == slot["requests"]["baseline"][field], "frozen export drift")
    out = Path(os.path.abspath(args.out))
    check(not os.path.lexists(out) and out.parent.resolve() == out.parent, "batch output must be a fresh physical path")
    usage = shutil.disk_usage(out.parent)
    check(usage.used / usage.total < .8, "disk at least 80%; no new batch")
    admission = {"freeze_sha256": freeze["freeze_sha256"], "batch_index": args.batch_index,
                 "output": str(out), "start_ns": start}
    write(Path(str(Path(args.request).absolute()) + f".batch-{args.batch_index}.json"), admission)
    runner.make_private_dir(out)
    copied = runner.write_private(out / "lock.json", Path(args.lock).read_bytes(), runner.Redactor([]), preserve_bytes=True)
    check(copied["sha256"] == freeze["lock_sha256"] and copied["redactions"] == 0, "study lock changed at durability boundary")
    write(out / "freeze.json", freeze)  # durable before any installation or provider
    custody = Custody(out, freeze, args.batch_index)
    arm = freeze["batch_order"][args.batch_index]
    records = []
    interrupted = False
    def interrupt(signum, frame):
        nonlocal interrupted
        interrupted = True  # active runner supervisor handles its own child group
    custody.cancelled = lambda: interrupted
    old = {sig: signal.signal(sig, interrupt) for sig in (signal.SIGINT, signal.SIGTERM)}
    previous_umask = os.umask(0o077)
    try:
        for index, slot in enumerate(freeze["schedule"]):
            custody.slot = index
            record = {"slot": index, "request_digest": runner.digest(slot["requests"][arm]),
                      "status": "not_run", "error": None, "directory": f"slot-{index:03d}",
                      "start_ns": None, "end_ns": None, "artifact_sha256": None,
                      "state_before": None, "state_after": None, "state_error": None}
            records.append(record)
        write(out / "schedule.json", {"freeze_sha256": freeze["freeze_sha256"], "slots": records})
        unsafe = False
        for index, slot in enumerate(freeze["schedule"]):
            record = records[index]
            if interrupted or unsafe:
                record["error"] = {"code": "batch_interrupted" if interrupted else "state_not_reusable"}
                continue
            custody.slot = index
            record["start_ns"] = time.monotonic_ns()
            record["state_before"], record["state_error"] = custody.observe()
            request_path = out / f"request-{index:03d}.json"
            write(request_path, slot["requests"][arm])
            options = SimpleNamespace(**vars(args))
            options.request, options.out = str(request_path), str(out / record["directory"])
            options.head, options.base = str(Path(args.head) / slot["view"]), str(Path(args.base) / slot["view"])
            options._warm = custody
            try:
                outcome = runner.command_run(options)
                artifact = runner.load_json(Path(options.out) / "episode.json", 64 * 1024 * 1024)
                check(artifact["model"] == freeze["model"], "frozen model/settings/version drift")
                record.update(status=outcome["status"], error=outcome["error"], artifact_sha256=outcome["artifact_sha256"])
                unsafe = artifact["cleanup"]["terminated_by"] is not None or bool(artifact["cleanup"]["survivors"])
            except (ValueError, OSError, runner.CapabilityRefusal) as error:
                record.update(status="refused", error={"code": getattr(error, "code", "batch_integrity_refused"),
                                                       "message": str(error)[:2000]})
                # No outcome substitution/retry. The original partial capture remains.
                unsafe = True
            record["state_after"], state_error = custody.observe()
            record["state_error"] = state_error or record["state_error"]
            record["end_ns"] = time.monotonic_ns()
            write(out / f"checkpoint-{index:03d}.json", record)
    finally:
        os.umask(previous_umask)
        for sig, handler in old.items():
            signal.signal(sig, handler)
    final_state, final_error = custody.observe()
    result = {"schema_version": 1, "profile": PROFILE, "freeze_sha256": freeze["freeze_sha256"],
              "batch_index": args.batch_index, "arm": arm, "admission": admission, "start_ns": start, "end_ns": time.monotonic_ns(),
              "slots": records, "interrupted": interrupted, "initial_state": {"orbit-home": {}, "repo/.orbit": None,
                  "repo/.orbit-graph": None}, "final_state": final_state, "state_error": final_error,
              "identity_probe": plugin.evidence(identity), "product_identity": freeze["product_identity"],
              "outside_wall": {"source_export_ms": None, "freeze_qualification_ms": None},
              "state_policy": POLICY}
    runner.seal(result, "batch_sha256")
    write(out / "batch.json", result)
    return {"profile": PROFILE, "batch": str(out / "batch.json"), "slots": len(records),
            "outcomes": dict(Counter(r["status"] for r in records))}
