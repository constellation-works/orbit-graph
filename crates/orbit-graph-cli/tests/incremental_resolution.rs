//! Incremental sync re-resolves references in unchanged files whose target
//! was defined, removed or renamed in a changed file, through the packaged
//! `orbit-graph` executable.

#![allow(clippy::expect_used)]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

/// Callers in files the scenarios never edit. Their outbound refs are what an
/// incremental sync must bring in line with a full sync.
const CALLERS: [&str; 2] = [
    "symbol:src/importer.rs#import_caller:function",
    "symbol:src/plain.rs#plain_caller:function",
];

#[test]
fn renaming_a_definition_demotes_remote_callers_like_a_full_sync() {
    let repo = fixture_repository();
    sync(repo.path(), false);
    assert_eq!(
        callee(repo.path(), CALLERS[0], "remote_target"),
        (
            "import_resolved".to_string(),
            Some("remote_target".to_string())
        )
    );
    assert_eq!(
        callee(repo.path(), CALLERS[1], "remote_target"),
        ("same_module".to_string(), Some("remote_target".to_string()))
    );

    write(
        repo.path(),
        "src/defs.rs",
        "pub fn remote_target_renamed() -> i32 {\n    1\n}\n",
    );
    let report = sync(repo.path(), false);
    assert_eq!(report["files_changed"], 1, "{report}");

    for caller in CALLERS {
        assert_eq!(
            callee(repo.path(), caller, "remote_target"),
            ("fuzzy_name".to_string(), None),
            "{caller}"
        );
    }
    assert_incremental_matches_full(
        repo.path(),
        &["symbol:src/defs.rs#remote_target_renamed:function"],
    );
}

#[test]
fn removing_a_definition_file_demotes_remote_callers_like_a_full_sync() {
    let repo = fixture_repository();
    sync(repo.path(), false);

    fs::remove_file(repo.path().join("src/defs.rs")).expect("remove definition file");
    let report = sync(repo.path(), false);
    assert_eq!(report["files_removed"], 1, "{report}");

    for caller in CALLERS {
        assert_eq!(
            callee(repo.path(), caller, "remote_target"),
            ("fuzzy_name".to_string(), None),
            "{caller}"
        );
    }
    assert_incremental_matches_full(repo.path(), &[]);
}

#[test]
fn adding_a_unique_definition_promotes_a_fuzzy_remote_call_like_a_full_sync() {
    let repo = fixture_repository();
    sync(repo.path(), false);
    assert_eq!(
        callee(repo.path(), CALLERS[1], "later_target"),
        ("fuzzy_name".to_string(), None)
    );

    write(
        repo.path(),
        "src/later.rs",
        "pub fn later_target() -> i32 {\n    2\n}\n",
    );
    let report = sync(repo.path(), false);
    assert_eq!(report["files_changed"], 1, "{report}");

    assert_eq!(
        callee(repo.path(), CALLERS[1], "later_target"),
        ("same_module".to_string(), Some("later_target".to_string()))
    );
    assert_incremental_matches_full(repo.path(), &["symbol:src/later.rs#later_target:function"]);
}

/// pass1 rewrites a changed file's symbol rows, and SQLite reuses the freed
/// rowids. When the file holds the highest ids, a definition added above the
/// others takes the first old id, so an unchanged definition moves to a new
/// id. A caller's stored hint must follow it, or `refs` for the moved symbol
/// comes back empty and the new symbol collects the caller instead.
#[test]
fn rewriting_the_file_with_the_highest_symbol_ids_keeps_hints_on_their_symbols() {
    let repo = TempDir::new().expect("create fixture repository");
    run_git(repo.path(), &["init", "-q", "-b", "main"]);
    write(repo.path(), "src/lib.rs", "mod caller;\nmod zdefs;\n");
    write(
        repo.path(),
        "src/caller.rs",
        "pub fn call() -> i32 {\n    crate::zdefs::alpha()\n}\n",
    );
    write(
        repo.path(),
        "src/zdefs.rs",
        "pub fn alpha() -> i32 {\n    1\n}\n\npub fn beta() -> i32 {\n    2\n}\n",
    );
    sync(repo.path(), false);
    let alpha = "symbol:src/zdefs.rs#alpha:function";
    assert_eq!(referencing_files(repo.path(), alpha), ["src/caller.rs"]);

    write(
        repo.path(),
        "src/zdefs.rs",
        "pub fn gamma() -> i32 {\n    3\n}\n\npub fn alpha() -> i32 {\n    1\n}\n\npub fn beta() -> i32 {\n    2\n}\n",
    );
    let report = sync(repo.path(), false);
    assert_eq!(report["files_changed"], 1, "{report}");

    assert_eq!(referencing_files(repo.path(), alpha), ["src/caller.rs"]);
    let gamma = "symbol:src/zdefs.rs#gamma:function";
    assert!(referencing_files(repo.path(), gamma).is_empty());
    assert_queries_match_full(
        repo.path(),
        &["symbol:src/caller.rs#call:function"],
        &[alpha, gamma, "symbol:src/zdefs.rs#beta:function"],
    );
}

