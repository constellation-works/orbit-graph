//! Invalid history bounds are rejected before creating or opening state.

#![allow(clippy::expect_used)]
#![allow(
    clippy::disallowed_methods,
    reason = "fixtures use fs::write; shipped writes use atomic_write"
)]

mod common;

use std::fs;
use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;

#[test]
fn zero_history_limits_do_not_initialize_state() {
    for command in [
        vec!["history", "sync", "--branch", "main", "--limit", "0"],
        vec!["history", "rebuild", "--branch", "main", "--limit", "0"],
        vec![
            "history",
            "rebuild",
            "--branch",
            "main",
            "--limit",
            "0",
            "--confirm",
        ],
    ] {
        let fixture = TempDir::new().expect("create isolated repository");
        let root = fixture.path();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec![
                "-c",
                "user.name=Graph Test",
                "-c",
                "user.email=graph@example.invalid",
                "commit",
                "--allow-empty",
                "-q",
                "-m",
                "fixture",
            ],
        ] {
            let output = common::git_command(root)
                .args(args)
                .output()
                .expect("run fixture git");
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        assert!(!root.join(".orbit-graph").exists());
        let output = Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
            .current_dir(root)
            .env_remove("ORBIT_TOOL_NAME")
            .env_remove("ORBIT_GRAPH_FORMAT")
            .arg("--json")
            .args(&command)
            .output()
            .expect("run history CLI");
        assert_eq!(
            output.status.code(),
            Some(2),
            "{command:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_slice(&output.stderr).expect("flat JSON error");
        assert_eq!(error["code"], "invalid_input");
        assert!(
            error["error"]
                .as_str()
                .is_some_and(|message| message.contains("limit must be greater than zero"))
        );
        assert!(
            !root.join(".orbit-graph").exists(),
            "a rejected {command:?} left state behind: {:?}",
            fs::read_dir(root)
                .expect("read fixture root")
                .map(|entry| entry.expect("read entry").file_name())
                .collect::<Vec<_>>()
        );
    }
}
