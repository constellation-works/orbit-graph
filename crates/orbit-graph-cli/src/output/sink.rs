//! The output sink: one resolved answer per invocation to "who is reading
//! this?".
//!
//! Resolved once from stdout, the process environment, and the shared
//! `--format` argument installed by [`install_format_argument`]. It is the only
//! module that reads terminal state, so no renderer re-derives these answers.

use std::ffi::OsString;
use std::io::{self, IsTerminal};

use clap::{Arg, ArgMatches, Command, ValueEnum};

const FORMAT_ARG_ID: &str = "output-format";

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
    /// `NO_COLOR`; any non-empty value disables styling.
    pub no_color: Option<String>,
    /// `CLICOLOR_FORCE`; any non-empty value enables styling on a terminal.
    pub clicolor_force: Option<String>,
    /// `TERM`; `dumb` disables styling.
    pub term: Option<String>,
}

impl SinkEnvironment {
    /// Read the sink-related process environment once.
    pub fn from_process() -> Self {
        Self {
            columns: read_var("COLUMNS"),
            format: read_var("ORBIT_GRAPH_FORMAT"),
            no_color: read_var("NO_COLOR"),
            clicolor_force: read_var("CLICOLOR_FORCE"),
            term: read_var("TERM"),
        }
    }
}

/// Terminal and mode decisions shared by success and error rendering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputSink {
    is_tty: bool,
    width: u16,
    color_allowed: bool,
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
            color_allowed: resolve_color(is_tty, environment),
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

    /// Whether human rendering may emit ANSI styling.
    // No renderer styles output yet; the decision stays resolved here, once,
    // and is exercised by the sink tests.
    #[allow(dead_code)]
    pub fn color_allowed(self) -> bool {
        self.color_allowed
    }

    /// The resolved output mode.
    pub fn mode(self) -> OutputMode {
        self.mode
    }

    /// Whether failures use the existing structured error envelope.
    pub fn structured_errors(self) -> bool {
        matches!(self.mode, OutputMode::Json | OutputMode::Ndjson)
    }
}

/// Add the shared `--format` argument at every command level that does not
/// already own that spelling. `overview --format summary|full` is therefore
/// unchanged; its output mode is selected at the root before `overview`.
pub fn install_format_argument(command: Command) -> Command {
    let children: Vec<String> = command
        .get_subcommands()
        .map(|child| child.get_name().to_owned())
        .collect();
    let declares_format = command
        .get_arguments()
        .any(|argument| argument.get_long() == Some("format"));
    let mut command = if declares_format {
        command
    } else {
        command.arg(
            Arg::new(FORMAT_ARG_ID)
                .long("format")
                .value_name("MODE")
                .value_parser(clap::value_parser!(FormatArg))
                .help("Output mode: auto, table, json, or ndjson (default: auto)"),
        )
    };
    for child in children {
        command = command.mut_subcommand(child, install_format_argument);
    }
    command
}

/// Return the deepest explicitly parsed shared output mode.
pub fn requested_format(matches: &ArgMatches) -> Option<FormatArg> {
    let mut level = matches;
    let mut requested = None;
    loop {
        if let Ok(Some(format)) = level.try_get_one::<FormatArg>(FORMAT_ARG_ID) {
            requested = Some(*format);
        }
        match level.subcommand() {
            Some((_, child)) => level = child,
            None => return requested,
        }
    }
}

/// Recover an explicit mode from argv when Clap rejects the invocation.
///
/// The scan intentionally stops interpreting `--format` after the `overview`
/// command token because that command owns the spelling for `summary|full`.
pub fn requested_format_from_args(args: &[OsString]) -> Option<FormatArg> {
    let mut requested = None;
    let mut overview = false;
    let mut index = 1;
    while index < args.len() {
        let token = args[index].to_string_lossy();
        if token == "overview" {
            overview = true;
        } else if !overview && token == "--format" {
            if let Some(value) = args.get(index + 1).and_then(|value| value.to_str()) {
                requested = parse_format(value);
                index += 1;
            }
        } else if !overview && let Some(value) = token.strip_prefix("--format=") {
            requested = parse_format(value);
        }
        index += 1;
    }
    requested
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

fn resolve_color(is_tty: bool, environment: &SinkEnvironment) -> bool {
    if !is_tty
        || environment.term.as_deref() == Some("dumb")
        || is_set(environment.no_color.as_deref())
    {
        return false;
    }
    if is_set(environment.clicolor_force.as_deref()) {
        return true;
    }
    true
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

fn is_set(value: Option<&str>) -> bool {
    value.is_some_and(|value| !value.is_empty())
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
