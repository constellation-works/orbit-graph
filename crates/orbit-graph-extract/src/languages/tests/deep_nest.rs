#![allow(missing_docs)]

//! Generated deep nests. Nothing large is committed: each test builds its
//! source in memory. A 100k parenthesis nest is far past the walk budget, so
//! extraction must finish and report the walk-depth flag instead of
//! overflowing the stack.

use std::path::Path;
use std::time::Duration;

use crate::Extractor;
use crate::languages::{
    CExtractor, CSharpExtractor, ExtractionLimits, GoExtractor, JavaExtractor, JavaScriptExtractor,
    KotlinExtractor, PythonExtractor, RubyExtractor, RustExtractor, TypeScriptExtractor,
    with_extraction_limits,
};

const DEEP: usize = 100_000;

fn nested_parens(inner: &str, depth: usize) -> String {
    let mut source = String::with_capacity(inner.len() + depth * 2);
    source.extend(std::iter::repeat_n('(', depth));
    source.push_str(inner);
    source.extend(std::iter::repeat_n(')', depth));
    source
}

fn extract(
    extractor: &dyn Extractor,
    path: &str,
    source: &str,
) -> (crate::ExtractedFile, ExtractionLimits) {
    with_extraction_limits(Duration::from_secs(120), || {
        extractor.extract(Path::new(path), source.as_bytes())
    })
}

fn call_names(file: &crate::ExtractedFile) -> Vec<&str> {
    file.refs
        .iter()
        .filter(|reference| reference.kind == "call")
        .map(|reference| reference.target_name.as_str())
        .collect()
}

fn mentions(names: &[&str], want: &str) -> bool {
    names.iter().any(|name| {
        *name == want
            || name
                .strip_suffix(want)
                .is_some_and(|prefix| prefix.ends_with('.') || prefix.ends_with("::"))
    })
}

fn assert_capped(label: &str, extractor: &dyn Extractor, path: &str, source: &str, calls: bool) {
    let (file, limits) = extract(extractor, path, source);
    assert!(
        !limits.parse_deadline_exceeded,
        "{label} parse should finish inside the test deadline"
    );
    assert!(
        limits.walk_depth_exceeded,
        "{label} should stop at the walk budget"
    );
    if calls {
        let names = call_names(&file);
        assert!(
            mentions(&names, "top"),
            "{label} should keep the shallow top() call, got {names:?}"
        );
        assert!(
            !mentions(&names, "bottom"),
            "{label} should not reach bottom() inside the nest, got {names:?}"
        );
    }
}

#[test]
fn shallow_python_nest_keeps_both_calls() {
    let source = format!("top()\nx = {}\n", nested_parens("bottom()", 8));
    let (file, limits) = extract(&PythonExtractor, "src/shallow.py", &source);
    assert!(!limits.walk_depth_exceeded);
    assert!(!limits.parse_deadline_exceeded);
    let names = call_names(&file);
    assert!(mentions(&names, "top"), "{names:?}");
    assert!(mentions(&names, "bottom"), "{names:?}");
}

#[test]
fn deep_nest_stops_without_crashing() {
    let nest = nested_parens("bottom()", DEEP);
    let cases: [(&str, &dyn Extractor, &str, String, bool); 10] = [
        (
            "python",
            &PythonExtractor,
            "src/deep.py",
            format!("top()\nx = {nest}\n"),
            true,
        ),
        (
            "javascript",
            &JavaScriptExtractor,
            "src/deep.js",
            format!("top();\nconst x = {nest};\n"),
            true,
        ),
        (
            "typescript",
            &TypeScriptExtractor,
            "src/deep.ts",
            format!("top();\nconst x = {nest};\n"),
            true,
        ),
        (
            "go",
            &GoExtractor,
            "src/deep.go",
            format!("package p\n\nfunc f() {{\n\ttop()\n\t_ = {nest}\n}}\n"),
            true,
        ),
        (
            "ruby",
            &RubyExtractor,
            "src/deep.rb",
            format!("def f\n  top()\n  x = {nest}\nend\n"),
            true,
        ),
        (
            "c",
            &CExtractor,
            "src/deep.c",
            format!("void f(void) {{\n  top();\n  int x = {nest};\n}}\n"),
            true,
        ),
        (
            "rust",
            &RustExtractor,
            "src/deep.rs",
            format!("fn f() {{\n    top();\n    let _ = {nest};\n}}\n"),
            true,
        ),
        (
            "java",
            &JavaExtractor,
            "src/Deep.java",
            format!("class C {{\n  void f() {{\n    top();\n    int x = {nest};\n  }}\n}}\n"),
            false,
        ),
        (
            "kotlin",
            &KotlinExtractor,
            "src/Deep.kt",
            format!("fun f() {{\n    top()\n    val x = {nest}\n}}\n"),
            false,
        ),
        (
            "csharp",
            &CSharpExtractor,
            "src/Deep.cs",
            format!("class C {{\n  void F() {{\n    Top();\n    int x = {nest};\n  }}\n}}\n"),
            false,
        ),
    ];

    for (label, extractor, path, source, calls) in cases {
        assert_capped(label, extractor, path, &source, calls);
    }
}
