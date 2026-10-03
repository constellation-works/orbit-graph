"""Prospective source identity and citation contract. No source imports/execution."""
import argparse
import ast
from collections import Counter
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import sys

from rust_source import RustIndex, UnsupportedRust

HERE = Path(__file__).resolve().parent
CONTRACT = json.loads((HERE / "contract.json").read_text(encoding="utf-8"))
IDENTITY_FIELDS = CONTRACT["identity"]


class ContractError(ValueError):
    """Invalid evaluator inputs/truth, as distinct from an uncredited answer."""


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def canonical_path(value):
    if not isinstance(value, str) or not value or "\\" in value or "\x00" in value:
        raise ContractError("expected a canonical relative POSIX file path")
    path = PurePosixPath(value)
    if path.is_absolute() or any(p in (".", "..") for p in value.split("/")) or str(path) != value:
        raise ContractError("file path must be relative, normalized and contained")
    return path


def identity(item):
    return {field: item[field] for field in IDENTITY_FIELDS}


def identity_key(item):
    return json.dumps(item, sort_keys=True, separators=(",", ":"))


def literal(node):
    # No ast.literal_eval: recognize syntax without constructing large values.
    if isinstance(node, ast.Constant):
        return type(node.value) in (str, bytes, int, float, complex, bool, type(None))
    if isinstance(node, ast.Tuple):
        return all(literal(child) for child in node.elts)
    if isinstance(node, ast.UnaryOp) and isinstance(node.op, (ast.UAdd, ast.USub)):
        return isinstance(node.operand, ast.Constant) and type(node.operand.value) in (int, float, complex)
    return False


def python_index(text, file):
    """Index a closed module syntax; uncertain top-level effects reject the module."""
    tree = ast.parse(text, filename=file)
    module = file.removesuffix(".py").replace("/", ".")
    if module.endswith(".__init__"):
        module = module.removesuffix(".__init__")
    if not module or any(not re.fullmatch(r"[A-Za-z_]\w*", x, re.ASCII) for x in module.split(".")):
        raise ContractError("Python module path must consist of identifier components")
    declarations, diagnostics = [], []

    def scope(body, owner=None):
        bindings = []
        candidates = []
        for node in body:
            name, kind, reason = None, None, None
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
                name = node.name
                kind = "class" if isinstance(node, ast.ClassDef) else "function"
                if node.decorator_list:
                    reason = "decorated declarations have uncertain runtime identity"
                elif isinstance(node, ast.ClassDef) and (node.bases or node.keywords):
                    reason = "class inheritance/metaclass semantics unsupported"
            elif isinstance(node, (ast.Import, ast.ImportFrom)) and owner is None:
                for imported in node.names:
                    if imported.name == "*":
                        raise ContractError("wildcard import leaves the module binding set unknown")
                    bound = imported.asname or (imported.name.split(".")[0]
                                               if isinstance(node, ast.Import) else imported.name)
                    bindings.append(bound)
                    candidates.append((node, bound, "import", "import aliases are not source declarations in this contract"))
                continue
            elif isinstance(node, ast.Assign):
                if len(node.targets) == 1 and isinstance(node.targets[0], ast.Name):
                    name, kind = node.targets[0].id, "assignment"
                    if owner or not literal(node.value):
                        reason = "only module-level literal assignments supported; dynamic/alias values are uncertain"
                else:
                    raise ContractError("destructuring/chained/attribute assignments are unsupported module effects")
            elif isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name):
                name, kind = node.target.id, "assignment"
                if owner or node.value is None or not literal(node.value):
                    reason = "annotated assignment needs a module-level literal value"
                if not isinstance(node.annotation, (ast.Name, ast.Constant)):
                    reason = "executable/complex annotations unsupported"
            elif isinstance(node, ast.Expr) and isinstance(node.value, ast.Constant) and isinstance(node.value.value, str):
                continue
            elif isinstance(node, ast.Pass):
                continue
            else:
                raise ContractError("unsupported Python module/class statement; conditionals and mutation may change bindings")
            bindings.append(name)
            candidates.append((node, name, kind, reason))
        counts = Counter(bindings)
        for node, name, kind, reason in candidates:
            if counts[name] != 1:
                reason = "duplicate/rebound Python declaration"
            if reason:
                diagnostics.append({"file": file, "line": node.lineno, "name": name, "reason": reason})
                if kind == "class":
                    raise ContractError("unsupported class binding may hide method ownership: " + reason)
            start = node.lineno
            item = {"language": "python", "file": file, "line": start,
                    "column": node.col_offset + 1, "end_line": node.end_lineno,
                    "kind": kind, "module": module, "owner": owner, "trait": None,
                    "leaf": name}
            qualified = (owner or module) + "." + name
            item["spellings"] = {qualified, (owner.removeprefix(module + ".") + "." + name) if owner else name}
            item["unsupported"] = reason
            declarations.append(item)
            if kind == "class":
                scope(node.body, qualified)
    scope(tree.body)
    return declarations, diagnostics


