#!/usr/bin/env bash
# scripts/check-architecture-map.sh — STD-02 R43: ARCHITECTURE.md maps every
# unit, and the map matches the tree.
#
# Fails, naming the file and the unit, when:
#   (a) a workspace crate has no section in ARCHITECTURE.md: a `##` heading
#       whose first code span is the crate's name;
#   (b) a top-level module of a crate (`src/<name>.rs` or `src/<name>/`;
#       `tests` and the crate roots excluded) is not named in its crate's
#       section: as a top-level entry of the section's file tree, or as a
#       code span `<name>.rs` or `<name>/`;
#   (c) a path the doc writes in code formatting does not exist: a code span
#       that looks like a path (it has a `/` or a file extension), or an entry
#       of a crate section's file tree, resolved through its indentation
#       against the crate's `src/`. A span resolves from the repository root,
#       from the section's crate (`src/`, then the crate directory), or as a
#       path suffix inside that crate (inside the repository outside a crate
#       section). Placeholders (`<..>`, `$`, `*`, `…`) are skipped;
#   (d) a crate root (each lib or bin target) or a top-level module file
#       (`<name>.rs`, else `<name>/mod.rs`) does not begin with a `//!` line.
#
# The crate list comes from `cargo metadata`, so a new crate or module fails
# the gate until the doc names it, with no edit here. Whether a unit's
# sentence matches its `//!` sentence stays a review matter. Needs cargo and
# python3.
set -euo pipefail

cd "$(dirname "$0")/.."

# A workspace with no crate yet has no unit to map.
if ! compgen -G 'crates/*/Cargo.toml' >/dev/null; then
  echo "architecture-map: no crates/*/Cargo.toml yet; nothing to check"
  exit 0
fi

for tool in cargo python3; do
  command -v "$tool" >/dev/null 2>&1 || {
    echo "architecture-map: cannot run: $tool not found" >&2
    exit 2
  }
done
if ! metadata=$(cargo metadata --format-version 1 --no-deps --offline 2>/dev/null); then
  echo "architecture-map: cannot run: cargo metadata failed" >&2
  exit 2
fi

exec python3 -I -c '
import json, os, re, sys

DOC = "ARCHITECTURE.md"
root = os.getcwd()
meta = json.loads(sys.argv[1])
fails = []

def fail(msg):
    fails.append(msg)

def rel(path):
    return os.path.relpath(path, root)

# --- the crates, from cargo metadata --------------------------------------
members = set(meta.get("workspace_members") or [])
crates = []
for pkg in meta["packages"]:
    if members and pkg["id"] not in members:
        continue
    crate_dir = os.path.dirname(pkg["manifest_path"])
    roots = [t["src_path"] for t in pkg["targets"]
             if any(k in ("lib", "rlib", "cdylib", "staticlib", "dylib", "proc-macro", "bin")
                    for k in t["kind"])]
    src = os.path.join(crate_dir, "src")
    crates.append({"name": pkg["name"], "dir": crate_dir, "src": src,
                   "roots": sorted(set(os.path.realpath(r) for r in roots))})
by_name = {c["name"]: c for c in crates}

if not os.path.isfile(DOC):
    print(f"architecture-map: {DOC} is missing; it maps every crate (STD-02 R43)", file=sys.stderr)
    sys.exit(1)
lines = open(DOC, encoding="utf-8").read().split("\n")

# --- the doc: sections, fences and code spans -----------------------------
SPAN = re.compile(r"`([^`\n]+)`")
PATHISH = re.compile(r"[A-Za-z0-9_.][A-Za-z0-9_./-]*")
EXT = re.compile(r"\.(rs|sh|py|md|toml|yml|yaml|txt|json|lock)$")
PLACEHOLDER = re.compile(r"[<>$*…]")

def looks_like_path(text):
    return (PATHISH.fullmatch(text) is not None and not text.startswith("-")
            and ("/" in text or EXT.search(text) is not None))

sections = []  # (crate name or None, first line index, last line index)
current = (None, 0)
for i, line in enumerate(lines):
    if line.startswith("## "):
        sections.append((current[0], current[1], i))
        spans = SPAN.findall(line)
        current = (spans[0] if spans and spans[0] in by_name else None, i)
sections.append((current[0], current[1], len(lines)))

def walk_paths(base):
    """Every file and directory under base, relative to it."""
    out = set()
    for dirpath, dirnames, filenames in os.walk(base):
        dirnames[:] = [d for d in dirnames if d not in (".git", "target")]
        for name in dirnames + filenames:
            out.add(os.path.relpath(os.path.join(dirpath, name), base))
    return out

repo_paths = None
crate_paths = {}

