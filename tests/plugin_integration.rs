//! Real-executable coverage for Orbit plugin and chronological evaluation paths.

#![allow(clippy::expect_used)]

use std::fs;
#[cfg(unix)]
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::fd::{FromRawFd, OwnedFd};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use serde_json::{Value, json};
use tempfile::TempDir;

use orbit_graph::plugin::{MAINTAIN_TOOL_NAME, RECOMMEND_TOOL_NAME, STATUS_TOOL_NAME};

#[test]
fn no_argv_plugin_supports_status_and_query_and_task_id_both_levels() {
    let fixture = evaluation_fixture();
    let repository = fixture.path().canonicalize().expect("canonical fixture");
    let _ = plugin_json(
        fixture.path(),
        MAINTAIN_TOOL_NAME,
        json!({
            "schema_version": 1,
            "operation": "import",
            "repository": repository,
            "branch": "main",
            "delivery": fixture_delivery(fixture.path(), "training")
        }),
    );

    let status = plugin_json(
        fixture.path(),
        STATUS_TOOL_NAME,
        json!({"schema_version": 1, "repository": repository, "branch": "main"}),
    );
    assert_eq!(status["operation"], "status");
    assert_eq!(status["status"]["verified_deliveries"], 1);
    let status_with_mode_environment = plugin_output_with_env(
        fixture.path(),
        STATUS_TOOL_NAME,
        json!({"schema_version": 1, "repository": repository, "branch": "main"}),
        &[
            ("ORBIT_GRAPH_FORMAT", std::ffi::OsStr::new("table")),
            ("CLICOLOR_FORCE", std::ffi::OsStr::new("1")),
        ],
    );
    assert!(status_with_mode_environment.status.success());
    let status_with_mode_environment: Value =
        serde_json::from_slice(&status_with_mode_environment.stdout).expect("plugin JSON");
    assert_eq!(status_with_mode_environment, status);
    #[cfg(unix)]
    {
        let status_with_tty = plugin_json_with_tty_stdout(
            fixture.path(),
            STATUS_TOOL_NAME,
            json!({"schema_version": 1, "repository": repository, "branch": "main"}),
        );
        assert_eq!(status_with_tty, status);
    }

    for level in ["file", "symbol"] {
        let query = plugin_json(
            fixture.path(),
            RECOMMEND_TOOL_NAME,
            json!({
                "schema_version": 1,
                "repository": repository,
                "query": "parser validation",
                "level": level,
                "branch": "main",
                "cutoff": "unix:20"
            }),
        );
        assert_eq!(query["adapter"]["task_text"], "request_query");
        assert!(
            query["result"]["recommendations"]
                .as_array()
                .is_some_and(|items| !items.is_empty()),
            "query recommendations missing in {level}: {query}"
        );

        let task = plugin_json(
            fixture.path(),
            RECOMMEND_TOOL_NAME,
            json!({
                "schema_version": 1,
                "repository": repository,
                "task_id": "TASK-TARGET",
                "task_snapshot": task_snapshot("TASK-TARGET", "parser validation", 15),
                "level": level,
                "branch": "main",
                "revision": git_stdout(fixture.path(), ["rev-parse", "HEAD~2"]),
                "cutoff": "unix:20"
            }),
        );
        assert_eq!(task["adapter"]["task_text"], "supplied_snapshot");
        assert_eq!(task["result"]["input"]["task_id"], "TASK-TARGET");
        assert!(
            task["result"]["recommendations"]
                .as_array()
                .is_some_and(|items| !items.is_empty()),
            "task recommendations missing in {level}: {task}"
        );
    }
}

