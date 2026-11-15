#![allow(missing_docs)]

//! Deeply nested source must stop the recursive AST walkers at their limit
//! instead of overflowing the thread stack. Each test runs on a default
//! 2 MiB test-harness thread, the stack size of a pass 1 rayon worker, and
//! builds its 100k-deep nest in memory.

use std::path::Path;
use std::time::Duration;

use crate::languages::{
    CExtractor, CSharpExtractor, ConfigExtractor, GoExtractor, JavaExtractor, JavaScriptExtractor,
    KotlinExtractor, MarkdownExtractor, PythonExtractor, RubyExtractor, RustExtractor,
    TypeScriptExtractor, with_extraction_limits,
};
use crate::{ExtractedFile, Extractor};

const DEPTH: usize = 100_000;

fn nested(open: &str, inner: &str, close: &str, depth: usize) -> String {
    format!("{}{inner}{}", open.repeat(depth), close.repeat(depth))
}

fn extract(extractor: &dyn Extractor, path: &str, source: &str) -> (ExtractedFile, bool) {
    let (file, limits) = with_extraction_limits(Duration::from_secs(120), || {
        extractor.extract(Path::new(path), source.as_bytes())
    });
    assert!(
        !limits.deadline_exceeded,
        "{path}: the parse deadline is not what this test exercises"
    );
    (file, limits.walk_depth_exceeded)
}

fn has_call(file: &ExtractedFile, name: &str) -> bool {
    file.refs
        .iter()
        .any(|reference| reference.kind == "call" && reference.target_name.ends_with(name))
}

struct Case {
    name: &'static str,
    extractor: &'static dyn Extractor,
    path: &'static str,
    /// Source around the nest: `[before, after]`; `{}` receives the nest.
    template: &'static str,
    open: &'static str,
    inner: &'static str,
    close: &'static str,
    /// Whether the language records call refs this test can check.
    records_calls: bool,
}

const CASES: &[Case] = &[
    Case {
        name: "python",
        extractor: &PythonExtractor,
        path: "deep.py",
        template: "def f():\n    top()\n    value = {}\n",
        open: "(",
        inner: "bottom()",
        close: ")",
        records_calls: true,
    },
    Case {
        name: "javascript",
        extractor: &JavaScriptExtractor,
        path: "deep.js",
        template: "function f() { top(); const value = {}; }\n",
        open: "(",
        inner: "bottom()",
        close: ")",
        records_calls: true,
    },
    Case {
        name: "typescript",
        extractor: &TypeScriptExtractor,
        path: "deep.ts",
        template: "function f(): void { top(); const value = {}; }\n",
        open: "(",
        inner: "bottom()",
        close: ")",
        records_calls: true,
    },
    Case {
        name: "go",
        extractor: &GoExtractor,
        path: "deep.go",
        template: "package p\n\nfunc f() {\n\ttop()\n\tvalue := {}\n}\n",
        open: "(",
        inner: "bottom()",
        close: ")",
        records_calls: true,
    },
    Case {
        name: "ruby",
        extractor: &RubyExtractor,
        path: "deep.rb",
        template: "def f\n  top()\n  value = {}\nend\n",
        open: "(",
        inner: "bottom()",
        close: ")",
        records_calls: true,
    },
    Case {
        name: "c",
        extractor: &CExtractor,
        path: "deep.c",
        template: "void f(void) { top(); int value = {}; }\n",
        open: "(",
        inner: "bottom()",
        close: ")",
        records_calls: true,
    },
    Case {
        name: "rust",
        extractor: &RustExtractor,
        path: "deep.rs",
        template: "fn f() { top(); let value = {}; }\n",
        open: "(",
        inner: "bottom()",
        close: ")",
        records_calls: true,
    },
    Case {
        name: "java",
        extractor: &JavaExtractor,
        path: "Deep.java",
        template: "class Deep { void f() { top(); int value = {}; } }\n",
        open: "(",
        inner: "bottom()",
        close: ")",
        records_calls: false,
    },
    Case {
        name: "kotlin",
        extractor: &KotlinExtractor,
        path: "Deep.kt",
        template: "fun f() { top(); val value = {} }\n",
        open: "(",
        inner: "bottom()",
        close: ")",
        records_calls: false,
    },
    Case {
        name: "csharp",
        extractor: &CSharpExtractor,
        path: "Deep.cs",
        template: "class Deep { void F() { Top(); var value = {}; } }\n",
        open: "(",
        inner: "Bottom()",
        close: ")",
        records_calls: false,
    },
];

fn build(case: &Case, depth: usize) -> String {
    case.template
        .replace("{}", &nested(case.open, case.inner, case.close, depth))
}

#[test]
fn deep_nest_stops_without_crashing() {
    for case in CASES {
        let source = build(case, DEPTH);
        let (file, walk_depth_exceeded) = extract(case.extractor, case.path, &source);
        assert!(
            walk_depth_exceeded,
            "{}: a {DEPTH}-deep nest must stop a walk at its limit",
            case.name
        );
        if case.records_calls {
            let (top, bottom) = ("top", "bottom");
            assert!(
                has_call(&file, top),
                "{}: the shallow call before the nest is kept",
                case.name
            );
            assert!(
                !has_call(&file, bottom),
                "{}: the innermost call is past the limit",
                case.name
            );
        }
    }
}