class Project:
    """One explicit source universe, read once and hashed from the parsed bytes.

    The caller freezes the manifest before collecting answers. Files outside that
    manifest cannot be cited, and answers never choose a smaller ambiguity universe.
    """
    def __init__(self, root, *, rust_root=None, python_files=()):
        self.root = Path(root).resolve(strict=True)
        if not self.root.is_dir():
            raise ContractError("root is not a directory")
        self.sources = {}
        self.declarations = []
        self.diagnostics = []
        self.failures = {}
        if rust_root is not None:
            canonical_path(rust_root)
            if not rust_root.endswith(".rs"):
                raise ContractError("rust_root must name an .rs file")
            try:
                index = RustIndex(self.read, rust_root)
                self.declarations.extend(index.declarations)
            except (UnsupportedRust, ContractError, RecursionError) as error:
                self.failures["rust"] = str(error)
        if not isinstance(python_files, (list, tuple)) or len(python_files) > 128:
            raise ContractError("python_files must be an array of at most 128 paths")
        if any(not isinstance(file, str) for file in python_files):
            raise ContractError("python_files entries must be strings")
        if len(set(python_files)) != len(python_files):
            raise ContractError("duplicate Python manifest path")
        python_declarations = []
        for file in python_files:
            canonical_path(file)
            if not file.endswith(".py"):
                raise ContractError("python_files must name .py files")
            try:
                declarations, diagnostics = python_index(self.read(file), file)
                python_declarations.extend(declarations)
                self.diagnostics.extend(diagnostics)
            except (SyntaxError, ContractError, RecursionError) as error:
                self.failures["python"] = f"{file}: {error}"
        # Any incomplete language index cannot safely prove name uniqueness.
        if "python" not in self.failures:
            bindings = Counter(name for item in python_declarations for name in item["spellings"])
            for item in python_declarations:
                if item["owner"]:
                    short = item["owner"].removeprefix(item["module"] + ".")
                    parts = short.split(".")
                    if any(bindings[".".join(parts[:n])] != 1 for n in range(1, len(parts) + 1)):
                        item["spellings"].discard(short + "." + item["leaf"])
            self.declarations.extend(python_declarations)
        self.manifest = {"rust_root": rust_root, "python_files": list(python_files)}

    def read(self, file):
        canonical_path(file)
        if file in self.sources:
            return self.sources[file]["text"]
        path = self.root
        for part in PurePosixPath(file).parts:
            path = path / part
            if path.is_symlink():
                raise ContractError(f"symlink source paths unsupported: {file}")
        try:
            path.resolve(strict=True).relative_to(self.root)
            with path.open("rb") as handle:
                raw = handle.read(CONTRACT["limits"]["source_bytes_per_file"] + 1)
        except (OSError, ValueError) as error:
            raise ContractError(f"cannot read contained source {file}: {error}") from error
        if len(raw) > CONTRACT["limits"]["source_bytes_per_file"]:
            raise ContractError(f"source size limit exceeded: {file}")
        try:
            text = raw.decode("utf-8")
        except UnicodeDecodeError as error:
            raise ContractError(f"source must be UTF-8: {file}") from error
        if "\r" in text or "\x00" in text:
            raise ContractError(f"source must use LF without NUL: {file}")
        self.sources[file] = {"text": text, "sha256": sha256(raw)}
        return text

    def check(self, selector):
        result = {"identity_ok": False, "citation_ok": False, "identity": None,
                  "reason": None, "citation_reason": "identity unresolved"}
        try:
            if not isinstance(selector, dict) or set(selector) != {"language", "name", "file", "line", "kind", "citation"}:
                raise ContractError("selector requires exactly language/name/file/line/kind/citation")
            language = selector["language"]
            if language not in ("rust", "python"):
                raise ContractError("unsupported language")
            canonical_path(selector["file"])
            if type(selector["line"]) is not int or selector["line"] < 1:
                raise ContractError("line must be a positive integer")
            name = selector["name"]
            if not isinstance(name, str) or len(name) > 1024:
                raise ContractError("name must be a string of at most 1024 characters")
            if language == "rust":
                path = r"[A-Za-z_]\w*(?:::[A-Za-z_]\w*)*"
                if not re.fullmatch(rf"(?:{path}|<{path} as {path}>::[A-Za-z_]\w*)", name, re.ASCII):
                    raise ContractError("malformed Rust spelling; use ::, dot spellings are not admitted")
            elif not re.fullmatch(r"[A-Za-z_]\w*(?:\.[A-Za-z_]\w*)*", name, re.ASCII):
                raise ContractError("malformed Python spelling")
            if not isinstance(selector["kind"], str):
                raise ContractError("kind must be a string")
            if language in self.failures:
                raise ContractError("unsupported source context: " + self.failures[language])
            candidates = [d for d in self.declarations if d["language"] == language and name in d["spellings"]]
            if len(candidates) != 1:
                reason = "ambiguous spelling" if candidates else "unresolved or unsupported declaration"
                raise ContractError(reason)
            declaration = candidates[0]
            if declaration.get("unsupported"):
                raise ContractError(declaration["unsupported"])
            if any(selector[key] != declaration[key] for key in ("file", "line", "kind")):
                raise ContractError("spelling resolves to a different file, declaration line or kind")
            result.update(identity_ok=True, identity=identity(declaration))
            result["citation_ok"], result["citation_reason"] = self.citation(declaration, selector["citation"])
        except (ContractError, TypeError) as error:
            result["reason"] = str(error)
        return result

    def citation(self, declaration, citation):
        if not isinstance(citation, dict) or set(citation) != {"start_line", "end_line", "quote"}:
            return False, "citation requires exactly start_line/end_line/quote"
        start, end = citation["start_line"], citation["end_line"]
        if type(start) is not int or type(end) is not int or not isinstance(citation["quote"], str):
            return False, "citation line bounds must be integers and quote a string"
        if not (start == declaration["line"] <= end <= declaration["end_line"]):
            return False, "citation must start at declaration line and remain in its span"
        lines = self.sources[declaration["file"]]["text"].split("\n")
        if citation["quote"] != "\n".join(lines[start - 1:end]):
            return False, "quote differs from exact complete source lines"
        return True, None

    def pins(self):
        return {"contract": CONTRACT["contract"],
                "contract_sha256": sha256((HERE / "contract.json").read_bytes()),
                "implementation_sha256": {name: sha256((HERE / name).read_bytes())
                                          for name in ("source_identity.py", "rust_source.py")},
                "source_sha256": {name: value["sha256"] for name, value in sorted(self.sources.items())},
                "manifest": self.manifest,
                "python_ast_version": f"{sys.version_info.major}.{sys.version_info.minor}"}