#[test]
fn chronological_evaluation_reports_four_variants_both_levels_and_no_stale_results() {
    let fixture = evaluation_fixture();
    let repository = fixture.path().canonicalize().expect("canonical fixture");
    let target_revision = git_stdout(fixture.path(), ["rev-parse", "HEAD~2"]);
    let corpus_path = fixture.path().join("evaluation.json");
    let corpus = json!({
        "schema_version": 1,
        "repository": repository,
        "landing_branch": "main",
        "source": {"system": "test-public-export", "record_id": "fixture-v1"},
        "complete": true,
        "coverage_note": "complete two-delivery synthetic fixture",
        "k": 3,
        "training_deliveries": [
            fixture_delivery(fixture.path(), "training"),
            fixture_delivery(fixture.path(), "future")
        ],
        "cases": [{
            "id": "prospective-target",
            "target_revision": target_revision.clone(),
            "cutoff": "unix:20",
            "task_snapshot": task_snapshot("TASK-TARGET", "parser validation", 15),
            "held_out_delivery": fixture_delivery(fixture.path(), "held-out"),
            "source": {"system": "test-public-observation", "record_id": "target@15"}
        }]
    });
    fs::write(
        &corpus_path,
        serde_json::to_vec_pretty(&corpus).expect("encode corpus"),
    )
    .expect("write corpus");
    let _ = plugin_json(
        fixture.path(),
        MAINTAIN_TOOL_NAME,
        json!({
            "schema_version": 1,
            "operation": "import",
            "repository": repository,
            "branch": "main",
            "delivery": fixture_delivery(fixture.path(), "held-out")
        }),
    );

    let output = run(
        fixture.path(),
        [
            "evaluate",
            "--input",
            corpus_path.to_string_lossy().as_ref(),
        ],
    );
    assert!(
        output.status.success(),
        "evaluation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("evaluation JSON");
    let repeated = run(
        fixture.path(),
        [
            "evaluate",
            "--input",
            corpus_path.to_string_lossy().as_ref(),
        ],
    );
    let repeated: Value = serde_json::from_slice(&repeated.stdout).expect("repeat evaluation JSON");
    assert_eq!(
        metric_projection(&report),
        metric_projection(&repeated),
        "non-latency metrics must be deterministic and independent of operational indexes"
    );
    assert_eq!(report["coverage"]["cases_evaluated"], 1);
    assert_eq!(report["coverage"]["cases_excluded"], 0);
    assert_eq!(report["metrics"].as_array().map(Vec::len), Some(8));
    let variants = report["metrics"]
        .as_array()
        .expect("metrics")
        .iter()
        .filter_map(|metric| metric["variant"].as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        variants,
        std::collections::BTreeSet::from([
            "combined",
            "frequency",
            "graph_only",
            "task_search_only"
        ])
    );
    assert!(report["metrics"].as_array().is_some_and(|metrics| {
        metrics.iter().all(|metric| {
            metric["stale_result_rate"] == 0.0
                && metric["mean_latency_ms"].is_number()
                && metric["recall_at_k"].is_number()
                && metric["precision_at_k"].is_number()
        })
    }));
    let status = run(fixture.path(), ["history", "status", "--branch", "main"]);
    assert!(status.status.success());
    let status: Value = serde_json::from_slice(&status.stdout).expect("history status JSON");
    assert_eq!(
        status["deliveries"], 1,
        "evaluation must preserve the pre-existing operational index exactly"
    );
    assert_eq!(report["coverage"]["isolated_indexes"], true);
    assert_eq!(
        report["cases"][0]["graph_snapshot"]["record_id"],
        target_revision
    );
    assert_eq!(
        report["cases"][0]["graph_structure_applied"]["graph_only:file"], true,
        "graph-only must use the frozen target graph rather than lexical-only fallback"
    );

    let human = run_cli(
        fixture.path(),
        [
            "--format",
            "table",
            "evaluate",
            "--input",
            corpus_path.to_string_lossy().as_ref(),
        ],
    );
    assert!(human.status.success());
    let human = String::from_utf8_lossy(&human.stdout);
    assert!(human.contains("COVERAGE NOTE"));
    assert!(human.contains("VARIANT"));
    assert!(human.contains("EXCLUSIONS"));
    assert!(human.contains("prospective-target"));

    let ndjson = run_cli(
        fixture.path(),
        [
            "--format",
            "ndjson",
            "evaluate",
            "--input",
            corpus_path.to_string_lossy().as_ref(),
        ],
    );
    assert!(ndjson.status.success());
    let records = String::from_utf8_lossy(&ndjson.stdout)
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("evaluation NDJSON record"))
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 10);
    assert_eq!(records[0]["record_type"], "evaluation_context");
    assert_eq!(records[0]["context"]["coverage"]["cases_evaluated"], 1);
    assert!(records[0]["context"].get("metrics").is_none());
    assert!(records[0]["context"].get("cases").is_none());
    assert!(
        records[1..9]
            .iter()
            .all(|record| record["record_type"] == "evaluation_metric")
    );
    assert_eq!(records[9]["record_type"], "evaluation_case");
    assert!(records[9]["case"]["truth_coverage"].is_object());
    let future = run(
        fixture.path(),
        [
            "recommend",
            "--query",
            "future_secret",
            "--variant",
            "task-search-only",
            "--revision",
            target_revision.as_str(),
            "--cutoff",
            "unix:20",
            "--branch",
            "main",
        ],
    );
    assert!(future.status.success());
    let future: Value = serde_json::from_slice(&future.stdout).expect("future query JSON");
    assert!(
        future["recommendations"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "future task and delivery evidence leaked across the cutoff: {future}"
    );
}

#[test]
fn bounded_history_bootstrap_advances_across_cli_and_plugin_batches() {
    for via_plugin in [false, true] {
        let fixture = many_commit_fixture();
        let repository = fixture.path().canonicalize().expect("canonical fixture");
        let mut reports = Vec::new();
        for _ in 0..4 {
            let report = if via_plugin {
                plugin_json(
                    fixture.path(),
                    MAINTAIN_TOOL_NAME,
                    json!({
                        "schema_version": 1,
                        "operation": "history_sync",
                        "repository": repository,
                        "branch": "main",
                        "limit": 2
                    }),
                )["result"]
                    .clone()
            } else {
                let output = run(
                    fixture.path(),
                    ["history", "sync", "--branch", "main", "--limit", "2"],
                );
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                serde_json::from_slice(&output.stdout).expect("history sync JSON")
            };
            reports.push(report);
        }
        assert_eq!(reports[0]["complete"], false);
        assert_eq!(reports[1]["complete"], false);
        assert_eq!(reports[2]["complete"], true);
        assert_eq!(reports[3]["complete"], true);
        assert!(reports[0]["resume_from"].is_string());
        assert_eq!(reports[3]["commits_indexed"], 0);
        let status = run(fixture.path(), ["history", "status", "--branch", "main"]);
        let status: Value = serde_json::from_slice(&status.stdout).expect("history status JSON");
        assert_eq!(status["deliveries"], 5);
        assert_eq!(status["complete"], true);
        assert_eq!(
            status["cursor"],
            git_stdout(fixture.path(), ["rev-parse", "HEAD"])
        );
    }
}

