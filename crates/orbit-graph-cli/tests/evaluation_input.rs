//! Chronological corpus evaluation through the real CLI and each renderer.

#![allow(clippy::expect_used)]
#![allow(
    clippy::disallowed_methods,
    reason = "fixtures use fs::write; shipped writes use atomic_write"
)]

mod common;

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::{Value, json};
use tempfile::TempDir;

#[test]
fn corpus_input_evaluates_a_prospective_case_in_every_output_mode() {
    let fixture = TempDir::new().expect("create evaluation repository");
    let root = fixture.path();
    git(root, &["init", "-q", "-b", "main"]);
    let repository = "https://example.invalid/graph-evaluation.git";
    git(root, &["remote", "add", "origin", repository]);
    fs::write(root.join("lib.rs"), "pub fn helper() -> i32 { 1 }\n").expect("write base source");
    git(root, &["add", "lib.rs"]);
    git(root, &["commit", "-q", "-m", "base"]);
    let base = git(root, &["rev-parse", "HEAD"]);
    fs::write(root.join("lib.rs"), "pub fn helper() -> i32 { 2 }\n").expect("write changed source");
    git(root, &["commit", "-q", "-am", "change helper"]);
    let head = git(root, &["rev-parse", "HEAD"]);
    let source = json!({"system": "fixture_observation"});
    let task = json!({
        "task_id": "fixture-task", "title": "Change helper", "description": "Update helper's result",
        "acceptance_criteria": ["helper returns the new result"], "source": source,
        "created_at": fact("unix:50"), "snapshot_available_at": fact("unix:100"),
        "text_availability": "known_pre_execution", "captured_at": "unix:100"
    });
    let mut corpus = json!({
        "schema_version": 1, "repository": repository, "landing_branch": "main",
        "source": source, "complete": true, "coverage_note": "one isolated prospective case",
        "k": 1, "training_deliveries": [],
        "cases": [{
            "id": "fixture-case", "target_revision": base, "cutoff": "unix:200",
            "task_snapshot": task, "source": source,
            "held_out_delivery": {
                "schema_version": 2, "repository": repository, "landing_branch": "main",
                "before_revision": base, "after_revision": head, "delivery_id": "fixture-delivery",
                "evidence": "verified_delivery", "source": source,
                "delivered_at": fact("unix:300"), "captured_at": "unix:400", "tasks": [task]
            }
        }]
    });
    let input = root.join("corpus.json");
    fs::write(&input, serde_json::to_vec(&corpus).expect("encode corpus")).expect("write corpus");
    for mode in ["auto", "table", "json", "ndjson"] {
        let output = run(
            root,
            &["evaluate", "--input", "corpus.json", "--format", mode],
        );
        assert!(
            output.status.success(),
            "{mode}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stderr.is_empty(),
            "{mode}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        match mode {
            "json" => {
                let report: Value =
                    serde_json::from_slice(&output.stdout).expect("evaluation JSON");
                assert_eq!(report["coverage"]["cases_evaluated"], 1);
                assert_eq!(report["coverage"]["cases_excluded"], 0);
                assert_eq!(report["cases"][0]["evaluated"], true);
                assert!(
                    report["cases"][0]["exclusions"]
                        .as_array()
                        .expect("exclusions")
                        .is_empty()
                );
                let metrics = report["metrics"].as_array().expect("variant metrics");
                // Nine rankings (five `recommend` variants, two baselines,
                // two combinations) at two levels.
                assert_eq!(metrics.len(), 18);
                assert!(metrics.iter().all(|metric| metric["cases"] == 1));
            }
            "ndjson" => {
                let records = String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .map(|line| serde_json::from_str::<Value>(line).expect("evaluation NDJSON"))
                    .collect::<Vec<_>>();
                // Context, 18 metrics, 32 paired differences (eight
                // rankings × two levels × two metrics), one case.
                assert_eq!(records.len(), 52);
                assert_eq!(records[0]["record_type"], "evaluation_context");
                assert_eq!(records[19]["record_type"], "evaluation_paired_difference");
                assert_eq!(records[51]["record_type"], "evaluation_case");
                assert_eq!(records[51]["case"]["evaluated"], true);
            }
            _ => {
                let text = String::from_utf8_lossy(&output.stdout);
                assert!(text.contains("fixture-case"));
                assert!(text.contains("combined"));
                assert!(!text.contains('\u{1b}'));
            }
        }
        assert!(
            !root.join(".orbit-graph").exists(),
            "evaluation isolates scratch indexes"
        );
    }

    corpus["unexpected_field"] = json!(true);
    fs::write(
        &input,
        serde_json::to_vec(&corpus).expect("encode invalid corpus"),
    )
    .expect("write invalid corpus");
    let invalid = run(root, &["evaluate", "--input", "corpus.json", "--json"]);
    assert_eq!(invalid.status.code(), Some(1));
    assert!(invalid.stdout.is_empty());
    let error: Value = serde_json::from_slice(&invalid.stderr).expect("decode error JSON");
    assert_eq!(error["code"], "invalid_input");
    assert!(
        error["error"]
            .as_str()
            .is_some_and(|message| message.contains("unexpected_field"))
    );
    assert!(!root.join(".orbit-graph").exists());
}

