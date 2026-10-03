#!/usr/bin/env python3
"""Stand-ins for bubblewrap in agent-eval tests.

$FAKE_BWRAP_MODE=denied reproduces a host that refuses unprivileged user
namespaces. Any other mode runs the command without confinement, mapping
whole /eval/... arguments back to their host sources, so a probe sees the
host's real paths: an ineffective sandbox.
"""
import os
import sys

sys.dont_write_bytecode = True

ZERO = {"--die-with-parent", "--unshare-user", "--unshare-pid", "--unshare-ipc",
        "--unshare-uts", "--unshare-cgroup-try", "--unshare-net", "--new-session"}
ONE = {"--hostname", "--proc", "--dev", "--tmpfs", "--dir", "--chdir"}
TWO = {"--ro-bind", "--ro-bind-try", "--bind", "--symlink"}


def main():
    argv = sys.argv[1:]
    if argv[:1] == ["--version"]:
        print("bubblewrap 0.0-fake")
        return 0
    if os.environ.get("FAKE_BWRAP_MODE") == "denied":
        print("bwrap: No permissions to create new namespace, likely because the kernel "
              "does not allow non-privileged user namespaces.", file=sys.stderr)
        return 1
    mapping, chdir, index = {}, None, 0
    while index < len(argv) and argv[index] != "--":
        flag = argv[index]
        if flag in ZERO:
            index += 1
        elif flag in ONE:
            if flag == "--chdir":
                chdir = argv[index + 1]
            index += 2
        elif flag in TWO:
            if flag != "--symlink" and argv[index + 2].startswith("/eval"):
                mapping[argv[index + 2]] = argv[index + 1]
            index += 3
        else:
            print(f"fake bwrap: unknown option {flag}", file=sys.stderr)
            return 1
    command = argv[index + 1:]

    def host(value):
        for inside in sorted(mapping, key=len, reverse=True):
            if value == inside or value.startswith(inside + "/"):
                return mapping[inside] + value[len(inside):]
        return value

    env = {key: host(value) for key, value in os.environ.items()}
    if chdir:
        os.chdir(host(chdir))
    os.execve(host(command[0]), [host(arg) for arg in command], env)
    return 127


if __name__ == "__main__":
    sys.exit(main())
