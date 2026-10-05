#!/usr/bin/env python3
"""Deterministic evaluator of explicit binaries; never invokes a provider."""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import re
import shutil
import sys
import tarfile
import time

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(ROOT / "scripts/agent-eval"))
import eval_broker as broker
import plugin_profile as plugin
from scoring import aggregate, canonical, score, select, MISSING


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest() if hasattr(hashlib, "file_digest") else hashlib.sha256(stream.read()).hexdigest()


def write_json(path, value):
    # Exclusive creation preserves every earlier attempt, including partial ones.
    with Path(path).open("x", encoding="utf-8") as stream:
        json.dump(value, stream, indent=2, ensure_ascii=False)
        stream.write("\n")


def frozen():
    lock = json.loads((HERE / "manifest.json").read_text())
    require(lock["schema_version"] == 1, "unsupported manifest")
    actual = {str(p.relative_to(HERE)) for p in (HERE / "fixtures").rglob("*") if p.is_file()}
    require(actual | {"corpus.json"} == set(lock["files"]), "fixture inventory drift")
    for name, identity in lock["files"].items():
        path = HERE / name
        require(not path.is_symlink() and path.resolve().is_relative_to(HERE), "unsafe fixture path")
        require(path.stat().st_size == identity["bytes"] and sha(path) == identity["sha256"], "frozen bytes differ: " + name)
    corpus = json.loads((HERE / "corpus.json").read_text())
    cases = corpus["cases"]
    require(len(cases) >= 30 and len({c["id"] for c in cases}) == len(cases), "case count/identity")
    require({c["tool"] for c in cases} == set(plugin.VERBS) == set(corpus["tools"]), "shipped surface coverage")
    require(corpus["warm_samples"] == 3, "warm protocol drift")
    fixture_paths = set()
    for repository in corpus["repositories"].values():
        require(repository["trees"], "empty repository")
        for tree in repository["trees"]:
            for destination, fixture in tree.items():
                require(destination and not Path(destination).is_absolute() and ".." not in Path(destination).parts,
                        "unsafe source path")
                require(fixture in lock["files"], "unfrozen source bytes")
                fixture_paths.add(fixture)
    require(fixture_paths == actual, "unreferenced frozen fixture")
    for case in cases:
        require(case["repository"] in corpus["repositories"], "missing fixture")
        require(case["source_review"] and (case["checks"] or case.get("expected_error")), "no truth")
        require(all(type(case["coverage"][k]) is int and case["coverage"][k] >= 0
                    for k in ("supported", "unsupported", "omitted", "unresolved")), "invalid denominator")
    return corpus


def environment(home, tmp, bin_dir):
    return {"PATH": str(bin_dir) + ":/usr/bin:/bin", "HOME": str(home), "XDG_CONFIG_HOME": str(home),
            "XDG_CACHE_HOME": str(home / "cache"), "XDG_DATA_HOME": str(home / "data"),
            "TMPDIR": str(tmp), "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8",
            "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": "/dev/null",
            "GIT_AUTHOR_NAME": "Synthetic graph truth", "GIT_AUTHOR_EMAIL": "truth@example.invalid",
            "GIT_COMMITTER_NAME": "Synthetic graph truth", "GIT_COMMITTER_EMAIL": "truth@example.invalid"}


def transport_ok(record, expected_error=False):
    return (record["stopped"] is None and record["error_type"] is None
            and not record["stderr_truncated"] and record["supervision"]["survivors"] == []
            and not record["supervision"]["signals"]
            and record.get("streams_utf8", True)
            and (record["exit_code"] == 0 or expected_error))


def semantic(value, paths):
    """Remove only the protocol's named time/path fields; retain all other data."""
    value = json.loads(canonical(value))
    def remove(node, parts):
        if not parts:
            return
        first, *rest = parts
        if first == "*" and isinstance(node, list):
            for item in node:
                remove(item, rest)
        elif isinstance(node, dict) and first in node:
            if rest:
                remove(node[first], rest)
            else:
                del node[first]
    for path in paths:
        remove(value, path.split("."))
    return value


