//! Shared terminal output policy and rendering boundary.
//!
//! Commands return a JSON document plus a [`View`]. The document is the stable
//! machine contract; the view describes the human rendering without reading
//! terminal state or writing to a process stream. [`OutputSink`] resolves that
//! process state once and is the only value a renderer needs.

use std::ffi::OsString;
use std::io::{self, IsTerminal, Write};

use clap::{Arg, ArgMatches, Command, ValueEnum};
use serde::Serialize;
use serde_json::Value;

use super::CliError;

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

/// A command's human rendering, kept separate from its stable JSON document.
#[derive(Debug, Default)]
pub enum View {
    /// Temporary compatibility boundary for commands awaiting a human view.
    /// The complete JSON document is rendered without changing its schema.
    #[default]
    Document,
    /// Human prose and tables rendered in order.
    Blocks(Vec<ViewBlock>),
}

/// One block in a human view.
#[derive(Debug)]
pub enum ViewBlock {
    /// Human-readable text written as supplied, followed by a newline.
    Text(String),
    /// A shared borderless table/plain-record representation.
    Table(TableView),
}

impl ViewBlock {
    /// Construct a prose block.
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }

    /// Construct a table block.
    pub fn table(table: TableView) -> Self {
        Self::Table(table)
    }
}

/// Column metadata used by the shared table renderer.
#[derive(Clone, Debug)]
pub struct Column {
    heading: String,
    alignment: Alignment,
}

impl Column {
    /// Construct a left-aligned text column.
    pub fn text(heading: impl Into<String>) -> Self {
        Self {
            heading: heading.into(),
            alignment: Alignment::Left,
        }
    }

