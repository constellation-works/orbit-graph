"""One-shot patch contract. No new agent tools; evaluator stays outside the mount."""
import importlib.util
from pathlib import Path
import re

PROFILE = "one-shot-patch-v1"
VERSION = "7"
CONTRACT = "bounded-unified-text-v1"
HARNESS = ("eval_runner.py", "eval_broker.py", "plugin_profile.py", "reply_provenance.py", "telemetry.py", "patch_profile.py")
HEX = re.compile(r"[0-9a-f]{64}\Z")


def check(value, message):
    if not value:
        raise ValueError(message)


def safe_path(value):
    return (isinstance(value, str) and len(value) <= 240
            and re.fullmatch(r"[A-Za-z0-9_][A-Za-z0-9_./-]*", value) is not None
            and all(p not in {"", ".", ".."} and not p.startswith(".") for p in value.split("/")))


def validate(request):
    pin = request.get("patch")
    check(isinstance(pin, dict) and set(pin) == {
        "contract", "protocol_sha256", "evaluator_sha256", "suite_sha256", "closure_sha256",
        "custody_sha256", "allowed_paths", "entrypoint", "max_patch_bytes", "max_changed_lines",
        "max_files", "test_wall_ms", "test_output_bytes", "resource_limits", "runtime"}, "patch contract fields differ")
    check(pin["contract"] == CONTRACT, "unknown patch format")
    for key in ("protocol_sha256", "evaluator_sha256", "suite_sha256", "closure_sha256", "custody_sha256"):
        check(isinstance(pin[key], str) and HEX.fullmatch(pin[key]), "invalid patch pin: " + key)
    runtime = pin["runtime"]
    check(isinstance(runtime, dict) and set(runtime) == {"harness", "provider_binary_sha256", "model", "baseline_binaries", "bwrap_sha256"},
          "invalid frozen runtime")
    check(set(runtime["harness"]) == set(HARNESS) and set(runtime["baseline_binaries"]) == {"rg", "git", "python"},
          "runtime file/binary set differs")
    check(all(isinstance(v, str) and HEX.fullmatch(v) for v in [*runtime["harness"].values(),
          *runtime["baseline_binaries"].values(), runtime["provider_binary_sha256"], runtime["bwrap_sha256"]]),
          "invalid runtime digest")
    check(isinstance(runtime["model"], dict) and set(runtime["model"]) == {"provider", "name", "version", "settings"}
          and runtime["model"]["provider"] == "codex-cli", "invalid frozen model")
    bounds = pin["resource_limits"]
    check(isinstance(bounds, dict) and set(bounds) == {"memory.max", "pids.max"}
          and all(v is None or (type(v) is int and v > 0) for v in bounds.values()),
          "invalid frozen memory/process limits")
    paths = pin["allowed_paths"]
    check(isinstance(paths, list) and 0 < len(paths) <= 32 and len(set(paths)) == len(paths)
          and all(safe_path(p) and p.startswith(("src/", "crates/", "tests/"))
                  and p.endswith(".py") for p in paths), "explicit Python source paths required")
    check(safe_path(pin["entrypoint"]) and pin["entrypoint"].startswith("src/")
          and pin["entrypoint"].endswith(".py"), "invalid JSON program entrypoint")
    for key, low, high in (("max_patch_bytes", 1, request["limits"]["call_bytes"]),
                           ("max_changed_lines", 1, 10000), ("max_files", 1, len(paths)),
                           ("test_wall_ms", 100, 60000), ("test_output_bytes", 128, 1048576)):
        check(type(pin[key]) is int and low <= pin[key] <= high, "invalid patch bound: " + key)


def evaluator():
    path = Path(__file__).resolve().parents[2] / "evals/patch-effectiveness-v1/eval.py"
    spec = importlib.util.spec_from_file_location("patch_evaluator", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def admit(episode):
    """Before provider output: verify closure/custody and perform sandbox canary."""
    ev = evaluator()
    return ev.admit(episode)


def check_output(text, pin):
    check(isinstance(text, str) and len(text.encode()) <= pin["max_patch_bytes"], "patch oversized")
    # Parsing and path validation are external outcomes; empty output is a no-op.
    check("\x00" not in text, "binary patch refused")


def runtime_pin(model, provider_sha256, binaries, bwrap):
    import eval_runner as runner
    return {"harness": {name: runner.sha256_file(runner.HERE / name) for name in HARNESS},
            "model": model, "provider_binary_sha256": provider_sha256,
            "baseline_binaries": {name: runner.sha256_file(path) for name, path in binaries.items()},
            "bwrap_sha256": runner.sha256_file(bwrap)}


def verify_runtime(episode, provider=None):
    import eval_runner as runner
    pin = episode.request["patch"]["runtime"]
    check(pin["harness"] == {name: runner.sha256_file(runner.HERE / name) for name in HARNESS}, "harness drift before output")
    check(pin["provider_binary_sha256"] == runner.sha256_file(episode.binaries["codex"]), "provider binary drift before output")
    check(pin["baseline_binaries"] == {name: runner.sha256_file(episode.binaries[name]) for name in ("rg", "git", "python")},
          "baseline tool binary drift before output")
    check(pin["model"]["name"] == episode.args.model and pin["model"]["settings"] == episode.recorded_settings(),
          "model/settings drift before output")
    if episode.contained:
        check(pin["bwrap_sha256"] == runner.sha256_file(episode.binaries["bwrap"]), "sandbox binary drift before output")
    if provider is not None:
        check(pin["model"]["version"] == provider["version"], "provider version drift before output")