def lifecycle_verdict(case, incremental, full, tree):
    truth = {"checks": case["snapshot_truth"][tree], "coverage": case["coverage"]}
    checked = [score(truth, snapshot) for snapshot in (incremental, full)]
    findings = [prefix + finding for prefix, verdict in zip(("incremental: ", "full: "), checked)
                for finding in verdict["findings"]]
    if incremental != full:
        findings.append("maintain index differs from fresh full graph queries")
    return {"passed": not findings, "findings": findings,
            "metrics": [metric for verdict in checked for metric in verdict["metrics"]]}


def plugin_cli_reply(record):
    """Decode the host payload, including its JSON error after stderr diagnostics."""
    if record["exit_code"] == 0 or record["stdout"].strip():
        envelope = json.loads(record["stdout"])
    else:
        errors = []
        for line in record["stderr"].splitlines():
            try:
                value = json.loads(line)
            except ValueError:
                continue
            if isinstance(value, dict) and isinstance(value.get("code"), str) and isinstance(value.get("message"), str):
                errors.append(value)
        require(len(errors) == 1, "missing or multiple structured host errors")
        envelope = errors[0]
    require(isinstance(envelope, dict), "plugin reply is not an object")
    payload = envelope["output"] if envelope.get("ok") is True else envelope
    if envelope.get("ok") is False:
        error = envelope.get("error", {})
        require(isinstance(error, dict), "malformed error envelope")
        return payload, error.get("code", "product_error")
    return payload, payload.get("code", "product_error") if record["exit_code"] != 0 else None


