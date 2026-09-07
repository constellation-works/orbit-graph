//! Smoke tests that exercise the packaged `orbit-graph` executable.

#![allow(clippy::expect_used)]

use std::fs;
#[cfg(unix)]
use std::io::Read;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::fd::{FromRawFd, OwnedFd};
#[cfg(unix)]
use std::os::unix::net::UnixStream;

use clap::CommandFactory;
use serde_json::Value;
use tempfile::TempDir;

use orbit_graph::cli::Cli;
use orbit_graph::{HistoryIndex, TaskTextAvailability, TemporalStatus};

#[test]
fn real_binary_indexes_and_queries_a_fixture() {
    let fixture = fixture_repository();

    let sync = run_json(fixture.path(), ["sync", "--full"]);
    assert!(
        sync["files_indexed"]
            .as_u64()
            .is_some_and(|count| count >= 1)
    );

    let search = run_json(
        fixture.path(),
        ["search", "helper", "--kind", "symbol", "--limit", "5"],
    );
    assert!(
        search["matches"]
            .as_array()
            .is_some_and(|matches| !matches.is_empty())
    );

    let show = run_json(
        fixture.path(),
        [
            "show",
            "symbol:src/lib.rs#entry:function",
            "--max-bytes",
            "256",
        ],
    );
    assert_eq!(show["metadata"]["file"], "src/lib.rs");
    assert!(
        show["source"]
            .as_str()
            .is_some_and(|source| source.contains("pub fn entry"))
    );

    let refs = run_json(
        fixture.path(),
        [
            "refs",
            "symbol:src/lib.rs#helper:function",
            "--confidence",
            "fuzzy",
            "--kind",
            "call",
        ],
    );
    assert!(refs["refs"].as_array().is_some_and(|refs| !refs.is_empty()));

    let callees = run_json(
        fixture.path(),
        ["callees", "symbol:src/lib.rs#entry:function"],
    );
    assert!(
        callees["callees"]
            .as_array()
            .is_some_and(|calls| !calls.is_empty())
    );

    let db_path = run_json(fixture.path(), ["db-path"]);
    assert!(
        db_path["path"]
            .as_str()
            .is_some_and(|path| path.contains("/.orbit-graph/"))
    );
}

#[test]
fn real_binary_rejects_malformed_selectors_with_json_error() {
    let fixture = fixture_repository();
    let human = run(fixture.path(), ["show", "not-a-selector"]);
    assert_eq!(human.status.code(), Some(1));
    assert!(human.stdout.is_empty());
    assert!(String::from_utf8_lossy(&human.stderr).contains("selectors must start with"));

    let output = run_explicit_json(fixture.path(), ["show", "not-a-selector"]);

    assert!(!output.status.success());
    let error: Value = serde_json::from_slice(&output.stderr).expect("JSON error payload");
    assert_eq!(error["error"]["code"], "selector_parse_error");
    assert!(
        error["error"]["message"]
            .as_str()
            .is_some_and(|message| { message.contains("selectors must start with") })
    );
}

#[test]
fn real_binary_help_succeeds() {
    let fixture = fixture_repository();
    let output = run(fixture.path(), ["--help"]);

    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("Usage: orbit-graph [OPTIONS] <COMMAND>"));
    for heading in [
        "Explore code:",
        "Follow relationships:",
        "Recommendations and history:",
        "Index and utilities:",
    ] {
        assert!(help.contains(heading), "missing help heading: {heading}");
    }
    for command in Cli::command()
        .get_subcommands()
        .map(|command| command.get_name())
    {
        assert!(
            help.lines()
                .any(|line| { line.split_whitespace().next() == Some(command) }),
            "registered command missing from help: {command}"
        );
    }
    for (command, description) in [
        ("overview", "Summarize indexed files and symbols"),
        (
            "search",
            "Search indexed symbols, strings, and configuration keys",
        ),
        ("show", "Show source and metadata for a graph selector"),
        ("refs", "List references to a symbol"),
        ("callees", "List outbound calls from a function or command"),
        ("implementors", "Find implementations of a trait"),
        ("deps", "List source-level imports for a file or directory"),
        (
            "trace",
            "Trace outbound calls from a discovered CLI command handler",
        ),
        ("impact", "Trace the downstream impact of a selector"),
        (
            "recommend",
            "Recommend current change destinations from historical evidence",
        ),
        (
            "history",
            "Inspect and maintain historical delivery evidence",
        ),
        (
            "evaluate",
            "Run leakage-safe chronological recommendation evaluation",
        ),
        ("sync", "Update or rebuild the source graph index"),
        ("db-path", "Print the current graph database path"),
        ("clean", "Remove obsolete graph databases"),
        ("version", "Print crate and extractor versions"),
    ] {
        assert!(
            help.lines().any(|line| {
                line.trim_start().starts_with(command) && line.contains(description)
            }),
            "missing command description: {command}"
        );
    }
    assert!(help.contains("orbit-graph <COMMAND> --help"));
    assert!(output.stderr.is_empty());
    assert!(!help.contains('\u{1b}'));

    let bare = run(fixture.path(), []);
    assert!(bare.status.success());
    assert_eq!(bare.stdout, output.stdout);
    assert!(bare.stderr.is_empty());

    let help_subcommand = run(fixture.path(), ["help"]);
    assert!(help_subcommand.status.success());
    assert_eq!(help_subcommand.stdout, output.stdout);
    assert!(help_subcommand.stderr.is_empty());

    for env in [[("NO_COLOR", "1")], [("TERM", "dumb")]] {
        let plain = run_with_env(fixture.path(), ["--help"], &env);
        assert!(plain.status.success());
        assert!(!String::from_utf8_lossy(&plain.stdout).contains('\u{1b}'));
        assert!(plain.stderr.is_empty());
    }

    for args in [["overview", "--help"], ["history", "--help"]] {
        let command_help = run(fixture.path(), args);
        assert!(command_help.status.success());
        assert!(String::from_utf8_lossy(&command_help.stdout).contains("Usage: orbit-graph"));
        assert!(command_help.stderr.is_empty());
    }

    let root_mode_help = run(fixture.path(), ["--format", "json", "--help"]);
    let nested_mode_help = run(fixture.path(), ["--format", "json", "refs", "--help"]);
    for explicit_mode_help in [root_mode_help, nested_mode_help] {
        assert!(explicit_mode_help.status.success());
        assert!(String::from_utf8_lossy(&explicit_mode_help.stdout).contains("Usage:"));
        assert!(explicit_mode_help.stderr.is_empty());
    }
}

#[test]
fn registered_command_inventory_matches_the_compatibility_matrix() {
    const MATRIX_PATHS: &[&str] = &[
        "sync",
        "history",
        "history import",
        "history sync",
        "history status",
        "history rebuild",
        "recommend",
        "evaluate",
        "search",
        "show",
        "refs",
        "callees",
        "impact",
        "trace",
        "overview",
        "implementors",
        "deps",
        "db-path",
        "clean",
        "version",
    ];

    fn collect_paths(command: &clap::Command, prefix: &str, paths: &mut Vec<String>) {
        for subcommand in command.get_subcommands() {
            let path = if prefix.is_empty() {
                subcommand.get_name().to_owned()
            } else {
                format!("{prefix} {}", subcommand.get_name())
            };
            paths.push(path.clone());
            collect_paths(subcommand, path.as_str(), paths);
        }
    }

    let mut registered = Vec::new();
    collect_paths(&Cli::command(), "", &mut registered);
    registered.sort();
    let mut matrix = MATRIX_PATHS
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    matrix.sort();
    assert_eq!(
        matrix, registered,
        "update the executable compatibility matrix"
    );
}