    /// Construct a right-aligned numeric column.
    pub fn number(heading: impl Into<String>) -> Self {
        Self {
            heading: heading.into(),
            alignment: Alignment::Right,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Alignment {
    Left,
    Right,
}

/// A borderless table whose redirected form is complete tab-separated records.
#[derive(Debug)]
pub struct TableView {
    columns: Vec<Column>,
    rows: Vec<Vec<String>>,
    empty_message: Option<String>,
}

impl TableView {
    /// Construct a table with its ordered columns.
    pub fn new(columns: Vec<Column>) -> Self {
        Self {
            columns,
            rows: Vec::new(),
            empty_message: None,
        }
    }

    /// Add one row. The cell count must equal the column count.
    pub fn push_row<I, S>(&mut self, cells: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let row: Vec<String> = cells.into_iter().map(Into::into).collect();
        assert_eq!(
            row.len(),
            self.columns.len(),
            "table row and column counts differ"
        );
        self.rows.push(row);
    }

    /// Set the diagnostic written to stderr when the table has no records.
    #[must_use]
    pub fn with_empty_message(mut self, message: impl Into<String>) -> Self {
        self.empty_message = Some(message.into());
        self
    }
}

/// A stable JSON document and its optional human/NDJSON projections.
#[derive(Debug)]
pub struct CommandOutput {
    document: Value,
    view: View,
    ndjson_records: Option<Vec<Value>>,
}

impl CommandOutput {
    /// Preserve the JSON document while a command awaits a dedicated view.
    pub fn document(document: Value) -> Self {
        Self {
            document,
            view: View::Document,
            ndjson_records: None,
        }
    }

    /// Attach a human view to a stable JSON document.
    pub fn with_view(document: Value, view: View) -> Self {
        Self {
            document,
            view,
            ndjson_records: None,
        }
    }

    /// Define the complete values that become NDJSON records.
    #[must_use]
    pub fn with_ndjson_records(mut self, records: Vec<Value>) -> Self {
        self.ndjson_records = Some(records);
        self
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

/// Render a command output to the two process streams selected by the contract.
pub fn emit(
    output: &CommandOutput,
    sink: OutputSink,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<(), CliError> {
    match sink.mode {
        OutputMode::Json => write_json(stdout, &output.document, sink.is_tty),
        OutputMode::Ndjson => {
            if let Some(records) = &output.ndjson_records {
                for record in records {
                    write_json(stdout, record, false)?;
                    stdout.flush().map_err(CliError::Stdout)?;
                }
                Ok(())
            } else {
                write_json(stdout, &output.document, false)
            }
        }
        OutputMode::Table | OutputMode::Plain => match &output.view {
            View::Document => write_json(stdout, &output.document, true),
            View::Blocks(blocks) => emit_blocks(blocks, sink, stdout, stderr),
        },
    }
}

fn emit_blocks(
    blocks: &[ViewBlock],
    sink: OutputSink,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<(), CliError> {
    for block in blocks {
        match block {
            ViewBlock::Text(text) => {
                stdout
                    .write_all(text.as_bytes())
                    .map_err(CliError::Stdout)?;
                if !text.ends_with('\n') {
                    stdout.write_all(b"\n").map_err(CliError::Stdout)?;
                }
            }
            ViewBlock::Table(table) => emit_table(table, sink, stdout, stderr)?,
        }
    }
    stdout.flush().map_err(CliError::Stdout)
}

fn emit_table(
    table: &TableView,
    sink: OutputSink,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<(), CliError> {
    if table.rows.is_empty() {
        if let Some(message) = &table.empty_message {
            writeln!(stderr, "{message}").map_err(CliError::Stderr)?;
            stderr.flush().map_err(CliError::Stderr)?;
        }
        return Ok(());
    }
    if sink.mode == OutputMode::Plain {
        for row in &table.rows {
            writeln!(stdout, "{}", row.join("\t")).map_err(CliError::Stdout)?;
        }
        return Ok(());
    }

    let widths = table_widths(table, sink.width);
    write_table_row(
        stdout,
        &table
            .columns
            .iter()
            .map(|column| column.heading.to_uppercase())
            .collect::<Vec<_>>(),
        &table.columns,
        &widths,
    )?;
    for row in &table.rows {
        write_table_row(stdout, row, &table.columns, &widths)?;
    }
    Ok(())
}

fn table_widths(table: &TableView, sink_width: u16) -> Vec<usize> {
    let mut widths = table
        .columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            table
                .rows
                .iter()
                .fold(display_width(&column.heading), |width, row| {
                    width.max(display_width(&row[index]))
                })
        })
        .collect::<Vec<_>>();
    if sink_width == 0 || widths.is_empty() {
        return widths;
    }
    let gutters = 2 * widths.len().saturating_sub(1);
    let available = usize::from(sink_width).saturating_sub(gutters);
    while widths.iter().sum::<usize>() > available {
        let Some(width) = widths
            .iter_mut()
            .filter(|width| **width > 8)
            .max_by_key(|width| **width)
        else {
            break;
        };
        *width -= 1;
    }
    widths
}

fn write_table_row(
    writer: &mut dyn Write,
    cells: &[String],
    columns: &[Column],
    widths: &[usize],
) -> Result<(), CliError> {
    for (index, ((cell, column), width)) in cells.iter().zip(columns).zip(widths).enumerate() {
        if index > 0 {
            writer.write_all(b"  ").map_err(CliError::Stdout)?;
        }
        let cell = truncate(cell, *width);
        match column.alignment {
            Alignment::Left => write!(writer, "{cell:<width$}").map_err(CliError::Stdout)?,
            Alignment::Right => write!(writer, "{cell:>width$}").map_err(CliError::Stdout)?,
        }
    }
    writer.write_all(b"\n").map_err(CliError::Stdout)
}

fn truncate(value: &str, width: usize) -> String {
    if display_width(value) <= width {
        return value.to_owned();
    }
    if width <= 1 {
        return "…".to_owned();
    }
    value.chars().take(width - 1).collect::<String>() + "…"
}

fn display_width(value: &str) -> usize {
    value.chars().count()
}

fn write_json(writer: &mut dyn Write, value: &Value, pretty: bool) -> Result<(), CliError> {
    if pretty {
        serde_json::to_writer_pretty(&mut *writer, value).map_err(CliError::Json)?;
    } else {
        serde_json::to_writer(&mut *writer, value).map_err(CliError::Json)?;
    }
    writer.write_all(b"\n").map_err(CliError::Stdout)?;
    writer.flush().map_err(CliError::Stdout)
}

/// Write a command failure to stderr in the sink's selected protocol.
pub fn emit_error(error: &CliError, sink: OutputSink) {
    let mut stderr = io::stderr().lock();
    if sink.structured_errors() {
        let _ = serde_json::to_writer(&mut stderr, &ErrorPayload::from(error));
        let _ = stderr.write_all(b"\n");
    } else if let CliError::Clap(error) = error {
        let _ = write!(stderr, "{error}");
    } else {
        let _ = writeln!(stderr, "error: {error}");
    }
    let _ = stderr.flush();
}

#[derive(Debug, Serialize)]
struct ErrorPayload<'a> {
    error: ErrorBody,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<&'a str>,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
}

impl<'a> From<&'a CliError> for ErrorPayload<'a> {
    fn from(error: &'a CliError) -> Self {
        Self {
            error: ErrorBody {
                code: error.code(),
                message: error.to_string(),
            },
            details: error.details(),
        }
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

#[cfg(test)]
mod tests {
    use std::io;

    use serde_json::json;

    use super::*;

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

    #[test]
    fn redirected_table_is_complete_plain_records() {
        let mut table = TableView::new(vec![Column::text("path"), Column::number("count")]);
        table.push_row(["a/very/long/path.rs", "12"]);
        let output = CommandOutput::with_view(
            json!({"records": [{"path": "a/very/long/path.rs", "count": 12}]}),
            View::Blocks(vec![ViewBlock::table(table)]),
        );
        let sink = OutputSink::resolve(false, &SinkEnvironment::default(), None, None);
        let mut stdout = Vec::new();
        emit(&output, sink, &mut stdout, &mut Vec::new()).expect("render plain output");
        assert_eq!(stdout, b"a/very/long/path.rs\t12\n");
    }

    #[test]
    fn empty_table_routes_its_diagnostic_to_stderr() {
        let table =
            TableView::new(vec![Column::text("path")]).with_empty_message("no matching paths");
        let output = CommandOutput::with_view(
            json!({"records": []}),
            View::Blocks(vec![ViewBlock::table(table)]),
        );
        let sink = OutputSink::resolve(false, &SinkEnvironment::default(), None, None);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        emit(&output, sink, &mut stdout, &mut stderr).expect("render empty output");
        assert!(stdout.is_empty());
        assert_eq!(stderr, b"no matching paths\n");
    }

    #[test]
    fn ndjson_uses_declared_record_units() {
        let output = CommandOutput::document(json!({"records": [1, 2]}))
            .with_ndjson_records(vec![json!({"value": 1}), json!({"value": 2})]);
        let sink = OutputSink::resolve(
            false,
            &SinkEnvironment::default(),
            None,
            Some(FormatArg::Ndjson),
        );
        let mut stdout = Vec::new();
        emit(&output, sink, &mut stdout, &mut Vec::new()).expect("render NDJSON records");
        assert_eq!(stdout, b"{\"value\":1}\n{\"value\":2}\n");
    }

    #[test]
    fn broken_stdout_pipe_is_classified_for_silent_success() {
        struct BrokenWriter;
        impl Write for BrokenWriter {
            fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
                Err(io::Error::from(io::ErrorKind::BrokenPipe))
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let output = CommandOutput::document(json!({"ok": true}));
        let sink = OutputSink::resolve(false, &SinkEnvironment::default(), None, None);
        let error = emit(&output, sink, &mut BrokenWriter, &mut Vec::new())
            .expect_err("broken pipe must reach the process boundary");
        assert!(error.is_broken_pipe());
    }
}
