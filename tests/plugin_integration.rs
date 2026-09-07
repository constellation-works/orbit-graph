//! Real-executable coverage for Orbit plugin and chronological evaluation paths.

#![allow(clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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
            "target_revision": git_stdout(fixture.path(), ["rev-parse", "HEAD~2"]),
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
        status["deliveries"], 2,
        "only explicit past/future training envelopes, never the held-out target, may enter history"
    );
    let target_revision = git_stdout(fixture.path(), ["rev-parse", "HEAD~2"]);
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
        "pub fn parser_test() -> bool { false }\n",
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
        "pub fn parser_test() -> bool { true }\n",
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
        "pub fn parser_test() -> bool { 2 + 2 == 4 }\n",
    )
    .expect("write held-out test");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "held out"]);
    fs::write(
        fixture.path().join("src/parser.rs"),
        "pub fn parse() -> bool { future_secret() }\npub fn future_secret() -> bool { true }\n",
    )
    .expect("write future parser");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "future"]);
    fixture
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
    let output = Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
        .current_dir(repository)
        .env("ORBIT_TOOL_NAME", tool)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
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
        .expect("run plugin");
    assert!(
        output.status.success(),
        "plugin {tool} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("plugin JSON")
}

fn run<const N: usize>(repository: &Path, args: [&str; N]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
        .current_dir(repository)
        .args(args)
        .output()
        .expect("run orbit-graph")
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