#[test]
fn real_binary_unknown_command_keeps_json_error_protocol() {
    let fixture = fixture_repository();
    let output = run_explicit_json(fixture.path(), ["not-a-command"]);

    assert!(!output.status.success());
    let error: Value = serde_json::from_slice(&output.stderr).expect("JSON error payload");
    assert_eq!(error["error"]["code"], "argument_error");
}

#[test]
fn real_binary_usage_errors_are_human_by_default_and_machine_readable_on_request() {
    let fixture = fixture_repository();

    for args in [["refs"], ["trace"]] {
        let human = run(fixture.path(), args);
        assert_eq!(human.status.code(), Some(2));
        assert!(human.stdout.is_empty());
        let diagnostic = String::from_utf8_lossy(&human.stderr);
        assert!(diagnostic.contains("required arguments"), "{diagnostic}");
        assert!(diagnostic.contains("Usage: orbit-graph"), "{diagnostic}");
        assert!(
            !diagnostic.contains("\\n"),
            "escaped diagnostic: {diagnostic}"
        );

        let json = run_explicit_json(fixture.path(), args);
        assert_eq!(json.status.code(), Some(2));
        assert!(json.stdout.is_empty());
        let envelope: Value = serde_json::from_slice(&json.stderr).expect("structured usage error");
        assert_eq!(envelope["error"]["code"], "argument_error");
        assert!(
            envelope["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("Usage: orbit-graph"))
        );
    }

    let ndjson = run(fixture.path(), ["--format", "ndjson", "refs"]);
    assert_eq!(ndjson.status.code(), Some(2));
    let envelope: Value = serde_json::from_slice(&ndjson.stderr).expect("NDJSON error envelope");
    assert_eq!(envelope["error"]["code"], "argument_error");

    let namespace = run(fixture.path(), ["history"]);
    assert_eq!(namespace.status.code(), Some(2));
    assert!(namespace.stdout.is_empty());
    let help = String::from_utf8_lossy(&namespace.stderr);
    assert!(help.contains("Usage: orbit-graph history"));
    for subcommand in ["import", "sync", "status", "rebuild"] {
        assert!(help.contains(subcommand), "missing {subcommand} in {help}");
    }
}

#[test]
fn real_binary_resolves_environment_modes_and_overview_format_without_ambiguity() {
    let fixture = fixture_repository();

    let environment_json = run_with_env(
        fixture.path(),
        ["version"],
        &[("ORBIT_GRAPH_FORMAT", "json")],
    );
    assert!(environment_json.status.success());
    assert_eq!(
        environment_json
            .stdout
            .iter()
            .filter(|byte| **byte == b'\n')
            .count(),
        1
    );
    let version: Value =
        serde_json::from_slice(&environment_json.stdout).expect("environment JSON mode");
    assert!(version["crate_version"].is_string());

    let explicit_table = run_with_env(
        fixture.path(),
        ["--format", "table", "version"],
        &[("ORBIT_GRAPH_FORMAT", "json")],
    );
    assert!(explicit_table.status.success());
    assert!(
        String::from_utf8_lossy(&explicit_table.stdout).starts_with("CRATE VERSION"),
        "explicit mode did not outrank environment: {}",
        String::from_utf8_lossy(&explicit_table.stdout)
    );

    let ndjson = run(fixture.path(), ["version", "--format", "ndjson"]);
    assert!(ndjson.status.success());
    assert_eq!(
        ndjson.stdout.iter().filter(|byte| **byte == b'\n').count(),
        1
    );
    serde_json::from_slice::<Value>(&ndjson.stdout).expect("one NDJSON detail record");

    let _ = run_json(fixture.path(), ["sync", "--full"]);
    let overview = run(
        fixture.path(),
        ["--format", "json", "overview", "--format", "full"],
    );
    assert!(
        overview.status.success(),
        "{}",
        String::from_utf8_lossy(&overview.stderr)
    );
    let overview: Value = serde_json::from_slice(&overview.stdout).expect("overview JSON");
    assert!(
        overview["files"]
            .as_array()
            .is_some_and(|files| !files.is_empty())
    );

    for environment in [[("NO_COLOR", "1")], [("TERM", "dumb")]] {
        let plain = run_with_env(
            fixture.path(),
            ["--format", "table", "version"],
            &environment,
        );
        assert!(plain.status.success());
        assert!(!String::from_utf8_lossy(&plain.stdout).contains('\u{1b}'));
    }
}

#[test]
fn real_binary_renders_exploration_views_and_lossless_record_units() {
    let fixture = fixture_repository();
    let _ = run_json(fixture.path(), ["sync", "--full"]);

    let commands: &[(&[&str], &[&str])] = &[
        (
            &["overview", "--format", "full"],
            &["FILES", "SYMBOLS", "src/lib.rs"],
        ),
        (&["search", "helper"], &["KIND", "MATCH", "PATH", "helper"]),
        (
            &["show", "symbol:src/lib.rs#entry:function"],
            &["KIND", "FILE", "Source:", "pub fn entry"],
        ),
        (
            &[
                "refs",
                "symbol:src/lib.rs#helper:function",
                "--confidence",
                "fuzzy",
            ],
            &["RECORD", "FILE", "CONFIDENCE", "src/lib.rs"],
        ),
        (
            &["callees", "symbol:src/lib.rs#entry:function"],
            &["TARGET", "CALL NAME", "helper"],
        ),
        (
            &["implementors", "symbol:src/lib.rs#Renderer:trait"],
            &["TYPE", "TRAIT", "Human", "Renderer"],
        ),
        (
            &["deps", "file:src/lib.rs"],
            &["FROM FILE", "TARGET PATH", "std::fmt"],
        ),
        (
            &["trace", "command:ship", "--depth", "2"],
            &["DEPTH", "TRAVERSAL", "ship", "helper"],
        ),
        (
            &[
                "impact",
                "symbol:src/lib.rs#helper:function",
                "--depth",
                "2",
                "--confidence",
                "fuzzy",
            ],
            &["SET", "DISTANCE", "SYMBOL", "entry"],
        ),
    ];
    for (args, expected) in commands {
        let mut table_args = vec!["--format", "table"];
        table_args.extend_from_slice(args);
        let output = Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
            .current_dir(fixture.path())
            .args(&table_args)
            .output()
            .expect("run human exploration view");
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let human = String::from_utf8(output.stdout).expect("human view is UTF-8");
        assert!(!human.trim_start().starts_with('{'), "{args:?}: {human}");
        for needle in *expected {
            assert!(
                human.contains(needle),
                "{args:?} missing {needle:?}: {human}"
            );
        }

        let mut json_args = vec!["--format", "json"];
        json_args.extend_from_slice(args);
        let json = Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
            .current_dir(fixture.path())
            .args(&json_args)
            .output()
            .expect("run exploration JSON");
        assert!(json.status.success(), "{args:?}");
        let document =
            serde_json::from_slice::<Value>(&json.stdout).expect("lossless JSON document");

        let mut ndjson_args = vec!["--format", "ndjson"];
        ndjson_args.extend_from_slice(args);
        let ndjson = Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
            .current_dir(fixture.path())
            .args(&ndjson_args)
            .output()
            .expect("run exploration NDJSON");
        assert!(ndjson.status.success(), "{args:?}");
        let records = parse_ndjson(&ndjson.stdout);
        assert!(!records.is_empty(), "{args:?} has no NDJSON record unit");
        assert_exploration_record_units(args[0], &document, &records);
    }

    let plain = run(fixture.path(), ["search", "unicode_helper"]);
    assert!(plain.status.success());
    let plain = String::from_utf8(plain.stdout).expect("plain view is UTF-8");
    assert!(!plain.starts_with("KIND"));
    assert_eq!(plain.lines().count(), 1);
    assert_eq!(plain.trim_end().split('\t').count(), 4);
    assert!(plain.contains("界界"));
    assert!(plain.contains("e\u{301}_very_long_component_name.rs"));

    for args in [
        vec!["search", "definitely_absent"],
        vec!["callees", "symbol:src/lib.rs#helper:function"],
        vec!["implementors", "symbol:src/lib.rs#Missing:trait"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
            .current_dir(fixture.path())
            .args(args)
            .output()
            .expect("run empty exploration view");
        assert!(output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
    }
}

#[cfg(unix)]
#[test]
fn real_binary_adapts_unicode_paths_to_narrow_terminals() {
    let fixture = fixture_repository();
    let _ = run_json(fixture.path(), ["sync", "--full"]);

    let (wide, wide_stderr) = run_in_pty(fixture.path(), &["search", "unicode_helper"], 64);
    assert!(wide.contains('…'), "{wide}");
    assert!(wide.contains("界"), "{wide}");
    assert_eq!(wide.lines().count(), 2, "{wide}");
    assert!(wide_stderr.is_empty(), "{wide_stderr}");

    let (narrow, narrow_stderr) = run_in_pty(fixture.path(), &["search", "unicode_helper"], 20);
    assert_eq!(narrow.lines().count(), 2, "{narrow}");
    assert!(narrow_stderr.contains("omitted columns"), "{narrow_stderr}");
}

#[cfg(unix)]
#[test]
fn real_binary_treats_a_closed_stdout_pipe_as_success() {
    let fixture = fixture_repository();
    let (reader, writer) = UnixStream::pair().expect("create stdout pipe");
    drop(reader);
    let writer: OwnedFd = writer.into();
    let output = Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
        .current_dir(fixture.path())
        .args(["--format", "json", "version"])
        .stdout(Stdio::from(writer))
        .output()
        .expect("run with closed stdout pipe");

    assert!(
        output.status.success(),
        "broken pipe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn real_binary_recommends_in_file_and_symbol_modes_and_validates_top_k() {
    let fixture = fixture_repository();
    let _ = run_json(fixture.path(), ["sync", "--full"]);

    let files = run_json(
        fixture.path(),
        [
            "recommend",
            "--query",
            "helper",
            "--level",
            "file",
            "--limit",
            "1",
        ],
    );
    assert_eq!(
        files["resolved_target_revision"],
        git_stdout(fixture.path(), ["rev-parse", "HEAD"])
    );
    assert_eq!(files["recommendations"].as_array().map(Vec::len), Some(1));
    assert_eq!(files["recommendations"][0]["selector"], "file:src/lib.rs");
    assert!(files["source_freshness"]["status"].is_string());

    let symbols = run_json(
        fixture.path(),
        [
            "recommend",
            "--query",
            "helper",
            "--level",
            "symbol",
            "--limit",
            "2",
        ],
    );
    assert!(symbols["recommendations"].as_array().is_some_and(|values| {
        values.iter().any(|value| {
            value["selector"]
                .as_str()
                .is_some_and(|selector| selector.contains("#helper:function"))
        })
    }));

    let bad_limit = run_explicit_json(
        fixture.path(),
        ["recommend", "--query", "helper", "--limit", "0"],
    );
    assert!(!bad_limit.status.success());
    let error: Value = serde_json::from_slice(&bad_limit.stderr).expect("JSON error");
    assert_eq!(error["error"]["code"], "graph_error");

    let both = run_explicit_json(
        fixture.path(),
        ["recommend", "--query", "helper", "--task-id", "TASK-1"],
    );
    assert!(!both.status.success());
    let error: Value = serde_json::from_slice(&both.stderr).expect("JSON error");
    assert_eq!(error["error"]["code"], "argument_error");
}

#[test]
fn real_binary_renders_recommendation_and_index_views_with_complete_record_boundaries() {
    let fixture = fixture_repository();
    let sync = run(fixture.path(), ["sync", "--full"]);
    assert!(sync.status.success());
    let sync_fields = String::from_utf8(sync.stdout)
        .expect("sync plain UTF-8")
        .trim_end()
        .split('\t')
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert_eq!(sync_fields.len(), 4);
    assert!(
        sync_fields
            .iter()
            .all(|field| field.parse::<u128>().is_ok())
    );
    let sync_ndjson = run(fixture.path(), ["sync", "--format", "ndjson"]);
    let sync_ndjson = parse_ndjson(&sync_ndjson.stdout);
    assert_eq!(sync_ndjson.len(), 1);
    assert!(sync_ndjson[0]["files_indexed"].is_number());

    let plain = run(
        fixture.path(),
        [
            "recommend",
            "--query",
            "helper",
            "--level",
            "file",
            "--limit",
            "1",
        ],
    );
    assert!(plain.status.success());
    assert!(plain.stderr.is_empty());
    let fields = String::from_utf8(plain.stdout)
        .expect("recommendation plain UTF-8")
        .trim_end()
        .split('\t')
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert_eq!(fields.len(), 5);
    assert_eq!(fields[0], "1");
    assert_eq!(fields[1], "file:src/lib.rs");
    assert!(fields[2].parse::<f64>().is_ok());
    assert!(fields[3].contains("current destination text overlaps the query"));
    assert!(fields[3].contains("cold_start"));
    assert!(fields[4].contains("unavailable"));

    let empty = run(
        fixture.path(),
        ["recommend", "--query", "definitely_absent_destination"],
    );
    assert!(empty.status.success());
    assert!(empty.stdout.is_empty());
    let empty_diagnostic = String::from_utf8_lossy(&empty.stderr);
    assert!(empty_diagnostic.contains("no recommendations"));
    assert!(empty_diagnostic.contains("cold_start"));

    let table = run(
        fixture.path(),
        [
            "--format",
            "table",
            "recommend",
            "--query",
            "helper",
            "--limit",
            "1",
        ],
    );
    assert!(table.status.success());
    let table = String::from_utf8_lossy(&table.stdout);
    assert!(table.starts_with("RANK"));
    assert!(table.contains("SELECTOR"));
    assert!(table.contains("EVIDENCE"));
    assert!(!table.contains('\u{1b}'));

    let ndjson = run(
        fixture.path(),
        [
            "--format",
            "ndjson",
            "recommend",
            "--query",
            "helper",
            "--limit",
            "1",
        ],
    );
    assert!(ndjson.status.success());
    let records = parse_ndjson(&ndjson.stdout);
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["record_type"], "recommendation_context");
    assert!(records[0]["context"]["source_freshness"].is_object());
    assert!(records[0]["context"]["fallbacks"].is_array());
    assert!(records[0]["context"].get("recommendations").is_none());
    assert_eq!(records[1]["record_type"], "recommendation");
    assert_eq!(records[1]["recommendation"]["rank"], 1);
    assert!(records[1]["recommendation"]["reasons"].is_array());

    let version = run(fixture.path(), ["version"]);
    assert!(version.status.success());
    let version = String::from_utf8(version.stdout).expect("version plain UTF-8");
    assert_eq!(version.trim_end().split('\t').count(), 2);

    let db_path = run(fixture.path(), ["db-path"]);
    assert!(db_path.status.success());
    let db_path = String::from_utf8(db_path.stdout).expect("db-path plain UTF-8");
    assert_eq!(db_path.trim_end().split('\t').count(), 3);
    let db_path_ndjson = run(fixture.path(), ["db-path", "--format", "ndjson"]);
    let db_path_ndjson = parse_ndjson(&db_path_ndjson.stdout);
    assert_eq!(db_path_ndjson.len(), 1);
    assert!(db_path_ndjson[0]["path"].is_string());

    let old_db = fixture.path().join(".orbit-graph/main.1.db");
    fs::write(&old_db, b"stale").expect("write obsolete database");
    let clean = run(fixture.path(), ["--format", "ndjson", "clean"]);
    assert!(clean.status.success());
    let clean = parse_ndjson(&clean.stdout);
    assert_eq!(clean[0]["record_type"], "clean_context");
    assert_eq!(clean[1]["record_type"], "deleted_database");
    assert_eq!(clean[1]["path"], old_db.to_string_lossy().as_ref());

    let other_old_db = fixture.path().join(".orbit-graph/main.2.db");
    fs::write(&other_old_db, b"stale").expect("write second obsolete database");
    let clean_human = run(fixture.path(), ["clean"]);
    assert!(clean_human.status.success());
    let clean_human = String::from_utf8_lossy(&clean_human.stdout);
    assert!(clean_human.contains("\t1\n"));
    assert!(clean_human.contains(other_old_db.to_string_lossy().as_ref()));
}

#[test]
fn real_binary_expands_corpus_cochanges_without_temporal_or_duplicate_leakage() {
    let fixture = TempDir::new().expect("create recommendation fixture");
    run_git(fixture.path(), ["init", "-b", "main"]);
    run_git(
        fixture.path(),
        ["config", "user.email", "graph@example.invalid"],
    );
    run_git(fixture.path(), ["config", "user.name", "Graph Test"]);
    fs::write(fixture.path().join("a.rs"), "pub fn alpha() -> i32 { 0 }\n").expect("write a");
    fs::write(fixture.path().join("b.rs"), "pub fn beta() -> i32 { 0 }\n").expect("write b");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "base"]);

    let before_one = git_stdout(fixture.path(), ["rev-parse", "HEAD"]);
    fs::write(fixture.path().join("a.rs"), "pub fn alpha() -> i32 { 1 }\n").expect("edit a one");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "one"]);
    let after_one = git_stdout(fixture.path(), ["rev-parse", "HEAD"]);

    fs::write(fixture.path().join("a.rs"), "pub fn alpha() -> i32 { 2 }\n").expect("edit a two");
    fs::write(fixture.path().join("b.rs"), "pub fn beta() -> i32 { 2 }\n").expect("edit b two");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "two"]);
    let after_two = git_stdout(fixture.path(), ["rev-parse", "HEAD"]);

    fs::write(fixture.path().join("a.rs"), "pub fn alpha() -> i32 { 3 }\n").expect("edit a three");
    fs::write(fixture.path().join("b.rs"), "pub fn beta() -> i32 { 3 }\n").expect("edit b three");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "three"]);
    let after_three = git_stdout(fixture.path(), ["rev-parse", "HEAD"]);

    fs::write(fixture.path().join("b.rs"), "pub fn beta() -> i32 { 4 }\n").expect("edit b future");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "future"]);
    let after_future = git_stdout(fixture.path(), ["rev-parse", "HEAD"]);

    import_cli_delivery(
        fixture.path(),
        &before_one,
        &after_one,
        "D1",
        "TASK-1",
        "needle",
        "2000-01-01T00:00:01Z",
        "1999-12-31T00:00:00Z",
    );
    import_cli_delivery(
        fixture.path(),
        &after_one,
        &after_two,
        "D2",
        "TASK-2",
        "unrelated",
        "2000-01-01T00:00:02Z",
        "1999-12-31T00:00:00Z",
    );
    import_cli_delivery(
        fixture.path(),
        &after_one,
        &after_two,
        "D2-ALIAS",
        "TASK-2",
        "unrelated",
        "2000-01-01T00:00:02Z",
        "1999-12-31T00:00:00Z",
    );
    import_cli_delivery(
        fixture.path(),
        &after_two,
        &after_three,
        "D3",
        "TASK-3",
        "unrelated",
        "2000-01-01T00:00:03Z",
        "1999-12-31T00:00:00Z",
    );
    import_cli_delivery(
        fixture.path(),
        &after_three,
        &after_future,
        "D-FUTURE",
        "PENDING",
        "futuresecret",
        "2001-01-01T00:00:00.900Z",
        "2000-12-31T00:00:00Z",
    );

    let cutoff = "2001-01-01T00:00:00.100Z";
    let result = run_json(
        fixture.path(),
        [
            "recommend",
            "--query",
            "needle",
            "--level",
            "file",
            "--cutoff",
            cutoff,
        ],
    );
    let beta = result["recommendations"]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["selector"] == "file:b.rs"))
        .expect("co-change destination b");
    assert_eq!(beta["association"]["support"], 2);
    assert_eq!(beta["association"]["source_count"], 3);
    assert_eq!(beta["association"]["destination_count"], 2);
    assert_eq!(beta["association"]["lift"], 1.0);
    assert_eq!(beta["counts"]["eligible_history_deliveries"], 3);
    assert!(beta["reasons"].as_array().is_some_and(|reasons| {
        reasons.iter().any(|reason| {
            reason["kind"] == "directional_cochange"
                && reason["explanation"]
                    .as_str()
                    .is_some_and(|text| text.contains("D2") && text.contains("D2-ALIAS"))
        })
    }));

    let future = run_json(
        fixture.path(),
        ["recommend", "--query", "futuresecret", "--cutoff", cutoff],
    );
    assert!(
        future["recommendations"]
            .as_array()
            .is_some_and(Vec::is_empty)
    );
    let future_text = run_json(
        fixture.path(),
        ["recommend", "--query", "futuretext", "--cutoff", cutoff],
    );
    assert!(
        future_text["recommendations"]
            .as_array()
            .is_some_and(Vec::is_empty)
    );

    let _ = run_json(
        fixture.path(),
        ["history", "sync", "--branch", "main", "--limit", "20"],
    );
    let snapshot_path = fixture.path().join("pending-snapshot.json");
    let snapshot = serde_json::json!({
        "task_id": "PENDING",
        "title": "needle",
        "description": "pending work before any delivery",
        "acceptance_criteria": ["recommend likely files"],
        "source": {"system": "task_service", "record_id": "PENDING@1"},
        "created_at": {
            "status": "known", "timestamp": "1999-12-30T00:00:00Z",
            "source": {"system": "task_service"}
        },
        "snapshot_available_at": {
            "status": "known", "timestamp": "1999-12-31T00:00:00Z",
            "source": {"system": "task_service", "record_id": "PENDING@1"}
        },
        "text_availability": "known_pre_execution",
        "captured_at": "1999-12-31T00:00:00Z"
    });
    fs::write(
        &snapshot_path,
        serde_json::to_vec(&snapshot).expect("encode snapshot"),
    )
    .expect("write snapshot");
    let snapshot_arg = snapshot_path.to_string_lossy();
    for level in ["file", "symbol"] {
        let pending = run_json(
            fixture.path(),
            [
                "recommend",
                "--task-id",
                "PENDING",
                "--task-snapshot",
                snapshot_arg.as_ref(),
                "--level",
                level,
            ],
        );
        assert_eq!(
            pending["recommendations"][0]["counts"]["eligible_history_deliveries"],
            3
        );
        assert!(pending["recommendations"].as_array().is_some_and(|items| {
            items.iter().all(|item| {
                item["supporting_delivery_ids"]
                    .as_array()
                    .is_some_and(|ids| ids.iter().all(|id| id != "D-FUTURE"))
            })
        }));
        assert!(pending["recommendations"].as_array().is_some_and(|items| {
            items.iter().any(|item| {
                item["selector"]
                    .as_str()
                    .is_some_and(|selector| selector.contains("a.rs"))
            })
        }));
    }
}