class Evaluation:
    def __init__(self, args, corpus):
        self.args, self.corpus = args, corpus
        parent = Path(args.output).parent.resolve(strict=True)
        require(parent.is_relative_to((ROOT / ".orbit/tmp").resolve()), "output must be beneath this checkout's .orbit/tmp/")
        require(not Path(args.output).is_symlink(), "symlink output")
        self.out = parent / Path(args.output).name
        self.out.mkdir(mode=0o700)  # Existing attempts are never replaced.
        self.raw = self.out / "raw"
        self.raw.mkdir(mode=0o700)
        self.bin = self.out / "bin"
        self.bin.mkdir(mode=0o700)
        self.rows, self.setup_records, self.counter = [], [], 0
        for name in ("corpus.json", "manifest.json"):
            with (self.out / name).open("xb") as stream:
                stream.write((HERE / name).read_bytes())
        self.identities = {"source_commit": args.source_commit, "plugin_commit": args.plugin_commit,
                           "candidate_sha256": args.candidate_sha256, "orbit_sha256": args.orbit_sha256,
                           "plugin_manifest_sha256": args.plugin_manifest_sha256,
                           "fixture_manifest_sha256": sha(HERE / "manifest.json"),
                           "corpus_sha256": sha(HERE / "corpus.json"), "runtime_limits": plugin.resource_bounds(),
                           "harness_files": {str(p.relative_to(ROOT)): sha(p) for p in
                                             (HERE / "eval.py", HERE / "scoring.py", ROOT / "scripts/agent-eval/eval_broker.py",
                                              ROOT / "scripts/agent-eval/plugin_profile.py")},
                           "binary_source_binding": "caller-supplied source commit plus independently hashed binary; not a reproducible-build attestation"}
        for name, source, expected in [("orbit-graph", args.candidate, args.candidate_sha256),
                                       ("orbit", args.orbit, args.orbit_sha256)]:
            source = Path(source).resolve(strict=True)
            require(re.fullmatch("[0-9a-f]{64}", expected) and sha(source) == expected, name + " digest mismatch")
            target = self.bin / name
            shutil.copyfile(source, target)
            target.chmod(0o700)
            require(sha(target) == expected, "staged binary differs")
        self.candidate, self.orbit = str(self.bin / "orbit-graph"), str(self.bin / "orbit")
        home, tmp = self.out / "export-home", self.out / "export-tmp"
        home.mkdir(mode=0o700); tmp.mkdir(mode=0o700)
        self.export_env = environment(home, tmp, self.bin)

    def record(self, role, argv, cwd, env, input_bytes=None, expected_error=False):
        result = broker.run_child(argv, str(cwd), env, 120, capture_limit=16 * 1024 * 1024,
                                  input_bytes=input_bytes)
        return self.keep(role, argv, cwd, result)

    def keep(self, role, argv, cwd, result, request=None):
        import base64
        self.counter += 1
        identity = f"raw/{self.counter:05d}.json"
        record = {"role": role, "argv": [str(x) for x in argv], "cwd": str(cwd), **plugin.evidence(result)}
        record["request"] = request
        record["output_bytes"] = len(result["stdout"]) + len(result["stderr"]) + result.get("binary_stdout_bytes", 0)
        record["stdout_base64"] = base64.b64encode(result["stdout"]).decode()
        record["stderr_base64"] = base64.b64encode(result["stderr"]).decode()
        try:
            result["stdout"].decode("utf-8"); result["stderr"].decode("utf-8")
            record["streams_utf8"] = True
        except UnicodeDecodeError:
            record["streams_utf8"] = False
        write_json(self.out / identity, record)
        if role not in ("measured-query", "strict-task-text-evaluate"):
            self.setup_records.append({"artifact": identity, "role": role, "exit_code": record["exit_code"],
                                       "output_bytes": record["output_bytes"], "latency_ms": record["elapsed_ms"]})
        return record, identity

    def setup(self, role, argv, cwd, env):
        record, artifact = self.record(role, argv, cwd, env)
        require(transport_ok(record), f"{role} failed; see {artifact}")
        return record["stdout"]

    def export(self):
        repo = Path(self.args.source_repo).resolve(strict=True)
        for name, commit in [("source", self.args.source_commit), ("plugin", self.args.plugin_commit)]:
            require(re.fullmatch("[0-9a-f]{40}", commit), name + " must be a full commit")
            kind = self.setup("verify-" + name, ["/usr/bin/git", "-C", str(repo), "cat-file", "-t", commit], repo, self.export_env)
            require(kind == "commit\n", "not a commit")
        # Product/source files must agree with the declared source; evaluator additions are independent.
        product_paths = ["Cargo.toml", "Cargo.lock", "crates", ".orbit-plugin", "scripts/bundle-plugin-binary.sh", "scripts/agent-eval"]
        dirty = self.setup("source-drift", ["/usr/bin/git", "-C", str(repo), "diff", self.args.source_commit, "--", *product_paths], repo, self.export_env)
        require(not dirty, "declared product source differs from checkout")
        self.identities["source_tree"] = self.setup("source-tree", ["/usr/bin/git", "-C", str(repo), "rev-parse", self.args.source_commit + "^{tree}"], repo, self.export_env).strip()
        # Binary archive output is retained losslessly (base64), without decoding arbitrary bytes.
        result = broker.run_child(["/usr/bin/git", "-C", str(repo), "archive", "--format=tar",
                                   self.args.plugin_commit, ".orbit-plugin"], str(repo), self.export_env, 60)
        import base64
        archive = result["stdout"]
        archive_record = dict(result); archive_record["stdout"] = b""; archive_record["binary_stdout_bytes"] = len(archive)
        rec, artifact = self.keep("export-plugin", ["git", "archive", self.args.plugin_commit, ".orbit-plugin"], repo, archive_record)
        write_json(self.out / "export.json", {"artifact": artifact, "sha256": hashlib.sha256(archive).hexdigest(),
                                            "bytes": len(archive), "base64": base64.b64encode(archive).decode()})
        require(transport_ok(rec), "plugin export failed")
        self.plugin_root = self.out / "export" / ".orbit-plugin"
        source_files = {}
        with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
            for member in tar:
                require(Path(member.name).parts[0] == ".orbit-plugin" and ".." not in Path(member.name).parts
                        and (member.isfile() or member.isdir()), "unsafe plugin archive")
                target = self.out / "export" / member.name
                if member.isdir():
                    target.mkdir(parents=True, exist_ok=True, mode=0o700)
                else:
                    data = tar.extractfile(member).read()
                    source_files[member.name] = hashlib.sha256(data).hexdigest()
                    target.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
                    target.write_bytes(data)
                    target.chmod(0o700 if member.mode & 0o111 else 0o600)
        manifest = self.plugin_root / "plugin.yaml"
        require(sha(manifest) == self.args.plugin_manifest_sha256, "plugin manifest hash mismatch")
        original = manifest.read_text()
        require(re.findall(r"(?m)^    - name: (\w+)$", original) == list(plugin.VERBS), "manifest tools drift")
        require(original.count("  origin: orbit\n") == 1, "unsupported namespace binding")
        manifest.write_text(original.replace("  origin: orbit\n", ""))
        # Reuse the real bundler, exactly as plugin_v2.rs does; no launcher imitation.
        bundler = self.setup("export-bundler", ["/usr/bin/git", "-C", str(repo), "show",
                            self.args.plugin_commit + ":scripts/bundle-plugin-binary.sh"], repo, self.export_env)
        script = self.out / "bundle-plugin-binary.sh"; script.write_text(bundler)
        self.setup("bundle-pinned-backend", ["/bin/sh", str(script), "--binary", self.candidate,
                                            str(self.plugin_root)], self.out, self.export_env)
        self.identities.update(plugin_source_files=source_files, installed_manifest_sha256=sha(manifest),
                               manifest_edits=["remove unverified first-party origin for local graph namespace",
                                               "bind backend SHA using shipped bundler"])
        write_json(self.out / "export-identities.json", self.identities)

    def git(self, fixture, *args, date=None):
        env = dict(fixture["env"])
        if date is not None:
            env.update(GIT_AUTHOR_DATE=f"@{date} +0000", GIT_COMMITTER_DATE=f"@{date} +0000")
        return self.setup("fixture-git", ["/usr/bin/git", "-C", str(fixture["repo"]), *args], fixture["repo"], env).strip()

    def fixture(self, case, surface):
        root = self.out / (case["id"] + "--" + surface)
        root.mkdir(mode=0o700)
        paths = {name: root / name for name in ("repo", "home", "tmp")}
        for path in paths.values():
            path.mkdir(mode=0o700)
        f = {**paths, "env": environment(paths["home"], paths["tmp"], self.bin), "commits": []}
        f["env"]["GIT_CEILING_DIRECTORIES"] = str(paths["repo"])
        self.git(f, "init", "-q", "-b", "main", "--template=")
        self.git(f, "config", "core.excludesFile", "/dev/null")
        self.git(f, "remote", "add", "origin", "https://example.invalid/synthetic-" + case["repository"] + ".git")
        trees = self.corpus["repositories"][case["repository"]]["trees"]
        previous = set()
        for i, tree in enumerate(trees):
            removed = previous - set(tree)
            if removed:
                self.git(f, "rm", "--", *sorted(removed))
            for target, source in tree.items():
                path = f["repo"] / target; path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes((HERE / source).read_bytes())
            self.git(f, "add", "--", *sorted(tree))
            self.git(f, "commit", "-qm", "Synthetic snapshot " + str(i), date=600 if i == 3 else 100 * (i + 1))
            f["commits"].append(self.git(f, "rev-parse", "HEAD"))
            previous = set(tree)
        if case["setup"] == "history":
            self.git(f, "checkout", "-q", f["commits"][2])
        elif case["setup"] == "lifecycle" and case.get("lifecycle") != "incremental":
            self.git(f, "checkout", "-q", f["commits"][0])
        if surface != "cli":
            self.setup("workspace-init", [self.orbit, "workspace", "init", "--name", plugin.WORKSPACE,
                                         "--ship-mode", "local"], f["repo"], f["env"])
            # workspace init appends scratch ignores. Restore only its owned new file.
            ignore = f["repo"] / ".gitignore"
            if ignore.exists():
                ignore.unlink()
            (f["repo"] / ".git/info").mkdir(exist_ok=True)
            with (f["repo"] / ".git/info/exclude").open("a") as stream:
                stream.write("\n.orbit/\n.orbit-graph/\n")
            self.setup("plugin-add", [self.orbit, "plugin", "add", str(self.plugin_root)], f["repo"], f["env"])
            self.setup("plugin-enable", [self.orbit, "plugin", "enable", "graph", "--grant", "fs,orbit_tools"], f["repo"], f["env"])
            reply, result = plugin.mcp(self.orbit, str(f["repo"]), f["env"], "tools/list", {}, 60)
            rec, artifact = self.keep("tools-list", [self.orbit, "mcp", "serve"], f["repo"], result)
            inventory = plugin.plugin_inventory(reply) if transport_ok(rec) else None
            require(inventory is not None, "private installed inventory failed: " + artifact)
            f["inventory_sha256"] = plugin.digest(inventory)
            if "inventory_sha256" in self.identities:
                require(f["inventory_sha256"] == self.identities["inventory_sha256"], "installed inventory drift")
            else:
                self.identities["inventory_sha256"] = f["inventory_sha256"]
                write_json(self.out / "inventory.json", inventory)
        f["base"], f["head"] = f["commits"][0], f["commits"][-1]
        f["target"] = f["commits"][2] if case["setup"] == "history" else f["head"]
        return f

    def invoke(self, f, tool, arguments, surface, role):
        arguments = dict(arguments)
        if tool != "version":
            arguments["repository"] = str(f["repo"])
        if surface == "mcp":
            reply, result = plugin.mcp(self.orbit, str(f["repo"]), f["env"], "tools/call",
                                       {"name": "graph_" + tool, "arguments": arguments}, 120,
                                       operator=tool == "maintain")
            argv = [self.orbit, "mcp", "serve", "--workspace", plugin.WORKSPACE]
            if tool == "maintain":
                argv.append("--operator")
            record, artifact = self.keep(role, argv, f["repo"], result,
                                         {"method": "tools/call", "name": "graph_" + tool, "arguments": arguments})
            payload = (reply or {}).get("structuredContent")
            error = payload.get("code", "product_error") if (reply or {}).get("isError") else None
            return payload, error, record, artifact
        if surface == "plugin-cli":
            env = dict(f["env"])
            if tool == "maintain":
                env["ORBIT_OPERATOR"] = "1"  # explicit private-host operator, never inherited
            argv = [self.orbit, "tool", "run", "graph." + tool, "--input", canonical(arguments), "--full"]
            record, artifact = self.record(role, argv, f["repo"], env)
            try:
                payload, error = plugin_cli_reply(record)
            except (ValueError, KeyError, TypeError):
                payload, error = None, "unparseable_reply"
            return payload, error, record, artifact
        return self.cli(f, tool, arguments, role)

    def cli(self, f, tool, args, role):
        argv = [self.candidate]
        if tool == "version":
            argv += ["version", "--json"]
        elif tool == "status":
            argv += ["history", "status", "--branch", "main"]
        elif tool == "maintain":
            if args["operation"] == "graph_sync":
                argv += ["sync"] + (["--full"] if args.get("full") else [])
            elif args["operation"] == "history_sync":
                argv += ["history", "sync", "--branch", "main"]
            elif args["operation"] == "import":
                path = f["tmp"] / ("delivery-" + str(self.counter) + ".json")
                write_json(path, args["delivery"])
                argv += ["history", "import", "--input", str(path)]
        else:
            argv += [tool]
            positional = {"search": "query", "show": "selector", "refs": "selector", "callees": "selector",
                          "impact": "selector", "trace": "command", "deps": "selector"}.get(tool)
            if positional:
                argv.append(args[positional])
            if tool == "overview" and "scope" in args:
                argv.append(args["scope"])
            if tool == "overview" and "selector" in args:
                argv.append(args["selector"])
            if tool == "changes":
                argv.append(args["base"] + ".." + args["head"])
            mappings = {"confidence": {"import_resolved": "import", "fuzzy_name": "fuzzy"},
                        "format": {"full": "full", "summary": "summary"}}
            for key, value in args.items():
                if key in ("repository", positional):
                    continue
                if tool == "changes" and key in ("base", "head") or tool == "overview" and key in ("scope", "selector"):
                    continue
                if key == "task_snapshot":
                    path = f["tmp"] / ("task-" + str(self.counter) + ".json")
                    write_json(path, value); value = str(path)
                if key == "format":
                    key = "detail"  # overview's public detail flag
                if key == "include_unresolved":
                    if value:
                        argv.append("--include-unresolved")
                    continue
                if key == "budget_ms" and tool in ("impact", "trace"):
                    argv += ["--budget-ms", str(value)]
                    continue
                if key == "symbols":
                    for selector in value:
                        argv += ["--symbol", selector]
                    continue
                if key == "limit" and tool == "impact":
                    continue  # standalone impact exposes only the fixed traversal cap
                argv += ["--" + key.replace("_", "-"), str(mappings.get(key, {}).get(value, value))]
        if tool != "version":
            argv += ["--json"]
        record, artifact = self.record(role, argv, f["home"] if tool == "version" else f["repo"], f["env"])
        try:
            payload = json.loads(record["stdout"] if record["exit_code"] == 0 else record["stderr"])
            error = payload.get("code") if record["exit_code"] != 0 else None
        except (ValueError, TypeError):
            payload, error = None, "unparseable_reply"
        # Standalone CLI and MCP differ in wrapping; preserve the raw artifact.
        if error is None and tool in ("search", "show", "refs", "callees", "impact", "trace", "deps", "overview", "changes"):
            if tool == "callees" and isinstance(payload, list):
                payload = {"callees": payload}
            payload = {"result": payload}
        if tool == "status" and error is None:
            payload = {"status": payload}
        return payload, error, record, artifact

    def setup_call(self, f, tool, args, surface):
        payload, error, record, artifact = self.invoke(f, tool, args, surface, "prerequisite-" + tool)
        require(transport_ok(record) and error is None, "prerequisite failed: " + artifact)
        return payload

    def prepare(self, case, f, surface):
        phase = case.get("lifecycle")
        if case["setup"] == "graph":
            self.setup_call(f, "maintain", {"operation": "graph_sync"}, surface)
        if case["setup"] == "lifecycle":
            if phase in ("fresh", "outdated"):
                self.git(f, "branch", "-f", "main", f["base"])
                self.setup_call(f, "maintain", {"operation": "history_sync"}, surface)
                self.setup_call(f, "maintain", {"operation": "graph_sync"}, surface)
            if phase == "outdated":
                self.git(f, "branch", "-f", "main", f["head"])
                self.git(f, "checkout", "-q", "main")
            if phase == "incremental":
                self.git(f, "checkout", "-q", f["base"])
                self.setup_call(f, "maintain", {"operation": "graph_sync"}, surface)
                self.git(f, "checkout", "-q", "main")
        if case["setup"] == "history":
            task = synthetic_task("synthetic-training", 50)
            delivery = envelope(f, 0, 1, task, 200)
            f["task"] = synthetic_task("synthetic-target", 400)
            f["corpus"] = {"schema_version": 1, "repository": delivery["repository"], "landing_branch": "main",
                           "source": SOURCE, "complete": True, "coverage_note": "one reviewed synthetic prospective case",
                           "k": 10, "training_deliveries": [delivery], "cases": [{
                               "id": "later-heldout", "target_revision": f["target"], "cutoff": "unix:500",
                               "task_snapshot": f["task"], "source": SOURCE,
                               "held_out_delivery": envelope(f, 2, 3, f["task"], 600)}]}
            self.setup_call(f, "maintain", {"operation": "import", "delivery": delivery}, surface)
            self.setup_call(f, "maintain", {"operation": "graph_sync"}, surface)

    def substituted(self, value, f):
        if isinstance(value, str) and value.startswith("$"):
            return f[value[1:]]
        if isinstance(value, dict):
            return {k: self.substituted(v, f) for k, v in value.items()}
        if isinstance(value, list):
            return [self.substituted(v, f) for v in value]
        return value

    def supplementary(self, case, f, sample):
        corpus = json.loads(canonical(f["corpus"]))
        if case["action"] == "evaluate_empty":
            corpus["cases"] = []
        path = f["tmp"] / ("corpus-" + str(sample) + ".json")
        write_json(path, corpus)
        argv = [self.candidate, "evaluate", "--input", str(path), "--json"]
        record, artifact = self.record("strict-task-text-evaluate", argv, f["repo"], f["env"])
        try:
            payload = json.loads(record["stdout"])
        except ValueError:
            payload = None
        return payload, None if record["exit_code"] == 0 else "evaluation_error", record, artifact

    def run_case(self, original, surface):
        try:
            f = self.fixture(original, surface)
            self.prepare(original, f, surface)
            case = self.substituted(original, f)
            if surface == "cli" and "cli_checks" in case:
                case["checks"] = case["cli_checks"]
            payloads = []
            for sample in range(4):
                if case.get("action"):
                    payload, error, record, artifact = self.supplementary(case, f, sample)
                else:
                    payload, error, record, artifact = self.invoke(f, case["tool"], case["input"], surface, "measured-query")
                verdict = score(case, payload, error, transport_ok(record, bool(case.get("expected_error"))))
                self.extra_checks(case, f, surface, payload, verdict, sample)
                payloads.append(semantic({"payload": payload, "error": error}, ["payload." + p for p in self.corpus["volatile_paths"]]))
                # State mutations are expected to change diagnostic counts on the first publication.
                compare_start = 1 if case["tool"] == "maintain" else 0
                if sample > compare_start and payloads[sample] != payloads[compare_start]:
                    verdict["passed"] = False
                    verdict["findings"].append("same-input same-source warm semantic mismatch")
                row = {"case_id": case["id"], "tool": case["tool"], "surface": surface,
                       "sample": sample, "state": "cold-after-declared-prerequisites" if sample == 0 else "warm",
                       "source_revisions": f["commits"], "request": case["input"], "raw_artifact": artifact,
                       "error_code": error, "exit_code": record["exit_code"], "latency_ms": record["elapsed_ms"],
                       "output_bytes": record["output_bytes"],
                       "semantic_sha256": hashlib.sha256(canonical(payloads[-1]).encode()).hexdigest(), "score": verdict}
                self.rows.append(row)
                with (self.out / "attempts.jsonl").open("a") as stream:
                    stream.write(canonical(row) + "\n")
        except (ValueError, OSError, KeyError, TypeError) as error:
            row = {"case_id": original["id"], "tool": original["tool"], "surface": surface, "sample": None,
                   "state": "setup-failed", "latency_ms": None, "output_bytes": 0,
                   "score": {"passed": False, "findings": [str(error)], "metrics": [], "declared_coverage": original["coverage"]}}
            self.rows.append(row)
            with (self.out / "attempts.jsonl").open("a") as stream:
                stream.write(canonical(row) + "\n")

    def extra_checks(self, case, f, surface, payload, verdict, sample):
        if case.get("lifecycle") in ("first", "full", "incremental") and payload is not None:
            incremental = self.graph_snapshot(f, surface)
            self.setup_call(f, "maintain", {"operation": "graph_sync", "full": True}, surface)
            full = self.graph_snapshot(f, surface)
            tree = "head" if case["lifecycle"] == "incremental" else "base"
            checked = lifecycle_verdict(case, incremental, full, tree)
            evidence = "lifecycle-" + case["id"] + "--" + surface + "-" + str(sample) + ".json"
            write_json(self.out / evidence, {"tree": tree, "source_revisions": f["commits"],
                       "incremental": incremental, "fresh_full": full, "truth": case["snapshot_truth"][tree],
                       "score": checked})
            verdict["lifecycle_artifact"] = evidence
            verdict["findings"].extend(checked["findings"])
            verdict["metrics"].extend(checked["metrics"])
            verdict["passed"] &= checked["passed"]
        if case.get("lifecycle") in ("fresh", "outdated"):
            expected = case["lifecycle"] == "fresh"
            good = select(payload, "status.complete") is expected
            if surface != "cli":
                good &= select(payload, "code_index.fresh") is expected
            if not good:
                verdict["passed"] = False
                verdict["findings"].append("status freshness disagrees with frozen checkout")

    def graph_snapshot(self, f, surface):
        queries = [("overview", "overview", {"format": "full"}),
                   ("deps", "deps", {"selector": "dir:src"})]
        queries.extend(("search-" + name, "search", {"query": name, "kind": "symbol"})
                       for name in ("keep", "remove", "added", "moved"))
        for path, name in (("src/lib.rs", "keep"), ("src/lib.rs", "remove"),
                           ("src/lib.rs", "added"), ("src/old.rs", "moved"), ("src/new.rs", "moved")):
            selector = "symbol:" + path + "#" + name + ":function"
            key = path.replace("/", "_").replace(".", "_") + "-" + name
            queries.extend((tool + "-" + key, tool, {"selector": selector}) for tool in ("callees", "refs"))
        return {key: select(self.setup_call(f, tool, arguments, surface), "result")
                for key, tool, arguments in queries}

    def finish(self, fatal=None):
        by_tool = aggregate(self.rows, self.corpus["tools"])
        required = {(case["id"], surface) for case in self.corpus["cases"] for surface in case["surfaces"]}
        actual = {(row["case_id"], row["surface"]) for row in self.rows if row.get("sample") is not None}
        coverage = []
        for case in self.corpus["cases"]:
            c = case["coverage"]
            denominator = c["supported"] + c["unsupported"]
            coverage.append({"case_id": case["id"], **c,
                             "unsupported_fraction": c["unsupported"] / denominator if denominator else None,
                             "omitted_fraction": c["omitted"] / denominator if denominator else None,
                             "denominator_note": "source-reviewed scope; unit-specific, never pooled across units"})
        report = {"schema_version": 1, "kind": "deterministic-tool-correctness-v1", "identities": self.identities,
                  "qualified": fatal is None and actual == required and all(v["passed"] for v in by_tool.values()),
                  "fatal_error": fatal, "missing_case_surfaces": sorted(required - actual),
                  "per_tool": by_tool, "coverage": coverage, "attempts": self.rows, "setup": self.setup_records,
                  "setup_output_bytes": sum(r["output_bytes"] for r in self.setup_records),
                  "setup_latency_ms": sum(r["latency_ms"] for r in self.setup_records),
                  "diagnostic_only": True, "effectiveness_or_generalization_claim": False,
                  "surface_gaps": self.corpus["surface_gaps"],
                  "limits": ["three warm samples are diagnostics, not a statistical benchmark",
                             "source/binary association is declared, not a reproducible-build proof",
                             "outer runtime memory/PID ceilings are observed when cgroup controllers are visible",
                             "no compiler, runtime binding, macro expansion, or agent effectiveness coverage"]}
        write_json(self.out / "report.json", report)
        write_json(self.out / "identities.json", self.identities)
        return report