#[test]
fn evaluation_rejects_unverified_pre_cutoff_and_unattested_hybrid_truth() {
    let fixture = evaluation_fixture();
    let repository = fixture.path().canonicalize().expect("canonical fixture");
    let target = git_stdout(fixture.path(), ["rev-parse", "HEAD~2"]);
    let mut pre_cutoff = fixture_delivery(fixture.path(), "held-out");
    pre_cutoff["delivered_at"]["timestamp"] = json!("unix:10");
    let mut contradictory = fixture_delivery(fixture.path(), "held-out");
    contradictory["delivered_at"]["timestamp"] = json!("unix:10");
    let mut git_only = fixture_delivery(fixture.path(), "held-out");
    git_only["delivery_id"] = json!("fixture:git-only-held-out");
    git_only["evidence"] = json!("git_only");
    let corpus = json!({
        "schema_version": 1,
        "repository": repository,
        "landing_branch": "main",
        "source": {"system": "test-public-export"},
        "complete": true,
        "coverage_note": "admission regression cases",
        "k": 3,
        "training_deliveries": [],
        "cases": [
            {
                "id": "pre-cutoff",
                "target_revision": target.clone(),
                "cutoff": "unix:20",
                "task_snapshot": task_snapshot("TASK-TARGET", "parser", 15),
                "held_out_delivery": pre_cutoff,
                "source": {"system": "test"}
            },
            {
                "id": "contradictory-chronology",
                "target_revision": target.clone(),
                "cutoff": "unix:20",
                "task_snapshot": task_snapshot("TASK-TARGET", "parser", 15),
                "held_out_delivery": contradictory,
                "prospective_delivery_lower_bound": {
                    "status": "known",
                    "timestamp": "unix:25",
                    "source": {"system": "test", "record_id": "run-start"}
                },
                "source": {"system": "test"}
            },
            {
                "id": "unverified",
                "target_revision": target.clone(),
                "cutoff": "unix:20",
                "task_snapshot": task_snapshot("TASK-TARGET", "parser", 15),
                "held_out_delivery": git_only,
                "source": {"system": "test"}
            },
            {
                "id": "hybrid-without-observation",
                "target_revision": target,
                "cutoff": "unix:20",
                "task_snapshot": task_snapshot("TASK-TARGET", "parser", 15),
                "held_out_delivery": fixture_delivery(fixture.path(), "held-out"),
                "hybrid_hits": [{"task_id": "TASK-TRAIN", "score": 1.0}],
                "source": {"system": "test"}
            }
        ]
    });
    let path = fixture.path().join("invalid-chronology.json");
    fs::write(&path, serde_json::to_vec(&corpus).expect("encode corpus")).expect("write corpus");
    let output = run(
        fixture.path(),
        ["evaluate", "--input", path.to_string_lossy().as_ref()],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("evaluation JSON");
    assert_eq!(report["coverage"]["cases_evaluated"], 0);
    assert!(case_exclusions(&report, 0).contains(&"held_out_delivery_not_proven_after_cutoff"));
    assert!(case_exclusions(&report, 1).contains(&"contradictory_delivery_chronology"));
    assert!(case_exclusions(&report, 2).contains(&"held_out_delivery_not_verified"));
    assert!(
        case_exclusions(&report, 3).contains(&"hybrid_hits_not_attested_strictly_before_cutoff")
    );
    let human = run_cli(
        fixture.path(),
        [
            "--format",
            "table",
            "evaluate",
            "--input",
            path.to_string_lossy().as_ref(),
        ],
    );
    assert!(human.status.success());
    let human = String::from_utf8_lossy(&human.stdout);
    assert!(human.contains("admission regression cases"));
    assert!(human.contains("held_out_delivery_not_verified"));
    assert!(human.contains("hybrid_hits_not_attested_strictly_before_cutoff"));
}

#[test]
fn public_adapter_is_idempotent_and_supports_honest_live_task_observations() {
    let fixture = adapter_fixture();
    let repository = fixture
        .path()
        .join("repo")
        .canonicalize()
        .expect("repository");
    let orbit_root = fixture
        .path()
        .join("orbit-root")
        .canonicalize()
        .expect("orbit root");
    let shim = fixture.path().join("orbit-shim");
    let sync = || {
        plugin_output_with_env(
            repository.as_path(),
            MAINTAIN_TOOL_NAME,
            json!({
                "schema_version": 1,
                "operation": "orbit_sync",
                "repository": repository,
                "branch": "main",
                "workspace": "ws-test",
                "orbit_root": orbit_root,
                "run_ids": ["RUN-1"],
                "task_snapshots": [task_snapshot("TASK-PRIOR", "original parser observation", 5)]
            }),
            &[("ORBIT_GRAPH_ORBIT_BIN", shim.as_os_str())],
        )
    };
    let first = sync();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first: Value = serde_json::from_slice(&first.stdout).expect("first sync JSON");
    assert_eq!(first["outcomes"][0]["status"], "inserted");
    std::thread::sleep(Duration::from_millis(20));
    let second = sync();
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let second: Value = serde_json::from_slice(&second.stdout).expect("second sync JSON");
    assert_eq!(second["outcomes"][0]["status"], "already_indexed");
    assert_eq!(second["status"]["deliveries"], 1);

    for input in [
        json!({
            "schema_version": 1,
            "repository": repository,
            "branch": "main",
            "query": "parser validation",
            "level": "file"
        }),
        json!({
            "schema_version": 1,
            "repository": repository,
            "branch": "main",
            "workspace": "ws-test",
            "orbit_root": orbit_root,
            "task_id": "TASK-TARGET",
            "hybrid": true,
            "level": "file"
        }),
    ] {
        let output = plugin_output_with_env(
            repository.as_path(),
            RECOMMEND_TOOL_NAME,
            input,
            &[("ORBIT_GRAPH_ORBIT_BIN", shim.as_os_str())],
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: Value = serde_json::from_slice(&output.stdout).expect("recommend JSON");
        assert!(
            value["result"]["recommendations"]
                .as_array()
                .is_some_and(|items| !items.is_empty())
        );
    }

    let replay = plugin_output_with_env(
        repository.as_path(),
        RECOMMEND_TOOL_NAME,
        json!({
            "schema_version": 1,
            "repository": repository,
            "branch": "main",
            "workspace": "ws-test",
            "orbit_root": orbit_root,
            "task_id": "TASK-TARGET",
            "cutoff": "unix:9999999999"
        }),
        &[("ORBIT_GRAPH_ORBIT_BIN", shim.as_os_str())],
    );
    assert!(
        !replay.status.success(),
        "post-execution text must fail strict replay"
    );

    let wrong_workspace = plugin_output_with_env(
        repository.as_path(),
        RECOMMEND_TOOL_NAME,
        json!({
            "schema_version": 1,
            "repository": repository,
            "workspace": "ws-wrong",
            "orbit_root": orbit_root,
            "task_id": "TASK-TARGET"
        }),
        &[("ORBIT_GRAPH_ORBIT_BIN", shim.as_os_str())],
    );
    assert!(!wrong_workspace.status.success());

    let wrong_root_path = fixture.path().join("wrong-root");
    fs::create_dir(&wrong_root_path).expect("create wrong root");
    let wrong_root = plugin_output_with_env(
        repository.as_path(),
        RECOMMEND_TOOL_NAME,
        json!({
            "schema_version": 1,
            "repository": repository,
            "workspace": "ws-test",
            "orbit_root": wrong_root_path,
            "task_id": "TASK-TARGET"
        }),
        &[("ORBIT_GRAPH_ORBIT_BIN", shim.as_os_str())],
    );
    assert!(!wrong_root.status.success());

    let other_repository = fixture.path().join("other-repo");
    run_git(
        fixture.path(),
        ["clone", repository.to_string_lossy().as_ref(), "other-repo"],
    );
    let wrong_repository = plugin_output_with_env(
        other_repository.as_path(),
        RECOMMEND_TOOL_NAME,
        json!({
            "schema_version": 1,
            "repository": other_repository,
            "workspace": "ws-test",
            "orbit_root": orbit_root,
            "task_id": "TASK-TARGET"
        }),
        &[("ORBIT_GRAPH_ORBIT_BIN", shim.as_os_str())],
    );
    assert!(!wrong_repository.status.success());

    let supplied_snapshot_bypass = plugin_output_with_env(
        repository.as_path(),
        MAINTAIN_TOOL_NAME,
        json!({
            "schema_version": 1,
            "operation": "orbit_sync",
            "repository": repository,
            "branch": "main",
            "workspace": "ws-wrong",
            "orbit_root": orbit_root,
            "run_ids": ["RUN-1"],
            "task_snapshots": [task_snapshot("TASK-PRIOR", "parser", 5)]
        }),
        &[("ORBIT_GRAPH_ORBIT_BIN", shim.as_os_str())],
    );
    assert!(supplied_snapshot_bypass.status.success());
    let supplied_snapshot_bypass: Value =
        serde_json::from_slice(&supplied_snapshot_bypass.stdout).expect("sync JSON");
    assert_eq!(
        supplied_snapshot_bypass["outcomes"][0]["status"], "excluded",
        "supplied snapshots must not bypass public workspace verification"
    );
}

#[test]
fn public_adapter_bounds_time_output_and_cleans_capture_files() {
    for (body, expected) in [
        ("sleep 5", "timed out"),
        ("head -c 2097152 /dev/zero", "exceeded"),
    ] {
        let fixture = adapter_fixture();
        let repository = fixture
            .path()
            .join("repo")
            .canonicalize()
            .expect("repository");
        let orbit_root = fixture
            .path()
            .join("orbit-root")
            .canonicalize()
            .expect("orbit root");
        let shim = fixture.path().join("bad-orbit-shim");
        executable(&shim, format!("#!/bin/sh\n{body}\n"));
        let started = Instant::now();
        let output = plugin_output_with_env(
            repository.as_path(),
            RECOMMEND_TOOL_NAME,
            json!({
                "schema_version": 1,
                "repository": repository,
                "workspace": "ws-test",
                "orbit_root": orbit_root,
                "task_id": "TASK-TARGET"
            }),
            &[
                ("ORBIT_GRAPH_ORBIT_BIN", shim.as_os_str()),
                (
                    "ORBIT_GRAPH_ORBIT_TIMEOUT_SECONDS",
                    std::ffi::OsStr::new("1"),
                ),
            ],
        );
        assert!(!output.status.success());
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(String::from_utf8_lossy(&output.stderr).contains(expected));
        assert_no_subprocess_captures();
    }
}

#[test]
fn evaluation_reports_added_deleted_renamed_and_unsupported_truth_coverage() {
    let fixture = mixed_truth_fixture();
    let repository = fixture.path().canonicalize().expect("repository");
    let before = git_stdout(fixture.path(), ["rev-parse", "HEAD~1"]);
    let after = git_stdout(fixture.path(), ["rev-parse", "HEAD"]);
    let corpus = json!({
        "schema_version": 1,
        "repository": repository,
        "landing_branch": "main",
        "source": {"system": "test"},
        "complete": true,
        "coverage_note": "mixed truth fixture",
        "k": 10,
        "training_deliveries": [],
        "cases": [{
            "id": "mixed",
            "target_revision": before.clone(),
            "cutoff": "unix:20",
            "task_snapshot": task_snapshot("TASK-MIXED", "rename delete add", 15),
            "held_out_delivery": {
                "schema_version": 2,
                "repository": repository,
                "landing_branch": "main",
                "before_revision": before,
                "after_revision": after,
                "delivery_id": "mixed-heldout",
                "evidence": "verified_delivery",
                "source": {"system": "test"},
                "delivered_at": {"status": "known", "timestamp": "unix:30", "source": {"system": "test"}},
                "captured_at": "unix:31",
                "tasks": [task_snapshot("TASK-MIXED", "rename delete add", 15)]
            },
            "source": {"system": "test"}
        }]
    });
    let path = fixture.path().join("mixed-corpus.json");
    fs::write(&path, serde_json::to_vec(&corpus).expect("encode corpus")).expect("write corpus");
    let output = run(
        fixture.path(),
        ["evaluate", "--input", path.to_string_lossy().as_ref()],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("evaluation JSON");
    let coverage = &report["cases"][0]["truth_coverage"];
    assert_eq!(coverage["file_changes_total"], 4);
    assert_eq!(coverage["file_truth_eligible"], 3);
    assert_eq!(
        coverage["file_truth_omitted"]["added_file_absent_at_target"],
        1
    );
    assert!(
        coverage["symbol_truth_eligible"]
            .as_u64()
            .is_some_and(|count| count >= 2)
    );
    assert_eq!(
        coverage["symbol_truth_omitted"]["added_symbol_absent_at_target"],
        1
    );
    assert_eq!(
        coverage["symbol_truth_omitted"]["symbol_truth_unavailable_unsupported_language"],
        1
    );
}

#[test]
fn manifests_are_versioned_and_describe_all_registered_tools() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for (file, name) in [
        ("orbit-graph-recommend.orbit-tool.yaml", RECOMMEND_TOOL_NAME),
        ("orbit-graph-status.orbit-tool.yaml", STATUS_TOOL_NAME),
        ("orbit-graph-maintain.orbit-tool.yaml", MAINTAIN_TOOL_NAME),
    ] {
        let manifest: Value = serde_yaml::from_slice(
            &fs::read(root.join("plugin").join(file)).expect("read manifest"),
        )
        .expect("parse manifest");
        assert_eq!(manifest["schemaVersion"], 1);
        assert_eq!(manifest["name"], name);
        assert!(
            manifest["parameters"]
                .as_array()
                .is_some_and(|p| !p.is_empty())
        );
    }
}

#[test]
fn installed_orbit_registration_and_invocation_when_authority_binary_is_requested() {
    let Ok(orbit_bin) = std::env::var("ORBIT_GRAPH_TEST_ORBIT_BIN") else {
        return;
    };
    let fixture = evaluation_fixture();
    let isolated = TempDir::new().expect("isolated Orbit root");
    let isolated_root = isolated.path().join(".orbit");
    let graph_bin = env!("CARGO_BIN_EXE_orbit-graph");
    let plugin = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("plugin");
    let initialized = Command::new(&orbit_bin)
        .current_dir(fixture.path())
        .args([
            "workspace",
            "init",
            "--name",
            "orbit-graph-plugin-test",
            "--ship-mode",
            "local",
            "--root",
        ])
        .arg(&isolated_root)
        .output()
        .expect("initialize isolated Orbit config");
    assert!(
        initialized.status.success(),
        "isolated Orbit init failed: {}",
        String::from_utf8_lossy(&initialized.stderr)
    );
    for manifest in [
        "orbit-graph-recommend.orbit-tool.yaml",
        "orbit-graph-status.orbit-tool.yaml",
        "orbit-graph-maintain.orbit-tool.yaml",
    ] {
        let output = Command::new(&orbit_bin)
            .args(["tool", "add", graph_bin, "--manifest"])
            .arg(plugin.join(manifest))
            .args(["--root"])
            .arg(&isolated_root)
            .output()
            .expect("register external tool");
        assert!(
            output.status.success(),
            "registration failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let input = json!({
        "schema_version": 1,
        "repository": fixture.path().canonicalize().expect("canonical fixture"),
        "branch": "main"
    });
    let output = Command::new(&orbit_bin)
        .current_dir(fixture.path())
        .env_remove("ORBIT_MANAGED_RUN_CONTEXT")
        .env_remove("ORBIT_ACTIVITY_TOOLS")
        .env_remove("ORBIT_ACTIVE_TASK_ID")
        .env_remove("ORBIT_TASK_ID")
        .env_remove("ORBIT_RUN_ID")
        .env_remove("ORBIT_REGISTRY_ROOT")
        .env_remove("ORBIT_WORKSPACE")
        .args(["tool", "run", STATUS_TOOL_NAME, "--input"])
        .arg(input.to_string())
        .args(["--full", "--root"])
        .arg(&isolated_root)
        .output()
        .expect("invoke installed external tool");
    assert!(
        output.status.success(),
        "installed invocation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).expect("Orbit tool JSON");
    assert_eq!(value["operation"], "status");

    let maintenance = isolated_tool_run(
        &orbit_bin,
        &isolated_root,
        fixture.path(),
        MAINTAIN_TOOL_NAME,
        json!({
            "schema_version": 1,
            "operation": "history_sync",
            "repository": fixture.path().canonicalize().expect("canonical fixture"),
            "branch": "main",
            "limit": 100
        }),
    );
    assert!(
        maintenance.status.success(),
        "installed maintenance invocation failed: {}",
        String::from_utf8_lossy(&maintenance.stderr)
    );
    let value: Value = serde_json::from_slice(&maintenance.stdout).expect("maintenance tool JSON");
    assert_eq!(value["operation"], "history_sync");

    for level in ["file", "symbol"] {
        for request in [
            json!({
                "schema_version": 1,
                "repository": fixture.path().canonicalize().expect("canonical fixture"),
                "branch": "main",
                "query": "parser",
                "level": level
            }),
            json!({
                "schema_version": 1,
                "repository": fixture.path().canonicalize().expect("canonical fixture"),
                "branch": "main",
                "task_id": "TASK-PENDING",
                "task_snapshot": task_snapshot("TASK-PENDING", "parser", 15),
                "cutoff": "unix:20",
                "level": level
            }),
        ] {
            let output = isolated_tool_run(
                &orbit_bin,
                &isolated_root,
                fixture.path(),
                RECOMMEND_TOOL_NAME,
                request,
            );
            assert!(
                output.status.success(),
                "installed {level} recommendation failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let value: Value =
                serde_json::from_slice(&output.stdout).expect("installed recommendation JSON");
            assert!(
                value["result"]["recommendations"]
                    .as_array()
                    .is_some_and(|items| !items.is_empty()),
                "installed {level} recommendation empty: {value}"
            );
        }
    }
}

#[test]
fn install_and_uninstall_scripts_work_and_reject_malformed_arguments_first() {
    let Ok(orbit_bin) = std::env::var("ORBIT_GRAPH_TEST_ORBIT_BIN") else {
        return;
    };
    let fixture = evaluation_fixture();
    let isolated = TempDir::new().expect("isolated Orbit root");
    let isolated_root = isolated.path().join(".orbit");
    let initialized = Command::new(&orbit_bin)
        .current_dir(fixture.path())
        .args([
            "workspace",
            "init",
            "--name",
            "orbit-graph-script-test",
            "--ship-mode",
            "local",
            "--root",
        ])
        .arg(&isolated_root)
        .output()
        .expect("initialize isolated Orbit config");
    assert!(initialized.status.success());
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let install = root.join("scripts/install-orbit-plugin.sh");
    let uninstall = root.join("scripts/uninstall-orbit-plugin.sh");
    let graph_bin = env!("CARGO_BIN_EXE_orbit-graph");
    let path = executable_path(&orbit_bin);

    let malformed = Command::new("sh")
        .current_dir(fixture.path())
        .env("PATH", &path)
        .arg(&install)
        .args(["--orbit-root", "", "--binary", graph_bin])
        .output()
        .expect("run malformed installer");
    assert_eq!(malformed.status.code(), Some(2));

    let installed = Command::new("sh")
        .current_dir(fixture.path())
        .env("PATH", &path)
        .arg(&install)
        .arg("--orbit-root")
        .arg(&isolated_root)
        .args(["--binary", graph_bin])
        .output()
        .expect("run installer");
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    let status = isolated_tool_run(
        &orbit_bin,
        &isolated_root,
        fixture.path(),
        STATUS_TOOL_NAME,
        json!({
            "schema_version": 1,
            "repository": fixture.path().canonicalize().expect("repository"),
            "branch": "main"
        }),
    );
    assert!(status.status.success());

    let removed = Command::new("sh")
        .current_dir(fixture.path())
        .env("PATH", &path)
        .arg(&uninstall)
        .arg("--orbit-root")
        .arg(&isolated_root)
        .output()
        .expect("run uninstaller");
    assert!(
        removed.status.success(),
        "{}",
        String::from_utf8_lossy(&removed.stderr)
    );
    let missing = isolated_tool_run(
        &orbit_bin,
        &isolated_root,
        fixture.path(),
        STATUS_TOOL_NAME,
        json!({
            "schema_version": 1,
            "repository": fixture.path().canonicalize().expect("repository")
        }),
    );
    assert!(!missing.status.success());
}

fn isolated_tool_run(
    orbit_bin: &str,
    orbit_root: &Path,
    repository: &Path,
    tool: &str,
    input: Value,
) -> Output {
    Command::new(orbit_bin)
        .current_dir(repository)
        .env_remove("ORBIT_MANAGED_RUN_CONTEXT")
        .env_remove("ORBIT_ACTIVITY_TOOLS")
        .env_remove("ORBIT_ACTIVE_TASK_ID")
        .env_remove("ORBIT_TASK_ID")
        .env_remove("ORBIT_RUN_ID")
        .env_remove("ORBIT_REGISTRY_ROOT")
        .env_remove("ORBIT_WORKSPACE")
        .args(["tool", "run", tool, "--input"])
        .arg(input.to_string())
        .args(["--full", "--root"])
        .arg(orbit_root)
        .output()
        .expect("invoke isolated Orbit tool")
}

fn executable_path(orbit_bin: &str) -> std::ffi::OsString {
    let mut paths = vec![
        Path::new(orbit_bin)
            .parent()
            .expect("Orbit binary parent")
            .to_path_buf(),
    ];
    if let Some(current) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&current));
    }
    std::env::join_paths(paths).expect("join executable path")
}

fn evaluation_fixture() -> TempDir {
    let fixture = TempDir::new().expect("create fixture");
    run_git(fixture.path(), ["init", "-b", "main"]);
    run_git(
        fixture.path(),
        ["config", "user.email", "graph@example.invalid"],
    );
    run_git(fixture.path(), ["config", "user.name", "Graph Test"]);
    fs::create_dir_all(fixture.path().join("src")).expect("create src");
    fs::create_dir_all(fixture.path().join("tests")).expect("create tests");
    fs::write(
        fixture.path().join("src/parser.rs"),
        "pub fn parse() -> bool { false }\n",
    )
    .expect("write parser");
    fs::write(
        fixture.path().join("tests/parser.rs"),
        "pub fn parser_test() -> bool { parse() && false }\n",
    )
    .expect("write test");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "base"]);
    fs::write(
        fixture.path().join("src/parser.rs"),
        "pub fn parse() -> bool { true }\n",
    )
    .expect("write training parser");
    fs::write(
        fixture.path().join("tests/parser.rs"),
        "pub fn parser_test() -> bool { parse() }\n",
    )
    .expect("write training test");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "training"]);
    fs::write(
        fixture.path().join("src/parser.rs"),
        "pub fn parse() -> bool { 1 + 1 == 2 }\n",
    )
    .expect("write held-out parser");
    fs::write(
        fixture.path().join("tests/parser.rs"),
        "pub fn parser_test() -> bool { parse() && true }\n",
    )
    .expect("write held-out test");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "held out"]);
    fs::write(
        fixture.path().join("src/parser.rs"),
        "pub fn parse() -> bool { future_secret() }\npub fn future_secret() -> bool { true }\n",
    )
    .expect("write future parser");
    fs::write(
        fixture.path().join("tests/parser.rs"),
        "pub fn parser_test() -> bool { true }\n",
    )
    .expect("remove future graph edge");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "future"]);
    fixture
}

