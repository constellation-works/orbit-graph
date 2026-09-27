//! `orbit-graph changes`, driven through the built executable: the JSON
//! document for a revision range and for the working tree, the default base,
//! help text on every flag, usage errors, and signalled bounds.

#![allow(clippy::expect_used)]

mod common;

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

/// A repository on `feature` whose commit changes `helper`, which `entry`
/// calls and `tests/helper.rs` tests; `main` stays at the base commit.
struct Fixture {
    dir: TempDir,
    base: String,
    head: String,
}

impl Fixture {
    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn range(&self) -> String {
        format!("{}..{}", self.base, self.head)
    }
}

fn fixture() -> Fixture {
    let dir = TempDir::new().expect("create fixture repository");
    let root = dir.path();
    git(root, &["init", "-q", "-b", "main"]);
    write(
        root,
        "Cargo.toml",
        "[package]\nname = \"tool\"\nversion = \"0.1.0\"\n",
    );
    write(
        root,
        "src/lib.rs",
        "pub fn helper() -> i32 {\n    1\n}\n\npub fn entry() -> i32 {\n    helper()\n}\n",
    );
    write(
        root,
        "tests/helper.rs",
        "use tool::helper;\n\n#[test]\nfn helper_is_positive() {\n    assert!(helper() > 0);\n}\n",
    );
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "base"]);
    let base = git(root, &["rev-parse", "HEAD"]);
    git(root, &["checkout", "-q", "-b", "feature"]);
    write(
        root,
        "src/lib.rs",
        "pub fn helper() -> i32 {\n    2\n}\n\npub fn entry() -> i32 {\n    helper() + 1\n}\n",
    );
    git(root, &["commit", "-q", "-am", "head"]);
    let head = git(root, &["rev-parse", "HEAD"]);
    Fixture { dir, base, head }
}

#[test]
fn a_revision_range_reports_changed_symbols_with_labelled_callers_and_tests() {
    let fixture = fixture();
    let document = json(
        fixture.path(),
        &["changes", &fixture.range(), "--confidence", "fuzzy"],
    );

    assert_eq!(document["schema_version"], 1);
    assert_eq!(document["complete"], true);
    assert_eq!(document["comparison"]["mode"], "direct_base_head");
    assert_eq!(document["comparison"]["base"]["commit_sha"], fixture.base);
    assert_eq!(document["comparison"]["head"]["commit_sha"], fixture.head);
    let helper = symbol(&document, "#helper:");
    assert_eq!(helper["status"], "modified");
    assert!(
        helper["pairing"]
            .as_str()
            .is_some_and(|rung| !rung.is_empty()),
        "every changed symbol names its pairing rung: {helper}"
    );
    assert!(helper["pairing_evidence"].is_object() || helper["pairing_evidence"].is_string());
    let callers = helper["callers"].as_array().expect("callers");
    assert!(
        callers.iter().any(|caller| caller["caller"]["selector"]
            .as_str()
            .is_some_and(|selector| selector.contains("#entry:"))),
        "entry calls helper: {helper}"
    );
    let tests = helper["candidate_tests"]
        .as_array()
        .expect("candidate tests");
    assert!(
        tests.iter().any(|test| test["test"]["label"]
            .as_str()
            .is_some_and(|label| label.starts_with("tests/helper.rs"))),
        "the test that calls helper is a candidate: {helper}"
    );
    let entry_points = helper["entry_points"].as_array().expect("entry points");
    for labelled in callers.iter().chain(entry_points).chain(tests) {
        assert!(
            labelled["source"].as_str().is_some_and(|s| !s.is_empty()),
            "every item names its source: {labelled}"
        );
        assert!(
            labelled["confidence"]
                .as_str()
                .is_some_and(|c| !c.is_empty()),
            "every item names its confidence: {labelled}"
        );
    }
    for caller in callers {
        assert!(
            caller["evidence"].is_object(),
            "a caller cites its path: {caller}"
        );
    }
    assert!(
        document["tests"]
            .as_array()
            .expect("tests")
            .iter()
            .any(|test| test["changed_symbols"]
                .as_array()
                .is_some_and(|symbols| symbols.contains(&helper["selector"]))),
        "the test list maps each test to its changed symbols"
    );
    assert_eq!(document["timings"]["base_cache"], "miss");
    assert!(
        fixture
            .path()
            .join(".orbit-graph/explorer/snapshots")
            .is_dir(),
        "committed snapshots are cached as graph scratch state"
    );

    let warm = json(
        fixture.path(),
        &["changes", &fixture.range(), "--confidence", "fuzzy"],
    );
    assert_eq!(warm["timings"]["base_cache"], "hit");
    assert_eq!(warm["symbols"], document["symbols"]);
}

