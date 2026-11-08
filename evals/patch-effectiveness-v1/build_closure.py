#!/usr/bin/env python3
"""Materialize a reviewable offline stdlib closure from the installed system Python.

No downloads, packages, credentials or model calls. Root reviews and freezes the
resulting exact file/mode manifest before any candidate. Linux qualification only.
"""
import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import sysconfig

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "scripts/agent-eval"))
import patch_profile


def build(out):
    ev = patch_profile.evaluator()
    out = Path(out).absolute()
    ev.disk_gate(out.parent)
    ev.check(not out.exists(), "closure output already exists")
    ev.runner.make_private_dir(out)
    def make_directory(relative):
        current = out
        for part in Path(relative).parts:
            current = current / part
            if not current.exists():
                current.mkdir(mode=0o700)
    for name in ("proc", "dev", "tmp", "source", "usr/bin"):
        make_directory(name)
    python = Path(sys.executable).resolve()
    ev.check(str(python).startswith("/usr/bin/"), "run with /usr/bin/python3 for a system stdlib closure")
    stdlib = Path(sysconfig.get_path("stdlib"))
    binaries = [python]

    def copy(source, relative):
        target = out / relative
        make_directory(target.parent.relative_to(out))
        shutil.copyfile(source, target)
        target.chmod(0o500 if os.access(source, os.X_OK) else 0o400)

    copy(python, "usr/bin/python3")
    for directory, dirs, files in os.walk(stdlib):
        dirs[:] = sorted(d for d in dirs if d not in {"__pycache__", "site-packages", "dist-packages", "test", "tests"})
        for name in sorted(files):
            source = Path(directory) / name
            if source.suffix not in {".py", ".so"}:
                continue
            relative = source.relative_to("/")
            copy(source, relative)
            if source.suffix == ".so":
                binaries.append(source)
    libraries = set()
    for binary in binaries:
        result = subprocess.run(["/usr/bin/ldd", str(binary)], env={"PATH": "/usr/bin", "LC_ALL": "C"},
                                capture_output=True, timeout=10, check=True)
        for line in result.stdout.decode().splitlines():
            ev.check("not found" not in line, "installed stdlib has missing shared libraries")
            match = re.search(r"(?:=> )?(/[^ ]+) \(", line)
            if match:
                libraries.add(match[1])
    for name in sorted(libraries):
        copy(Path(name), Path(name).relative_to("/"))
    lock = ev.tree_lock(out)
    ev.private_json(out.parent / (out.name + ".lock.json"), lock)
    return {"closure": str(out), "closure_sha256": ev.runner.digest(lock), "files": len(lock),
            "python": sys.version, "qualification": "root must review manifest and run strict canary/controls"}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", required=True)
    args = parser.parse_args()
    print(json.dumps(build(args.out), indent=2, sort_keys=True))