fn many_commit_fixture() -> TempDir {
    let fixture = TempDir::new().expect("create fixture");
    run_git(fixture.path(), ["init", "-b", "main"]);
    run_git(
        fixture.path(),
        ["config", "user.email", "graph@example.invalid"],
    );
    run_git(fixture.path(), ["config", "user.name", "Graph Test"]);
    for value in 0..=5 {
        fs::write(fixture.path().join("value.txt"), value.to_string()).expect("write value");
        run_git(fixture.path(), ["add", "."]);
        run_git(fixture.path(), ["commit", "-m", &format!("commit {value}")]);
    }
    fixture
}

fn adapter_fixture() -> TempDir {
    let fixture = TempDir::new().expect("create adapter fixture");
    let repository = fixture.path().join("repo");
    fs::create_dir_all(&repository).expect("create repository");
    fs::create_dir_all(fixture.path().join("orbit-root")).expect("create Orbit root");
    run_git(&repository, ["init", "-b", "main"]);
    run_git(
        &repository,
        ["config", "user.email", "graph@example.invalid"],
    );
    run_git(&repository, ["config", "user.name", "Graph Test"]);
    fs::write(
        repository.join("parser.rs"),
        "pub fn parse() -> bool { false }\n",
    )
    .expect("write base");
    run_git(&repository, ["add", "."]);
    run_git(&repository, ["commit", "-m", "base"]);
    let before = git_stdout(&repository, ["rev-parse", "HEAD"]);
    fs::write(
        repository.join("parser.rs"),
        "pub fn parse() -> bool { true }\n",
    )
    .expect("write delivery");
    run_git(&repository, ["add", "."]);
    run_git(&repository, ["commit", "-m", "delivery"]);
    let after = git_stdout(&repository, ["rev-parse", "HEAD"]);
    let canonical = repository.canonicalize().expect("canonical repository");
    let orbit_root = fixture
        .path()
        .join("orbit-root")
        .canonicalize()
        .expect("canonical Orbit root");
    let workspace_list = json!([{
        "id": "ws-test", "name": "test", "repo_root": canonical
    }]);
    let run_show = json!({
        "run": {"state": "success", "finished_at": "2026-09-07T00:00:30Z"},
        "pipeline_state": {"step_outputs": {
            "0": {"workspace_path": canonical},
            "2": {
                "phase": "commit", "committed": true, "task_id": "TASK-PRIOR",
                "base_sha": before, "commit_sha": after
            }
        }}
    });
    let prior = public_task("TASK-PRIOR", "done", "parser validation");
    let target = public_task("TASK-TARGET", "in-progress", "parser validation");
    let script = format!(
        "#!/bin/sh\ncase \"$*\" in *\"run show RUN-1\"*|*\"--root {}\"*) ;; *) exit 9 ;; esac\ncase \"$*\" in\n  *\"workspace list\"*) printf '%s\\n' '{}' ;;\n  *\"run show RUN-1\"*) printf '%s\\n' '{}' ;;\n  *\"TASK-PRIOR\"*) printf '%s\\n' '{}' ;;\n  *\"TASK-TARGET\"*) printf '%s\\n' '{}' ;;\n  *\"orbit.search\"*) printf '%s\\n' '{{\"results\":[{{\"id\":\"TASK-PRIOR\",\"score\":1.0}}]}}' ;;\n  *) printf '%s\\n' '{{\"results\":[]}}' ;;\nesac\n",
        orbit_root.display(),
        workspace_list,
        run_show,
        prior,
        target
    );
    executable(&fixture.path().join("orbit-shim"), script);
    fixture
}