#[test]
fn real_binary_live_default_cutoff_accepts_new_pending_snapshot() {
    let fixture = fixture_repository();
    let old_date = "2000-01-01T00:00:00Z";
    let amended = Command::new("git")
        .current_dir(fixture.path())
        .env("GIT_AUTHOR_DATE", old_date)
        .env("GIT_COMMITTER_DATE", old_date)
        .args(["commit", "--amend", "--no-edit"])
        .output()
        .expect("amend old target commit");
    assert!(
        amended.status.success(),
        "git amend failed: {}",
        String::from_utf8_lossy(&amended.stderr)
    );

    let captured_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("current time after Unix epoch")
        .as_secs();
    let snapshot_path = fixture.path().join("live-pending-snapshot.json");
    let snapshot = serde_json::json!({
        "task_id": "PENDING-LIVE",
        "title": "helper",
        "description": "new pending work captured after the old target commit",
        "acceptance_criteria": ["recommend the helper destination"],
        "source": {"system": "task_service", "record_id": "PENDING-LIVE@1"},
        "created_at": {
            "status": "known", "timestamp": "unix:0",
            "source": {"system": "task_service"}
        },
        "snapshot_available_at": {
            "status": "known", "timestamp": format!("unix:{captured_seconds}"),
            "source": {"system": "task_service", "record_id": "PENDING-LIVE@1"}
        },
        "text_availability": "known_pre_execution",
        "captured_at": format!("unix:{captured_seconds}")
    });
    fs::write(
        &snapshot_path,
        serde_json::to_vec(&snapshot).expect("encode live snapshot"),
    )
    .expect("write live snapshot");

    let before_future = git_stdout(fixture.path(), ["rev-parse", "HEAD"]);
    fs::write(
        fixture.path().join("src/lib.rs"),
        "pub fn helper() -> i32 { 2 }\n\npub fn entry() -> i32 { helper() }\n",
    )
    .expect("edit future delivery");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "pending delivery"]);
    let after_future = git_stdout(fixture.path(), ["rev-parse", "HEAD"]);
    import_cli_delivery(
        fixture.path(),
        &before_future,
        &after_future,
        "D-PENDING-LIVE",
        "PENDING-LIVE",
        "helper",
        "2001-01-01T00:00:00Z",
        "2000-01-01T00:00:00Z",
    );

    let snapshot_arg = snapshot_path.to_string_lossy();
    for level in ["file", "symbol"] {
        let result = run_json(
            fixture.path(),
            [
                "recommend",
                "--task-id",
                "PENDING-LIVE",
                "--task-snapshot",
                snapshot_arg.as_ref(),
                "--level",
                level,
            ],
        );
        let effective_cutoff = result["effective_cutoff"]
            .as_str()
            .expect("effective cutoff");
        assert!(
            effective_cutoff.contains('T') && effective_cutoff.contains('.'),
            "live cutoff should preserve subsecond observation time: {effective_cutoff}"
        );
        assert!(
            result["recommendations"]
                .as_array()
                .is_some_and(|items| !items.is_empty()),
            "live recommendation should resolve in {level} mode: {result}"
        );
        assert!(result["recommendations"].as_array().is_some_and(|items| {
            items.iter().all(|item| {
                item["supporting_delivery_ids"]
                    .as_array()
                    .is_some_and(|ids| ids.iter().all(|id| id != "D-PENDING-LIVE"))
            })
        }));
    }
}

