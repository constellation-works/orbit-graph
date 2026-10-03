"""Syntax-validated Rust declaration index; no compilation or macro expansion."""
import json
from pathlib import Path
import subprocess

HERE = Path(__file__).resolve().parent
PARSER_SOURCES = ("syntax/Cargo.toml", "syntax/Cargo.lock", "syntax/src/main.rs")
PARSER = HERE.parents[1] / ".orbit/tmp/source-identity-syntax-target/debug/source-identity-syntax"


class UnsupportedRust(ValueError):
    pass


def parse(source):
    try:
        process = subprocess.run([str(PARSER)], input=source, text=True, encoding="utf-8",
                                 capture_output=True, timeout=10, env={})
        if process.returncode or process.stderr:
            raise UnsupportedRust("Rust syntax frontend failed: " + process.stderr[:500])
        output = json.loads(process.stdout)
        if "error" in output:
            raise UnsupportedRust(output["error"])
        expected = {name: (HERE / name).read_text(encoding="utf-8") for name in PARSER_SOURCES}
        if output.get("sources") != expected:
            raise UnsupportedRust("stale syntax frontend; rebuild offline using README instructions")
        return output["items"]
    except (OSError, subprocess.TimeoutExpired, ValueError, KeyError) as error:
        raise UnsupportedRust("Rust syntax frontend unavailable/invalid: " + str(error)) from error


