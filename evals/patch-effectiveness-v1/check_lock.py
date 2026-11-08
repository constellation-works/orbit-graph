#!/usr/bin/env python3
"""Check exact proposed qualification artifacts; never update historical profiles."""
import hashlib
import json
from pathlib import Path
import sys

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
FILES = ["evals/patch-effectiveness-v1/" + name for name in (
    "eval.py", "apply.py", "build_closure.py", "protocol.json", "check_lock.py", "test_eval.py",
    "fixtures/source.py", "fixtures/visible.py", "fixtures/suite.json", "fixtures/review.md")]
FILES += ["scripts/agent-eval/" + name for name in (
    "eval_runner.py", "eval_broker.py", "plugin_profile.py", "reply_provenance.py", "telemetry.py", "patch_profile.py")]
FILES += ["evals/plugin-agent-navigation/eval.py", "scripts/agent-eval/tests/fake_codex.py"]


def check():
    lock = json.loads((HERE / "qualification.lock.json").read_text())
    actual = {name: hashlib.sha256((ROOT / name).read_bytes()).hexdigest() for name in FILES}
    if lock != {"schema_version": 1, "status": "proposed-qualification-only", "files": actual}:
        raise ValueError("qualification lock drift: review artifacts and regenerate this new profile's lock")
    print(json.dumps({"status": "passed", "files": len(actual), "runtime_admitted": False}))


if __name__ == "__main__":
    try:
        check()
    except (ValueError, OSError) as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
