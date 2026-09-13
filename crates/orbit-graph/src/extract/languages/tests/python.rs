#![allow(missing_docs)]

use std::path::Path;

use crate::extract::Extractor;

use super::PythonExtractor;

fn extract(source: &str) -> crate::extract::ExtractedFile {
    PythonExtractor.extract(Path::new("src/sample.py"), source.as_bytes())
}

#[test]
fn extracts_symbols_imports_extends_and_fuzzy_calls() {
    let source = r#"
import os.path
from pkg.service import Service as Svc

class Base:
    pass

class Widget(Base, mixins.Renderable):
    def run(self, worker: Svc) -> str:
        worker.perform()
        helper()

def helper():
    return "ok"
"#;

    let file = extract(source);

    assert!(PythonExtractor.supports(Path::new("src/sample.py")));
    assert!(!PythonExtractor.supports(Path::new("src/sample.go")));
    assert!(
        file.symbols.iter().all(|symbol| {
            symbol.span_start < symbol.span_end && symbol.span_end <= source.len()
        })
    );
    assert!(
        file.symbols
            .iter()
            .any(|symbol| { symbol.kind == "class" && symbol.qualified == "Widget" })
    );
    assert!(file.symbols.iter().any(|symbol| {
        symbol.kind == "method"
            && symbol.qualified == "Widget.run"
            && symbol.parent_symbol.as_deref() == Some("Widget")
    }));
    assert!(
        file.symbols
            .iter()
            .any(|symbol| { symbol.kind == "function" && symbol.qualified == "helper" })
    );

    assert!(
        file.imports
            .iter()
            .any(|import| { import.target_path == "os.path" && import.target_symbol.is_none() })
    );
    assert!(file.imports.iter().any(|import| {
        import.target_path == "pkg.service" && import.target_symbol.as_deref() == Some("Svc")
    }));
    assert!(file.relations.iter().any(|relation| {
        relation.from_qualified == "Widget"
            && relation.to_qualified == "Base"
            && relation.kind == "extends"
    }));
    assert!(file.refs.iter().any(|reference| {
        reference.kind == "call"
            && reference.target_name == "perform"
            && reference.target_qualified.as_deref() == Some("worker.perform")
            && reference.confidence == "fuzzy_name"
    }));
}

#[test]
fn fixture_imports_and_calls_preserve_resolution_inputs() {
    let file = extract(
        "from mod import process\nimport mod\nimport package.module as alias\n\ndef test_process():\n    process(5)\n    mod.process(5)\n    alias.process(5)\n",
    );
    let import_rows = file
        .imports
        .iter()
        .map(|import| (import.target_path.as_str(), import.target_symbol.as_deref()))
        .collect::<Vec<_>>();
    let call_rows = file
        .refs
        .iter()
        .filter(|reference| reference.kind == "call")
        .map(|reference| {
            (
                reference.target_name.as_str(),
                reference.target_qualified.as_deref(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        import_rows,
        vec![
            ("mod", None),
            ("mod", Some("process")),
            ("package.module", Some("alias")),
        ]
    );
    assert_eq!(
        call_rows,
        vec![
            ("process", Some("process")),
            ("process", Some("mod.process")),
            ("process", Some("alias.process")),
        ]
    );
}

#[test]
fn extracts_click_commands_and_marks_dynamic_names_fuzzy() {
    let source = r#"
import click

@click.command()
def status():
    pass

@click.group(name="ops")
def cli():
    pass

@cli.command("serve")
def serve():
    status()

@cli.command(name=COMMAND_NAME)
def dynamic():
    pass
"#;

    let file = extract(source);

    assert!(file.commands.iter().any(|command| {
        command.name == "status" && command.handler_symbol.as_deref() == Some("status")
    }));
    assert!(file.commands.iter().any(|command| {
        command.name == "ops" && command.handler_symbol.as_deref() == Some("cli")
    }));
    assert!(file.commands.iter().any(|command| {
        command.name == "serve" && command.handler_symbol.as_deref() == Some("serve")
    }));
    assert!(file.commands.iter().all(|command| {
        command.span_start < source.len() && command.file_path == "src/sample.py"
    }));
    assert!(file.refs.iter().any(|reference| {
        reference.kind == "command"
            && reference.target_name == "COMMAND_NAME"
            && reference.target_qualified.is_none()
            && reference.confidence == "fuzzy_name"
    }));
}

#[test]
fn attribute_call_records_its_receiver_expression() {
    let source = r#"
class Runner:
    def run(self, rows):
        rows.append(1)
        self.start()
        helper()
"#;

    let file = extract(source);

    assert_eq!(
        call_ref(&file, "append").unresolved_receiver.as_deref(),
        Some("rows")
    );
    // `self`/`cls` receivers name the enclosing definition's own class, so a
    // same-file attribute of that name stays resolvable.
    assert_eq!(call_ref(&file, "start").unresolved_receiver, None);
    assert_eq!(call_ref(&file, "helper").unresolved_receiver, None);
}

#[test]
fn method_chain_receiver_calls_are_extracted() {
    let source = r#"
def resolve_import(tx, imported_name):
    return symbols_by_name(tx, imported_name).into_iter().filter(matches_import).collect()
"#;

    let file = extract(source);
    let call_names: Vec<&str> = file
        .refs
        .iter()
        .filter(|reference| reference.kind == "call")
        .map(|reference| reference.target_name.as_str())
        .collect();

    for expected in ["symbols_by_name", "into_iter", "filter", "collect"] {
        assert!(
            call_names.contains(&expected),
            "missing callee {expected} from method chain receiver, got {call_names:?}"
        );
    }
}

fn call_ref<'a>(
    file: &'a crate::extract::ExtractedFile,
    target_name: &str,
) -> &'a crate::extract::RawRef {
    let mut matches = file
        .refs
        .iter()
        .filter(|reference| reference.kind == "call" && reference.target_name == target_name);
    let found = matches.next().unwrap_or_else(|| {
        panic!("missing call ref for {target_name}");
    });
    assert!(
        matches.next().is_none(),
        "expected one call ref for {target_name}"
    );
    found
}
