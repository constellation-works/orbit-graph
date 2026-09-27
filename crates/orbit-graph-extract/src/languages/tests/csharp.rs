#![allow(missing_docs)]

use std::path::Path;

use crate::Extractor;
use crate::languages::CSharpExtractor;

fn extract(source: &str) -> crate::ExtractedFile {
    CSharpExtractor.extract(Path::new("src/Sample.cs"), source.as_bytes())
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
fn supports_csharp_files() {
    assert_eq!(CSharpExtractor.lang(), "csharp");
    assert!(CSharpExtractor.supports(Path::new("src/Worker.cs")));
    assert!(!CSharpExtractor.supports(Path::new("src/Worker.java")));
}

#[test]
fn extracts_classes_interfaces_imports_relations_generics_and_fuzzy_calls() {
    let source = r#"
using System.Collections.Generic;

namespace Demo;

class Worker<T> : BaseWorker, IWorker, IDisposable
{
    public void Run(Helper helper)
    {
        helper.Execute();
    }
}

interface IWorker
{
    void Run(Helper helper);
}
"#;

    let file = extract(source);

    assert!(file.symbols.iter().any(|symbol| {
        symbol.kind == "class" && symbol.name == "Worker" && symbol.qualified == "Demo::Worker"
    }));
    assert!(
        file.symbols
            .iter()
            .any(|symbol| { symbol.kind == "interface" && symbol.name == "IWorker" })
    );
    assert!(
        file.symbols
            .iter()
            .any(|symbol| symbol.kind == "method" && symbol.name == "Run")
    );
    assert!(
        file.imports
            .iter()
            .any(|import| import.target_path == "System.Collections.Generic")
    );
    assert!(file.relations.iter().any(|relation| {
        relation.kind == "extends"
            && relation.from_qualified == "Demo::Worker"
            && relation.to_qualified == "BaseWorker"
    }));
    assert!(file.relations.iter().any(|relation| {
        relation.kind == "implements"
            && relation.from_qualified == "Demo::Worker"
            && relation.to_qualified == "IWorker"
    }));
    assert!(file.relations.iter().any(|relation| {
        relation.kind == "implements"
            && relation.from_qualified == "Demo::Worker"
            && relation.to_qualified == "IDisposable"
    }));
    assert!(file.refs.iter().any(|reference| {
        reference.kind == "call"
            && reference.target_name == "Execute"
            && reference.target_qualified.is_none()
            && reference.confidence == "fuzzy_name"
    }));
    assert_byte_spans(&file, source);
}

#[test]
fn dotted_calls_skip_strings_and_comments_and_keep_real_calls() {
    let source = r#"
class Demo
{
    void Fake() {}
    void Real(Helper helper)
    {
        string s = "obj.Fake()";
        string verbatim = @"obj.Fake()";
        string raw = """obj.Fake()""";
        string bare = $"obj.Fake()";
        // obj.Fake()
        /* obj.Fake() */
        helper.Execute();
        helper . Finish ();
        string live = $"prefix {helper.Keep()} obj.Fake()";
        string formatted = $"{helper.Again():obj.Fake()}";
        string verbatimLive = $@"pre {helper.Once()} obj.Fake()";
    }
}
"#;

    let file = extract(source);
    assert_dotted_call(source, &file, "helper.Execute()", "Execute");
    assert_dotted_call(source, &file, "helper . Finish ()", "Finish");
    assert_dotted_call(source, &file, "helper.Keep()", "Keep");
    assert_dotted_call(source, &file, "helper.Again()", "Again");
    assert_dotted_call(source, &file, "helper.Once()", "Once");
    assert_eq!(
        call_names(&file),
        vec!["Execute", "Finish", "Keep", "Again", "Once"],
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