fn mixed_truth_fixture() -> TempDir {
    let fixture = TempDir::new().expect("create mixed truth fixture");
    run_git(fixture.path(), ["init", "-b", "main"]);
    run_git(
        fixture.path(),
        ["config", "user.email", "graph@example.invalid"],
    );
    run_git(fixture.path(), ["config", "user.name", "Graph Test"]);
    fs::write(
        fixture.path().join("old.rs"),
        "pub fn retained() -> bool { false }\n",
    )
    .expect("write old");
    fs::write(fixture.path().join("delete.rs"), "pub fn doomed() {}\n").expect("write delete");
    fs::write(fixture.path().join("data.txt"), "before\n").expect("write data");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "base"]);
    run_git(fixture.path(), ["mv", "old.rs", "renamed.rs"]);
    fs::write(
        fixture.path().join("renamed.rs"),
        "pub fn retained() -> bool { false }\n",
    )
    .expect("edit renamed");
    fs::remove_file(fixture.path().join("delete.rs")).expect("remove deleted fixture file");
    fs::write(fixture.path().join("data.txt"), "after\n").expect("edit data");
    fs::write(fixture.path().join("added.rs"), "pub fn added() {}\n").expect("write added");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "mixed changes"]);
    fixture
}

fn public_task(id: &str, status: &str, title: &str) -> Value {
    json!({
        "id": id,
        "title": title,
        "description": "update parser validation",
        "acceptance_criteria": ["parser changes"],
        "status": status,
        "created_at": "2026-09-06T00:00:00Z",
        "history": [{"event": "started", "to_status": "in-progress"}]
    })
}