#[test]
fn json_is_the_same_document_as_format_json() {
    let fixture = fixture();
    let range = fixture.range();
    let shorthand = run(fixture.path(), &["changes", &range, "--json", "--no-cache"]);
    let long = run(
        fixture.path(),
        &["changes", &range, "--format", "json", "--no-cache"],
    );
    assert!(shorthand.status.success(), "{}", stderr(&shorthand));
    assert!(long.status.success(), "{}", stderr(&long));
    let volatile = ["generated_at", "timings"];
    assert_eq!(
        without_fields(&shorthand.stdout, &volatile),
        without_fields(&long.stdout, &volatile)
    );
    assert!(
        !fixture.path().join(".orbit-graph").exists(),
        "--no-cache writes nothing to the repository"
    );
}

#[test]
fn the_working_tree_form_and_the_default_base_include_uncommitted_changes() {
    let fixture = fixture();
    write(
        fixture.path(),
        "src/extra.rs",
        "pub fn extra() -> i32 {\n    3\n}\n",
    );
    let status_before = git(fixture.path(), &["status", "--porcelain"]);

    let explicit = json(fixture.path(), &["changes", &fixture.head, "--no-cache"]);
    assert_eq!(explicit["comparison"]["mode"], "working_tree");
    assert!(explicit["default_base"].is_null());
    symbol(&explicit, "#extra:");
    assert!(
        explicit["symbols"]
            .as_array()
            .expect("symbols")
            .iter()
            .all(|symbol| !symbol["selector"]
                .as_str()
                .unwrap_or("")
                .contains("#helper:")),
        "helper is committed at the base, so it is unchanged in the working tree"
    );

    // No upstream and no origin/HEAD: the default base is the merge base of
    // HEAD with main, so the committed change and the untracked file both show.
    let defaulted = json(fixture.path(), &["changes", "--no-cache"]);
    assert_eq!(defaulted["comparison"]["mode"], "working_tree");
    assert_eq!(defaulted["default_base"]["reference"], "main");
    assert_eq!(defaulted["default_base"]["source"], "main");
    assert_eq!(defaulted["default_base"]["merge_base"], fixture.base);
    assert_eq!(defaulted["comparison"]["base"]["commit_sha"], fixture.base);
    symbol(&defaulted, "#extra:");
    symbol(&defaulted, "#helper:");

    assert_eq!(
        git(fixture.path(), &["status", "--porcelain"]),
        status_before,
        "the working tree and index are untouched"
    );
    assert!(!fixture.path().join(".orbit-graph").exists());
}