#[test]
fn real_binary_filters_deleted_destinations_from_stale_structure() {
    let fixture = TempDir::new().expect("create stale structure fixture");
    run_git(fixture.path(), ["init", "-b", "main"]);
    run_git(
        fixture.path(),
        ["config", "user.email", "graph@example.invalid"],
    );
    run_git(fixture.path(), ["config", "user.name", "Graph Test"]);
    fs::write(
        fixture.path().join("caller.rs"),
        "mod removed;\npub fn keeper() -> i32 { removed::gone() }\n",
    )
    .expect("write caller");
    fs::write(
        fixture.path().join("removed.rs"),
        "pub fn gone() -> i32 { 1 }\n",
    )
    .expect("write removed");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "base"]);
    let _ = run_json(fixture.path(), ["sync", "--full"]);

    fs::write(
        fixture.path().join("caller.rs"),
        "pub fn keeper() -> i32 { 1 }\n",
    )
    .expect("remove call");
    fs::remove_file(fixture.path().join("removed.rs")).expect("delete callee");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "delete callee"]);

    for level in ["file", "symbol"] {
        let result = run_json(
            fixture.path(),
            ["recommend", "--query", "keeper", "--level", level],
        );
        assert!(result["recommendations"].as_array().is_some_and(|items| {
            items.iter().all(|item| {
                !item["selector"].as_str().is_some_and(|selector| {
                    selector.contains("removed.rs") || selector.contains("gone")
                })
            })
        }));
        assert!(result["fallbacks"].as_array().is_some_and(|items| {
            items
                .iter()
                .any(|item| item["kind"] == "stale_structure_excluded")
        }));
    }
}