fn executable(path: &Path, contents: String) {
    fs::write(path, contents).expect("write executable shim");
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(path).expect("shim metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).expect("chmod shim");
    }
}

fn assert_no_subprocess_captures() {
    let prefix = format!("orbit-graph-{}-", std::process::id());
    let leftovers = fs::read_dir(std::env::temp_dir())
        .expect("read temp directory")
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with(prefix.as_str()))
        .collect::<Vec<_>>();
    assert!(
        leftovers.is_empty(),
        "leftover subprocess captures: {leftovers:?}"
    );
}

fn fixture_delivery(repository: &Path, kind: &str) -> Value {
    let (before, after, task, delivered, snapshot) = match kind {
        "training" => ("HEAD~3", "HEAD~2", "TASK-TRAIN", 10, 5),
        "held-out" => ("HEAD~2", "HEAD~1", "TASK-TARGET", 30, 15),
        "future" => ("HEAD~1", "HEAD", "TASK-FUTURE", 40, 35),
        _ => panic!("unsupported delivery fixture {kind}"),
    };
    let title = if kind == "future" {
        "future_secret"
    } else {
        "parser validation"
    };
    json!({
        "schema_version": 2,
        "repository": repository.canonicalize().expect("canonical repository"),
        "landing_branch": "main",
        "before_revision": git_stdout(repository, ["rev-parse", before]),
        "after_revision": git_stdout(repository, ["rev-parse", after]),
        "delivery_id": format!("fixture:{kind}"),
        "evidence": "verified_delivery",
        "source": {"system": "test-delivery-feed", "record_id": kind},
        "delivered_at": {
            "status": "known", "timestamp": format!("unix:{delivered}"),
            "source": {"system": "test-delivery-feed", "record_id": format!("{kind}:landed")}
        },
        "captured_at": format!("unix:{}", delivered + 1),
        "tasks": [task_snapshot(task, title, snapshot)]
    })
}

