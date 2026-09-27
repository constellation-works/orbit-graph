//! The output sink: one resolved answer per invocation to "who is reading
//! this?".
//!
//! Resolved once from stdout, the process environment, and the shared
//! `--format`/`--json` arguments installed by [`install_format_argument`]. It
//! is the only module that reads terminal state, so no renderer re-derives
//! these answers. Nothing is styled, so there is no color decision to make
//! (STD-01 §R17): clap is built without its `color` feature and log lines
//! carry no ANSI escapes.

use std::ffi::OsString;
use std::io::{self, IsTerminal};

use clap::builder::{PossibleValue, PossibleValuesParser, TypedValueParser};
use clap::{Arg, ArgAction, ArgMatches, Command, ValueEnum};

use crate::command::CliError;

const FORMAT_ARG_ID: &str = "output-format";
const JSON_ARG_ID: &str = "output-json";

/// The command whose `--format` also accepts the deprecated detail values.
const LEGACY_DETAIL_COMMAND: &str = "overview";

/// Help for the shared `--format` option. `auto` output that is redirected or
/// piped has no header row, so the help says how to get field names.
const FORMAT_HELP: &str = "Output mode: auto, table, json, or ndjson (default: auto). \
auto prints a headed table on a terminal but headerless tab-separated rows when piped; \
use table for a header or json/ndjson for named fields";

/// Help for the shared `--json` shorthand (STD-01 §R7).
const JSON_HELP: &str =
    "Shorthand for --format json; combining it with a different --format is an error";

/// A user-facing output mode before `auto` has been resolved against stdout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum FormatArg {
    /// Use a table on a terminal and complete plain output otherwise.
    Auto,
    /// Render the command's human view with headers and terminal adaptation.
    Table,
    /// Emit one JSON document.
    Json,
    /// Emit one complete JSON record per line.
    Ndjson,
}

/// A detail level passed through the deprecated `overview --format
/// summary|full` spelling, which now belongs to `overview --detail`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LegacyDetail {
    /// `--format summary`.
    Summary,
    /// `--format full`.
    Full,
}

impl LegacyDetail {
    /// The value as the user spelled it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Summary => "summary",
            Self::Full => "full",
        }
    }
}

/// One parsed value of the shared `--format` option.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FormatValue {
    Mode(FormatArg),
    Detail(LegacyDetail),
}

/// What the shared output arguments asked for, resolved once after parsing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OutputRequest {
    /// The explicit output mode, if any.
    pub format: Option<FormatArg>,
    /// A deprecated `overview --format summary|full` value, if one was given.
    pub legacy_detail: Option<LegacyDetail>,
}

/// The concrete rendering selected for one invocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputMode {
    /// A terminal-oriented human view.
    Table,
    /// A complete, unstyled, tab-separated human view for redirection.
    Plain,
    /// One JSON document.
    Json,
    /// One JSON document per record and line.
    Ndjson,
}

/// Environment inputs captured once so sink resolution is deterministic in tests.
#[derive(Clone, Debug, Default)]
pub struct SinkEnvironment {
    /// `COLUMNS`, used only for a terminal sink.
    pub columns: Option<String>,
    /// The standalone `ORBIT_GRAPH_FORMAT` override.
    pub format: Option<String>,
}

impl SinkEnvironment {
    /// Read the sink-related process environment once.
    pub fn from_process() -> Self {
        Self {
            columns: read_var("COLUMNS"),
            format: read_var("ORBIT_GRAPH_FORMAT"),
        }
    }
}

/// Terminal and mode decisions shared by success and error rendering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputSink {
    is_tty: bool,
    width: u16,
    mode: OutputMode,
}

impl OutputSink {
    /// Resolve a sink using stdout and the process environment.
    pub fn from_process(requested: Option<FormatArg>) -> Self {
        let is_tty = io::stdout().is_terminal();
        let terminal_width = is_tty.then(query_terminal_width).flatten();
        Self::resolve(
            is_tty,
            &SinkEnvironment::from_process(),
            terminal_width,
            requested,
        )
    }

    /// Resolve a sink from explicit inputs.
    pub fn resolve(
        is_tty: bool,
        environment: &SinkEnvironment,
        terminal_width: Option<u16>,
        requested: Option<FormatArg>,
    ) -> Self {
        Self {
            is_tty,
            width: resolve_width(is_tty, environment, terminal_width),
            mode: resolve_mode(is_tty, environment, requested),
        }
    }

    /// Whether stdout is a terminal.
    pub fn is_tty(self) -> bool {
        self.is_tty
    }

    /// Available terminal columns, or zero when output must not be truncated.
    pub fn width(self) -> u16 {
        self.width
    }

    /// The resolved output mode.
    pub fn mode(self) -> OutputMode {
        self.mode
    }

    /// Whether failures are written as a JSON error object.
    pub fn structured_errors(self) -> bool {
        matches!(self.mode, OutputMode::Json | OutputMode::Ndjson)
    }
}

/// Add the shared `--format` and `--json` arguments at every command level,
/// so either spelling is accepted at the root or after any command (STD-01
/// §R4, §R7). On `overview`, `--format` also accepts the hidden, deprecated
/// `summary` and `full` values of its former detail option (STD-01 §R35).
pub fn install_format_argument(command: Command) -> Command {
    let children: Vec<String> = command
        .get_subcommands()
        .map(|child| child.get_name().to_owned())
        .collect();
    let legacy_detail = command.get_name() == LEGACY_DETAIL_COMMAND;
    let mut command = command
        .arg(
            Arg::new(FORMAT_ARG_ID)
                .long("format")
                .value_name("MODE")
                .value_parser(format_value_parser(legacy_detail))
                .help(FORMAT_HELP),
        )
        .arg(
            Arg::new(JSON_ARG_ID)
                .long("json")
                .action(ArgAction::SetTrue)
                .help(JSON_HELP),
        );
    for child in children {
        command = command.mut_subcommand(child, install_format_argument);
    }
    command
}

