#![allow(missing_docs)]

use std::path::Path;

use crate::extract::Extractor;
use crate::extract::languages::RustExtractor;

fn extract(source: &str) -> crate::extract::ExtractedFile {
    RustExtractor.extract(Path::new("src/sample.rs"), source.as_bytes())
}

fn extract_at(path: &str, source: &str) -> crate::extract::ExtractedFile {
    RustExtractor.extract(Path::new(path), source.as_bytes())
}

fn symbol_kinds(file: &crate::extract::ExtractedFile) -> Vec<&str> {
    file.symbols
        .iter()
        .map(|symbol| symbol.kind.as_str())
        .collect()
}

#[test]
fn extracts_required_symbol_kinds_with_byte_spans() {
    let source = r#"
const LIMIT: usize = 3;
type Name = String;
struct Widget;
enum Mode { Fast }
trait Render {
    fn render(&self) -> String {
        String::new()
    }
}
impl Render for Widget {
    fn render(&self) -> String {
        helper()
    }
}
fn helper() -> String {
    String::new()
}
#[test]
fn helper_test() {}
mod nested {
    pub fn child() {}
}
"#;
    let file = extract(source);
    let kinds = symbol_kinds(&file);

    for kind in [
        "const",
        "type_alias",
        "struct",
        "enum",
        "trait",
        "impl",
        "method",
        "function",
        "test",
        "module",
    ] {
        assert!(kinds.contains(&kind), "missing symbol kind {kind}");
    }
    assert!(
        file.symbols.iter().all(|symbol| {
            symbol.span_start < symbol.span_end && symbol.span_end <= source.len()
        })
    );
}

#[test]
fn records_trait_impl_as_relation() {
    let file = extract(
        r#"
trait Render {}
struct Widget;
impl Render for Widget {}
"#,
    );

    let Some(relation) = file
        .relations
        .iter()
        .find(|relation| relation.kind == "impl")
    else {
        panic!("impl relation");
    };
    assert_eq!(relation.from_qualified, "Widget");
    assert_eq!(relation.to_qualified, "Render");
    assert_eq!(relation.confidence, "exact");
}

#[test]
fn records_trait_bounds_and_type_uses_as_refs() {
    let file = extract(
        r#"
use std::fmt::Display;

struct Boxed<T: Display> {
    value: Vec<T>,
}

fn render<T>(value: T) -> String
where
    T: Display,
{
    String::new()
}
"#,
    );

    assert!(file.refs.iter().any(|reference| {
        reference.kind == "trait_bound" && reference.target_name == "Display"
    }));
    assert!(
        file.refs
            .iter()
            .any(|reference| reference.kind == "type" && reference.target_name == "Vec")
    );
    assert!(
        !file.relations.iter().any(|relation| {
            relation.to_qualified.ends_with("Display") && relation.kind == "impl"
        })
    );
}

#[test]
fn records_use_imports_and_refs() {
    let file = extract(
        r#"
use std::fmt::Display;
use crate::task::{Task, TaskId as Id};
"#,
    );

    assert!(file.imports.iter().any(|import| {
        import.target_path == "std::fmt" && import.target_symbol.as_deref() == Some("Display")
    }));
    assert!(file.imports.iter().any(|import| {
        import.target_path == "crate::task" && import.target_symbol.as_deref() == Some("Id")
    }));
    assert!(file.refs.iter().any(|reference| {
        reference.kind == "use"
            && reference.target_name == "Display"
            && reference.target_qualified.as_deref() == Some("std::fmt::Display")
    }));
}

#[test]
fn fixture_qualified_calls_and_import_forms_preserve_resolution_inputs() {
    let calls = extract_at(
        "src/lib.rs",
        "mod a;\nmod b;\nfn caller() { a::run(); b::run(); }\n",
    );
    let qualified_calls = calls
        .refs
        .iter()
        .filter(|reference| reference.kind == "call")
        .map(|reference| reference.target_qualified.as_deref())
        .collect::<Vec<_>>();
    assert_eq!(qualified_calls, vec![Some("a::run"), Some("b::run")]);

    let imports =
        extract("use crate::a::{run, other};\nuse super::b::run as b_run;\nuse crate::c::*;\n");
    let import_rows = imports
        .imports
        .iter()
        .map(|import| (import.target_path.as_str(), import.target_symbol.as_deref()))
        .collect::<Vec<_>>();
    assert_eq!(
        import_rows,
        vec![
            ("crate::a", Some("other")),
            ("crate::a", Some("run")),
            ("crate::c", None),
            ("super::b", Some("b_run")),
        ]
    );
}