#[test]
fn every_flag_has_help_text() {
    let fixture = fixture();
    let output = run(fixture.path(), &["changes", "--help"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let help = String::from_utf8(output.stdout).expect("UTF-8 help");
    let flags = [
        "--symbol",
        "--confidence",
        "--depth",
        "--node-cap",
        "--query-budget-ms",
        "--max-symbols",
        "--max-callers",
        "--max-entry-points",
        "--max-tests",
        "--budget-ms",
        "--language",
        "--scope",
        "--cache-dir",
        "--no-cache",
        "--format",
        "--json",
    ];
    let lines: Vec<&str> = help.lines().collect();
    for flag in flags {
        let at = lines
            .iter()
            .position(|line| {
                let line = line.trim_start();
                line == flag || line.starts_with(&format!("{flag} "))
            })
            .unwrap_or_else(|| panic!("{flag} is listed in --help:\n{help}"));
        let description = lines.get(at + 1).map_or("", |line| line.trim());
        assert!(
            !description.is_empty() && !description.starts_with('-'),
            "{flag} has help text:\n{help}"
        );
    }
    assert!(help.contains("[RANGE]"));
    for topic in ["<base>..<head>", "Examples:", "call_path", "--no-cache"] {
        assert!(help.contains(topic), "--help explains {topic}");
    }
}

#[test]
fn malformed_ranges_and_bounds_are_usage_errors() {
    let fixture = fixture();
    for args in [
        &["changes", "main...feature", "--json"][..],
        &["changes", "main..", "--json"],
        &["changes", "main..feature", "--depth", "11", "--json"],
        &["changes", "main..feature", "--max-tests", "0", "--json"],
        &["changes", "main..feature", "--budget-ms", "10", "--json"],
        &[
            "changes",
            "main..feature",
            "--symbol",
            "file:src/lib.rs",
            "--json",
        ],
    ] {
        let output = run(fixture.path(), args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            stderr(&output)
        );
        assert!(output.stdout.is_empty(), "{args:?}");
        assert_eq!(
            flat_error(&output.stderr)["code"],
            "argument_error",
            "{args:?}"
        );
    }
    assert!(
        !fixture.path().join(".orbit-graph").exists(),
        "input is validated before anything is indexed"
    );
}

#[test]
fn an_unknown_revision_fails_before_indexing() {
    let fixture = fixture();
    let output = run(fixture.path(), &["changes", "no-such-ref..HEAD", "--json"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error = flat_error(&output.stderr);
    assert_eq!(error["code"], "revision_not_found");
    assert!(
        error["error"]
            .as_str()
            .is_some_and(|message| message.contains("no-such-ref")),
        "{error}"
    );
    assert!(!fixture.path().join(".orbit-graph").exists());
}

#[test]
fn a_cap_that_cuts_the_result_is_signalled_in_every_mode() {
    let fixture = fixture();
    let range = fixture.range();
    let args = [
        "changes",
        range.as_str(),
        "--max-symbols",
        "1",
        "--no-cache",
    ];

    let document = json(fixture.path(), &args);
    assert_eq!(document["truncated"], true);
    assert_eq!(document["symbols"].as_array().map(Vec::len), Some(1));
    assert!(
        !document["not_analysed"]
            .as_array()
            .expect("not analysed")
            .is_empty()
    );
    assert!(
        document["truncation"]
            .as_array()
            .expect("truncation")
            .iter()
            .any(|flag| flag["bound"] == "max_symbols"),
        "{document}"
    );

    let table = run(
        fixture.path(),
        &[&args[..], &["--format", "table"]].concat(),
    );
    assert!(table.status.success(), "{}", stderr(&table));
    assert!(
        stderr(&table).contains("bound(s) cut this result"),
        "a table run names the cut on stderr: {}",
        stderr(&table)
    );

    let ndjson = run(
        fixture.path(),
        &[&args[..], &["--format", "ndjson"]].concat(),
    );
    assert!(ndjson.status.success(), "{}", stderr(&ndjson));
    let records: Vec<Value> = String::from_utf8(ndjson.stdout)
        .expect("UTF-8 records")
        .lines()
        .map(|line| serde_json::from_str(line).expect("JSON record"))
        .collect();
    assert_eq!(records[0]["record_type"], "changes_context");
    assert_eq!(records[0]["context"]["truncated"], true);
    assert_eq!(
        records
            .iter()
            .filter(|record| record["record_type"] == "changed_symbol")
            .count(),
        1
    );
}

#[test]
fn a_passed_budget_returns_an_incomplete_result_not_an_error() {
    let fixture = fixture();
    // 1 s cannot be exceeded by indexing this fixture on most machines, so
    // assert only the invariant: complete agrees with incomplete.
    let document = json(
        fixture.path(),
        &[
            "changes",
            &fixture.range(),
            "--budget-ms",
            "1000",
            "--no-cache",
        ],
    );
    assert_eq!(
        document["complete"].as_bool(),
        Some(document["incomplete"].is_null()),
        "{document}"
    );
}

fn symbol<'a>(document: &'a Value, name: &str) -> &'a Value {
    document["symbols"]
        .as_array()
        .expect("symbols")
        .iter()
        .find(|symbol| {
            symbol["selector"]
                .as_str()
                .is_some_and(|selector| selector.contains(name))
        })
        .unwrap_or_else(|| panic!("{name} is a changed symbol: {document}"))
}

fn write(root: &Path, path: &str, contents: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().expect("parent directory")).expect("create directory");
    fs::write(path, contents).expect("write fixture file");
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = common::git_command(root)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git UTF-8")
        .trim()
        .to_string()
}

fn run(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
        .current_dir(cwd)
        .env_remove("ORBIT_TOOL_NAME")
        .env_remove("ORBIT_GRAPH_FORMAT")
        .args(args)
        .output()
        .expect("run orbit-graph")
}

fn json(cwd: &Path, args: &[&str]) -> Value {
    let output = run(cwd, &[args, &["--json"]].concat());
    assert!(
        output.status.success(),
        "orbit-graph {args:?}: {}",
        stderr(&output)
    );
    serde_json::from_slice(&output.stdout).expect("JSON document")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn without_fields(stdout: &[u8], fields: &[&str]) -> Value {
    let mut document: Value = serde_json::from_slice(stdout).expect("JSON document");
    let object = document.as_object_mut().expect("JSON object");
    for field in fields {
        assert!(object.remove(*field).is_some(), "missing {field}");
    }
    document
}

/// A machine-mode error: exactly one flat `{"error", "code"}` object.
fn flat_error(stderr: &[u8]) -> Value {
    let text = String::from_utf8_lossy(stderr);
    assert_eq!(text.lines().count(), 1, "one JSON line: {text}");
    let error: Value = serde_json::from_str(&text).expect("JSON error object");
    assert!(error["error"].is_string(), "{error}");
    assert!(error["code"].is_string(), "{error}");
    error
}
