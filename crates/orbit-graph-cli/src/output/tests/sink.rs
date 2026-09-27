use std::ffi::OsString;

use clap::{Arg, Command};

use crate::command::CliError;
use crate::output::sink::{
    FormatArg, LegacyDetail, OutputMode, OutputSink, SinkEnvironment, install_format_argument,
    requested_format_from_args, requested_output,
};

#[test]
fn sink_resolves_terminal_and_redirected_invariants() {
    let terminal = OutputSink::resolve(
        true,
        &SinkEnvironment {
            columns: Some("96".to_owned()),
            ..SinkEnvironment::default()
        },
        Some(80),
        None,
    );
    assert_eq!(terminal.mode(), OutputMode::Table);
    assert_eq!(terminal.width(), 96);

    let redirected = OutputSink::resolve(
        false,
        &SinkEnvironment {
            columns: Some("120".to_owned()),
            ..SinkEnvironment::default()
        },
        Some(80),
        None,
    );
    assert_eq!(redirected.mode(), OutputMode::Plain);
    assert_eq!(redirected.width(), 0);
}

#[test]
fn explicit_mode_outranks_the_environment() {
    let environment = SinkEnvironment {
        format: Some("json".to_owned()),
        ..SinkEnvironment::default()
    };
    assert_eq!(
        OutputSink::resolve(false, &environment, None, Some(FormatArg::Table)).mode(),
        OutputMode::Table
    );
    assert_eq!(
        OutputSink::resolve(false, &environment, None, None).mode(),
        OutputMode::Json
    );
}

#[test]
fn rejected_argv_scan_reads_json_and_format_at_any_level() {
    let args = |values: &[&str]| values.iter().map(OsString::from).collect::<Vec<_>>();
    assert_eq!(
        requested_format_from_args(&args(&[
            "orbit-graph",
            "--format",
            "json",
            "overview",
            "--format",
            "full",
        ])),
        Some(FormatArg::Json)
    );
    assert_eq!(
        requested_format_from_args(&args(&["orbit-graph", "overview", "--format", "json"])),
        Some(FormatArg::Json)
    );
    assert_eq!(
        requested_format_from_args(&args(&["orbit-graph", "refs", "--format=json"])),
        Some(FormatArg::Json)
    );
    assert_eq!(
        requested_format_from_args(&args(&["orbit-graph", "refs", "--json"])),
        Some(FormatArg::Json)
    );
}

fn test_command() -> Command {
    install_format_argument(
        Command::new("orbit-graph")
            .subcommand(Command::new("overview"))
            .subcommand(Command::new("refs").arg(Arg::new("selector"))),
    )
}

fn resolve(args: &[&str]) -> Result<crate::output::sink::OutputRequest, CliError> {
    let matches = test_command()
        .try_get_matches_from(args)
        .expect("arguments parse");
    requested_output(&matches)
}

#[test]
fn json_is_format_json_and_conflicts_with_another_mode() {
    for args in [
        &["orbit-graph", "--json", "refs"][..],
        &["orbit-graph", "refs", "--json"],
        &["orbit-graph", "--json", "refs", "--format", "json"],
    ] {
        assert_eq!(
            resolve(args).expect("resolve").format,
            Some(FormatArg::Json),
            "{args:?}"
        );
    }
    for args in [
        &["orbit-graph", "--json", "refs", "--format", "table"][..],
        &["orbit-graph", "--format", "ndjson", "refs", "--json"],
    ] {
        let error = resolve(args).expect_err("conflicting modes");
        assert_eq!(error.exit_code(), 2, "{args:?}");
        assert!(error.to_string().contains("--json conflicts"), "{error}");
    }
}

#[test]
fn only_overview_accepts_the_legacy_detail_values() {
    let request = resolve(&[
        "orbit-graph",
        "--format",
        "json",
        "overview",
        "--format",
        "full",
    ])
    .expect("legacy detail");
    assert_eq!(request.format, Some(FormatArg::Json));
    assert_eq!(request.legacy_detail, Some(LegacyDetail::Full));
    assert!(
        test_command()
            .try_get_matches_from(["orbit-graph", "refs", "--format", "full"])
            .is_err()
    );
    assert!(
        test_command()
            .try_get_matches_from(["orbit-graph", "--format", "summary", "refs"])
            .is_err()
    );
}
