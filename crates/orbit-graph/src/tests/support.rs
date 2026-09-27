//! Fixture helpers shared by the crate's unit tests (`STD-04 §R7`).

use std::path::Path;
use std::process::Command;

/// A `git` command run in `repo_root` that sees none of the host's Git setup.
///
/// Every inherited `GIT_*` variable is removed, so an exported `GIT_DIR` or
/// `GIT_WORK_TREE` cannot redirect it. The system and global config files are
/// not read, and `HOME` and `XDG_CONFIG_HOME` name a path inside the fixture
/// that does not exist, so no per-user excludes, attributes, hooks path or
/// signing setting applies. Commits get a fixed identity.
pub(crate) fn git_command(repo_root: &Path) -> Command {
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

/// Makes `root` a Git repository of its own, so repository discovery from
/// inside it stops there instead of walking up into whatever repository holds
/// the temporary directory.
///
/// The repository has no commits, so it selects the `HEAD` database family as
/// a directory outside Git does, and it ignores nothing.
pub(crate) fn set_discovery_boundary(root: &Path) {
    init_fixture_repository(root, "main");
}

/// Creates an empty repository at `root` whose unborn `HEAD` names `branch`.
///
/// It is created in process, so no host template, hook or default branch
/// applies, and its excludes file is the null device, so the host's global
/// excludes do not either.
pub(crate) fn init_fixture_repository(root: &Path, branch: &str) -> git2::Repository {
    let mut options = git2::RepositoryInitOptions::new();
    options.initial_head(branch);
    let repo = git2::Repository::init_opts(root, &options).expect("init fixture repository");
    repo.config()
        .and_then(|config| config.open_level(git2::ConfigLevel::Local))
        .and_then(|mut config| config.set_str("core.excludesFile", null_device()))
        .expect("disable host excludes for the fixture repository");
    repo
}

fn null_device() -> &'static str {
    if cfg!(windows) { "NUL" } else { "/dev/null" }
}