#[test]
fn qualifies_nested_module_symbols() {
    let file = extract(
        r#"
mod outer {
    mod inner {
        pub struct Thing;
        pub fn build() -> Thing {
            Thing
        }
    }
}
"#,
    );

    assert!(
        file.symbols
            .iter()
            .any(|symbol| { symbol.kind == "module" && symbol.qualified == "outer::inner" })
    );
    assert!(
        file.symbols
            .iter()
            .any(|symbol| { symbol.kind == "struct" && symbol.qualified == "outer::inner::Thing" })
    );
    assert!(
        file.symbols.iter().any(|symbol| {
            symbol.kind == "function" && symbol.qualified == "outer::inner::build"
        })
    );
}

#[test]
fn ambiguous_method_call_lowers_to_fuzzy_name() {
    let file = extract(
        r#"
fn drive(runner: Runner) {
    runner.run();
}
"#,
    );

    assert!(file.refs.iter().any(|reference| {
        reference.kind == "call"
            && reference.target_name == "run"
            && reference.target_qualified.is_none()
            && reference.confidence == "fuzzy_name"
    }));
}

#[test]
fn method_call_records_its_receiver_expression() {
    let file = extract(
        r#"
impl WorkspaceCommand {
    fn execute(self) {
        match self.command {
            Subcommand::List(args) => args.execute(),
        }
        self.log();
        helper();
    }
}
"#,
    );

    let dispatch = call_ref(&file, "execute");
    assert_eq!(dispatch.unresolved_receiver.as_deref(), Some("args"));

    // `self`/`Self` receivers name the enclosing definition's own type, so a
    // same-file method of that name stays resolvable.
    assert_eq!(call_ref(&file, "log").unresolved_receiver, None);
    assert_eq!(call_ref(&file, "helper").unresolved_receiver, None);
}

#[test]
fn chained_method_call_records_the_whole_receiver_expression() {
    let file = extract(
        r#"
fn load() -> Option<i32> {
    fetch()?.parse()
}
"#,
    );

    assert_eq!(
        call_ref(&file, "parse").unresolved_receiver.as_deref(),
        Some("fetch()?")
    );
    assert_eq!(call_ref(&file, "fetch").unresolved_receiver, None);
}

#[test]
fn extracts_clap_subcommand_handlers_from_simple_match_arms() {
    let file = extract(
        r#"
use clap::Subcommand;

#[derive(Subcommand)]
enum TaskSubcommand {
    Add(AddArgs),
}

struct AddArgs;

fn dispatch(command: TaskSubcommand) {
    match command {
        TaskSubcommand::Add(args) => add(args),
    }
}

fn add(_args: AddArgs) {}
"#,
    );

    let command = file
        .commands
        .iter()
        .find(|command| command.name == "task add")
        .map(|command| command.handler_symbol.as_deref());
    assert_eq!(command, Some(Some("add")));
}

#[test]
fn extracts_nested_clap_subcommands_with_name_overrides() {
    let file = extract(
        r#"
use clap::Subcommand;

#[derive(Subcommand)]
enum RootSubcommand {
    Task(TaskSubcommand),
}

#[derive(Subcommand)]
enum TaskSubcommand {
    Add(AddArgs),
    #[command(name = "review-thread")]
    ReviewThread(ReviewArgs),
}

struct AddArgs;
struct ReviewArgs;

fn dispatch(command: TaskSubcommand) {
    match command {
        TaskSubcommand::Add(args) => add(args),
        TaskSubcommand::ReviewThread(args) => review(args),
    }
}

fn add(_args: AddArgs) {}
fn review(_args: ReviewArgs) {}
"#,
    );

    assert!(file.commands.iter().any(|command| {
        command.name == "root task add" && command.handler_symbol.as_deref() == Some("add")
    }));
    assert!(file.commands.iter().any(|command| {
        command.name == "root task review-thread"
            && command.handler_symbol.as_deref() == Some("review")
    }));
}