def exists_span(text, crate):
    global repo_paths
    path = text.rstrip("/")
    if os.path.exists(os.path.join(root, path)):
        return True
    if crate is not None:
        if any(os.path.exists(os.path.join(base, path)) for base in (crate["src"], crate["dir"])):
            return True
        if crate["name"] not in crate_paths:
            crate_paths[crate["name"]] = walk_paths(crate["dir"])
        scope = crate_paths[crate["name"]]
    else:
        if repo_paths is None:
            repo_paths = walk_paths(root)
        scope = repo_paths
    return any(p == path or p.endswith("/" + path) for p in scope)

documented = {c["name"]: set() for c in crates}
seen_sections = set()
for crate_name, start, end in sections:
    crate = by_name.get(crate_name)
    if crate is not None:
        seen_sections.add(crate_name)
    in_fence = False
    stack = []  # (indent, directory path relative to src)
    for i in range(start, end):
        line = lines[i]
        lineno = i + 1
        if line.lstrip().startswith("```"):
            in_fence = not in_fence
            stack = []
            continue
        if in_fence:
            # A crate section'"'"'s fenced block is its file tree under src/.
            if crate is None or not line.strip() or line.lstrip().startswith("<!--"):
                continue
            indent = len(line) - len(line.lstrip(" "))
            field = re.split(r"\s{2,}", line.strip())[0]
            while stack and stack[-1][0] >= indent:
                stack.pop()
            parent = stack[-1][1] if stack else ""
            for token in field.split(" "):
                if not looks_like_path(token) and not PLACEHOLDER.search(token):
                    continue
                full = parent + token
                if not stack:
                    documented[crate_name].add(re.split(r"[/.]", token)[0])
                if token.endswith("/"):
                    stack.append((indent, full))
                if PLACEHOLDER.search(full):
                    continue
                if not os.path.exists(os.path.join(crate["src"], full)):
                    fail(f"{DOC}:{lineno}: `{crate_name}`: the tree names "
                         f"`{full}`, which does not exist under {rel(crate['"'"'src'"'"'])}/")
            continue
        for text in SPAN.findall(line):
            if crate is not None and (text.endswith(".rs") or text.endswith("/")) \
                    and PATHISH.fullmatch(text) and "/" not in text.rstrip("/"):
                documented[crate_name].add(re.split(r"[/.]", text)[0])
            if PLACEHOLDER.search(text) or not looks_like_path(text):
                continue
            if not exists_span(text, crate):
                where = f"`{crate_name}` section" if crate is not None else "the doc"
                fail(f"{DOC}:{lineno}: {where} names `{text}`, which does not exist")

# --- (a), (b), (d) per crate ----------------------------------------------
def first_line(path):
    try:
        with open(path, encoding="utf-8") as handle:
            return handle.readline()
    except OSError:
        return ""

def needs_doc(path, unit):
    if not first_line(path).startswith("//!"):
        fail(f"{rel(path)}: `{unit}` does not begin with a `//!` doc line")

for crate in crates:
    name = crate["name"]
    if name not in seen_sections:
        fail(f"{DOC}: crate `{name}` ({rel(crate['"'"'dir'"'"'])}) has no section "
             f"(a `## ` heading whose first code span is `{name}`)")
    for path in crate["roots"]:
        needs_doc(path, f"{name}: crate root {os.path.basename(path)}")
    if not os.path.isdir(crate["src"]):
        continue
    roots = set(crate["roots"])
    modules = {}
    for entry in sorted(os.listdir(crate["src"])):
        path = os.path.join(crate["src"], entry)
        if entry.startswith(".") or entry in ("tests", "bin"):
            continue
        if os.path.isdir(path):
            modules.setdefault(entry, None)
            if os.path.isfile(os.path.join(path, "mod.rs")):
                modules[entry] = modules[entry] or os.path.join(path, "mod.rs")
        elif entry.endswith(".rs") and os.path.realpath(path) not in roots:
            modules[entry[:-3]] = path
    for module, path in sorted(modules.items()):
        shown = rel(path) if path else rel(os.path.join(crate["src"], module)) + "/"
        if name in seen_sections and module not in documented[name]:
            fail(f"{DOC}: the `{name}` section does not name module `{module}` ({shown})")
        if path:
            needs_doc(path, f"{name}::{module}")

for message in fails:
    print(f"architecture-map: {message}", file=sys.stderr)
if fails:
    print(f"architecture-map: {len(fails)} problem(s); map each unit in {DOC} (STD-02 R43)", file=sys.stderr)
    sys.exit(1)
print(f"architecture-map: ok ({len(crates)} crate(s))")
' "$metadata"
