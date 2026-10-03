"""Independent fixture-only oracle: Python AST and restricted Rust function bodies.

Never imports orbit-graph or consumes graph output. This is deliberately not a
language-general call resolver: fixtures exclude aliases, nested Rust blocks,
macros containing calls, imports and methods. Changes to that grammar need a
new corpus version and an independently reviewed oracle.
"""
import ast
import re


def functions(language, files):
    found = {}
    for path, source in files.items():
        if language == "python" and path.endswith(".py"):
            tree = ast.parse(source)
            for node in tree.body:
                if isinstance(node, ast.FunctionDef):
                    calls = {n.func.id for n in ast.walk(node)
                             if isinstance(n, ast.Call) and isinstance(n.func, ast.Name)}
                    found[node.name] = {"path": path, "line": node.lineno,
                                        "kind": "function",
                                        "body": ast.dump(node, include_attributes=False),
                                        "calls": calls}
        elif language == "rust" and path.endswith(".rs"):
            # One brace-free body per function is part of the frozen fixture grammar.
            pattern = r"(?:pub )?fn (\w+)\([^\n]*\)[^{]*\{([^{}]*)\}"
            for match in re.finditer(pattern, source):
                name, body = match.groups()
                found[name] = {"path": path, "line": source[:match.start()].count("\n") + 1,
                               "kind": "test" if source[:match.start()].rstrip().endswith("#[test]") else "function",
                               "body": body, "calls": set(re.findall(r"\b(\w+)\(", body))}
            definitions = re.findall(r"\bfn (\w+)\(", source)
            if sorted(definitions) != sorted(found):
                raise ValueError("Rust fixture escaped the oracle's restricted grammar")
    for definition in found.values():
        definition["calls"] &= found.keys()
    if not found:
        raise ValueError("fixture has no independently parsed definitions")
    return found


def selector(definition, name):
    return f"symbol:{definition['path']}#{name}:{definition['kind']}"


def expected(case, language, head, base):
    defs = functions(language, head)
    target = case["target"]
    kind = case["kind"]
    if kind == "unsupported":
        # Independent reason: callback identity or external consumers absent.
        marker = "callback identity is supplied at runtime" if language == "python" else "External runtime consumers are not supplied"
        if not any(marker in source for source in head.values()):
            raise ValueError("unsupported-case witness is absent")
        return [], True
    if target not in defs:
        raise ValueError(f"unknown oracle target {target}")
    if kind == "discovery":
        names = {target}
    elif kind == "callers":
        names = {name for name, d in defs.items() if target in d["calls"]}
    elif kind == "callees":
        names = defs[target]["calls"]
    elif kind == "impact_tests":
        reached = {target}
        while True:
            expanded = reached | {name for name, d in defs.items() if d["calls"] & reached}
            if expanded == reached:
                break
            reached = expanded
        names = {name for name in reached if name.startswith("test_")}
    elif kind == "change":
        before = functions(language, base)
        names = {name for name, d in defs.items()
                 if name not in before or d["body"] != before[name]["body"]}
    else:
        raise ValueError(f"unknown oracle kind {kind}")
    return sorted(selector(defs[name], name) for name in names), False
