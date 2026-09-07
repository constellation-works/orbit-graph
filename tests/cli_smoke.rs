//! Smoke tests that exercise the packaged `orbit-graph` executable.

#![allow(clippy::expect_used)]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tempfile::TempDir;

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
    let output = run(fixture.path(), ["show", "not-a-selector"]);

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
    assert!(String::from_utf8_lossy(&output.stdout).contains("Usage: orbit-graph <COMMAND>"));
    assert!(output.stderr.is_empty());

    let bare = run(fixture.path(), []);
    assert!(bare.status.success());
    assert_eq!(bare.stdout, output.stdout);
    assert!(bare.stderr.is_empty());
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

    let bad_limit = run(
        fixture.path(),
        ["recommend", "--query", "helper", "--limit", "0"],
    );
    assert!(!bad_limit.status.success());
    let error: Value = serde_json::from_slice(&bad_limit.stderr).expect("JSON error");
    assert_eq!(error["error"]["code"], "graph_error");

    let both = run(
        fixture.path(),
        ["recommend", "--query", "helper", "--task-id", "TASK-1"],
    );
    assert!(!both.status.success());
    let error: Value = serde_json::from_slice(&both.stderr).expect("JSON error");
    assert_eq!(error["error"]["code"], "argument_error");
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

    let synced = run_json(
        fixture.path(),
        ["history", "sync", "--branch", "main", "--limit", "10"],
    );
    assert_eq!(synced["complete"], true);
    let status = run_json(fixture.path(), ["history", "status", "--branch", "main"]);
    assert_eq!(status["schema_version"], 3);
    assert_eq!(status["extractor_version"], 2);
    assert_eq!(status["verified_deliveries"], 1);
    assert_eq!(status["git_only_deliveries"], 1);
    assert_eq!(status["task_associations"], 2);

    let rebuilt = run_json(
        fixture.path(),
        ["history", "rebuild", "--branch", "main", "--limit", "10"],
    );
    assert_eq!(rebuilt["removed_deliveries"], 2);
    assert_eq!(rebuilt["sync"]["deliveries_inserted"], 1);
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
    let output = run(
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
    let output = run(
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
    let output = run(cwd, args);
    assert!(
        output.status.success(),
        "orbit-graph failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON command output")
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
    Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("run orbit-graph")
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
        "pub fn helper() -> i32 { 1 }\n\npub fn entry() -> i32 { helper() }\n",
    )
    .expect("write fixture source");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "fixture"]);
    fixture
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