#[test]
fn emits_clap_subcommand_when_arm_has_no_single_handler() {
    let file = extract(
        r#"
use clap::Subcommand;

#[derive(Subcommand)]
enum JobSubcommand {
    Run(RunArgs),
}

struct RunArgs;

fn dispatch(command: JobSubcommand) {
    match command {
        JobSubcommand::Run(args) => {
            audit();
            run(args)
        }
    }
}

fn audit() {}
fn run(_args: RunArgs) {}
"#,
    );

    let command = file
        .commands
        .iter()
        .find(|command| command.name == "job run")
        .map(|command| command.handler_symbol.as_deref());
    assert_eq!(command, Some(None));
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

fn call_names(file: &crate::extract::ExtractedFile) -> Vec<&str> {
    file.refs
        .iter()
        .filter(|reference| reference.kind == "call")
        .map(|reference| reference.target_name.as_str())
        .collect()
}

#[test]
fn closure_in_method_chain_call_is_extracted() {
    let file = extract(
        r#"
fn resolve_import(candidates: Vec<Candidate>) -> Vec<Candidate> {
    candidates
        .into_iter()
        .filter(|candidate| qualified_matches_import(candidate))
        .collect()
}
"#,
    );

    let names = call_names(&file);
    for expected in ["into_iter", "filter", "qualified_matches_import", "collect"] {
        assert!(
            names.contains(&expected),
            "missing callee {expected}, got {names:?}"
        );
    }
}

#[test]
fn nested_call_in_argument_position_is_extracted() {
    let file = extract(
        r#"
fn combine(value: i32) -> i32 {
    outer(inner(value))
}
"#,
    );

    let names = call_names(&file);
    assert!(
        names.contains(&"outer"),
        "missing outer call, got {names:?}"
    );
    assert!(
        names.contains(&"inner"),
        "missing inner nested call, got {names:?}"
    );
}

#[test]
fn try_wrapped_call_as_chain_receiver_is_extracted() {
    let file = extract(
        r#"
fn load() -> Option<i32> {
    fetch()?.parse().ok()
}
"#,
    );

    let names = call_names(&file);
    assert!(
        names.contains(&"fetch"),
        "missing ?-wrapped receiver call, got {names:?}"
    );
    assert!(
        names.contains(&"parse"),
        "missing chained call after ?, got {names:?}"
    );
    assert!(
        names.contains(&"ok"),
        "missing final chained call, got {names:?}"
    );
}

#[test]
fn await_chain_call_is_extracted() {
    let file = extract(
        r#"
async fn run() {
    fetch_value().await.process();
}
"#,
    );

    let names = call_names(&file);
    assert!(
        names.contains(&"fetch_value"),
        "missing awaited receiver call, got {names:?}"
    );
    assert!(
        names.contains(&"process"),
        "missing call chained after .await, got {names:?}"
    );
}

#[test]
fn turbofish_method_chain_receiver_does_not_leak_as_call_name() {
    let file = extract(
        r#"
fn resolve_import(tx: &Connection, imported_name: &str, import: &Import) -> Vec<Candidate> {
    symbols_by_name(tx, imported_name)
        .into_iter()
        .filter(|candidate| {
            qualified_matches_import(candidate, import, imported_name)
        })
        .collect::<Vec<_>>()
}
"#,
    );

    let names = call_names(&file);
    for expected in [
        "symbols_by_name",
        "qualified_matches_import",
        "filter",
        "collect",
    ] {
        assert!(
            names.contains(&expected),
            "missing callee {expected}, got {names:?}"
        );
    }
    assert!(
        file.refs.iter().all(|reference| {
            !reference.target_name.contains('(')
                && !reference.target_name.contains('\n')
                && !reference.target_name.contains('.')
        }),
        "a ref target_name leaked receiver source text: {names:?}"
    );
}

#[test]
fn skips_command_extraction_for_non_orbit_workspace_crates() {
    let file = extract_at(
        "crates/orbit-graph-cli/src/commands/mod.rs",
        r#"
use clap::Subcommand;

#[derive(Subcommand)]
enum Command {
    Trace(TraceCommand),
}

struct TraceCommand;

fn dispatch(command: Command) {
    match command {
        Command::Trace(args) => args.run(),
    }
}

impl TraceCommand {
    fn run(self) {}
}
"#,
    );

    assert!(file.commands.is_empty());
}
