//! Registered-command coverage, checked against the parser the executable
//! builds. The binary has no library target, so these assertions live in the
//! crate rather than in `tests/cli_smoke.rs`.

use clap::CommandFactory;

use crate::command::Cli;

#[test]
fn every_public_argument_has_help() {
    fn check(command: &clap::Command, path: &str) {
        for argument in command
            .get_arguments()
            .filter(|argument| !argument.is_hide_set())
        {
            assert!(
                argument
                    .get_help()
                    .or_else(|| argument.get_long_help())
                    .is_some_and(|help| !help.to_string().trim().is_empty()),
                "STD-01 §R22: {path} argument {} has no description",
                argument.get_id()
            );
        }
        for child in command.get_subcommands() {
            check(child, &format!("{path} {}", child.get_name()));
        }
    }
    check(&Cli::parser(), "orbit-graph");
}

#[test]
fn every_registered_command_appears_in_the_top_level_help() {
    let help = Cli::parser().render_help().to_string();
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
        "changes",
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

#[test]
fn composed_parser_validates_the_complete_grammar() {
    Cli::parser().debug_assert();
}

#[test]
fn grouped_help_uses_changed_parser_descriptions_and_keeps_new_commands_visible() {
    let parser = crate::output::install_format_argument(Cli::command())
        .mut_subcommand("sync", |command| {
            command.about("A changed parser description")
        })
        .subcommand(clap::Command::new("new-command").about("A new command description"));
    let help = crate::command::grouped_help(parser)
        .render_help()
        .to_string();
    assert!(help.contains("A changed parser description"));
    assert!(!help.contains("Update or rebuild the source graph index"));
    assert!(help.contains("new-command  A new command description"));
}
