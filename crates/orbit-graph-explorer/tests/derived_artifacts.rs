//! Parity checks for committed artifacts that claim to be generated from the
//! explorer binary (STD-04 §R11, §R13): the README's "Every flag" block and
//! the `direct-call` sample export. Each test runs the real built
//! `orbit-graph-explorer` and compares its output with the committed bytes.
//!
//! After an intended change to the help text or the report output, rerun
//! with `UPDATE_GOLDENS=1` to rewrite the committed copies, then review and
//! commit the diff:
//!
//! ```sh
//! UPDATE_GOLDENS=1 cargo test -p orbit-graph-explorer --test derived_artifacts --locked
//! ```

#![allow(clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

use common::corpus;

/// The `--generated-at` the committed `direct-call` sample was pinned to.
const SAMPLE_GENERATED_AT: &str = "2026-09-13T00:00:00Z";
const SAMPLE_NAME: &str = "direct-call";

const FLAGS_HEADING: &str = "## Every flag";
const TEXT_FENCE: &str = "```text\n";
const CLOSING_FENCE: &str = "\n```\n";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn updating() -> bool {
    std::env::var_os("UPDATE_GOLDENS").is_some_and(|value| value == "1")
}

/// Byte range of the text between the fences of the first ```text block
/// after the "Every flag" heading.
fn flags_block_range(readme: &str) -> std::ops::Range<usize> {
    let heading = readme
        .find(FLAGS_HEADING)
        .expect("README has an \"Every flag\" section");
    let open = heading
        + readme[heading..]
            .find(TEXT_FENCE)
            .expect("\"Every flag\" section has a ```text block")
        + TEXT_FENCE.len();
    let close = open
        + readme[open..]
            .find(CLOSING_FENCE)
            .expect("the ```text block is closed");
    open..close
}

#[test]
fn readme_every_flag_block_is_the_binary_help_output() {
    let output = Command::new(env!("CARGO_BIN_EXE_orbit-graph-explorer"))
        .arg("--help")
        .output()
        .expect("run orbit-graph-explorer --help");
    assert!(output.status.success(), "{output:?}");
    let help = String::from_utf8(output.stdout).expect("help is UTF-8");
    let help = help.trim_end_matches('\n');

    let readme_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md");
    let readme = fs::read_to_string(&readme_path).expect("read explorer README");
    let range = flags_block_range(&readme);

    if updating() {
        let mut updated = readme.clone();
        updated.replace_range(range, help);
        fs::write(&readme_path, updated).expect("rewrite explorer README");
        return;
    }
    let documented = &readme[range];
    if documented != help {
        let first_difference = documented
            .lines()
            .zip(help.lines())
            .enumerate()
            .find(|(_, (readme_line, help_line))| readme_line != help_line)
            .map_or_else(
                || "one is a prefix of the other".to_string(),
                |(index, (readme_line, help_line))| {
                    format!(
                        "line {} of the block:\n  README: {readme_line}\n  --help: {help_line}",
                        index + 1
                    )
                },
            );
        panic!(
            "crates/orbit-graph-explorer/README.md \"Every flag\" block differs from \
             `orbit-graph-explorer --help` at {first_difference}\n\
             Regenerate with UPDATE_GOLDENS=1 (see this file's header)."
        );
    }
}

#[test]
fn committed_direct_call_sample_is_a_fresh_regeneration() {
    let case = corpus::build_case(SAMPLE_NAME);
    let out = tempfile::tempdir().expect("create output directory");
    let output = Command::new(env!("CARGO_BIN_EXE_orbit-graph-explorer"))
        .args([
            "report",
            "--repo",
            case.repository.to_str().expect("utf8 repository path"),
            "--base",
            case.base(),
            "--head",
            case.head(),
            "--out",
            out.path().to_str().expect("utf8 output path"),
            "--name",
            SAMPLE_NAME,
            "--excerpts",
            "controlled",
            "--generated-at",
            SAMPLE_GENERATED_AT,
        ])
        .output()
        .expect("run orbit-graph-explorer report");
    assert!(output.status.success(), "{output:?}");

    let samples = repo_root().join("docs/evaluation/change-explorer/samples");
    let mut stale = Vec::new();
    for extension in ["json", "html"] {
        let file = format!("{SAMPLE_NAME}.{extension}");
        let fresh = fs::read(out.path().join(&file)).expect("read regenerated sample");
        let committed_path = samples.join(&file);
        if updating() {
            fs::write(&committed_path, &fresh).expect("rewrite committed sample");
            continue;
        }
        let committed = fs::read(&committed_path).expect("read committed sample");
        if committed != fresh {
            stale.push(file);
        }
    }
    assert!(
        stale.is_empty(),
        "docs/evaluation/change-explorer/samples/{{{}}} differ from a fresh `report` of the \
         direct-call fixture; regenerate with UPDATE_GOLDENS=1 (see this file's header)",
        stale.join(",")
    );
}