fn task_snapshot(task_id: &str, title: &str, captured: usize) -> Value {
    json!({
        "task_id": task_id,
        "title": title,
        "description": "update parser and its validation test",
        "acceptance_criteria": ["parser behavior and tests change together"],
        "source": {"system": "test-task-api", "record_id": format!("{task_id}@{captured}")},
        "created_at": {
            "status": "known", "timestamp": "unix:1",
            "source": {"system": "test-task-api", "record_id": task_id}
        },
        "snapshot_available_at": {
            "status": "known", "timestamp": format!("unix:{captured}"),
            "source": {"system": "test-task-api", "record_id": format!("{task_id}@{captured}")}
        },
        "text_availability": "known_pre_execution",
        "captured_at": format!("unix:{captured}")
    })
}

fn plugin_json(repository: &Path, tool: &str, input: Value) -> Value {
    let output = plugin_output_with_env(repository, tool, input, &[]);
    assert!(
        output.status.success(),
        "plugin {tool} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("plugin JSON")
}

fn plugin_output_with_env(
    repository: &Path,
    tool: &str,
    input: Value,
    environment: &[(&str, &std::ffi::OsStr)],
) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orbit-graph"));
    command
        .current_dir(repository)
        .env("ORBIT_TOOL_NAME", tool)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (key, value) in environment {
        command.env(key, value);
    }
    command
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child
                .stdin
                .as_mut()
                .expect("plugin stdin")
                .write_all(input.to_string().as_bytes())?;
            child.wait_with_output()
        })
        .expect("run plugin")
}

