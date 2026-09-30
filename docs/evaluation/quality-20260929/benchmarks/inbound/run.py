#!/usr/bin/env python3
"""Run the same locked release benchmark against an existing repository copy.

The manifest, sources, lockfile and build output are copied to a new temporary
project. The printed project path is retained for inspecting or deleting the
results. Nothing is built or generated inside the source repository.
"""

import argparse
import json
import os
import pathlib
import shutil
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("repository", type=pathlib.Path)
parser.add_argument("--offline", action="store_true", help="require dependencies already cached")
args = parser.parse_args()
repository = args.repository.resolve(strict=True)
library = repository / "crates/orbit-graph"
if not (library / "Cargo.toml").is_file():
    parser.error(f"no orbit-graph library at {library}")

harness = pathlib.Path(__file__).resolve().parent
project = pathlib.Path(tempfile.mkdtemp(prefix="orbit-graph-inbound-benchmark-"))
shutil.copytree(harness / "src", project / "src")
shutil.copyfile(harness / "Cargo.lock", project / "Cargo.lock")
(project / "Cargo.toml").write_text(
    f'''[package]
name = "orbit-graph-inbound-benchmark"
version = "0.0.0"
edition = "2024"

[dependencies]
orbit-graph = {{ path = {json.dumps(str(library))} }}
git2 = {{ version = "=0.21.0", default-features = false }}
rusqlite = {{ version = "=0.40.2", features = ["bundled"] }}
tempfile = "=3.27.0"
''',
    encoding="utf-8",
)
command = [
    "cargo", "run", "--release", "--locked", "--manifest-path",
    str(project / "Cargo.toml"), "--bin", "orbit-graph-inbound-benchmark",
]
if args.offline:
    command.append("--offline")
environment = os.environ.copy()
environment["CARGO_TARGET_DIR"] = str(project / "target")
print(f"benchmark_project={project}", flush=True)
print(f"source_repository={repository}", flush=True)
# Replace this process with Cargo, preserving its exit status and signal
# handling. Cargo supervises its own build subprocesses; the harness adds none.
os.execvpe(command[0], command, environment)
