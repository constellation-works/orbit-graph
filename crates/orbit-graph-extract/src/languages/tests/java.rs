#![allow(missing_docs)]

use std::path::Path;

use crate::Extractor;
use crate::languages::JavaExtractor;

fn extract(source: &str) -> crate::ExtractedFile {
    JavaExtractor.extract(Path::new("src/sample.java"), source.as_bytes())
}

fn assert_byte_spans(file: &crate::ExtractedFile, source: &str) {
    assert!(
        file.symbols
            .iter()
            .all(|symbol| symbol.span_start < symbol.span_end && symbol.span_end <= source.len())
    );
    assert!(file.relations.iter().all(|relation| {
        relation.def_span_start < relation.def_span_end && relation.def_span_end <= source.len()
    }));
    assert!(file.refs.iter().all(|reference| {
        reference.from_span_start < reference.from_span_end
            && reference.from_span_end <= source.len()
    }));
}

#[test]
fn supports_java_files() {
    assert_eq!(JavaExtractor.lang(), "java");
    assert!(JavaExtractor.supports(Path::new("src/Worker.java")));
    assert!(!JavaExtractor.supports(Path::new("src/Worker.kt")));
}

#[test]
fn extracts_classes_interfaces_imports_relations_generics_and_fuzzy_calls() {
    let source = r#"
package demo;

import java.util.List;

class Worker<T> extends BaseWorker implements Runnable, Closeable {
    void run(Helper helper) {
        helper.execute();
    }
}

interface Closeable {
    void close();
}
"#;

    let file = extract(source);

    assert!(file.symbols.iter().any(|symbol| {
        symbol.kind == "class" && symbol.name == "Worker" && symbol.qualified == "Worker"
    }));
    assert!(
        file.symbols
            .iter()
            .any(|symbol| { symbol.kind == "interface" && symbol.name == "Closeable" })
    );
    assert!(
        file.symbols
            .iter()
            .any(|symbol| symbol.kind == "method" && symbol.name == "run")
    );
    assert!(
        file.imports
            .iter()
            .any(|import| import.target_path == "java.util.List")
    );
    assert!(file.relations.iter().any(|relation| {
        relation.kind == "extends"
            && relation.from_qualified == "Worker"
            && relation.to_qualified == "BaseWorker"
    }));
    assert!(file.relations.iter().any(|relation| {
        relation.kind == "implements"
            && relation.from_qualified == "Worker"
            && relation.to_qualified == "Runnable"
    }));
    assert!(file.relations.iter().any(|relation| {
        relation.kind == "implements"
            && relation.from_qualified == "Worker"
            && relation.to_qualified == "Closeable"
    }));
    assert!(file.refs.iter().any(|reference| {
        reference.kind == "call"
            && reference.target_name == "execute"
            && reference.target_qualified.is_none()
            && reference.confidence == "fuzzy_name"
    }));
    assert_byte_spans(&file, source);
}

#[test]
fn dotted_calls_skip_strings_and_comments_and_keep_real_calls() {
    let source = r#"
class Demo {
    void fake() {}
    void real(Helper helper) {
        String s = "obj.fake()";
        String block = """
            obj.fake()
            """;
        char quote = '"';
        // obj.fake()
        /* obj.fake() */
        helper/* obj.fake() */.execute();
        helper . finish ();
        String live = "prefix \{helper.keep()} obj.fake()";
    }
}
"#;

    let file = extract(source);
    assert_dotted_call(source, &file, "helper/* obj.fake() */.execute()", "execute");
    assert_dotted_call(source, &file, "helper . finish ()", "finish");
    assert_dotted_call(source, &file, "helper.keep()", "keep");
    assert_eq!(
        call_names(&file),
        vec!["execute", "finish", "keep"],
        "string and comment text must not become calls: {:?}",
        file.refs
    );
    assert_byte_spans(&file, source);
}

fn assert_dotted_call(source: &str, file: &crate::ExtractedFile, snippet: &str, name: &str) {
    assert_eq!(source.matches(snippet).count(), 1, "snippet {snippet}");
    let at = source.find(snippet).expect(snippet);
    let relative = snippet.find(name).expect(name);
    let start = at + relative;
    let end = start + name.len();
    assert!(
        file.refs.iter().any(|reference| {
            reference.kind == "call"
                && reference.target_name == name
                && reference.target_qualified.is_none()
                && reference.confidence == "fuzzy_name"
                && reference.from_span_start == start
                && reference.from_span_end == end
        }),
        "missing {name} at {start}..{end} in {snippet}; refs={:?}",
        file.refs
    );
}

fn call_names(file: &crate::ExtractedFile) -> Vec<&str> {
    file.refs
        .iter()
        .filter(|reference| reference.kind == "call")
        .map(|reference| reference.target_name.as_str())
        .collect()
}
