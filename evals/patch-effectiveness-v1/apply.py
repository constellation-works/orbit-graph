"""Conservative unified text diff: existing regular files only, exact context, no Git extensions."""
import os
from pathlib import Path
import re
import stat


class Rejected(ValueError):
    def __init__(self, outcome, message):
        self.outcome = outcome
        super().__init__(message)


def reject(condition, outcome, message):
    if not condition:
        raise Rejected(outcome, message)


def parse(data, pin):
    from patch_profile import safe_path
    reject(len(data) <= pin["max_patch_bytes"], "invalid_patch", "patch oversized")
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError:
        raise Rejected("invalid_patch", "patch is not UTF-8") from None
    reject("\x00" not in text and "\r" not in text, "invalid_patch", "binary/CR patch refused")
    if not text:  # explicit no-op; behavioral tests still run
        return [], 0
    reject(text.endswith("\n"), "invalid_patch", "patch must end in LF")
    lines, pos, files, changes = text.splitlines(keepends=True), 0, [], 0
    while pos < len(lines):
        old = re.fullmatch(r"--- a/([^\n]+)\n", lines[pos])
        reject(old is not None and pos + 1 < len(lines), "invalid_patch", "expected --- a/path")
        new = re.fullmatch(r"\+\+\+ b/([^\n]+)\n", lines[pos + 1])
        reject(new is not None, "invalid_patch", "expected +++ b/path")
        name = old[1]
        reject(safe_path(name) and new[1] == name and name in pin["allowed_paths"],
               "rejected_path", "path not declared, renamed, absolute or unsafe")
        reject(name not in [f[0] for f in files], "invalid_patch", "duplicate file")
        pos += 2
        hunks = []
        while pos < len(lines) and lines[pos].startswith("@@"):
            match = re.fullmatch(r"@@ -(\d{1,8})(?:,(\d{1,8}))? \+(\d{1,8})(?:,(\d{1,8}))? @@\n", lines[pos])
            reject(match is not None, "invalid_patch", "unsupported hunk header")
            start_old, count_old, start_new, count_new = [int(v) if v is not None else 1 for v in match.groups()]
            reject(start_old > 0 and start_new > 0, "invalid_patch", "zero line positions refused")
            pos += 1
            before, after = [], []
            while pos < len(lines) and lines[pos][:1] in {" ", "+", "-"}:
                # A file header starts the next section only after hunk counts are complete.
                if len(before) == count_old and len(after) == count_new:
                    break
                line = lines[pos]
                if line[0] in " -":
                    before.append(line[1:])
                if line[0] in " +":
                    after.append(line[1:])
                changes += line[0] != " "
                pos += 1
            reject(len(before) == count_old and len(after) == count_new,
                   "invalid_patch", "hunk counts differ")
            hunks.append((start_old, start_new, before, after))
        reject(bool(hunks), "invalid_patch", "file has no hunks")
        files.append((name, hunks))
    reject(len(files) <= pin["max_files"] and changes <= pin["max_changed_lines"],
           "invalid_patch", "diff size bound exceeded")
    return files, changes


def apply(root, data, pin):
    """Prepare every replacement before writing; private clone has no concurrent writers.

    Open directory FDs with NOFOLLOW for each component and use those same FDs
    for final file opens. No patch-controlled string ever reaches a command.
    """
    files, changes = parse(data, pin)
    pending, opened = [], []
    root_fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    opened.append(root_fd)
    try:
        for name, hunks in files:
            parent = root_fd
            for component in name.split("/")[:-1]:
                try:
                    parent = os.open(component, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=parent)
                except OSError:
                    raise Rejected("rejected_path", "non-directory or symlink parent") from None
                opened.append(parent)
            try:
                fd = os.open(name.split("/")[-1], os.O_RDONLY | os.O_NOFOLLOW, dir_fd=parent)
            except OSError:
                raise Rejected("rejected_path", "missing or symlink target") from None
            with os.fdopen(fd, "rb") as stream:
                info = os.fstat(stream.fileno())
                reject(stat.S_ISREG(info.st_mode) and info.st_nlink == 1,
                       "rejected_path", "non-regular or hardlinked target")
                original = stream.read(16 * 1024 * 1024 + 1)
            reject(len(original) <= 16 * 1024 * 1024, "invalid_patch", "source too large")
            try:
                source = original.decode("utf-8").splitlines(keepends=True)
            except UnicodeDecodeError:
                raise Rejected("invalid_patch", "binary source") from None
            result, cursor = [], 0
            for start_old, start_new, before, after in hunks:
                offset = start_old - 1
                reject(cursor <= offset <= len(source), "apply_failure", "overlapping or misplaced hunk")
                result.extend(source[cursor:offset])
                reject(len(result) == start_new - 1 and source[offset:offset + len(before)] == before,
                       "apply_failure", "source context differs")
                result.extend(after)
                cursor = offset + len(before)
            result.extend(source[cursor:])
            replacement = "".join(result).encode()
            reject(len(replacement) <= 16 * 1024 * 1024, "invalid_patch", "result too large")
            pending.append((parent, name.split("/")[-1], info, replacement, replacement != original))
        for parent, leaf, info, replacement, _ in pending:
            fd = os.open(leaf, os.O_WRONLY | os.O_NOFOLLOW, dir_fd=parent)
            with os.fdopen(fd, "wb") as stream:
                current = os.fstat(stream.fileno())
                reject((current.st_dev, current.st_ino, current.st_nlink) == (info.st_dev, info.st_ino, 1),
                       "rejected_path", "target changed during application")
                stream.truncate(0)
                stream.write(replacement)
        return {"patch_bytes": len(data), "changed_lines": changes, "files": len(files),
                "no_op": not any(row[4] for row in pending)}
    finally:
        for fd in reversed(opened):
            os.close(fd)