#[test]
fn real_binary_imports_syncs_reports_and_rebuilds_history() {
    let fixture = fixture_repository();
    let before = git_stdout(fixture.path(), ["rev-parse", "HEAD"]);
    fs::write(
        fixture.path().join("src/lib.rs"),
        "pub fn helper() -> i32 { 2 }\n\npub fn entry() -> i32 { helper() }\n",
    )
    .expect("edit fixture source");
    run_git(fixture.path(), ["add", "."]);
    run_git(
        fixture.path(),
        ["commit", "-m", "deliver update\n\nTask-Id: ORB-CLI"],
    );
    let after = git_stdout(fixture.path(), ["rev-parse", "HEAD"]);
    let repository = fixture
        .path()
        .canonicalize()
        .expect("canonical fixture")
        .to_string_lossy()
        .into_owned();
    let envelope = serde_json::json!({
        "schema_version": 2,
        "repository": repository,
        "landing_branch": "main",
        "before_revision": before,
        "after_revision": after,
        "delivery_id": "verified-cli-1",
        "evidence": "verified_delivery",
        "source": {"system": "cli_test", "record_id": "delivery-1"},
        "delivered_at": {
            "status": "known", "timestamp": "2026-09-07T00:00:00Z",
            "source": {"system": "delivery_service", "record_id": "landed-1"}
        },
        "captured_at": "2026-09-07T00:01:00Z",
        "tasks": [{
            "task_id": "ORB-CLI",
            "title": "Exercise history CLI",
            "description": "Verify the public import contract",
            "acceptance_criteria": ["CLI operations return JSON"],
            "source": {"system": "cli_test", "record_id": "ORB-CLI"},
            "created_at": {
                "status": "known", "timestamp": "2026-09-06T20:00:00Z",
                "source": {"system": "task_service", "record_id": "created-1"}
            },
            "snapshot_available_at": {
                "status": "known", "timestamp": "2026-09-06T21:00:00Z",
                "source": {"system": "task_service", "record_id": "snapshot-7"}
            },
            "text_availability": "known_pre_execution",
            "captured_at": "2026-09-07T00:00:30Z"
        }]
    });
    let envelope_path = fixture.path().join("delivery.json");
    fs::write(
        envelope_path.as_path(),
        serde_json::to_vec(&envelope).expect("encode envelope"),
    )
    .expect("write envelope");
    let envelope_arg = envelope_path.to_string_lossy();

    let imported = run_json(
        fixture.path(),
        ["history", "import", "--input", envelope_arg.as_ref()],
    );
    assert_eq!(imported["inserted"], true);
    let stored = HistoryIndex::open(fixture.path(), "main")
        .expect("open imported history")
        .deliveries()
        .expect("read imported delivery");
    let stored_delivery = &stored[0].delivery;
    assert_eq!(stored_delivery.captured_at, "2026-09-07T00:01:00Z");
    assert_eq!(stored_delivery.delivered_at.status, TemporalStatus::Known);
    assert_eq!(
        stored_delivery.delivered_at.timestamp.as_deref(),
        Some("2026-09-07T00:00:00Z")
    );
    assert_eq!(
        stored_delivery.tasks[0].text_availability,
        TaskTextAvailability::KnownPreExecution
    );
    assert_eq!(
        stored_delivery.tasks[0].created_at.timestamp.as_deref(),
        Some("2026-09-06T20:00:00Z")
    );
    assert_eq!(
        stored_delivery.tasks[0]
            .snapshot_available_at
            .source
            .record_id
            .as_deref(),
        Some("snapshot-7")
    );
    let duplicate = run_json(
        fixture.path(),
        ["history", "import", "--input", envelope_arg.as_ref()],
    );
    assert_eq!(duplicate["inserted"], false);
    let imported_human = run(
        fixture.path(),
        ["history", "import", "--input", envelope_arg.as_ref()],
    );
    assert!(imported_human.status.success());
    let imported_human = String::from_utf8_lossy(&imported_human.stdout);
    assert!(imported_human.contains("operation\timport"));
    assert!(imported_human.contains("delivery\tverified-cli-1"));
    assert!(imported_human.contains("inserted\tfalse"));
    let imported_ndjson = run(
        fixture.path(),
        [
            "history",
            "import",
            "--input",
            envelope_arg.as_ref(),
            "--format",
            "ndjson",
        ],
    );
    let imported_ndjson = parse_ndjson(&imported_ndjson.stdout);
    assert_eq!(imported_ndjson.len(), 1);
    assert_eq!(imported_ndjson[0]["delivery_id"], "verified-cli-1");

    let recommended = run_json(
        fixture.path(),
        [
            "recommend",
            "--query",
            "history CLI import contract",
            "--level",
            "symbol",
            "--limit",
            "3",
        ],
    );
    assert!(
        recommended["recommendations"]
            .as_array()
            .is_some_and(|items| {
                items.iter().any(|item| {
                    item["selector"]
                        .as_str()
                        .is_some_and(|selector| selector.contains("src/lib.rs#helper:function"))
                        && item["supporting_delivery_ids"]
                            .as_array()
                            .is_some_and(|ids| ids.iter().any(|id| id == "verified-cli-1"))
                })
            })
    );
    let recommended_human = run(
        fixture.path(),
        [
            "recommend",
            "--query",
            "history CLI import contract",
            "--level",
            "symbol",
            "--limit",
            "3",
        ],
    );
    assert!(recommended_human.status.success());
    let recommended_human = String::from_utf8_lossy(&recommended_human.stdout);
    assert!(recommended_human.contains("src/lib.rs#helper:function"));
    assert!(recommended_human.contains("history"));

    let synced = run_json(
        fixture.path(),
        ["history", "sync", "--branch", "main", "--limit", "10"],
    );
    assert_eq!(synced["complete"], true);
    let synced_human = run(
        fixture.path(),
        ["history", "sync", "--branch", "main", "--limit", "10"],
    );
    assert!(synced_human.status.success());
    let synced_human = String::from_utf8_lossy(&synced_human.stdout);
    assert!(synced_human.contains("operation\tsync"));
    assert!(synced_human.contains("commits indexed\t"));
    let synced_ndjson = run(
        fixture.path(),
        ["history", "sync", "--branch", "main", "--format", "ndjson"],
    );
    let synced_ndjson = parse_ndjson(&synced_ndjson.stdout);
    assert_eq!(synced_ndjson.len(), 1);
    assert!(synced_ndjson[0]["complete"].is_boolean());
    let status = run_json(fixture.path(), ["history", "status", "--branch", "main"]);
    assert_eq!(status["schema_version"], 3);
    assert_eq!(status["extractor_version"], 2);
    assert_eq!(status["verified_deliveries"], 1);
    assert_eq!(status["git_only_deliveries"], 1);
    assert_eq!(status["task_associations"], 2);
    let status_human = run(fixture.path(), ["history", "status", "--branch", "main"]);
    assert!(status_human.status.success());
    let status_human = String::from_utf8_lossy(&status_human.stdout);
    assert!(status_human.contains("verified deliveries\t1"));
    assert!(status_human.contains("complete\ttrue"));
    let status_ndjson = run(
        fixture.path(),
        [
            "history", "status", "--branch", "main", "--format", "ndjson",
        ],
    );
    let status_ndjson = parse_ndjson(&status_ndjson.stdout);
    assert_eq!(status_ndjson.len(), 1);
    assert!(status_ndjson[0]["verified_deliveries"].is_number());

    let rebuilt = run_json(
        fixture.path(),
        ["history", "rebuild", "--branch", "main", "--limit", "10"],
    );
    assert_eq!(rebuilt["removed_deliveries"], 2);
    assert_eq!(rebuilt["sync"]["deliveries_inserted"], 1);
    let rebuilt_human = run(
        fixture.path(),
        ["history", "rebuild", "--branch", "main", "--limit", "10"],
    );
    assert!(rebuilt_human.status.success());
    assert!(String::from_utf8_lossy(&rebuilt_human.stdout).contains("operation\trebuild"));
    let rebuilt_ndjson = run(
        fixture.path(),
        [
            "history", "rebuild", "--branch", "main", "--format", "ndjson",
        ],
    );
    let rebuilt_ndjson = parse_ndjson(&rebuilt_ndjson.stdout);
    assert_eq!(rebuilt_ndjson.len(), 1);
    assert!(rebuilt_ndjson[0]["sync"].is_object());
}