#[test]
fn shallow_nest_keeps_every_call() {
    for case in CASES.iter().filter(|case| case.records_calls) {
        let source = build(case, 8);
        let (file, walk_depth_exceeded) = extract(case.extractor, case.path, &source);
        assert!(
            !walk_depth_exceeded,
            "{}: a shallow nest is within the limit",
            case.name
        );
        assert!(has_call(&file, "top"), "{}: top() kept", case.name);
        assert!(has_call(&file, "bottom"), "{}: bottom() kept", case.name);
    }
}

#[test]
fn deep_non_code_nest_stops_without_crashing() {
    let markdown_nest = format!("{}x\n", "> ".repeat(DEPTH));
    let cases: [(&dyn Extractor, &str, String); 2] = [
        (&MarkdownExtractor, "deep.md", markdown_nest),
        (
            &ConfigExtractor,
            "deep.json",
            nested("{\"a\":", "1", "}", DEPTH),
        ),
    ];
    for (extractor, path, source) in &cases {
        // Only the absence of a crash is asserted: these formats are parsed by
        // line scanners or by serde, which carry their own depth limits.
        let _ = extract(*extractor, path, source);
    }
}

#[test]
fn walk_state_is_scoped_to_one_extraction() {
    let case = &CASES[0];
    let (_, exceeded) = extract(case.extractor, case.path, &build(case, DEPTH));
    assert!(exceeded);
    let (_, exceeded) = extract(case.extractor, case.path, &build(case, 8));
    assert!(!exceeded, "a stopped walk does not leak into the next run");
}

/// Nests that are not parenthesized expressions: blocks, scopes, types and
/// declarators walk through other recursive paths than the expression walkers.
#[test]
fn deep_structural_nests_stop_without_crashing() {
    let n = |unit: &str| unit.repeat(DEPTH);
    let shapes: Vec<(&str, &dyn Extractor, &str, String)> = vec![
        (
            "python lambda",
            &PythonExtractor,
            "a.py",
            format!("def f():\n    x = {}y\n", n("lambda: ")),
        ),
        (
            "python type annotation",
            &PythonExtractor,
            "a.py",
            format!("def f(x: {}int{}):\n    pass\n", n("list["), n("]")),
        ),
        (
            "javascript block",
            &JavaScriptExtractor,
            "a.js",
            format!("function f() {{ {}{} }}", n("{"), n("}")),
        ),
        (
            "typescript type",
            &TypeScriptExtractor,
            "a.ts",
            format!("function f(x: {}number{}) {{}}", n("Array<"), n(">")),
        ),
        (
            "go function literal",
            &GoExtractor,
            "a.go",
            format!(
                "package p\nfunc f() {{ x := {}1{} }}",
                n("func() { "),
                n("}()")
            ),
        ),
        (
            "go slice type",
            &GoExtractor,
            "a.go",
            format!("package p\nfunc f(x {}int) {{}}", n("[]")),
        ),
        (
            "ruby module",
            &RubyExtractor,
            "a.rb",
            format!("{}x = 1\n{}", n("module A\n"), n("end\n")),
        ),
        (
            "c block",
            &CExtractor,
            "a.c",
            format!("void f(void) {{ {}{} }}", n("{"), n("}")),
        ),
        (
            "c declarator",
            &CExtractor,
            "a.c",
            format!("int {}x{};", n("("), n(")")),
        ),
        (
            "c preprocessor conditional",
            &CExtractor,
            "a.c",
            format!("{}int x;\n{}", n("#if A\n"), n("#endif\n")),
        ),
        (
            "rust module",
            &RustExtractor,
            "a.rs",
            format!("{}fn g() {{}}{}", n("mod a {"), n("}")),
        ),
        (
            "rust generic type",
            &RustExtractor,
            "a.rs",
            format!("fn f(x: {}u8{}) {{}}", n("Vec<"), n(">")),
        ),
        (
            "rust use tree",
            &RustExtractor,
            "a.rs",
            format!("use a::{}b{};", n("c::{"), n("}")),
        ),
        (
            "java class",
            &JavaExtractor,
            "A.java",
            format!("{}int x;{}", n("class A {"), n("}")),
        ),
        (
            "csharp namespace",
            &CSharpExtractor,
            "A.cs",
            format!("{}class A {{}}{}", n("namespace A {"), n("}")),
        ),
        (
            "kotlin block",
            &KotlinExtractor,
            "A.kt",
            format!("fun f() {{ {}{} }}", n("{"), n("}")),
        ),
    ];
    for (name, extractor, path, source) in &shapes {
        let (_, walk_depth_exceeded) = extract(*extractor, path, source);
        assert!(walk_depth_exceeded, "{name}: the walk stops at its limit");
    }
}
