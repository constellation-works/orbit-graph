"""Bounded, dependency-free Rust item reader; never expands macros or cfg.

This is a declaration grammar, not a compiler. Unsupported item/context syntax
invalidates the Rust index, so unexamined bindings cannot establish uniqueness.
"""
from dataclasses import dataclass
import re


class UnsupportedRust(ValueError):
    pass


@dataclass(frozen=True)
class Token:
    text: str
    line: int
    column: int


IDENT = re.compile(r"[A-Za-z_][A-Za-z_0-9]*\Z")


def lex(source):
    tokens = []
    i, line, column = 0, 1, 1

    def advance(end):
        nonlocal i, line, column
        part = source[i:end]
        count = part.count("\n")
        column = len(part.rsplit("\n", 1)[-1]) + 1 if count else column + len(part)
        line += count
        i = end

    while i < len(source):
        if source[i].isspace():
            advance(i + 1)
            continue
        if source.startswith("//", i):
            end = source.find("\n", i)
            advance(len(source) if end < 0 else end)
            continue
        if source.startswith("/*", i):
            end, depth = i + 2, 1
            while depth and end < len(source):
                if source.startswith("/*", end):
                    depth += 1
                    end += 2
                elif source.startswith("*/", end):
                    depth -= 1
                    end += 2
                else:
                    end += 1
            if depth:
                raise UnsupportedRust("unterminated block comment")
            advance(end)
            continue
        start, token_line, token_column = i, line, column
        raw = re.match(r'(?:b|c)?r(#+|)"', source[i:])
        quoted = re.match(r'(?:b|c)?"', source[i:])
        char = re.match(r"(?:b)?'(?:[^'\\\n]|\\(?:.|u\{[0-9a-fA-F]+\}))'", source[i:])
        if raw:
            closing = '"' + raw.group(1)
            end = source.find(closing, i + len(raw.group()))
            if end < 0:
                raise UnsupportedRust("unterminated raw string")
            advance(end + len(closing))
        elif quoted:
            end = i + len(quoted.group())
            while end < len(source) and source[end] != '"':
                end += 2 if source[end] == "\\" else 1
            if end >= len(source):
                raise UnsupportedRust("unterminated string")
            advance(end + 1)
        elif char:
            advance(i + len(char.group()))
        else:
            match = re.match(r"'[A-Za-z_]\w*|[A-Za-z_]\w*|[0-9][A-Za-z_0-9.]*|::|->|=>|[^\w\s]", source[i:], re.ASCII)
            if not match:
                raise UnsupportedRust(f"unsupported token at {line}:{column}")
            advance(i + len(match.group()))
        tokens.append(Token(source[start:i], token_line, token_column))
    stack = []
    pairs = {}
    for index, token in enumerate(tokens):
        if token.text in ("(", "[", "{"):
            stack.append(index)
        elif token.text in (")", "]", "}"):
            if not stack or tokens[stack[-1]].text != {")": "(", "]": "[", "}": "{"}[token.text]:
                raise UnsupportedRust(f"unbalanced delimiter at line {token.line}")
            opening = stack.pop()
            pairs[opening] = index
    if stack:
        raise UnsupportedRust("unclosed delimiter")
    return tokens, pairs


