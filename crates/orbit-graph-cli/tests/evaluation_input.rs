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
                assert_eq!(metrics.len(), 8);
                assert!(metrics.iter().all(|metric| metric["cases"] == 1));
            }
            "ndjson" => {
                let records = String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .map(|line| serde_json::from_str::<Value>(line).expect("evaluation NDJSON"))
                    .collect::<Vec<_>>();
                assert_eq!(records.len(), 10);
                assert_eq!(records[0]["record_type"], "evaluation_context");
                assert_eq!(records[9]["record_type"], "evaluation_case");
                assert_eq!(records[9]["case"]["evaluated"], true);
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