SOURCE = {"system": "reviewed_synthetic_public_envelope"}


def fact(timestamp):
    return {"status": "known", "timestamp": "unix:" + str(timestamp), "source": SOURCE}


def synthetic_task(task_id, available):
    return {"task_id": task_id, "title": "Improve parser parse", "description": "Change parser behavior",
            "acceptance_criteria": ["parse returns the new reviewed value"], "source": SOURCE,
            "created_at": fact(available - 10), "snapshot_available_at": fact(available),
            "text_availability": "known_pre_execution", "captured_at": "unix:" + str(available)}


def envelope(fixture, before, after, task, delivered):
    return {"schema_version": 2, "repository": "https://example.invalid/synthetic-history.git", "landing_branch": "main",
            "before_revision": fixture["commits"][before], "after_revision": fixture["commits"][after],
            "delivery_id": "synthetic-delivery-" + str(delivered), "evidence": "verified_delivery", "source": SOURCE,
            "delivered_at": fact(delivered), "captured_at": "unix:" + str(delivered + 50), "tasks": [task]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("check", help="verify frozen source truth without invoking a candidate")
    run = sub.add_parser("run", help="capture every attempt against explicit private installed binaries")
    for name in ("candidate", "candidate-sha256", "orbit", "orbit-sha256", "source-repo", "source-commit",
                 "plugin-commit", "plugin-manifest-sha256", "output"):
        run.add_argument("--" + name, required=True)
    run.add_argument("--case", action="append", help="capture only named cases for diagnostics; incomplete coverage never qualifies")
    args = parser.parse_args()
    corpus = frozen()
    if args.command == "check":
        print(canonical({"valid": True, "cases": len(corpus["cases"]), "tools": corpus["tools"]}))
        return 0
    evaluation = Evaluation(args, corpus)
    try:
        evaluation.export()
        if args.case:
            require(set(args.case) <= {case["id"] for case in corpus["cases"]}, "unknown selected case")
        for case in corpus["cases"]:
            if args.case and case["id"] not in args.case:
                continue
            for surface in case["surfaces"]:
                evaluation.run_case(case, surface)
            print(canonical({"case": case["id"], "completed_samples": len(evaluation.rows)}), flush=True)
        report = evaluation.finish()
    except (ValueError, OSError, KeyError, TypeError) as error:
        report = evaluation.finish(str(error))
    print(canonical({"report": str(evaluation.out / "report.json"), "qualified": report["qualified"]}))
    return 0 if report["qualified"] else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (ValueError, OSError, KeyError) as error:
        print(canonical({"error": str(error)}), file=sys.stderr)
        sys.exit(2)