class RustIndex:
    def __init__(self, read, root):
        self.root = root
        self.read = read
        self.declarations = []
        self.imports = {}
        self.modules = set()
        self.files = set()
        self.pending = []
        self.parse_file(root, (), root.rsplit("/", 1)[0] if "/" in root else "")
        self.finish()

    def parse_file(self, file, module, module_dir):
        if file in self.files or len(self.files) >= 128:
            raise UnsupportedRust("reused/cyclic module file or module limit exceeded")
        self.files.add(file)
        source = self.read(file)
        tokens, pairs = lex(source)
        self.scope(tokens, pairs, 0, len(tokens), file, module, module_dir)

    def add(self, token, end, file, module, kind, owner=None, trait=None):
        item = {"language": "rust", "file": file, "line": token.line,
                "column": token.column, "end_line": end.line, "kind": kind,
                "module": "::".join(("crate",) + module), "owner": owner,
                "trait": trait, "leaf": token.text}
        self.declarations.append(item)
        return item

    def scope(self, ts, pairs, start, stop, file, module, module_dir, context=None):
        i = start
        while i < stop:
            path_attr = None
            if ts[i].text == "#":
                if i + 1 >= stop or ts[i + 1].text != "[":
                    raise UnsupportedRust("inner/unknown attribute unsupported")
                end = pairs[i + 1]
                attribute = [t.text for t in ts[i + 2:end]]
                if (len(attribute) != 3 or attribute[:2] != ["path", "="]
                        or not re.fullmatch(r'"[A-Za-z_0-9./-]+"', attribute[2])):
                    raise UnsupportedRust("only literal #[path] on external modules is supported; cfg/derive are uncertain")
                path_attr = attribute[2][1:-1]
                i = end + 1
            if i < stop and ts[i].text == "pub":
                i += 1
                if i < stop and ts[i].text == "(":
                    i = pairs[i] + 1
            if i >= stop:
                raise UnsupportedRust("missing item")
            if path_attr is not None and ts[i].text != "mod":
                raise UnsupportedRust("#[path] requires an external module")
            keyword = ts[i].text
            if keyword in ("async", "unsafe", "const"):
                # Qualifiers are deliberately narrow; const items are not functions.
                while i < stop and ts[i].text in ("async", "unsafe", "const"):
                    i += 1
                if i >= stop or ts[i].text != "fn":
                    raise UnsupportedRust("only function qualifiers supported here")
                keyword = "fn"
            if keyword == "fn":
                if i + 2 >= stop or not IDENT.fullmatch(ts[i + 1].text):
                    raise UnsupportedRust("malformed function name")
                name = ts[i + 1]
                cursor = i + 2
                if ts[cursor].text == "<":
                    cursor = self.angle(ts, cursor, stop)
                if cursor >= stop or ts[cursor].text != "(":
                    raise UnsupportedRust("function parameters missing")
                cursor = pairs[cursor] + 1
                while cursor < stop and ts[cursor].text not in ("{", ";"):
                    if ts[cursor].text in ("(", "["):
                        cursor = pairs[cursor] + 1
                    elif ts[cursor].text == "<":
                        cursor = self.angle(ts, cursor, stop)
                    else:
                        cursor += 1
                if cursor == stop:
                    raise UnsupportedRust("function terminator missing")
                if ts[cursor].text == ";" and (context is None or context[0] != "trait"):
                    raise UnsupportedRust("bodyless function outside a trait")
                end = pairs[cursor] if ts[cursor].text == "{" else cursor
                item = self.add(name, ts[end], file, module, "fn")
                if context:
                    self.pending.append((item, module, context))
                i = end + 1
            elif keyword in ("struct", "enum", "trait") and context is None:
                if i + 2 >= stop or not IDENT.fullmatch(ts[i + 1].text):
                    raise UnsupportedRust("malformed type declaration")
                name, cursor = ts[i + 1], i + 2
                if ts[cursor].text not in ("{", "(", ";"):
                    raise UnsupportedRust("generic types, supertraits and where clauses on types unsupported")
                if keyword in ("enum", "trait") and ts[cursor].text != "{":
                    raise UnsupportedRust("enum/trait body missing")
                end = pairs[cursor] if ts[cursor].text in ("{", "(") else cursor
                self.add(name, ts[end], file, module, keyword)
                if keyword == "trait":
                    self.scope(ts, pairs, cursor + 1, end, file, module, module_dir,
                               ("trait", name.text, None))
                i = end + 1
                if ts[cursor].text == "(":
                    if i >= stop or ts[i].text != ";":
                        raise UnsupportedRust("tuple struct terminator missing")
                    i += 1
            elif keyword == "impl" and context is None:
                cursor = i + 1
                while cursor < stop and ts[cursor].text != "{":
                    cursor += 1
                header = [t.text for t in ts[i + 1:cursor]]
                if cursor == stop or header.count("for") > 1:
                    raise UnsupportedRust("malformed impl")
                trait = None
                if "for" in header:
                    split = header.index("for")
                    trait = self.path_tokens(header[:split])
                    owner = self.path_tokens(header[split + 1:])
                else:
                    owner = self.path_tokens(header)
                if not self.valid_path(owner) or (trait is not None and not self.valid_path(trait)):
                    raise UnsupportedRust("generic/negative/qualified impl type unsupported")
                end = pairs[cursor]
                self.scope(ts, pairs, cursor + 1, end, file, module, module_dir,
                           ("impl", owner, trait))
                i = end + 1
            elif keyword == "type" and context and context[0] == "impl" and context[2]:
                # Associated types do not introduce module-level aliases.
                cursor = i + 1
                while cursor < stop and ts[cursor].text != ";":
                    if ts[cursor].text in ("(", "["):
                        cursor = pairs[cursor]
                    cursor += 1
                if cursor == stop or cursor < i + 4 or ts[i + 2].text != "=":
                    raise UnsupportedRust("unsupported associated type")
                i = cursor + 1
            elif keyword == "use" and context is None:
                cursor = i + 1
                while cursor < stop and ts[cursor].text != ";":
                    cursor += 1
                parts = [t.text for t in ts[i + 1:cursor]]
                if cursor == stop or parts.count("as") > 1:
                    raise UnsupportedRust("malformed use")
                if "as" in parts:
                    split = parts.index("as")
                    if len(parts) != split + 2:
                        raise UnsupportedRust("malformed import alias")
                    path, alias = self.path_tokens(parts[:split]), parts[-1]
                else:
                    path, alias = self.path_tokens(parts), parts[-1] if parts else ""
                if not self.valid_path(path) or not IDENT.fullmatch(alias):
                    raise UnsupportedRust("group/glob imports unsupported")
                key = (module, alias)
                if key in self.imports:
                    raise UnsupportedRust("ambiguous import binding")
                self.imports[key] = path
                i = cursor + 1
            elif keyword == "mod" and context is None:
                if i + 2 >= stop or not IDENT.fullmatch(ts[i + 1].text):
                    raise UnsupportedRust("malformed module")
                name, cursor = ts[i + 1].text, i + 2
                child = module + (name,)
                if child in self.modules:
                    raise UnsupportedRust("duplicate/conditional module binding")
                self.modules.add(child)
                if ts[cursor].text == "{":
                    if path_attr:
                        raise UnsupportedRust("#[path] on inline module unsupported")
                    end = pairs[cursor]
                    self.scope(ts, pairs, cursor + 1, end, file, child,
                               self.join(module_dir, name))
                elif ts[cursor].text == ";":
                    # Explicit file paths only: no file-existence-dependent guessing.
                    if not path_attr:
                        raise UnsupportedRust("external modules require literal #[path]")
                    if file != self.root:
                        raise UnsupportedRust("external modules declared inside external files are not supported")
                    child_file = self.join(module_dir, path_attr)
                    child_dir = (child_file.rsplit("/", 1)[0] if child_file.endswith("/mod.rs")
                                 else child_file.removesuffix(".rs"))
                    if not child_file.endswith(".rs"):
                        raise UnsupportedRust("module path must end in .rs")
                    self.parse_file(child_file, child, child_dir)
                    end = cursor
                else:
                    raise UnsupportedRust("malformed module body")
                i = end + 1
            else:
                raise UnsupportedRust(f"unsupported Rust item {keyword!r} in {file}:{ts[i].line}")

    @staticmethod
    def join(parent, child):
        return parent + "/" + child if parent else child

    @staticmethod
    def path_tokens(parts):
        if not parts or len(parts) % 2 == 0 or any(
                (not IDENT.fullmatch(part)) if n % 2 == 0 else part != "::"
                for n, part in enumerate(parts)):
            raise UnsupportedRust("unsupported/malformed path syntax")
        return "".join(parts)

    @staticmethod
    def valid_path(value):
        return bool(re.fullmatch(r"[A-Za-z_]\w*(?:::[A-Za-z_]\w*)*", value, re.ASCII))

    @staticmethod
    def angle(ts, start, stop):
        depth = 0
        for i in range(start, stop):
            if ts[i].text == "<":
                depth += 1
            elif ts[i].text == ">":
                depth -= 1
                if depth == 0:
                    return i + 1
            elif ts[i].text in ("{", ";"):
                break
        raise UnsupportedRust("unsupported or unbalanced generic signature")

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
        for item in self.declarations:
            if item["kind"] in ("struct", "enum", "trait"):
                path = item["module"] + "::" + item["leaf"]
                if path in types or (tuple(item["module"].split("::")[1:]), item["leaf"]) in self.imports:
                    raise UnsupportedRust("duplicate type/import binding")
                types[path] = item["kind"]
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
            if trait:
                trait = self.qualify(trait, module)
                if types.get(trait) != "trait" and not trait.startswith(("std::", "core::")):
                    raise UnsupportedRust(f"unverified trait binding {trait}")
                item["trait"] = trait
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
