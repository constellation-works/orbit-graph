//! The real plugin executable bounds its request before parsing or dispatch.

#![allow(clippy::expect_used)]

use std::io::{Seek, Write};
use std::process::{Command, Stdio};

use serde_json::Value;
use tempfile::TempDir;

const REQUEST_LIMIT: usize = 1024 * 1024;

fn version_request(bytes: usize) -> (TempDir, std::process::Output) {
    let root = TempDir::new().expect("isolated plugin root");
    let mut input = tempfile::tempfile().expect("request file");
    let mut request = br#"{"schema_version":1,"tool":"orbit.graph.version","input":{}}"#.to_vec();
    request.resize(bytes, b' ');
    input.write_all(&request).expect("write request");
    input.rewind().expect("rewind request");
    let output = Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
        .env_clear()
        .env("ORBIT_TOOL_NAME", "orbit.graph.version")
        .env("HOME", root.path())
        .env("TMPDIR", root.path())
        .current_dir(root.path())
        .stdin(Stdio::from(input))
        .output()
        .expect("run plugin executable");
    (root, output)
}

#[test]
fn plugin_request_at_the_byte_limit_is_accepted() {
    let (_, output) = version_request(REQUEST_LIMIT);
    assert!(output.status.success(), "{output:?}");
    let response: Value = serde_json::from_slice(&output.stdout).expect("plugin envelope");
    assert_eq!(response["ok"], true, "{response}");
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn oversized_plugin_request_is_refused_before_dispatch() {
    let (root, output) = version_request(REQUEST_LIMIT + 1);
    assert!(output.status.success(), "structured errors exit zero");
    let response: Value = serde_json::from_slice(&output.stdout).expect("plugin envelope");
    assert_eq!(response["ok"], false, "{response}");
    assert_eq!(response["error"]["code"], "invalid_request");
    assert_eq!(response["error"]["retryable"], false);
    let message = response["error"]["message"]
        .as_str()
        .expect("actionable error");
    assert!(message.contains("1048576"), "{message}");
    assert!(message.contains("reduce"), "{message}");
    assert_eq!(
        std::fs::read_dir(root.path()).expect("list root").count(),
        0
    );
}
