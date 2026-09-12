//! Smoke tests that exercise the packaged `orbit-graph-explorer` executable.

#![allow(clippy::expect_used)]

use std::path::Path;
use std::process::{Command, Output};

mod common;

use common::{build_fixture, fingerprint_working_tree};

#[test]
fn help_succeeds_and_describes_the_snapshot_command() {
    let output = run(Path::new("."), &["--help"]);
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("snapshot --base"), "{stdout}");
    assert!(stdout.contains("not a stable machine contract"), "{stdout}");
}

#[test]
fn missing_arguments_fail_with_empty_stdout() {
    let output = run(Path::new("."), &["snapshot", "--base", "HEAD"]);
    assert!(!output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("`--head` is required"), "{stderr}");
}

#[test]
fn snapshot_diagnostic_reports_both_revisions_and_leaves_the_tree_alone() {
    let fixture = build_fixture();
    let before = fingerprint_working_tree(fixture.path());

    let output = run(
        fixture.path(),
        &[
            "snapshot",
            "--base",
            fixture.base.as_str(),
            "--head",
            fixture.head.as_str(),
            "--selector",
            "symbol:src/lib.rs#removed_helper:function",
        ],
    );
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("mode\tdirect_base_head"), "{stdout}");
    assert!(stdout.contains("working_tree\tclean"), "{stdout}");
    assert!(
        stdout.contains(format!("base\tref\t{}\tcommit\t{}", fixture.base, fixture.base).as_str()),
        "{stdout}"
    );
    assert!(
        stdout.contains(format!("head\tref\t{}\tcommit\t{}", fixture.head, fixture.head).as_str()),
        "{stdout}"
    );
    assert!(
        stdout
            .contains("base\tselector\tsymbol:src/lib.rs#removed_helper:function\tresolved\ttrue"),
        "{stdout}"
    );
    assert!(
        stdout
            .contains("head\tselector\tsymbol:src/lib.rs#removed_helper:function\tresolved\tfalse"),
        "{stdout}"
    );

    assert_eq!(
        fingerprint_working_tree(fixture.path()),
        before,
        "the explorer binary must not touch the user working tree"
    );
}

#[test]
fn dirty_working_tree_is_reported_by_the_binary() {
    let fixture = build_fixture();
    std::fs::write(fixture.path().join("scratch.rs"), "pub fn scratch() {}\n")
        .expect("add untracked file");

    let output = run(
        fixture.path(),
        &[
            "snapshot",
            "--base",
            fixture.base.as_str(),
            "--head",
            fixture.head.as_str(),
        ],
    );
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("working_tree\tdirty"), "{stdout}");
    assert!(stdout.contains("uncommitted change(s)"), "{stdout}");
}

fn run(current_dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orbit-graph-explorer"))
        .args(args)
        .current_dir(current_dir)
        .output()
        .expect("run orbit-graph-explorer")
}
