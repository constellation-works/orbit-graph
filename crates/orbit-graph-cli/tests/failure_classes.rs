//! Malformed persisted rows are command failures, distinct from bad CLI input.

#![allow(clippy::expect_used)]
#![allow(
    clippy::disallowed_methods,
    reason = "fixtures use fs::write; shipped writes use atomic_write"
)]

mod common;

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use rusqlite::Connection;
use serde_json::Value;
use tempfile::TempDir;

#[test]
fn corrupt_persisted_reference_tokens_remain_command_failures() {
    for column in ["kind", "confidence"] {
        let fixture = TempDir::new().expect("create fixture repository");
        let root = fixture.path();
        let output = common::git_command(root)
            .args(["init", "-q", "-b", "main"])
            .output()
            .expect("initialize git fixture");
        assert!(output.status.success());
        fs::write(
            root.join("lib.rs"),
            "pub fn helper() {}\npub fn entry() { helper(); }\n",
        )
        .expect("write source fixture");
        let sync = run(root, &["sync", "--full", "--json"]);
        assert!(
            sync.status.success(),
            "{}",
            String::from_utf8_lossy(&sync.stderr)
        );
        let report: Value = serde_json::from_slice(&sync.stdout).expect("parse sync report");
        let db = Connection::open(report["database_path"].as_str().expect("database path"))
            .expect("open fixture database");
        let changed = db
            .execute(
                &format!(
                    "UPDATE refs SET {column} = 'unknown_stored_token' WHERE target_name = 'helper'"
                ),
                [],
            )
            .expect("seed corrupt stored token");
        assert!(changed > 0);
        drop(db);
        let result = run(root, &["refs", "symbol:lib.rs#helper:function", "--json"]);
        assert_eq!(
            result.status.code(),
            Some(1),
            "persisted {column} corruption is not a usage error: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(result.stdout.is_empty());
        let error: Value = serde_json::from_slice(&result.stderr).expect("flat JSON error");
        assert_eq!(error["code"], "graph_error");
        assert!(
            error["error"]
                .as_str()
                .is_some_and(|message| message.contains("unknown_stored_token"))
        );
    }
}

fn run(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
        .current_dir(cwd)
        .env_remove("ORBIT_TOOL_NAME")
        .env_remove("ORBIT_GRAPH_FORMAT")
        .args(args)
        .output()
        .expect("run graph CLI")
}
