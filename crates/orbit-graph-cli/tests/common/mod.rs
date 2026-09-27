//! Fixture helpers shared by the executable's integration tests
//! (`STD-04 §R7`). Each test crate that runs Git declares `mod common;`.

use std::path::Path;
use std::process::Command;

/// A `git` command run in `repo_root` that sees none of the host's Git setup.
///
/// Every inherited `GIT_*` variable is removed, so an exported `GIT_DIR` or
/// `GIT_WORK_TREE` cannot redirect it. The system and global config files are
/// not read, and `HOME` and `XDG_CONFIG_HOME` name a path inside the fixture
/// that does not exist, so no per-user excludes, attributes, hooks path or
/// signing setting applies. Commits get a fixed identity.
pub fn git_command(repo_root: &Path) -> Command {
    let home = repo_root.join(".git").join("hermetic-home");
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_str().is_some_and(|key| key.starts_with("GIT_")) {
            command.env_remove(key);
        }
    }
    command
        .current_dir(repo_root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", null_device())
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &home)
        .env("GIT_AUTHOR_NAME", "Orbit Graph Test")
        .env("GIT_AUTHOR_EMAIL", "orbit-graph-test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Orbit Graph Test")
        .env("GIT_COMMITTER_EMAIL", "orbit-graph-test@example.invalid");
    command
}

fn null_device() -> &'static str {
    if cfg!(windows) { "NUL" } else { "/dev/null" }
}