class RustIndex:
    def __init__(self, read, root):
        self.root = root
        self.read = read
        self.declarations = []
        self.imports = {}
        self.modules = set()
        self.files = set()
        self.pending = []
        self.diagnostics = []
        self.parse_file(root, (), root.rsplit("/", 1)[0] if "/" in root else "")
        self.finish()
        self.diagnostics = [{"file": d["file"], "line": d["line"], "name": d["leaf"],
                             "reason": d["unsupported"]} for d in self.declarations if d.get("unsupported")]

    def parse_file(self, file, module, module_dir, uncertain=None):
        if file in self.files or len(self.files) >= 128:
            raise UnsupportedRust("reused/cyclic module file or module limit exceeded")
        self.files.add(file)
        self.scope(parse(self.read(file)), file, module, module_dir, uncertain=uncertain)

    def scope(self, nodes, file, module, module_dir, context=None, uncertain=None):
        for node in nodes:
            kind = node["kind"]
            reason = uncertain or node.get("unsupported")
            if kind in ("fn", "struct", "enum", "trait", "unsupported"):
                item = {"language": "rust", "file": file, "line": node["line"],
                        "column": node["column"], "end_line": node["end_line"], "kind": kind,
                        "module": "::".join(("crate",) + module), "owner": None,
                        "trait": None, "leaf": node["name"], "unsupported": reason}
                if kind == "unsupported":
                    item["unsupported"] = reason or "named binding is outside the scored declaration kinds"
                self.declarations.append(item)
                if context:
                    self.pending.append((item, module, context))
                if kind == "trait":
                    self.scope(node["children"], file, module, module_dir,
                               ("trait", node["name"], None), reason)
            elif kind == "impl":
                self.scope(node["children"], file, module, module_dir,
                           ("impl", node["owner"], node["trait"]), reason)
            elif kind == "use":
                key = (module, node["name"])
                if key in self.imports or uncertain:
                    raise UnsupportedRust("ambiguous/conditional import binding")
                self.imports[key] = node["target"]
            elif kind == "mod":
                name = node["name"]
                child = module + (name,)
                if child in self.modules:
                    raise UnsupportedRust("duplicate/conditional module binding")
                self.modules.add(child)
                if node["children"] is not None:
                    if node["path"]:
                        raise UnsupportedRust("path on inline module unsupported")
                    self.scope(node["children"], file, child, self.join(module_dir, name), uncertain=reason)
                else:
                    if not node["path"]:
                        raise UnsupportedRust("external modules require literal #[path]")
                    if file != self.root:
                        raise UnsupportedRust("external modules declared inside external files are not supported")
                    child_file = self.join(module_dir, node["path"])
                    child_dir = (child_file.rsplit("/", 1)[0] if child_file.endswith("/mod.rs")
                                 else child_file.removesuffix(".rs"))
                    if not child_file.endswith(".rs"):
                        raise UnsupportedRust("module path must end in .rs")
                    self.parse_file(child_file, child, child_dir, reason)
            else:
                raise UnsupportedRust("unknown syntax frontend node")

    @staticmethod
    def join(parent, child):
        return parent + "/" + child if parent else child

    def qualify(self, path, module, seen=()):
        parts = path.split("::")
        if parts[0] == "crate":
            return path
        if parts[0] in ("std", "core"):
            return path
        if parts[0] == "self":
            return "::".join(("crate",) + module + tuple(parts[1:]))
        if parts[0] == "super":
            base = list(module)
            while parts and parts[0] == "super":
                if not base:
                    raise UnsupportedRust("super escapes crate")
                base.pop()
                parts.pop(0)
            return "::".join(["crate", *base, *parts])
        key = (module, parts[0])
        if key in self.imports:
            if key in seen:
                raise UnsupportedRust("cyclic import aliases")
            imported = self.qualify(self.imports[key], module, (*seen, key))
            return "::".join([imported, *parts[1:]])
        return "::".join(("crate",) + module + tuple(parts))

    def finish(self):
        reserved = {"std", "core"}
        if (any(module[-1] in reserved for module in self.modules)
                or any(alias in reserved for _, alias in self.imports)
                or any(item["leaf"] in reserved and item["kind"] != "fn"
                       for item in self.declarations)):
            raise UnsupportedRust("shadowed std/core roots make external trait identity uncertain")
        types = {}
        associated = {id(item) for item, _, _ in self.pending}
        for item in self.declarations:
            if (item["kind"] in ("struct", "enum", "trait", "unsupported")
                    and id(item) not in associated):
                path = item["module"] + "::" + item["leaf"]
                if path in types or (tuple(item["module"].split("::")[1:]), item["leaf"]) in self.imports:
                    raise UnsupportedRust("duplicate type/import binding")
                types[path] = item["kind"]
        uncertain_types = {d["module"] + "::" + d["leaf"]: d["unsupported"]
                           for d in self.declarations if d.get("unsupported") and id(d) not in associated}
        owner_bindings = {}
        for path in types:
            owner_bindings.setdefault(path.split("::")[-1], set()).add(path)
        for (scope, alias), _ in self.imports.items():
            target = self.qualify(alias, scope)
            if target not in types and not target.startswith(("std::", "core::")):
                raise UnsupportedRust("only verified local type or std/core imports are supported")
            # Imported names remain blockers even when no local method uses them.
            owner_bindings.setdefault(alias, set()).add(target)
            prefix = "::".join(("crate",) + scope)
            if any(d["module"] == prefix and d["leaf"] == alias and d["owner"] is None
                   for d in self.declarations):
                raise UnsupportedRust("import collides with a declaration binding")
        for item, module, (mode, owner, trait) in self.pending:
            owner = self.qualify(owner, module)
            if mode == "trait":
                item["owner"], item["trait"] = owner, owner
                continue
            if types.get(owner) not in ("struct", "enum"):
                raise UnsupportedRust(f"impl owner {owner} is not a verified local type")
            item["owner"] = owner
            if owner in uncertain_types:
                item["unsupported"] = uncertain_types[owner]
            if trait:
                trait = self.qualify(trait, module)
                if types.get(trait) != "trait" and not trait.startswith(("std::", "core::")):
                    raise UnsupportedRust(f"unverified trait binding {trait}")
                item["trait"] = trait
                if trait in uncertain_types:
                    item["unsupported"] = uncertain_types[trait]
        for item in self.declarations:
            module = tuple(item["module"].split("::")[1:])
            owner = item["owner"]
            full = (owner or item["module"]) + "::" + item["leaf"]
            names = {full, full.removeprefix("crate::")}
            if owner:
                owners = {owner, owner.removeprefix("crate::")}
                owner_leaf = owner.split("::")[-1]
                if owner_bindings.get(owner_leaf) == {owner}:
                    owners.add(owner_leaf)
                for (scope, alias) in self.imports:
                    if scope == module and owner_bindings.get(alias) == {owner}:
                        owners.add(alias)
                names.update(name + "::" + item["leaf"] for name in owners)
                if item["trait"]:
                    trait = item["trait"]
                    traits = {trait, trait.removeprefix("crate::")}
                    leaf = trait.split("::")[-1]
                    if self.qualify(leaf, module) == trait:
                        traits.add(leaf)
                    for (scope, alias) in self.imports:
                        if scope == module and self.qualify(alias, module) == trait:
                            traits.add(alias)
                    names.update(f"<{o} as {t}>::{item['leaf']}" for o in owners for t in traits)
            else:
                names.add(item["leaf"])
            item["spellings"] = names