fn format_value_parser(legacy_detail: bool) -> impl TypedValueParser<Value = FormatValue> {
    let mut values: Vec<PossibleValue> = FormatArg::value_variants()
        .iter()
        .filter_map(ValueEnum::to_possible_value)
        .collect();
    if legacy_detail {
        values.push(PossibleValue::new(LegacyDetail::Summary.as_str()).hide(true));
        values.push(PossibleValue::new(LegacyDetail::Full.as_str()).hide(true));
    }
    PossibleValuesParser::new(values).map(|value| match value.as_str() {
        "summary" => FormatValue::Detail(LegacyDetail::Summary),
        "full" => FormatValue::Detail(LegacyDetail::Full),
        other => FormatValue::Mode(parse_format(other).unwrap_or(FormatArg::Auto)),
    })
}

/// Resolve the shared output arguments once after parsing (STD-01 §R7, §R8).
///
/// The deepest explicit `--format` mode wins, as before. `--json` at any
/// level is `--format json`; given with a different explicit mode it is a
/// usage error rather than a silent precedence pick.
pub fn requested_output(matches: &ArgMatches) -> Result<OutputRequest, CliError> {
    let mut level = matches;
    let mut request = OutputRequest::default();
    let mut json = false;
    loop {
        match level.try_get_one::<FormatValue>(FORMAT_ARG_ID) {
            Ok(Some(FormatValue::Mode(format))) => request.format = Some(*format),
            Ok(Some(FormatValue::Detail(detail))) => request.legacy_detail = Some(*detail),
            _ => {}
        }
        json |= level.try_get_one::<bool>(JSON_ARG_ID).ok().flatten() == Some(&true);
        match level.subcommand() {
            Some((_, child)) => level = child,
            None => break,
        }
    }
    if json {
        if let Some(format) = request.format.filter(|format| *format != FormatArg::Json) {
            return Err(CliError::Usage(format!(
                "--json conflicts with --format {}; pass one output mode",
                format_name(format)
            )));
        }
        request.format = Some(FormatArg::Json);
    }
    Ok(request)
}

/// Recover an explicit mode from argv when Clap rejects the invocation, so a
/// usage error honors a requested machine mode (STD-01 §R19). `--json`
/// selects JSON; otherwise the last recognizable `--format` mode wins.
pub fn requested_format_from_args(args: &[OsString]) -> Option<FormatArg> {
    let mut requested = None;
    let mut json = false;
    let mut index = 1;
    while index < args.len() {
        let token = args[index].to_string_lossy();
        if token == "--json" {
            json = true;
        } else if token == "--format" {
            if let Some(value) = args.get(index + 1).and_then(|value| value.to_str()) {
                requested = parse_format(value).or(requested);
                index += 1;
            }
        } else if let Some(value) = token.strip_prefix("--format=") {
            requested = parse_format(value).or(requested);
        }
        index += 1;
    }
    if json {
        Some(FormatArg::Json)
    } else {
        requested
    }
}

fn resolve_width(is_tty: bool, environment: &SinkEnvironment, terminal_width: Option<u16>) -> u16 {
    if !is_tty {
        return 0;
    }
    environment
        .columns
        .as_deref()
        .and_then(parse_width)
        .or(terminal_width)
        .unwrap_or(0)
}

fn parse_width(raw: &str) -> Option<u16> {
    match raw.trim().parse::<u16>() {
        Ok(0) | Err(_) => None,
        Ok(width) => Some(width),
    }
}

fn resolve_mode(
    is_tty: bool,
    environment: &SinkEnvironment,
    requested: Option<FormatArg>,
) -> OutputMode {
    let format = requested
        .or_else(|| environment.format.as_deref().and_then(parse_format))
        .unwrap_or(FormatArg::Auto);
    match format {
        FormatArg::Auto if is_tty => OutputMode::Table,
        FormatArg::Auto => OutputMode::Plain,
        FormatArg::Table => OutputMode::Table,
        FormatArg::Json => OutputMode::Json,
        FormatArg::Ndjson => OutputMode::Ndjson,
    }
}

fn parse_format(raw: &str) -> Option<FormatArg> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "auto" => Some(FormatArg::Auto),
        "table" => Some(FormatArg::Table),
        "json" => Some(FormatArg::Json),
        "ndjson" => Some(FormatArg::Ndjson),
        _ => None,
    }
}

fn format_name(format: FormatArg) -> &'static str {
    match format {
        FormatArg::Auto => "auto",
        FormatArg::Table => "table",
        FormatArg::Json => "json",
        FormatArg::Ndjson => "ndjson",
    }
}

fn read_var(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

#[cfg(unix)]
fn query_terminal_width() -> Option<u16> {
    // SAFETY: `winsize` has no invalid bit patterns. `ioctl` writes it, and
    // the return value is checked before the populated field is read.
    let size = unsafe {
        let mut size: libc::winsize = std::mem::zeroed();
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &raw mut size) != 0 {
            return None;
        }
        size
    };
    (size.ws_col > 0).then_some(size.ws_col)
}

#[cfg(not(unix))]
fn query_terminal_width() -> Option<u16> {
    None
}