/// A typed receiver call `foo.hi()` (`<Foo>::hi`, from ORB-13098) resolves
/// through the type-member rung to a trait's default body only while some
/// `impl Greet for Foo` exists, and to an inherent `Foo::hi` when one exists. Adding or removing that impl in another file
/// changes no symbol named `hi`, yet the unchanged caller's ref must follow,
/// as a full sync would store it.
#[test]
fn adding_or_removing_a_trait_impl_re_resolves_typed_receiver_calls() {
    let repo = TempDir::new().expect("create fixture repository");
    run_git(repo.path(), &["init", "-q", "-b", "main"]);
    write(
        repo.path(),
        "src/lib.rs",
        "mod caller;\nmod foo;\nmod greet;\n",
    );
    write(
        repo.path(),
        "src/greet.rs",
        "pub trait Greet {\n    fn hi(&self) -> i32 {\n        1\n    }\n}\n",
    );
    write(repo.path(), "src/foo.rs", "pub struct Foo;\n");
    write(
        repo.path(),
        "src/caller.rs",
        "use crate::foo::Foo;\nuse crate::greet::Greet;\n\npub fn call(foo: &Foo) -> i32 {\n    foo.hi()\n}\n",
    );
    sync(repo.path(), false);
    let caller = "symbol:src/caller.rs#call:function";
    let greet_hi = "symbol:src/greet.rs#Greet::hi:method";
    let unimplemented = callee(repo.path(), caller, "hi");
    assert_eq!(unimplemented.0, "fuzzy_name", "{unimplemented:?}");

    write(
        repo.path(),
        "src/foo.rs",
        "use crate::greet::Greet;\n\npub struct Foo;\n\nimpl Greet for Foo {}\n",
    );
    let report = sync(repo.path(), false);
    assert_eq!(report["files_changed"], 1, "{report}");
    let implemented = callee(repo.path(), caller, "hi");
    assert_eq!(
        implemented,
        ("exact".to_string(), Some("Greet::hi".to_string()))
    );
    assert_queries_match_full(repo.path(), &[caller], &[greet_hi]);

    // An inherent method wins over the trait's default body.
    write(
        repo.path(),
        "src/foo.rs",
        "use crate::greet::Greet;\n\npub struct Foo;\n\nimpl Foo {\n    pub fn hi(&self) -> i32 {\n        2\n    }\n}\n\nimpl Greet for Foo {}\n",
    );
    let report = sync(repo.path(), false);
    assert_eq!(report["files_changed"], 1, "{report}");
    assert_eq!(
        callee(repo.path(), caller, "hi"),
        ("exact".to_string(), Some("<Foo>::hi".to_string()))
    );
    assert_queries_match_full(repo.path(), &[caller], &[greet_hi]);

    write(repo.path(), "src/foo.rs", "pub struct Foo;\n");
    let report = sync(repo.path(), false);
    assert_eq!(report["files_changed"], 1, "{report}");
    assert_eq!(callee(repo.path(), caller, "hi"), unimplemented);
    assert_queries_match_full(repo.path(), &[caller], &[greet_hi]);
}

/// The files holding refs to `target`, as `refs` reports them.
fn referencing_files(repo: &Path, target: &str) -> Vec<String> {
    run_json(repo, ["refs", target])["refs"]
        .as_array()
        .expect("refs array")
        .iter()
        .map(|reference| reference["file"].as_str().expect("ref file").to_string())
        .collect()
}

