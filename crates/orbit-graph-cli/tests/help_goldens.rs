//! Public help and a representative machine payload, captured from the real
//! executable. Update explicitly with UPDATE_GOLDENS=1 and review the bytes.

#![allow(clippy::expect_used)]
#![allow(
    clippy::disallowed_methods,
    reason = "the explicit golden-update mode writes checked-in fixtures"
)]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const COMMANDS: &[&[&str]] = &[
    &[],
    &["sync"],
    &["search"],
    &["show"],
    &["refs"],
    &["callees"],
    &["impact"],
    &["changes"],
    &["recommend"],
    &["history"],
    &["history", "import"],
    &["history", "sync"],
    &["history", "status"],
    &["history", "rebuild"],
    &["evaluate"],
    &["trace"],
    &["overview"],
    &["implementors"],
    &["deps"],
    &["db-path"],
    &["clean"],
    &["version"],
];

#[test]
fn every_command_help_matches_the_public_golden() {
    let cwd = TempDir::new().expect("create isolated working directory");
    for command in COMMANDS {
        let output = run(cwd.path(), command, &["--help"]);
        assert!(output.status.success(), "{command:?}");
        assert!(output.stderr.is_empty(), "{command:?}");
        assert!(!output.stdout.contains(&0x1b), "help has no ANSI styling");
        let name = if command.is_empty() {
            "root".to_owned()
        } else {
            command.join("-")
        };
        check_golden(&format!("{name}.txt"), &output.stdout);
    }
    assert!(
        !cwd.path().join(".orbit-graph").exists(),
        "help creates no graph state"
    );
}

#[test]
fn version_json_matches_the_public_golden() {
    let cwd = TempDir::new().expect("create isolated working directory");
    git2::Repository::init(cwd.path()).expect("anchor repository discovery");
    let output = run(cwd.path(), &["version"], &["--json"]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    check_golden("version.json", &output.stdout);
    assert!(!cwd.path().join(".orbit-graph").exists());
}

fn run(cwd: &Path, command: &[&str], args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
        .current_dir(cwd)
        .env_remove("ORBIT_TOOL_NAME")
        .env_remove("ORBIT_GRAPH_FORMAT")
        .env_remove("ORBIT_GRAPH_LOCK_TIMEOUT_MS")
        .env_remove("ORBIT_GRAPH_FAULT_INJECT")
        .env("TERM", "dumb")
        .env("COLUMNS", "1")
        .env("NO_COLOR", "1")
        .args(command)
        .args(args)
        .output()
        .expect("run public CLI surface")
}

fn check_golden(name: &str, actual: &[u8]) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/help")
        .join(name);
    if std::env::var("UPDATE_GOLDENS").as_deref() == Ok("1") {
        fs::create_dir_all(path.parent().expect("golden directory"))
            .expect("create golden directory");
        fs::write(&path, actual).expect("write explicit golden update");
    }
    let expected = fs::read(&path).expect("read public golden; regenerate with UPDATE_GOLDENS=1");
    assert_eq!(
        actual,
        expected,
        "public CLI surface changed: {}",
        path.display()
    );
}
