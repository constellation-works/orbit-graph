use std::ffi::OsString;

use crate::output::sink::{
    FormatArg, OutputMode, OutputSink, SinkEnvironment, requested_format_from_args,
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
    assert!(terminal.color_allowed());

    let redirected = OutputSink::resolve(
        false,
        &SinkEnvironment {
            columns: Some("120".to_owned()),
            clicolor_force: Some("1".to_owned()),
            ..SinkEnvironment::default()
        },
        Some(80),
        None,
    );
    assert_eq!(redirected.mode(), OutputMode::Plain);
    assert_eq!(redirected.width(), 0);
    assert!(!redirected.color_allowed());
}

#[test]
fn color_controls_and_explicit_mode_precedence_are_centralized() {
    for environment in [
        SinkEnvironment {
            no_color: Some("1".to_owned()),
            ..SinkEnvironment::default()
        },
        SinkEnvironment {
            term: Some("dumb".to_owned()),
            clicolor_force: Some("1".to_owned()),
            ..SinkEnvironment::default()
        },
    ] {
        assert!(!OutputSink::resolve(true, &environment, Some(80), None).color_allowed());
    }

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
fn rejected_argv_scan_does_not_confuse_overview_detail_format() {
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
        requested_format_from_args(&args(&["orbit-graph", "overview", "--format", "json",])),
        None
    );
    assert_eq!(
        requested_format_from_args(&args(&["orbit-graph", "refs", "--format=json",])),
        Some(FormatArg::Json)
    );
}
