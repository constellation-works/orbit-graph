#!/usr/bin/env python3
"""Stand-in for orbit-graph: echoes its argument vector and cwd as JSON."""
import json
import os
import sys

sys.dont_write_bytecode = True

if sys.argv[1:2] == ["version"]:
    print(json.dumps({"crate_version": "0.0.0-fake"}))
    sys.exit(0)
if sys.argv[1:2] == ["sync"]:
    os.makedirs(".orbit-graph", exist_ok=True)
    with open(".orbit-graph/fake.db", "w") as stream:
        stream.write("index\n")
print(json.dumps({"argv": sys.argv[1:], "cwd": os.getcwd()}))
