//! Source inspection keeps machine bytes intact and human terminals inert.

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
fn show_escapes_terminal_controls_without_changing_machine_source() {
    let fixture = TempDir::new().expect("create source fixture");
    let root = fixture.path();
    let output = common::git_command(root)
        .args(["init", "-q", "-b", "main"])
        .output()
        .expect("initialize fixture");
    assert!(output.status.success());
    let source = "pub fn source() {\n\tlet value = \"\u{1b}[2J\u{7}\r\";\n}\n";
    fs::write(root.join("lib.rs"), source).expect("write source controls");
    let sync = Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
        .current_dir(root)
        .env_remove("ORBIT_TOOL_NAME")
        .args(["sync", "--full", "--json"])
        .output()
        .expect("sync source fixture");
    assert!(
        sync.status.success(),
        "{}",
        String::from_utf8_lossy(&sync.stderr)
    );
    for mode in ["auto", "table", "json", "ndjson"] {
        let output = Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
            .current_dir(root)
            .env_remove("ORBIT_TOOL_NAME")
            .args(["show", "file:lib.rs", "--format", mode])
            .output()
            .expect("show source fixture");
        assert!(output.status.success());
        if mode == "json" || mode == "ndjson" {
            let payload: Value = serde_json::from_slice(&output.stdout).expect("machine source");
            assert_eq!(payload["source"], source);
        } else {
            assert!(
                !output
                    .stdout
                    .iter()
                    .any(|byte| matches!(byte, 0x1b | 0x07 | 0x0d)),
                "{mode} output emitted raw terminal controls"
            );
            let rendered = String::from_utf8(output.stdout).expect("human source");
            assert!(rendered.contains("\\x1B[2J\\x07\\r"));
            assert!(
                rendered.contains("pub fn source() {\n\tlet value"),
                "source layout remains readable"
            );
        }
    }
}
