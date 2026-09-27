#![allow(missing_docs)]

use std::path::Path;

use crate::Extractor;
use crate::languages::KotlinExtractor;

fn extract(source: &str) -> crate::ExtractedFile {
    KotlinExtractor.extract(Path::new("src/sample.kt"), source.as_bytes())
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
fn supports_kotlin_files() {
    assert_eq!(KotlinExtractor.lang(), "kotlin");
    assert!(KotlinExtractor.supports(Path::new("src/Worker.kt")));
    assert!(KotlinExtractor.supports(Path::new("src/script.kts")));
    assert!(!KotlinExtractor.supports(Path::new("src/Worker.java")));
}

#[test]
fn extracts_classes_interfaces_imports_relations_generics_and_fuzzy_calls() {
    let source = r#"
package demo

import demo.shared.Widget

open class BaseWorker
interface Runnable
interface Closeable

class Worker<T> : BaseWorker(), Runnable, Closeable {
    fun run(helper: Helper) {
        helper.execute()
    }
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
            .any(|import| import.target_path == "demo.shared.Widget")
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
    fun fake() {}
    fun real(helper: Helper) {
        val s = "obj.fake()"
        val block = """
            obj.fake()
            ${helper.block()}
        """
        val simple = "$helper.fake()"
        // obj.fake()
        /* obj.fake() */
        helper.execute()
        helper . finish ()
        val live = "prefix ${helper.keep()} obj.fake()"
    }
}
"#;

    let file = extract(source);
    assert_dotted_call(source, &file, "helper.block()", "block");
    assert_dotted_call(source, &file, "helper.execute()", "execute");
    assert_dotted_call(source, &file, "helper . finish ()", "finish");
    assert_dotted_call(source, &file, "helper.keep()", "keep");
    assert_eq!(
        call_names(&file),
        vec!["block", "execute", "finish", "keep"],
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