#[test]
fn real_binary_rejects_history_repository_mismatch_with_json_error() {
    let fixture = fixture_repository();
    fs::write(
        fixture.path().join("src/lib.rs"),
        "pub fn helper() -> i32 { 2 }\n",
    )
    .expect("edit fixture source");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "second"]);
    let after = git_stdout(fixture.path(), ["rev-parse", "HEAD"]);
    let before = git_stdout(fixture.path(), ["rev-parse", "HEAD~1"]);
    let envelope_path = fixture.path().join("bad-delivery.json");
    let envelope = serde_json::json!({
        "schema_version": 2, "repository": "wrong", "landing_branch": "main",
        "before_revision": before, "after_revision": after, "delivery_id": "bad",
        "evidence": "verified_delivery", "source": {"system": "test"},
        "delivered_at": {"status": "unavailable", "source": {"system": "test"}},
        "captured_at": "2026-09-07T00:00:00Z", "tasks": []
    });
    fs::write(
        &envelope_path,
        serde_json::to_vec(&envelope).expect("encode"),
    )
    .expect("write bad envelope");
    let envelope_arg = envelope_path.to_string_lossy();
    let output = run_explicit_json(
        fixture.path(),
        ["history", "import", "--input", envelope_arg.as_ref()],
    );
    assert!(!output.status.success());
    let error: Value = serde_json::from_slice(&output.stderr).expect("JSON error");
    assert_eq!(error["error"]["code"], "graph_error");
    assert!(
        error["details"]
            .as_str()
            .is_some_and(|details| details.contains("mismatch"))
    );
}

