//! The library suite does not depend on where the temporary directory is or
//! on the caller's Git environment (`STD-04 §R6`, `§R7`).

use std::fs;
use std::process::Command;

use super::support::git_command;

/// Runs this crate's unit tests in a child process whose `TMPDIR` lies inside
/// a committed repository that ignores it, and whose `GIT_DIR` names no
/// repository. The environment is set on the child command only.
///
/// Before ORB-13169, fixtures had no discovery boundary, so the library found
/// the outer repository and indexed nothing under its ignored directory, and
/// fixture `git` commands followed the exported `GIT_DIR`: 88 of 223 tests
/// failed this way.
#[test]
fn library_suite_passes_with_tmpdir_in_an_ignoring_repository_and_a_bogus_git_dir() {
    let outer = tempfile::tempdir().expect("create outer repository directory");
    let run_git = |args: &[&str]| {
        let output = git_command(outer.path())
            .args(args)
            .output()
            .expect("spawn git");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run_git(&["init", "-b", "main"]);
    fs::write(outer.path().join(".gitignore"), "scratch/\n").expect("write outer .gitignore");
    run_git(&["add", ".gitignore"]);
    run_git(&["commit", "-m", "ignore scratch"]);
    let scratch = outer.path().join("scratch");
    fs::create_dir(&scratch).expect("create ignored scratch directory");

    let suite = std::env::current_exe().expect("locate the unit-test binary");
    let output = Command::new(suite)
        .args([
            "--skip",
            "library_suite_passes_with_tmpdir_in_an_ignoring_repository",
        ])
        .env("TMPDIR", &scratch)
        .env("GIT_DIR", outer.path().join("not-a-repository"))
        // The suite's outside-Git case cannot run with TMPDIR inside a
        // repository; it skips visibly here and runs in the parent's CI job.
        .env_remove("CI")
        .output()
        .expect("run the unit-test binary");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "the library suite failed with TMPDIR={} and a bogus GIT_DIR:\n{}\n{}",
        scratch.display(),
        failures(&stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("test result: ok.") && !stdout.contains(" 0 passed;"),
        "the child ran no tests:\n{stdout}"
    );
}

/// The failure section of a libtest run, or all of it when there is none.
fn failures(stdout: &str) -> &str {
    stdout
        .find("\nfailures:\n")
        .map_or(stdout, |start| &stdout[start..])
}