def score(project, required, submitted):
    if not isinstance(required, list) or not required or not isinstance(submitted, list):
        raise ContractError("required must be a nonempty array and submitted an array")
    truth = {}
    for selector in required:
        checked = project.check(selector)
        if not checked["identity_ok"] or not checked["citation_ok"]:
            raise ContractError("invalid required identity/evidence: " + json.dumps(checked, sort_keys=True))
        key = identity_key(checked["identity"])
        if key in truth:
            raise ContractError("duplicate required canonical identity")
        truth[key] = checked["identity"]
    found, cited, items = set(), set(), []
    for selector in submitted:
        checked = project.check(selector)
        key = identity_key(checked["identity"])
        classification = "uncredited"
        if checked["identity_ok"]:
            classification = "required" if key in truth else "verified_extra"
            if key in truth:
                found.add(key)
                if checked["citation_ok"]:
                    cited.add(key)
        items.append({"selector": selector, "classification": classification, **checked})
    return {"schema_version": 1, "pins": project.pins(),
            "identity_recall": {"matched": len(found), "required": len(truth)},
            "citation_recall": {"matched": len(cited), "required": len(truth)},
            "missing": [value for key, value in truth.items() if key not in found],
            "items": items, "diagnostics": project.diagnostics,
            "unsupported_sources": project.failures,
            "semantic_review": {"status": "pending", "result": None,
                                "reason": "independent human/source review required"}}


def score_request(root, request):
    if not isinstance(request, dict) or set(request) != {"schema_version", "manifest", "required", "submitted"}:
        raise ContractError("request requires exactly schema_version/manifest/required/submitted")
    if type(request["schema_version"]) is not int or request["schema_version"] != 1:
        raise ContractError("unsupported request schema_version")
    manifest = request["manifest"]
    if not isinstance(manifest, dict) or set(manifest) != {"rust_root", "python_files"}:
        raise ContractError("manifest requires exactly rust_root/python_files")
    return score(Project(root, **manifest), request["required"], request["submitted"])


def parse_request(text):
    def pairs(entries):
        result = {}
        for key, value in entries:
            if key in result:
                raise ContractError("duplicate JSON field: " + key)
            result[key] = value
        return result

    def constant(value):
        raise ContractError("nonfinite JSON number: " + value)

    return json.loads(text, object_pairs_hook=pairs, parse_constant=constant)


def main():
    parser = argparse.ArgumentParser(prog="source_identity.py", description="Score prospective source identities and exact citations; JSON output only.")
    parser.add_argument("--root", required=True, type=Path, help="frozen source root")
    parser.add_argument("--input", required=True, type=Path, help="JSON request; see README.md")
    args = parser.parse_args()
    try:
        request = parse_request(args.input.read_text(encoding="utf-8"))
        result = score_request(args.root, request)
    except (OSError, ValueError, TypeError, RecursionError) as error:
        print(json.dumps({"schema_version": 1, "code": "invalid_input", "error": str(error)}), file=sys.stderr)
        return 2
    try:
        print(json.dumps(result, indent=2, sort_keys=True, ensure_ascii=True))
        sys.stdout.flush()
    except BrokenPipeError:
        try:
            sys.stdout.close()
        except BrokenPipeError:
            pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