/// Every caller's outbound refs, and the inbound `refs` and `impact` of each
/// target, after the incremental sync that just ran equal those after
/// rebuilding the same tree from scratch.
fn assert_incremental_matches_full(repo: &Path, targets: &[&str]) {
    assert_queries_match_full(repo, &CALLERS, targets);
}

fn assert_queries_match_full(repo: &Path, callers: &[&str], targets: &[&str]) {
    let incremental = graph_queries(repo, callers, targets);
    let rebuilt = sync(repo, true);
    assert!(
        rebuilt["files_indexed"].as_u64().is_some_and(|n| n >= 3),
        "{rebuilt}"
    );
    assert_eq!(incremental, graph_queries(repo, callers, targets));
}

fn graph_queries(repo: &Path, callers: &[&str], targets: &[&str]) -> Vec<Value> {
    let outbound = callers
        .iter()
        .map(|caller| run_json(repo, ["callees", "--include-unresolved", caller]));
    let inbound = targets.iter().flat_map(|target| {
        [
            run_json(repo, ["refs", "--confidence", "fuzzy", target]),
            run_json(repo, ["impact", target]),
        ]
    });
    outbound.chain(inbound).collect()
}

/// The confidence and resolved qualified name of the caller's one callee
/// named `target_name`.
fn callee(repo: &Path, caller: &str, target_name: &str) -> (String, Option<String>) {
    let callees = run_json(repo, ["callees", "--include-unresolved", caller]);
    let matches = callees["callees"]
        .as_array()
        .expect("callees array")
        .iter()
        .filter(|callee| callee["target_name"] == target_name)
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "{callees}");
    (
        matches[0]["confidence"]
            .as_str()
            .expect("confidence")
            .to_string(),
        matches[0]["target_qualified"].as_str().map(str::to_string),
    )
}

fn sync(repo: &Path, full: bool) -> Value {
    if full {
        run_json(repo, ["sync", "--full"])
    } else {
        run_json(repo, ["sync"])
    }
}

fn fixture_repository() -> TempDir {
    let repo = TempDir::new().expect("create fixture repository");
    run_git(repo.path(), &["init", "-q", "-b", "main"]);
    write(
        repo.path(),
        "src/lib.rs",
        "mod defs;\nmod importer;\nmod later;\nmod plain;\n",
    );
    write(
        repo.path(),
        "src/defs.rs",
        "pub fn remote_target() -> i32 {\n    1\n}\n",
    );
    write(
        repo.path(),
        "src/importer.rs",
        "use crate::defs::remote_target;\n\npub fn import_caller() -> i32 {\n    remote_target()\n}\n",
    );
    write(
        repo.path(),
        "src/plain.rs",
        "pub fn plain_caller() -> i32 {\n    remote_target() + later_target()\n}\n",
    );
    repo
}

fn write(repo: &Path, path: &str, contents: &str) {
    let target = repo.join(path);
    fs::create_dir_all(target.parent().expect("parent directory")).expect("create directory");
    fs::write(target, contents).expect("write fixture file");
    // Incremental sync compares mtimes before hashing; make sure a rewrite
    // within the same timestamp tick still reads as a change.
    bump_mtime(repo, path);
}

fn bump_mtime(repo: &Path, path: &str) {
    let file = fs::File::options()
        .write(true)
        .open(repo.join(path))
        .expect("open fixture file");
    let modified = file
        .metadata()
        .and_then(|metadata| metadata.modified())
        .expect("fixture mtime");
    file.set_modified(modified + std::time::Duration::from_secs(2))
        .expect("bump fixture mtime");
}

fn run_json<const N: usize>(cwd: &Path, args: [&str; N]) -> Value {
    let output = run(cwd, args);
    assert!(
        output.status.success(),
        "orbit-graph {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON command output")
}

fn run<const N: usize>(cwd: &Path, args: [&str; N]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
        .current_dir(cwd)
        .args(["--format", "json"])
        .args(args)
        .output()
        .expect("run orbit-graph")
}

fn run_git(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}
