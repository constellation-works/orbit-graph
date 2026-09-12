//! Registered-command coverage, checked against the parser the executable
//! builds. The binary has no library target, so these assertions live in the
//! crate rather than in `tests/cli_smoke.rs`.

use clap::CommandFactory;

use crate::command::Cli;

#[test]
fn every_registered_command_appears_in_the_top_level_help() {
    let help = Cli::command().render_help().to_string();
    for command in Cli::command()
        .get_subcommands()
        .map(|command| command.get_name())
    {
        assert!(
            help.lines()
                .any(|line| { line.split_whitespace().next() == Some(command) }),
            "registered command missing from help: {command}"
        );
    }
}

#[test]
fn registered_command_inventory_matches_the_compatibility_matrix() {
    const MATRIX_PATHS: &[&str] = &[
        "sync",
        "history",
        "history import",
        "history sync",
        "history status",
        "history rebuild",
        "recommend",
        "evaluate",
        "search",
        "show",
        "refs",
        "callees",
        "impact",
        "trace",
        "overview",
        "implementors",
        "deps",
        "db-path",
        "clean",
        "version",
    ];

    fn collect_paths(command: &clap::Command, prefix: &str, paths: &mut Vec<String>) {
        for subcommand in command.get_subcommands() {
            let path = if prefix.is_empty() {
                subcommand.get_name().to_owned()
            } else {
                format!("{prefix} {}", subcommand.get_name())
            };
            paths.push(path.clone());
            collect_paths(subcommand, path.as_str(), paths);
        }
    }

    let mut registered = Vec::new();
    collect_paths(&Cli::command(), "", &mut registered);
    registered.sort();
    let mut matrix = MATRIX_PATHS
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    matrix.sort();
    assert_eq!(
        matrix, registered,
        "update the executable compatibility matrix"
    );
}
