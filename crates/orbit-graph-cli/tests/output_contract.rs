//! The CLI output contract (STD-01), driven through the built `orbit-graph`
//! executable: `--json`, one meaning for `--format`, printed tokens accepted
//! back as input, signalled truncation, flat machine errors, `null` for absent
//! values, and the plugin-protocol boundary.

#![allow(clippy::expect_used)]
#![allow(
    clippy::disallowed_methods,
    reason = "fixtures are written with fs::write; clippy.toml bans it only from shipped code"
)]

mod common;

use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::fd::{FromRawFd, OwnedFd};

use serde_json::Value;
use tempfile::TempDir;

use orbit_graph::{
    HistoryIndex, IMPACT_NODE_CAP, RecommendationLevel, RecommendationVariant, RefConfidence,
    RefKind, SearchKind, TRACE_NODE_CAP,
};

const HELPER: &str = "symbol:src/lib.rs#helper:function";

#[test]
fn json_flag_is_byte_identical_to_format_json_for_every_command() {
    let fixture = synced_fixture();
    let commands: &[&[&str]] = &[
        &["overview"],
        &["overview", "--detail", "full"],
        &["search", "helper"],
        &["show", HELPER],
        &["refs", HELPER],
        &["callees", "symbol:src/lib.rs#entry:function"],
        &["implementors", "symbol:src/lib.rs#Renderer:trait"],
        &["deps", "file:src/lib.rs"],
        &["trace", "ship"],
        &["impact", HELPER],
        &["recommend", "--query", "helper"],
        &["history", "status", "--branch", "main"],
        &["sync"],
        &["db-path"],
        &["clean"],
        &["version"],
    ];
    for args in commands {
        let format_json = run(fixture.path(), &[&["--format", "json"], *args].concat());
        let root_json = run(fixture.path(), &[&["--json"], *args].concat());
        let trailing_json = run(fixture.path(), &[*args, &["--json"]].concat());
        for output in [&format_json, &root_json, &trailing_json] {
            assert!(
                output.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        // Two fields record when the command ran; everything else is fixed.
        let volatile: &[&str] = match args[0] {
            "recommend" => &["effective_cutoff"],
            "sync" => &["duration_ms"],
            _ => &[],
        };
        if volatile.is_empty() {
            assert_eq!(root_json.stdout, format_json.stdout, "{args:?}");
            assert_eq!(trailing_json.stdout, format_json.stdout, "{args:?}");
        } else {
            let expected = without_fields(&format_json.stdout, volatile);
            assert_eq!(without_fields(&root_json.stdout, volatile), expected);
            assert_eq!(without_fields(&trailing_json.stdout, volatile), expected);
        }
    }

    for args in [
        &["--json", "--format", "table", "version"][..],
        &["--json", "version", "--format", "ndjson"],
        &["version", "--format", "table", "--json"],
    ] {
        let conflict = run(fixture.path(), args);
        assert_eq!(conflict.status.code(), Some(2), "{args:?}");
        assert!(conflict.stdout.is_empty(), "{args:?}");
        let error = flat_error(&conflict.stderr);
        assert_eq!(error["code"], "argument_error");
        assert!(
            error["error"]
                .as_str()
                .is_some_and(|message| message.contains("--json conflicts with --format")),
            "{error}"
        );
    }
    let same_mode = run(fixture.path(), &["--json", "version", "--format", "json"]);
    assert!(same_mode.status.success());
}

#[test]
fn overview_format_is_an_output_mode_and_detail_has_its_own_flag() {
    let fixture = synced_fixture();

    let summary = run(fixture.path(), &["overview", "--format", "json"]);
    assert!(
        summary.status.success(),
        "{}",
        String::from_utf8_lossy(&summary.stderr)
    );
    assert!(summary.stderr.is_empty());
    let summary: Value = serde_json::from_slice(&summary.stdout).expect("overview JSON");
    assert_eq!(summary["format"], "summary");

    let detail = run(fixture.path(), &["overview", "--detail", "full", "--json"]);
    assert!(detail.status.success());
    assert!(detail.stderr.is_empty());
    let detail: Value = serde_json::from_slice(&detail.stdout).expect("overview JSON");
    assert_eq!(detail["format"], "full");

    // The former detail spelling still works for one release, with a warning.
    for args in [
        &["overview", "--format", "full", "--json"][..],
        &["--format", "json", "overview", "--format", "full"],
    ] {
        let legacy = run(fixture.path(), args);
        assert!(legacy.status.success(), "{args:?}");
        let warning = String::from_utf8_lossy(&legacy.stderr);
        assert!(
            warning.starts_with("warning: `overview --format summary|full` is deprecated")
                && warning.contains("--detail"),
            "{warning}"
        );
        let legacy: Value = serde_json::from_slice(&legacy.stdout).expect("overview JSON");
        assert_eq!(legacy, detail, "{args:?}");
    }
    let human = run(fixture.path(), &["overview", "--format", "full"]);
    assert!(human.status.success());
    assert!(String::from_utf8_lossy(&human.stderr).contains("deprecated"));
    assert!(String::from_utf8_lossy(&human.stdout).contains("src/lib.rs"));

    let conflict = run(
        fixture.path(),
        &["overview", "--detail", "summary", "--format", "full"],
    );
    assert_eq!(conflict.status.code(), Some(2));
    assert!(conflict.stdout.is_empty());
    assert!(String::from_utf8_lossy(&conflict.stderr).contains("pass only --detail"));

    // The detail values are not output modes anywhere else.
    let elsewhere = run(fixture.path(), &["search", "helper", "--format", "full"]);
    assert_eq!(elsewhere.status.code(), Some(2));
}

#[test]
fn every_printed_variant_level_confidence_and_kind_token_is_accepted_back() {
    let fixture = synced_fixture();

    // Every token the library can print, plus every token this fixture prints.
    let mut variants = tokens([
        RecommendationVariant::Combined,
        RecommendationVariant::TaskSearchOnly,
        RecommendationVariant::GraphOnly,
        RecommendationVariant::Frequency,
    ]);
    let mut levels = tokens([RecommendationLevel::File, RecommendationLevel::Symbol]);
    let mut confidences = tokens([
        RefConfidence::Exact,
        RefConfidence::ImportResolved,
        RefConfidence::SameModule,
        RefConfidence::FuzzyName,
    ]);
    let mut ref_kinds = tokens([
        RefKind::Call,
        RefKind::Type,
        RefKind::Use,
        RefKind::TraitBound,
        RefKind::Impl,
        RefKind::Extends,
        RefKind::Implements,
    ]);
    let mut search_kinds = tokens([SearchKind::Symbol, SearchKind::String, SearchKind::Config]);

    for variant in variants.clone() {
        let document = json(
            fixture.path(),
            &["recommend", "--query", "helper", "--variant", &variant],
        );
        variants.push(string(&document["variant"]));
        levels.push(string(&document["level"]));
    }
    let refs = json(fixture.path(), &["refs", HELPER, "--confidence", "fuzzy"]);
    for entry in refs["refs"].as_array().into_iter().flatten() {
        confidences.push(string(&entry["confidence"]));
        ref_kinds.push(string(&entry["kind"]));
    }
    let trace = json(fixture.path(), &["trace", "ship", "--confidence", "fuzzy"]);
    for child in trace["root"]["children"].as_array().into_iter().flatten() {
        confidences.push(string(&child["confidence"]));
    }
    let search = json(fixture.path(), &["search", "helper"]);
    for entry in search["matches"].as_array().into_iter().flatten() {
        search_kinds.push(string(&entry["kind"]));
    }

    for variant in &variants {
        accepted(
            fixture.path(),
            &["recommend", "--query", "helper", "--variant", variant],
        );
    }
    for level in &levels {
        accepted(
            fixture.path(),
            &["recommend", "--query", "helper", "--level", level],
        );
    }
    for confidence in &confidences {
        accepted(
            fixture.path(),
            &["refs", HELPER, "--confidence", confidence],
        );
        accepted(
            fixture.path(),
            &["impact", HELPER, "--confidence", confidence],
        );
        accepted(
            fixture.path(),
            &["trace", "ship", "--confidence", confidence],
        );
    }
    for kind in &ref_kinds {
        accepted(fixture.path(), &["refs", HELPER, "--kind", kind]);
    }
    for kind in &search_kinds {
        accepted(fixture.path(), &["search", "helper", "--kind", kind]);
    }

    // The earlier kebab-case spellings stay accepted.
    for variant in ["task-search-only", "graph-only"] {
        let document = json(
            fixture.path(),
            &["recommend", "--query", "helper", "--variant", variant],
        );
        assert_eq!(document["variant"], variant.replace('-', "_"));
    }
}

#[test]
fn capped_search_and_recommend_report_total_truncated_and_a_notice_in_every_mode() {
    let fixture = synced_fixture();

    let complete = run(fixture.path(), &["--format", "json", "search", "helper"]);
    assert!(complete.stderr.is_empty());
    let complete: Value = serde_json::from_slice(&complete.stdout).expect("search JSON");
    let matches = complete["matches"].as_array().expect("matches").len();
    assert!(matches >= 2, "{complete}");
    assert_eq!(complete["total"], matches);
    assert_eq!(complete["truncated"], false);

    let capped = run(
        fixture.path(),
        &["--format", "json", "search", "helper", "--limit", "1"],
    );
    assert!(capped.status.success());
    let document: Value = serde_json::from_slice(&capped.stdout).expect("search JSON");
    assert_eq!(document["matches"].as_array().map(Vec::len), Some(1));
    assert_eq!(document["truncated"], true);
    assert_eq!(document["total"], Value::Null);
    assert_one_notice(&capped.stderr, "showing 1 of more than 1 search matches");
    for mode in ["table", "ndjson", "auto"] {
        let output = run(
            fixture.path(),
            &["--format", mode, "search", "helper", "--limit", "1"],
        );
        assert!(output.status.success(), "{mode}");
        assert_one_notice(&output.stderr, "showing 1 of more than 1 search matches");
    }

    let recommend = ["recommend", "--query", "helper", "--limit", "1"];
    let capped = run(
        fixture.path(),
        &[&["--format", "json"], &recommend[..]].concat(),
    );
    assert!(capped.status.success());
    let document: Value = serde_json::from_slice(&capped.stdout).expect("recommend JSON");
    let total = document["total"].as_u64().expect("recommendation total");
    assert!(total > 1, "{document}");
    assert_eq!(document["truncated"], true);
    assert_eq!(
        document["recommendations"].as_array().map(Vec::len),
        Some(1)
    );
    let expected = format!("showing 1 of {total} recommendations");
    assert_one_notice(&capped.stderr, &expected);
    for mode in ["table", "ndjson"] {
        let output = run(
            fixture.path(),
            &[&["--format", mode], &recommend[..]].concat(),
        );
        assert!(output.status.success(), "{mode}");
        assert_one_notice(&output.stderr, &expected);
    }
    let ndjson = run(
        fixture.path(),
        &[&["--format", "ndjson"], &recommend[..]].concat(),
    );
    let context: Value = serde_json::from_str(
        String::from_utf8_lossy(&ndjson.stdout)
            .lines()
            .next()
            .expect("context record"),
    )
    .expect("NDJSON context record");
    assert_eq!(context["context"]["truncated"], true);
    assert_eq!(context["context"]["total"], total);

    for args in [
        &["search", ""][..],
        &["search", "   "],
        &["search", "helper", "--limit", "0"],
    ] {
        for mode in [&[][..], &["--format", "json"]] {
            let output = run(fixture.path(), &[mode, args].concat());
            assert_eq!(output.status.code(), Some(2), "{mode:?} {args:?}");
            assert!(output.stdout.is_empty(), "{args:?}");
        }
    }
}

#[test]
fn node_capped_impact_and_trace_report_truncation_on_stderr() {
    let fixture = TempDir::new().expect("create fixture repository");
    let fanout = IMPACT_NODE_CAP.max(TRACE_NODE_CAP) + 5;
    let leaves = (0..fanout)
        .map(|index| format!("pub fn leaf{index}() {{}}\n"))
        .collect::<String>();
    let calls = (0..fanout)
        .map(|index| format!("leaf{index}(); "))
        .collect::<String>();
    write(
        fixture.path(),
        "src/lib.rs",
        &format!("{leaves}pub fn hub() {{ {calls}}}\n"),
    );
    let python_leaves = (0..fanout)
        .map(|index| format!("def leaf{index}():\n    return {index}\n\n"))
        .collect::<String>();
    let python_calls = (0..fanout)
        .map(|index| format!("    leaf{index}()\n"))
        .collect::<String>();
    write(
        fixture.path(),
        "src/cli.py",
        &format!("import click\n\n{python_leaves}@click.command()\ndef ship():\n{python_calls}"),
    );
    commit(fixture.path());
    json(fixture.path(), &["sync", "--full"]);

    let impact = run(
        fixture.path(),
        &[
            "--format",
            "json",
            "impact",
            "symbol:src/lib.rs#hub:function",
            "--direction",
            "outbound",
        ],
    );
    assert!(impact.status.success());
    let document: Value = serde_json::from_slice(&impact.stdout).expect("impact JSON");
    assert_eq!(document["truncated"], true, "{document}");
    assert_eq!(document["total"], Value::Null);
    assert_one_notice(&impact.stderr, "impacted nodes");

    let trace = run(fixture.path(), &["--format", "json", "trace", "ship"]);
    assert!(trace.status.success());
    let document: Value = serde_json::from_slice(&trace.stdout).expect("trace JSON");
    assert_eq!(document["truncated"], true, "{document}");
    assert_eq!(document["total"], Value::Null);
    assert_one_notice(&trace.stderr, "trace nodes");
}

#[test]
fn orbit_tool_name_with_arguments_is_a_usage_error_with_empty_stdout() {
    let fixture = synced_fixture();
    for mode in [&[][..], &["--format", "json"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
            .current_dir(fixture.path())
            .env("ORBIT_TOOL_NAME", "orbit.graph.version")
            .args(mode)
            .args(["search", "foo"])
            .stdin(Stdio::null())
            .output()
            .expect("run orbit-graph");
        assert_eq!(output.status.code(), Some(2), "{mode:?}");
        assert!(output.stdout.is_empty(), "{mode:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("ORBIT_TOOL_NAME"), "{stderr}");
        if mode.is_empty() {
            assert!(stderr.starts_with("error: "), "{stderr}");
        } else {
            assert_eq!(flat_error(&output.stderr)["code"], "argument_error");
        }
    }
}

#[test]
fn bare_invocation_with_an_open_piped_stdin_prints_help_without_reading_it() {
    let fixture = synced_fixture();
    let mut child = Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
        .current_dir(fixture.path())
        .env_remove("ORBIT_TOOL_NAME")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn orbit-graph");
    // Keep the write end open for the whole wait, as `sleep 5 | orbit-graph`
    // does: a binary that reads stdin to EOF never finishes.
    let stdin = child.stdin.take().expect("child stdin");
    let status = wait_bounded(&mut child, Duration::from_secs(20));
    drop(stdin);
    assert!(status.success());
    let mut stdout = String::new();
    child
        .stdout
        .take()
        .expect("child stdout")
        .read_to_string(&mut stdout)
        .expect("read help");
    assert!(
        stdout.contains("Usage: orbit-graph [OPTIONS] <COMMAND>"),
        "{stdout}"
    );
}

#[test]
fn unresolved_show_fails_with_not_found_and_errors_are_flat_objects_on_stderr() {
    let fixture = synced_fixture();

    let missing = run(
        fixture.path(),
        &[
            "--format",
            "json",
            "show",
            "symbol:src/lib.rs#missing:function",
        ],
    );
    assert_eq!(missing.status.code(), Some(1));
    assert!(missing.stdout.is_empty());
    let error = flat_error(&missing.stderr);
    assert_eq!(error["code"], "not_found");
    assert!(
        error["error"]
            .as_str()
            .is_some_and(|message| message.contains("symbol:src/lib.rs#missing:function")),
        "{error}"
    );

    let human = run(
        fixture.path(),
        &["show", "symbol:src/lib.rs#missing:function"],
    );
    assert_eq!(human.status.code(), Some(1));
    assert!(human.stdout.is_empty());
    assert!(String::from_utf8_lossy(&human.stderr).starts_with("error: selector "));

    for mode in [&["--format", "json"][..], &["--format", "ndjson"]] {
        let usage = run(fixture.path(), &[mode, &["search"]].concat());
        assert_eq!(usage.status.code(), Some(2), "{mode:?}");
        assert!(usage.stdout.is_empty());
        assert_eq!(flat_error(&usage.stderr)["code"], "argument_error");
    }
}

#[test]
fn absent_values_are_null_rather_than_omitted() {
    let fixture = synced_fixture();

    let shown = json(fixture.path(), &["show", "file:src/lib.rs"]);
    assert_eq!(shown["source_encoding"], "utf-8");
    assert!(shown["source"].is_string());
    assert_null(&shown, "source_bytes");
    assert_null(&shown["metadata"], "qualified");

    let overview = json(fixture.path(), &["overview"]);
    assert_null(&overview, "scope");

    let refs = json(fixture.path(), &["refs", HELPER]);
    assert_null(&refs, "fallback");

    let trace = json(fixture.path(), &["trace", "ship"]);
    assert_eq!(trace["truncated"], false);
    assert_eq!(trace["total"], trace["visited_nodes"]);

    let impact = json(fixture.path(), &["impact", HELPER]);
    assert_eq!(impact["direction"], "both");
    assert_eq!(impact["truncated"], false);
    assert!(impact["total"].is_u64(), "{impact}");
    assert_null(&impact, "fallback");
    for entry in impact["touched"].as_array().expect("touched") {
        assert!(entry["origin"].is_string(), "{entry}");
    }

    let recommend = json(fixture.path(), &["recommend", "--query", "helper"]);
    for recommendation in recommend["recommendations"].as_array().expect("list") {
        assert!(
            recommendation.get("fallback_reason").is_some(),
            "{recommendation}"
        );
        assert!(
            recommendation.get("association").is_some(),
            "{recommendation}"
        );
    }

    write(fixture.path(), "src/blob.rs", "");
    fs::write(
        fixture.path().join("src/blob.rs"),
        b"pub fn b() {}\n// \xff\xfe\n",
    )
    .expect("write non-UTF-8 source");
    commit(fixture.path());
    json(fixture.path(), &["sync"]);
    let bytes = json(fixture.path(), &["show", "file:src/blob.rs"]);
    assert_null(&bytes, "source");
    assert_eq!(bytes["source_encoding"], "bytes");
    assert!(bytes["source_bytes"].is_array(), "{bytes}");
    let human = run(fixture.path(), &["show", "file:src/blob.rs"]);
    assert!(human.status.success());
    assert!(String::from_utf8_lossy(&human.stdout).contains("Source is not UTF-8"));
}

#[test]
fn warnings_reach_stderr_without_rust_log() {
    let fixture = fixture_repository();
    write(
        fixture.path(),
        "data/huge.json",
        &format!(
            "{{\"HUGE_MARKER_KEY\": \"{}\"}}\n",
            "a".repeat(4 * 1024 * 1024)
        ),
    );
    let output = Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
        .current_dir(fixture.path())
        .env_remove("RUST_LOG")
        .env_remove("ORBIT_TOOL_NAME")
        .args(["--format", "json", "sync"])
        .output()
        .expect("run orbit-graph");
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("byte cap") && stderr.contains("data/huge.json"),
        "{stderr}"
    );
    assert!(!stderr.contains('\u{1b}'), "{stderr}");
}

#[cfg(unix)]
#[test]
fn help_and_errors_on_a_terminal_carry_no_styling() {
    let fixture = synced_fixture();
    for args in [&["--help"][..], &["search", "--help"]] {
        let stdout = run_on_terminal(fixture.path(), args, &[("CLICOLOR_FORCE", "1")]);
        assert!(stdout.contains("Usage: orbit-graph"), "{stdout}");
        assert!(!stdout.contains('\u{1b}'), "{args:?}: {stdout:?}");
    }
}

// --- helpers -----------------------------------------------------------------

fn fixture_repository() -> TempDir {
    let fixture = TempDir::new().expect("create fixture repository");
    write(
        fixture.path(),
        "src/lib.rs",
        "use std::fmt::Debug;\n\npub trait Renderer {}\npub struct Human;\nimpl Renderer for Human {}\n\npub fn helper() -> i32 { 1 }\n\npub fn entry() -> i32 { helper() }\n\npub fn caller() -> i32 { entry() }\n",
    );
    write(
        fixture.path(),
        "src/cli.py",
        "import click\n\n@click.command()\ndef ship():\n    helper()\n\ndef helper():\n    return 'ok'\n",
    );
    commit(fixture.path());
    fixture
}

/// A committed, synced fixture with an empty history index.
fn synced_fixture() -> TempDir {
    let fixture = fixture_repository();
    json(fixture.path(), &["sync", "--full"]);
    HistoryIndex::open(fixture.path(), "main").expect("initialize an empty history index");
    fixture
}

fn write(root: &Path, path: &str, contents: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().expect("parent directory")).expect("create directory");
    fs::write(path, contents).expect("write fixture file");
}

fn commit(root: &Path) {
    if !root.join(".git").exists() {
        git(root, &["init", "-q", "-b", "main"]);
    }
    git(root, &["add", "."]);
    git(
        root,
        &[
            "-c",
            "user.email=graph@example.invalid",
            "-c",
            "user.name=Graph Test",
            "commit",
            "-q",
            "-m",
            "fixture",
        ],
    );
}

fn git(root: &Path, args: &[&str]) {
    let output = common::git_command(root)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
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

/// Run with `--format json`, which predates `--json`, and parse stdout.
fn json(cwd: &Path, args: &[&str]) -> Value {
    let output = run(cwd, &[&["--format", "json"], args].concat());
    assert!(
        output.status.success(),
        "orbit-graph {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON document")
}

fn accepted(cwd: &Path, args: &[&str]) {
    let output = run(cwd, &[&["--format", "json"], args].concat());
    assert!(
        output.status.success(),
        "a printed token was rejected: orbit-graph {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn tokens<T: serde::Serialize, const N: usize>(values: [T; N]) -> Vec<String> {
    values
        .iter()
        .map(|value| string(&serde_json::to_value(value).expect("serialize token")))
        .collect()
}

fn string(value: &Value) -> String {
    value.as_str().expect("string token").to_owned()
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
    let object = error.as_object().expect("error object");
    assert!(object["error"].is_string(), "{error}");
    assert!(object["code"].is_string(), "{error}");
    assert_eq!(object.len(), 2, "{error}");
    error
}

fn assert_one_notice(stderr: &[u8], expected: &str) {
    let text = String::from_utf8_lossy(stderr);
    let notices = text
        .lines()
        .filter(|line| line.contains(expected))
        .collect::<Vec<_>>();
    assert_eq!(notices.len(), 1, "{expected:?} in {text:?}");
}

fn assert_null(object: &Value, field: &str) {
    assert_eq!(
        object.get(field),
        Some(&Value::Null),
        "{field} must be null, not omitted: {object}"
    );
}

/// Wait for `child`, killing it and failing if it runs past `limit`.
fn wait_bounded(child: &mut Child, limit: Duration) -> std::process::ExitStatus {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("poll orbit-graph") {
            return status;
        }
        if started.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            panic!("orbit-graph did not finish within {limit:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Run with stdout on a pseudo-terminal and return what it printed.
#[cfg(unix)]
fn run_on_terminal(cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
    let mut master = -1;
    let mut slave = -1;
    // SAFETY: openpty initializes both descriptors on success; no termios or
    // window size is supplied.
    let result = unsafe {
        libc::openpty(
            &raw mut master,
            &raw mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(result, 0, "openpty failed");
    // SAFETY: successful openpty returned newly owned file descriptors.
    let master = unsafe { OwnedFd::from_raw_fd(master) };
    // SAFETY: successful openpty returned newly owned file descriptors.
    let slave = unsafe { OwnedFd::from_raw_fd(slave) };
    let mut command = Command::new(env!("CARGO_BIN_EXE_orbit-graph"));
    command
        .current_dir(cwd)
        .env_remove("ORBIT_TOOL_NAME")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(slave))
        .stderr(Stdio::piped());
    for (key, value) in env {
        command.env(key, value);
    }
    let mut child = command.spawn().expect("spawn orbit-graph on a terminal");
    // Drop the parent's slave so EOF/EIO on the master reflects the child only.
    drop(command);
    let mut reader = fs::File::from(master);
    let mut stdout = Vec::new();
    loop {
        let mut buffer = [0_u8; 4096];
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => stdout.extend_from_slice(&buffer[..count]),
            Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
            Err(error) => panic!("read terminal output: {error}"),
        }
    }
    let status = wait_bounded(&mut child, Duration::from_secs(20));
    assert!(status.success());
    String::from_utf8(stdout)
        .expect("terminal output is UTF-8")
        .replace("\r\n", "\n")
}