#[test]
fn folds_baselines_grid_and_gates_through_the_real_executable() {
    let fixture = TempDir::new().expect("create evaluation repository");
    let root = fixture.path();
    git(root, &["init", "-q", "-b", "main"]);
    let repository = "https://example.invalid/graph-evaluation-folds.git";
    git(root, &["remote", "add", "origin", repository]);
    fs::write(root.join("other.rs"), "pub fn unrelated_thing() {}\n").expect("write other");
    let mut revisions = Vec::new();
    for value in 0..4 {
        fs::write(
            root.join("lib.rs"),
            format!("pub fn helper_value() -> i32 {{ {value} }}\n"),
        )
        .expect("write source");
        git(root, &["add", "."]);
        git(root, &["commit", "-q", "-m", &format!("value {value}")]);
        revisions.push(git(root, &["rev-parse", "HEAD"]));
    }
    let source = json!({"system": "fixture_observation"});
    let task = |id: &str, title: &str, available: &str| {
        json!({
            "task_id": id, "title": title, "description": "Update helper_value in lib.rs",
            "acceptance_criteria": ["the result changes"], "source": source,
            "created_at": fact("unix:10"), "snapshot_available_at": fact(available),
            "text_availability": "known_pre_execution", "captured_at": available
        })
    };
    let delivery = |id: &str, before: &str, after: &str, at: &str, task: Value| {
        json!({
            "schema_version": 2, "repository": repository, "landing_branch": "main",
            "before_revision": before, "after_revision": after, "delivery_id": id,
            "evidence": "verified_delivery", "source": source,
            "delivered_at": fact(at), "captured_at": at, "tasks": [task]
        })
    };
    let case = |id: &str, fold: &str, target: &str, cutoff: &str, held_out: Value, task: Value| {
        json!({
            "id": id, "fold": fold, "target_revision": target, "cutoff": cutoff,
            "task_snapshot": task, "source": source, "held_out_delivery": held_out
        })
    };
    let first = task("task-first", "Change helper_value", "unix:180");
    let second = task("task-second", "Change helper_value again", "unix:380");
    let stray = task("task-stray", "Change helper_value once more", "unix:390");
    let mut corpus = json!({
        "schema_version": 1, "repository": repository, "landing_branch": "main",
        "source": source, "complete": true, "coverage_note": "two folds and a stray case",
        "k": 10,
        "training_deliveries": [delivery(
            "training", &revisions[0], &revisions[1], "unix:100",
            task("task-training", "Adjust helper_value", "unix:50")
        )],
        "folds": [
            {"id": "week-2", "cutoff": "unix:350", "revision": revisions[2]},
            {"id": "week-1", "cutoff": "unix:150", "revision": revisions[1]}
        ],
        "word_index_grid": {"selection_folds": 1, "symbol_boost": [1.0, 2.0], "min_support": [1]},
        "cases": [
            case("first", "week-1", &revisions[1], "unix:200",
                delivery("first", &revisions[1], &revisions[2], "unix:300", first.clone()), first),
            case("second", "week-2", &revisions[2], "unix:400",
                delivery("second", &revisions[2], &revisions[3], "unix:500", second.clone()), second),
            case("stray", "week-9", &revisions[2], "unix:400",
                delivery("stray", &revisions[2], &revisions[3], "unix:500", stray.clone()), stray)
        ]
    });
    let input = root.join("corpus.json");
    fs::write(&input, serde_json::to_vec(&corpus).expect("encode corpus")).expect("write corpus");
    let output = run(root, &["evaluate", "--input", "corpus.json", "--json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("evaluation JSON");
    assert_eq!(report["schema_version"], 3);
    assert_eq!(report["baseline_text_fields"], "title_description_criteria");
    assert_eq!(report["coverage"]["cases_evaluated"], 2, "{report:#}");

    // Folds are read in cutoff order, whatever order the corpus lists them in.
    let folds = report["folds"].as_array().expect("folds");
    assert_eq!(folds.len(), 2);
    assert_eq!(folds[0]["id"], "week-1");
    assert_eq!(folds[0]["role"], "selection");
    assert_eq!(folds[0]["revision"], revisions[1].as_str());
    assert_eq!(folds[1]["id"], "week-2");
    assert_eq!(folds[1]["role"], "headline");
    for fold in folds {
        assert_eq!(fold["cases_evaluated"], 1);
        assert_eq!(fold["bm25_file_documents"], 2, "{fold}");
    }
    assert_eq!(report["cases"][2]["id"], "stray");
    assert_eq!(report["cases"][2]["fold"], Value::Null);
    assert_eq!(
        report["cases"][2]["exclusions"],
        json!(["fold_missing_or_undeclared"])
    );
    assert_eq!(report["cases"][0]["mostly_added_files"], false);

    // Both baselines find the file and the symbol the task text names.
    let metric = |variant: &str, level: &str| {
        report["metrics"]
            .as_array()
            .expect("metrics")
            .iter()
            .find(|row| row["variant"] == variant && row["level"] == level)
            .cloned()
            .unwrap_or_else(|| panic!("{variant} {level} row"))
    };
    assert_eq!(report["metrics"].as_array().map(Vec::len), Some(18));
    for (variant, level) in [
        ("bm25_code", "file"),
        ("bm25_code", "symbol"),
        ("explicit_mentions", "file"),
        ("explicit_mentions", "symbol"),
        ("word_index_plus_mentions", "file"),
        ("word_index_bm25_rrf", "file"),
    ] {
        let row = metric(variant, level);
        assert_eq!(row["mean_recall_at_k"], 1.0, "{row}");
        assert_eq!(row["coverage"], 1.0, "{row}");
        assert_eq!(row["stale_result_rate"], 0.0, "{row}");
    }
    assert_eq!(
        report["split_metrics"].as_array().map(|splits| splits
            .iter()
            .map(|split| split["cases"].clone())
            .collect::<Vec<_>>()),
        Some(vec![json!(0), json!(2)])
    );
    assert_eq!(
        report["paired_differences"].as_array().map(Vec::len),
        Some(32)
    );

    // The grid is chosen on week-1 and read on week-2.
    let sweep = &report["sweep"];
    assert_eq!(sweep["selection_folds"], json!(["week-1"]));
    assert_eq!(sweep["headline_folds"], json!(["week-2"]));
    assert_eq!(sweep["cells"].as_array().map(Vec::len), Some(2));
    assert_eq!(sweep["cells"][1]["options"]["symbol_boost"], 2.0);
    assert_eq!(sweep["cells"][1]["options"]["min_support"], 1);
    assert!(sweep["selected_cell"].is_u64(), "{sweep:#}");
    assert_eq!(sweep["headline"]["cases"], 1);
    assert_eq!(
        sweep["headline"]["metrics"].as_array().map(Vec::len),
        Some(18)
    );
    // One headline case gives no interval, so neither gate can be read.
    let gates = &report["gates"];
    assert_eq!(gates["scope"], "headline_folds");
    assert_eq!(gates["cases"], 1);
    assert_eq!(
        gates["results"]
            .as_array()
            .expect("gate results")
            .iter()
            .map(|gate| (gate["gate"].clone(), gate["status"].clone()))
            .collect::<Vec<_>>(),
        vec![
            (json!("G1"), json!("unevaluable")),
            (json!("G2"), json!("unevaluable"))
        ]
    );

    let table = run(
        root,
        &["evaluate", "--input", "corpus.json", "--format", "table"],
    );
    assert!(table.status.success());
    let table = String::from_utf8_lossy(&table.stdout);
    for heading in [
        "BM25 FILES",
        "WORD_INDEX VS",
        "GATE",
        "SELECTION FILE RECALL",
        "MEAN RECALL",
    ] {
        assert!(table.contains(heading), "{heading} missing from:\n{table}");
    }

    corpus["word_index_grid"]["selection_folds"] = json!(2);
    fs::write(&input, serde_json::to_vec(&corpus).expect("encode corpus")).expect("write corpus");
    let refused = run(root, &["evaluate", "--input", "corpus.json", "--json"]);
    // A refused value inside a well-formed corpus is a usage error.
    assert_eq!(refused.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&refused.stderr).expect("decode error JSON");
    assert_eq!(error["code"], "invalid_input");
    assert!(
        error["error"]
            .as_str()
            .is_some_and(|message| message.contains("selection_folds")),
        "{error}"
    );
    assert!(!root.join(".orbit-graph").exists());
}

fn fact(timestamp: &str) -> Value {
    json!({"status": "known", "timestamp": timestamp, "source": {"system": "fixture_clock"}})
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = common::git_command(root)
        .args(args)
        .output()
        .expect("run isolated git");
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("Git stdout")
        .trim()
        .to_owned()
}

fn run(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
        .current_dir(cwd)
        .env_remove("ORBIT_TOOL_NAME")
        .env_remove("ORBIT_GRAPH_FORMAT")
        .args(args)
        .output()
        .expect("run evaluation CLI")
}