#[test]
fn real_binary_rejects_invalid_history_timestamps_without_partial_import() {
    let fixture = fixture_repository();
    fs::write(
        fixture.path().join("src/lib.rs"),
        "pub fn helper() -> i32 { 2 }\n",
    )
    .expect("edit fixture source");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "second"]);
    let after = git_stdout(fixture.path(), ["rev-parse", "HEAD"]);
    let before = git_stdout(fixture.path(), ["rev-parse", "HEAD~1"]);
    let repository = fixture
        .path()
        .canonicalize()
        .expect("canonical fixture")
        .to_string_lossy()
        .into_owned();
    let envelope = serde_json::json!({
        "schema_version": 2, "repository": repository, "landing_branch": "main",
        "before_revision": before, "after_revision": after, "delivery_id": "bad-time",
        "evidence": "verified_delivery", "source": {"system": "test"},
        "delivered_at": {
            "status": "known", "timestamp": "not-a-time",
            "source": {"system": "test_clock"}
        },
        "captured_at": "2026-09-07T00:00:00Z", "tasks": []
    });
    let envelope_path = fixture.path().join("bad-time.json");
    fs::write(
        &envelope_path,
        serde_json::to_vec(&envelope).expect("encode invalid envelope"),
    )
    .expect("write invalid envelope");
    let envelope_arg = envelope_path.to_string_lossy();
    let output = run_explicit_json(
        fixture.path(),
        ["history", "import", "--input", envelope_arg.as_ref()],
    );
    assert!(!output.status.success());
    let error: Value = serde_json::from_slice(&output.stderr).expect("JSON error");
    assert!(error.to_string().contains("timestamp"), "{error}");
    let status = run_json(fixture.path(), ["history", "status", "--branch", "main"]);
    assert_eq!(status["deliveries"], 0);
}

#[test]
fn real_binary_rejects_signed_rfc3339_components_without_changing_history_state() {
    let fixture = fixture_repository();
    fs::write(
        fixture.path().join("src/lib.rs"),
        "pub fn helper() -> i32 { 2 }\n",
    )
    .expect("edit fixture source");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "second"]);
    let after = git_stdout(fixture.path(), ["rev-parse", "HEAD"]);
    let before = git_stdout(fixture.path(), ["rev-parse", "HEAD~1"]);
    let repository = fixture
        .path()
        .canonicalize()
        .expect("canonical fixture")
        .to_string_lossy()
        .into_owned();
    let valid = serde_json::json!({
        "schema_version": 2, "repository": repository, "landing_branch": "main",
        "before_revision": before, "after_revision": after,
        "delivery_id": "valid-time", "evidence": "verified_delivery",
        "source": {"system": "test"},
        "delivered_at": {
            "status": "known", "timestamp": "unix:1788739200",
            "source": {"system": "test_clock"}
        },
        "captured_at": "2026-09-07T04:00:00.123+02:30", "tasks": []
    });
    let envelope_path = fixture.path().join("time.json");
    fs::write(
        &envelope_path,
        serde_json::to_vec(&valid).expect("encode valid envelope"),
    )
    .expect("write valid envelope");
    let envelope_arg = envelope_path.to_string_lossy();
    let imported = run_json(
        fixture.path(),
        ["history", "import", "--input", envelope_arg.as_ref()],
    );
    assert_eq!(imported["inserted"], true);

    for (index, captured_at) in [
        "2026-+9-07T04:00:00Z",
        "2026-09-+7T04:00:00Z",
        "2026-09-07T+4:00:00Z",
        "2026-09-07T04:+0:00Z",
        "2026-09-07T04:00:+0Z",
        "2026-09-07T04:00:00+9:00",
        "2026-09-07T04:00:00+00:+9",
    ]
    .into_iter()
    .enumerate()
    {
        let mut invalid = valid.clone();
        invalid["delivery_id"] = serde_json::json!(format!("invalid-time-{index}"));
        invalid["captured_at"] = serde_json::json!(captured_at);
        fs::write(
            &envelope_path,
            serde_json::to_vec(&invalid).expect("encode invalid envelope"),
        )
        .expect("write invalid envelope");
        let output = run(
            fixture.path(),
            ["history", "import", "--input", envelope_arg.as_ref()],
        );
        assert!(!output.status.success(), "accepted {captured_at}");
    }

    let status = run_json(fixture.path(), ["history", "status", "--branch", "main"]);
    assert_eq!(status["deliveries"], 1);
    assert_eq!(status["cursor"], Value::Null);
}

fn run_json<const N: usize>(cwd: &Path, args: [&str; N]) -> Value {
    let output = run_explicit_json(cwd, args);
    assert!(
        output.status.success(),
        "orbit-graph failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON command output")
}

fn parse_ndjson(bytes: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(|line| serde_json::from_str(line).expect("NDJSON record"))
        .collect()
}

fn assert_exploration_record_units(command: &str, document: &Value, records: &[Value]) {
    let array = |field: &str| document[field].as_array().cloned().unwrap_or_default();
    match command {
        "search" => assert_eq!(records, array("matches")),
        "show" => assert_eq!(records, std::slice::from_ref(document)),
        "callees" => assert_eq!(records, array("callees")),
        "overview" => {
            let files = array("files");
            assert_eq!(records.len(), files.len() + 1);
            assert_eq!(records[0]["record_type"], "overview_context");
            assert_eq!(
                records[0]["context"]["total_files"],
                document["total_files"]
            );
            for (record, file) in records[1..].iter().zip(files) {
                assert_eq!(record["record_type"], "overview_file");
                assert_eq!(record["file"], file);
            }
        }
        "refs" => {
            let refs = array("refs");
            let relations = array("relations");
            assert_eq!(records[0]["record_type"], "refs_context");
            assert_eq!(records[0]["context"]["target"], document["target"]);
            assert_eq!(
                records
                    .iter()
                    .filter(|record| record["record_type"] == "reference")
                    .count(),
                refs.len()
            );
            assert_eq!(
                records
                    .iter()
                    .filter(|record| record["record_type"] == "relation")
                    .count(),
                relations.len()
            );
        }
        "implementors" => {
            let implementors = array("implementors");
            assert_eq!(records.len(), implementors.len() + 1);
            assert_eq!(records[0]["context"]["trait_name"], document["trait_name"]);
            for (record, implementor) in records[1..].iter().zip(implementors) {
                assert_eq!(record["implementor"], implementor);
            }
        }
        "deps" => {
            let imports = array("imports");
            assert_eq!(records.len(), imports.len() + 1);
            assert_eq!(records[0]["context"]["scope"], document["scope"]);
            for (record, import) in records[1..].iter().zip(imports) {
                assert_eq!(record["import"], import);
            }
        }
        "trace" => {
            assert_eq!(
                records[0]["context"]["visited_nodes"],
                document["visited_nodes"]
            );
            assert_eq!(records[1]["root"], document["root"]);
        }
        "impact" => {
            let touched = array("touched");
            assert_eq!(
                records[0]["context"]["visited_nodes"],
                document["visited_nodes"]
            );
            assert_eq!(
                records
                    .iter()
                    .filter(|record| record["record_type"] == "impact")
                    .count(),
                touched.len()
            );
        }
        _ => panic!("unexpected exploration command {command}"),
    }
}

