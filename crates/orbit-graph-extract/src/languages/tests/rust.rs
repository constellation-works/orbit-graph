#![allow(missing_docs)]

use std::path::Path;

use crate::Extractor;
use crate::languages::RustExtractor;

fn extract(source: &str) -> crate::ExtractedFile {
    RustExtractor.extract(Path::new("src/sample.rs"), source.as_bytes())
}

fn extract_at(path: &str, source: &str) -> crate::ExtractedFile {
    RustExtractor.extract(Path::new(path), source.as_bytes())
}

fn symbol_kinds(file: &crate::ExtractedFile) -> Vec<&str> {
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
fn extracts_nested_functions_with_their_own_call_spans() {
    let file = extract(
        r#"
fn outer() {
    fn inner() {
        helper();
    }

    inner();
}

fn helper() {}
"#,
    );

    let inner = file
        .symbols
        .iter()
        .find(|symbol| symbol.qualified == "outer::inner")
        .expect("nested function symbol");
    assert_eq!(inner.kind, "function");
    assert_eq!(inner.parent_symbol.as_deref(), Some("outer"));

    let helper_call = file
        .refs
        .iter()
        .find(|reference| reference.kind == "call" && reference.target_name == "helper")
        .expect("call inside nested function");
    assert!(
        helper_call.from_span_start >= inner.span_start
            && helper_call.from_span_end <= inner.span_end,
        "nested call must be attributable to inner, got {helper_call:?}"
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
fn drive() {
    let runner = build();
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
fn self_and_self_path_calls_record_the_enclosing_impl_type() {
    let file = extract(
        r#"
struct Worker;

impl Worker {
    fn run(&self) { self.helper(); }
    fn helper(&self) { Self::run(self); }
}
"#,
    );

    assert_eq!(
        call_ref(&file, "helper").target_qualified.as_deref(),
        Some("<Worker>::helper")
    );
    assert_eq!(
        call_ref(&file, "run").target_qualified.as_deref(),
        Some("<Worker>::run")
    );
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

fn call_ref<'a>(file: &'a crate::ExtractedFile, target_name: &str) -> &'a crate::RawRef {
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

fn call_names(file: &crate::ExtractedFile) -> Vec<&str> {
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

fn runtime_invocations(file: &crate::ExtractedFile) -> Vec<(&str, &str)> {
    file.refs
        .iter()
        .filter(|reference| reference.kind == "runtime_invocation")
        .map(|reference| {
            assert_eq!(reference.confidence, "fuzzy_name");
            assert!(reference.target_qualified.is_none(), "{reference:?}");
            let enclosing = file
                .symbols
                .iter()
                .filter(|symbol| {
                    symbol.span_start <= reference.from_span_start
                        && reference.from_span_end <= symbol.span_end
                })
                .min_by_key(|symbol| symbol.span_end - symbol.span_start)
                .map_or("<module>", |symbol| symbol.name.as_str());
            (enclosing, reference.target_name.as_str())
        })
        .collect()
}

#[test]
fn records_command_invocations_by_program_name() {
    let file = extract_at(
        "tests/cli.rs",
        r##"
use std::process::Command;

#[test]
fn literal_program() {
    Command::new("git").arg("status").output().unwrap();
}

#[test]
fn qualified_literal_program() {
    std::process::Command::new(r#"cargo"#).status().unwrap();
}

#[test]
fn cargo_bin_exe() {
    Command::new(env!("CARGO_BIN_EXE_orbit-graph")).arg("--help");
}

#[test]
fn assert_cmd_cargo_bin() {
    assert_cmd::Command::cargo_bin("orbit-graph").unwrap().assert();
}

fn helper() -> Command {
    Command::cargo_bin("tool").unwrap()
}
"##,
    );

    assert_eq!(
        runtime_invocations(&file),
        vec![
            ("literal_program", "git"),
            ("qualified_literal_program", "cargo"),
            ("cargo_bin_exe", "orbit-graph"),
            ("assert_cmd_cargo_bin", "orbit-graph"),
            ("helper", "tool"),
        ]
    );
    // `Command::new` itself is still an ordinary call ref.
    assert!(
        file.refs
            .iter()
            .any(|reference| reference.kind == "call" && reference.target_name == "new")
    );
}

#[test]
fn skips_command_calls_whose_program_the_syntax_does_not_name() {
    let file = extract_at(
        "tests/cli.rs",
        r#"
use std::process::Command;

fn dynamic(path: &str) {
    Command::new(path);
    Command::new(bin_path());
    Command::new(env!("CARGO_PKG_NAME"));
    Builder::new("not-a-command");
    Command::cargo_bin(env!("CARGO_PKG_NAME"));
}
"#,
    );

    assert_eq!(runtime_invocations(&file), Vec::<(&str, &str)>::new());
}

fn macro_call_ref<'a>(
    file: &'a crate::ExtractedFile,
    source: &str,
    target_name: &str,
    line: usize,
) -> &'a crate::RawRef {
    let matches = file
        .refs
        .iter()
        .filter(|reference| {
            reference.kind == "call"
                && reference.target_name == target_name
                && source[..reference.from_span_start].matches('\n').count() == line
        })
        .collect::<Vec<_>>();
    assert_eq!(
        matches.len(),
        1,
        "expected one call ref for {target_name} on line {line}, got {:?}",
        file.refs
    );
    matches[0]
}

#[test]
fn calls_inside_std_macro_arguments_are_recovered() {
    let source = r#"
fn checks(store: &Store, root: &Path) {
    assert!(store.delete_bundle("ORB-1").expect("delete"));
    assert!(!registry_has_task(&root, "ORB-1"));
    assert_eq!(sanitize("a.b"), "a_b");
    assert_ne!(first(&root), second(root));
    let paths = vec![partition_path(root, "ws")];
    println!("{}", render(describe(&root)));
    let text = format!("{} {}", crate::util::label(1), Helper::name());
    debug_assert!(matches!(parse::<u8>("1"), Ok(_)));
}
"#;
    let file = extract(source);

    let delete = macro_call_ref(&file, source, "delete_bundle", 2);
    assert_eq!(delete.unresolved_receiver.as_deref(), Some("store"));
    // The receiver is a typed parameter, so the call names its type's member.
    assert_eq!(
        delete.target_qualified.as_deref(),
        Some("<Store>::delete_bundle")
    );
    assert_eq!(
        macro_call_ref(&file, source, "expect", 2)
            .unresolved_receiver
            .as_deref(),
        Some("store.delete_bundle(\"ORB-1\")")
    );
    let negated = macro_call_ref(&file, source, "registry_has_task", 3);
    assert_eq!(negated.unresolved_receiver, None);
    assert_eq!(
        negated.target_qualified.as_deref(),
        Some("registry_has_task")
    );
    macro_call_ref(&file, source, "sanitize", 4);
    macro_call_ref(&file, source, "first", 5);
    macro_call_ref(&file, source, "second", 5);
    macro_call_ref(&file, source, "partition_path", 6);
    // Nested calls in nested macro arguments.
    macro_call_ref(&file, source, "render", 7);
    macro_call_ref(&file, source, "describe", 7);
    let label = macro_call_ref(&file, source, "label", 8);
    assert_eq!(
        label.target_qualified.as_deref(),
        Some("crate::util::label")
    );
    assert_eq!(label.confidence, "import_resolved");
    assert_eq!(
        &source[label.from_span_start..label.from_span_end],
        "crate::util::label"
    );
    assert_eq!(
        macro_call_ref(&file, source, "name", 8)
            .target_qualified
            .as_deref(),
        Some("Helper::name")
    );
    // Turbofish call inside a macro nested in another macro.
    macro_call_ref(&file, source, "parse", 9);

    // Macro names, string contents, and the macro's own delimiters are not calls.
    for name in call_names(&file) {
        assert!(
            !matches!(
                name,
                "assert" | "assert_eq" | "vec" | "format" | "matches" | "println"
            ),
            "macro name leaked as a call: {name}"
        );
        assert!(
            !name.contains(['(', '.', '"', ' ', '\n']),
            "bad call name {name}"
        );
    }
}

#[test]
fn self_method_calls_inside_macros_record_the_enclosing_impl_type() {
    let source = r#"
struct Worker;
impl Worker {
    fn run(&self) {
        assert!(self.ready());
        assert_eq!(Self::limit(), 3);
    }
}
"#;
    let file = extract(source);

    let ready = macro_call_ref(&file, source, "ready", 4);
    assert_eq!(ready.target_qualified.as_deref(), Some("<Worker>::ready"));
    assert_eq!(ready.unresolved_receiver, None);
    let limit = macro_call_ref(&file, source, "limit", 5);
    assert_eq!(limit.target_qualified.as_deref(), Some("<Worker>::limit"));
}

#[test]
fn declarations_and_keywords_inside_macros_are_not_calls() {
    let source = r#"
fn generate() {
    quote! {
        fn generated(value: u8) -> u8 { value }
        struct Wrapper(u8);
    };
    let total = sum!(for x in (0..3) { x });
    let _ = if_chain!(if ready (a) { b });
}
"#;
    let file = extract(source);
    let names = call_names(&file);

    for absent in ["generated", "Wrapper", "in", "if"] {
        assert!(
            !names.contains(&absent),
            "{absent} is not a call: {names:?}"
        );
    }
}

#[test]
fn functions_passed_as_values_are_recorded_as_calls() {
    let source = r#"
fn skill_link_roots(root: &Path) -> Vec<PathBuf> { vec![] }

impl Loader {
    fn parse(text: &str) -> u8 { 0 }
    fn load(&self, roots: Vec<PathBuf>, items: Vec<&str>) {
        let links = roots.iter().map(skill_link_roots).collect::<Vec<_>>();
        let parsed = items.into_iter().map(Self::parse);
        let names = items.iter().map(ToString::to_string);
        let wrapped = items.into_iter().map(Some);
        run_step(helpers::normalize, roots);
    }
}
"#;
    let file = extract(source);

    let value = macro_call_ref(&file, source, "skill_link_roots", 6);
    assert_eq!(value.target_qualified.as_deref(), Some("skill_link_roots"));
    assert_eq!(value.unresolved_receiver, None);
    assert_eq!(
        macro_call_ref(&file, source, "parse", 7)
            .target_qualified
            .as_deref(),
        Some("<Loader>::parse")
    );
    assert_eq!(
        macro_call_ref(&file, source, "to_string", 8)
            .target_qualified
            .as_deref(),
        Some("ToString::to_string")
    );
    assert_eq!(
        macro_call_ref(&file, source, "normalize", 10)
            .target_qualified
            .as_deref(),
        Some("helpers::normalize")
    );

    // Locals, parameters, and capitalised constructors passed as values are
    // not function refs.
    let names = call_names(&file);
    for absent in ["roots", "items", "Some"] {
        assert!(
            !names.contains(&absent),
            "{absent} is not a call: {names:?}"
        );
    }
}

#[test]
fn closure_and_pattern_bound_names_passed_as_values_are_not_calls() {
    let source = r#"
fn drive(input: Option<Handler>, handlers: Vec<Handler>) {
    if let Some(handler) = input {
        dispatch(handler);
    }
    for entry in handlers {
        dispatch(entry);
    }
    let callback = |event| notify(event);
    subscribe(callback);
    match input {
        Some(Handler { name, .. }) => report(name),
        other => report(other),
    }
}
"#;
    let file = extract(source);
    let names = call_names(&file);

    for absent in ["handler", "entry", "event", "callback", "name", "other"] {
        assert!(!names.contains(&absent), "{absent} is a local: {names:?}");
    }
    for present in ["dispatch", "notify", "subscribe", "report"] {
        assert!(
            names.contains(&present),
            "missing call {present}: {names:?}"
        );
    }
}

#[test]
fn method_calls_on_typed_bindings_record_the_receiver_type() {
    let source = r#"
fn handle(runtime: &OrbitRuntime, store: Arc<dyn TaskStore>, mut boxed: Box<Loader>, items: &[Item]) {
    runtime.enable_plugin("demo");
    store.save_task(1);
    boxed.load();
    items.len();
    let config: &ResolvedConfig = runtime.config();
    config.crew();
    let built = Builder::new();
    built.finish();
    let literal = Options { force: true };
    literal.validate();
    let inferred = runtime.clone();
    inferred.enable_plugin("x");
    let shadowed: Parser = Parser::new();
    let shadowed = shadowed.parse();
    shadowed.render();
    assert!(runtime.is_ready());
}
"#;
    let file = extract(source);

    let typed = [
        ("enable_plugin", 2, Some("<OrbitRuntime>::enable_plugin")),
        ("save_task", 3, Some("<dyn TaskStore>::save_task")),
        ("load", 4, Some("<Loader>::load")),
        ("len", 5, None),
        ("crew", 7, Some("<ResolvedConfig>::crew")),
        ("finish", 9, Some("<Builder>::finish")),
        ("validate", 11, Some("<Options>::validate")),
        ("enable_plugin", 13, None),
        ("render", 16, None),
        ("is_ready", 17, Some("<OrbitRuntime>::is_ready")),
    ];
    for (method, line, expected) in typed {
        let reference = macro_call_ref(&file, source, method, line);
        assert_eq!(
            reference.target_qualified.as_deref(),
            expected,
            "{method} on line {line}"
        );
        // The receiver is still recorded: a typed receiver narrows the
        // target, it never licenses a bare-name match (ORB-12416).
        assert!(reference.unresolved_receiver.is_some(), "{method}");
    }
}

#[test]
fn self_typed_parameters_and_constructors_name_the_impl_type() {
    let source = r#"
struct Worker;
impl Worker {
    fn merge(&self, other: &Self) {
        other.flush();
        let fresh = Self::new();
        fresh.flush();
    }
}
"#;
    let file = extract(source);

    for line in [4, 6] {
        assert_eq!(
            macro_call_ref(&file, source, "flush", line)
                .target_qualified
                .as_deref(),
            Some("<Worker>::flush")
        );
    }
}

#[test]
fn type_parameters_are_unknown_and_use_aliases_name_the_renamed_type() {
    let source = r#"
use crate::a::Foo as Bar;
fn g<T: Tr>(x: T, y: Bar, z: impl Tr) {
    x.m();
    y.m();
    z.m();
    let w = Bar::new();
    w.m();
    Bar::build();
}
impl<U> Wrapper<U> {
    fn h(&self, u: U) { u.m(); }
}
"#;
    let file = extract(source);

    for (line, expected) in [
        (3, None),
        (4, Some("<crate::a::Foo>::m")),
        (5, Some("<dyn Tr>::m")),
        (7, Some("<crate::a::Foo>::m")),
        (11, None),
    ] {
        assert_eq!(
            macro_call_ref(&file, source, "m", line)
                .target_qualified
                .as_deref(),
            expected,
            "m on line {line}"
        );
    }
    let build = macro_call_ref(&file, source, "build", 8);
    assert_eq!(
        build.target_qualified.as_deref(),
        Some("crate::a::Foo::build")
    );
    assert!(build.spelled_path, "{build:?}");
}

#[test]
fn only_paths_written_at_the_call_site_are_marked_spelled() {
    let source = r#"
mod tests {
    fn run(store: &Store) {
        helper();
        crate::util::helper();
        store.save();
        assert!(crate::util::check());
    }
}
"#;
    let file = extract(source);

    for (name, line, spelled) in [
        ("helper", 3, false),
        ("helper", 4, true),
        ("save", 5, false),
        ("check", 6, true),
    ] {
        let call = macro_call_ref(&file, source, name, line);
        assert_eq!(
            call.spelled_path, spelled,
            "{name} on line {line}: {call:?}"
        );
    }
}

#[test]
fn scoped_type_calls_and_trait_impl_methods_keep_their_type_path() {
    let source = r#"
struct Config;
impl Default for Config {
    fn default() -> Self { Config::load() }
}
impl Config {
    fn load() -> Self { Config }
}
fn read() {
    let _ = ResolvedConfig::load();
    let _ = crate::config::ResolvedConfig::load();
    let _ = <Config as Default>::default();
}
"#;
    let file = extract(source);

    let symbols = file
        .symbols
        .iter()
        .map(|symbol| symbol.qualified.as_str())
        .collect::<Vec<_>>();
    assert!(
        symbols.contains(&"<Config as Default>::default"),
        "{symbols:?}"
    );
    assert!(symbols.contains(&"<Config>::load"), "{symbols:?}");

    assert_eq!(
        macro_call_ref(&file, source, "load", 3)
            .target_qualified
            .as_deref(),
        Some("Config::load")
    );
    assert_eq!(
        macro_call_ref(&file, source, "load", 9)
            .target_qualified
            .as_deref(),
        Some("ResolvedConfig::load")
    );
    assert_eq!(
        macro_call_ref(&file, source, "load", 10)
            .target_qualified
            .as_deref(),
        Some("crate::config::ResolvedConfig::load")
    );
    assert_eq!(
        macro_call_ref(&file, source, "default", 11)
            .target_qualified
            .as_deref(),
        Some("<Config as Default>::default")
    );
}