#[cfg(unix)]
fn plugin_json_with_tty_stdout(repository: &Path, tool: &str, input: Value) -> Value {
    let mut master_fd = -1;
    let mut slave_fd = -1;
    // SAFETY: `openpty` initializes both descriptors on success; ownership is
    // transferred immediately to standard library descriptor wrappers.
    let opened = unsafe {
        libc::openpty(
            &raw mut master_fd,
            &raw mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(opened, 0, "open plugin stdout pseudo-terminal");
    // SAFETY: successful `openpty` returned distinct, owned descriptors.
    let master = unsafe { fs::File::from_raw_fd(master_fd) };
    // SAFETY: successful `openpty` returned distinct, owned descriptors.
    let slave = unsafe { OwnedFd::from_raw_fd(slave_fd) };
    let mut child = Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
        .current_dir(repository)
        .env("ORBIT_TOOL_NAME", tool)
        .env("ORBIT_GRAPH_FORMAT", "table")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::from(slave))
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn plugin with TTY stdout");
    {
        use std::io::Write;
        child
            .stdin
            .as_mut()
            .expect("plugin stdin")
            .write_all(input.to_string().as_bytes())
            .expect("write plugin input");
    }
    drop(child.stdin.take());
    // A PTY has a finite kernel buffer. Drain it while the plugin is still
    // running so a larger JSON response cannot block the child before it
    // exits. Keeping the master in the reader also keeps the PTY alive until
    // the slave closes after the child has finished writing.
    let stdout_reader = std::thread::spawn(move || {
        let mut master = master;
        let mut stdout = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            match master.read(&mut buffer) {
                Ok(0) => return Ok(stdout),
                Ok(count) => stdout.extend_from_slice(&buffer[..count]),
                Err(error) if error.raw_os_error() == Some(libc::EIO) => return Ok(stdout),
                Err(error) => return Err(error),
            }
        }
    });
    let output = child.wait_with_output().expect("wait for TTY plugin");
    assert!(
        output.status.success(),
        "TTY plugin failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = stdout_reader
        .join()
        .expect("TTY stdout reader panicked")
        .expect("read plugin pseudo-terminal");
    serde_json::from_slice(&stdout).expect("TTY plugin JSON")
}

fn run<const N: usize>(repository: &Path, args: [&str; N]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
        .current_dir(repository)
        .args(["--format", "json"])
        .args(args)
        .output()
        .expect("run orbit-graph")
}

fn run_cli<const N: usize>(repository: &Path, args: [&str; N]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
        .current_dir(repository)
        .args(args)
        .output()
        .expect("run orbit-graph CLI")
}

fn run_git<const N: usize>(repository: &Path, args: [&str; N]) {
    let output = Command::new("git")
        .current_dir(repository)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout<const N: usize>(repository: &Path, args: [&str; N]) -> String {
    let output = Command::new("git")
        .current_dir(repository)
        .args(args)
        .output()
        .expect("run git");
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .expect("git UTF-8")
        .trim()
        .to_string()
}

fn metric_projection(report: &Value) -> Vec<Value> {
    report["metrics"]
        .as_array()
        .expect("metrics")
        .iter()
        .map(|metric| {
            json!({
                "variant": metric["variant"],
                "level": metric["level"],
                "k": metric["k"],
                "cases": metric["cases"],
                "relevant": metric["relevant"],
                "true_positives": metric["true_positives"],
                "recall_at_k": metric["recall_at_k"],
                "precision_at_k": metric["precision_at_k"],
                "stale_result_rate": metric["stale_result_rate"],
            })
        })
        .collect()
}

fn case_exclusions(report: &Value, index: usize) -> Vec<&str> {
    report["cases"][index]["exclusions"]
        .as_array()
        .expect("case exclusions")
        .iter()
        .filter_map(Value::as_str)
        .collect()
}