#[allow(clippy::too_many_arguments)]
fn import_cli_delivery(
    root: &Path,
    before: &str,
    after: &str,
    delivery_id: &str,
    task_id: &str,
    title: &str,
    delivered_at: &str,
    snapshot_available_at: &str,
) {
    let repository = root
        .canonicalize()
        .expect("canonical fixture")
        .to_string_lossy()
        .into_owned();
    let mut tasks = vec![serde_json::json!({
        "task_id": task_id,
        "title": title,
        "description": "neutral delivery description",
        "acceptance_criteria": [],
        "source": {"system": "task_service", "record_id": task_id},
        "created_at": {
            "status": "known", "timestamp": "1999-01-01T00:00:00Z",
            "source": {"system": "task_service", "record_id": task_id}
        },
        "snapshot_available_at": {
            "status": "known", "timestamp": snapshot_available_at,
            "source": {"system": "task_service", "record_id": task_id}
        },
        "text_availability": "known_pre_execution",
        "captured_at": "2002-01-01T00:00:00Z"
    })];
    if delivery_id == "D1" {
        tasks.push(serde_json::json!({
            "task_id": "FUTURE-TEXT",
            "title": "futuretext",
            "description": "must not be visible before its snapshot cutoff",
            "acceptance_criteria": [],
            "source": {"system": "task_service", "record_id": "FUTURE-TEXT"},
            "created_at": {
                "status": "known", "timestamp": "1999-01-01T00:00:00Z",
                "source": {"system": "task_service"}
            },
            "snapshot_available_at": {
                "status": "known", "timestamp": "2001-01-01T00:00:00.900Z",
                "source": {"system": "task_service"}
            },
            "text_availability": "known_pre_execution",
            "captured_at": "2002-01-01T00:00:00Z"
        }));
    }
    let envelope = serde_json::json!({
        "schema_version": 2,
        "repository": repository,
        "landing_branch": "main",
        "before_revision": before,
        "after_revision": after,
        "delivery_id": delivery_id,
        "evidence": "verified_delivery",
        "source": {"system": "cli_test", "record_id": delivery_id},
        "delivered_at": {
            "status": "known", "timestamp": delivered_at,
            "source": {"system": "delivery_service", "record_id": delivery_id}
        },
        "captured_at": "2002-01-01T00:00:00Z",
        "tasks": tasks
    });
    let path = root.join(format!("{delivery_id}.json"));
    fs::write(
        &path,
        serde_json::to_vec(&envelope).expect("encode delivery"),
    )
    .expect("write delivery");
    let path_arg = path.to_string_lossy();
    let imported = run_json(root, ["history", "import", "--input", path_arg.as_ref()]);
    assert_eq!(imported["inserted"], true);
}

fn run<const N: usize>(cwd: &Path, args: [&str; N]) -> Output {
    run_with_env(cwd, args, &[])
}

fn run_explicit_json<const N: usize>(cwd: &Path, args: [&str; N]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
        .current_dir(cwd)
        .args(["--format", "json"])
        .args(args)
        .output()
        .expect("run orbit-graph in JSON mode")
}

fn run_with_env<const N: usize>(cwd: &Path, args: [&str; N], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orbit-graph"));
    command.current_dir(cwd).args(args);
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().expect("run orbit-graph")
}

fn fixture_repository() -> TempDir {
    let fixture = TempDir::new().expect("create fixture repository");
    run_git(fixture.path(), ["init", "-b", "main"]);
    run_git(
        fixture.path(),
        ["config", "user.email", "graph@example.invalid"],
    );
    run_git(fixture.path(), ["config", "user.name", "Graph Test"]);
    fs::create_dir_all(fixture.path().join("src")).expect("create source directory");
    fs::write(
        fixture.path().join("src/lib.rs"),
        "use std::fmt::Debug;\n\npub trait Renderer {}\npub struct Human;\nimpl Renderer for Human {}\n\npub fn helper() -> i32 { 1 }\n\npub fn entry() -> i32 { helper() }\n\npub fn caller() -> i32 { entry() }\n",
    )
    .expect("write fixture source");
    let unicode_dir = fixture.path().join("src/界界");
    fs::create_dir_all(&unicode_dir).expect("create Unicode source directory");
    fs::write(
        unicode_dir.join("e\u{301}_very_long_component_name.rs"),
        "pub fn unicode_helper() -> &'static str { \"café\" }\n",
    )
    .expect("write Unicode fixture source");
    fs::write(
        fixture.path().join("src/cli.py"),
        "import click\n\n@click.command()\ndef ship():\n    helper()\n\ndef helper():\n    return 'ok'\n",
    )
    .expect("write command fixture source");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "fixture"]);
    fixture
}

#[cfg(unix)]
fn run_in_pty(cwd: &Path, args: &[&str], columns: u16) -> (String, String) {
    let mut master = -1;
    let mut slave = -1;
    let mut size = libc::winsize {
        ws_row: 24,
        ws_col: columns,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: openpty initializes both descriptors on success; no termios is supplied.
    let result = unsafe {
        libc::openpty(
            &raw mut master,
            &raw mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut size,
        )
    };
    assert_eq!(result, 0, "openpty failed");
    // SAFETY: successful openpty returned newly owned file descriptors.
    let master = unsafe { OwnedFd::from_raw_fd(master) };
    // SAFETY: successful openpty returned newly owned file descriptors.
    let slave = unsafe { OwnedFd::from_raw_fd(slave) };
    let output = Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
        .current_dir(cwd)
        .args(args)
        .stdout(Stdio::from(slave))
        .output()
        .expect("run orbit-graph in PTY");
    assert!(
        output.status.success(),
        "PTY command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut reader = std::fs::File::from(master);
    let mut stdout = Vec::new();
    loop {
        let mut buffer = [0_u8; 4096];
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => stdout.extend_from_slice(&buffer[..count]),
            Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
            Err(error) => panic!("read PTY output: {error}"),
        }
    }
    (
        String::from_utf8(stdout)
            .expect("PTY output is UTF-8")
            .replace("\r\n", "\n"),
        String::from_utf8(output.stderr).expect("stderr is UTF-8"),
    )
}

fn run_git<const N: usize>(cwd: &Path, args: [&str; N]) {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout<const N: usize>(cwd: &Path, args: [&str; N]) -> String {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("Git output is UTF-8")
        .trim()
        .to_string()
}
