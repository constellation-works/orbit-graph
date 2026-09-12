//! Borderless table rendering: a header row followed by exactly one line per
//! record, and complete tab-separated records when stdout is redirected.
//!
//! Width and truncation come from the [`OutputSink`] the renderer passes in, so
//! the table module never reads terminal state itself.

use std::io::Write;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::command::CliError;
use crate::output::sink::{OutputMode, OutputSink};

/// Column metadata used by the shared table renderer.
#[derive(Clone, Debug)]
pub struct Column {
    heading: String,
    alignment: Alignment,
    width_policy: WidthPolicy,
    truncation: Truncation,
}

impl Column {
    /// Construct a left-aligned text column.
    pub fn text(heading: impl Into<String>) -> Self {
        Self {
            heading: heading.into(),
            alignment: Alignment::Left,
            width_policy: WidthPolicy::Flexible,
            truncation: Truncation::Tail,
        }
    }

    /// Construct a left-aligned column that is never shrunk or dropped.
    pub fn fixed(heading: impl Into<String>) -> Self {
        Self {
            heading: heading.into(),
            alignment: Alignment::Left,
            width_policy: WidthPolicy::Fixed,
            truncation: Truncation::Tail,
        }
    }

    /// Construct a flexible path-like column truncated through the middle.
    pub fn path(heading: impl Into<String>) -> Self {
        Self {
            heading: heading.into(),
            alignment: Alignment::Left,
            width_policy: WidthPolicy::Flexible,
            truncation: Truncation::Middle,
        }
    }

    /// Construct a right-aligned numeric column.
    pub fn number(heading: impl Into<String>) -> Self {
        Self {
            heading: heading.into(),
            alignment: Alignment::Right,
            width_policy: WidthPolicy::Fixed,
            truncation: Truncation::Tail,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Alignment {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WidthPolicy {
    Fixed,
    Flexible,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Truncation {
    Tail,
    Middle,
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

/// Render one table into the sink's human form, routing diagnostics to stderr.
pub(crate) fn emit_table(
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
    if sink.mode() == OutputMode::Plain {
        for row in &table.rows {
            let cells = row.iter().map(|cell| escape_cell(cell)).collect::<Vec<_>>();
            writeln!(stdout, "{}", cells.join("\t")).map_err(CliError::Stdout)?;
        }
        return Ok(());
    }

    let layout = table_layout(table, sink.width());
    if !layout.dropped.is_empty() {
        writeln!(
            stderr,
            "terminal width {}: omitted columns {} (use --format json for full values)",
            sink.width(),
            layout.dropped.join(", ")
        )
        .map_err(CliError::Stderr)?;
        stderr.flush().map_err(CliError::Stderr)?;
    }
    if let Some(required) = layout.fixed_width_required {
        writeln!(
            stderr,
            "terminal width {} is narrower than the {required} columns required by fixed fields",
            sink.width()
        )
        .map_err(CliError::Stderr)?;
        stderr.flush().map_err(CliError::Stderr)?;
    }
    write_table_row(
        stdout,
        &table
            .columns
            .iter()
            .map(|column| column.heading.to_uppercase())
            .collect::<Vec<_>>(),
        &table.columns,
        &layout,
    )?;
    for row in &table.rows {
        write_table_row(stdout, row, &table.columns, &layout)?;
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct TableLayout {
    pub(crate) indices: Vec<usize>,
    pub(crate) widths: Vec<usize>,
    pub(crate) dropped: Vec<String>,
    pub(crate) fixed_width_required: Option<usize>,
}

pub(crate) fn table_layout(table: &TableView, sink_width: u16) -> TableLayout {
    let natural_widths = table
        .columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            table.rows.iter().fold(
                display_width(&escape_cell(&column.heading)),
                |width, row| width.max(display_width(&escape_cell(&row[index]))),
            )
        })
        .collect::<Vec<_>>();
    let mut indices = (0..natural_widths.len()).collect::<Vec<_>>();
    let mut dropped = Vec::new();
    if sink_width == 0 || natural_widths.is_empty() {
        return TableLayout {
            indices,
            widths: natural_widths,
            dropped,
            fixed_width_required: None,
        };
    }
    let sink_width = usize::from(sink_width);
    let widths = loop {
        let mut widths = indices
            .iter()
            .map(|index| natural_widths[*index])
            .collect::<Vec<_>>();
        while rendered_width(&widths) > sink_width {
            let Some((position, _)) = widths
                .iter()
                .enumerate()
                .filter(|(position, width)| {
                    table.columns[indices[*position]].width_policy == WidthPolicy::Flexible
                        && **width > 8
                })
                .max_by_key(|(_, width)| **width)
            else {
                break;
            };
            widths[position] -= 1;
        }
        if rendered_width(&widths) <= sink_width {
            break widths;
        }
        let Some(position) = indices
            .iter()
            .rposition(|index| table.columns[*index].width_policy == WidthPolicy::Flexible)
        else {
            break widths;
        };
        let index = indices.remove(position);
        dropped.push(table.columns[index].heading.to_uppercase());
    };
    dropped.reverse();
    let required = rendered_width(&widths);
    TableLayout {
        indices,
        widths,
        dropped,
        fixed_width_required: (required > sink_width).then_some(required),
    }
}

fn rendered_width(widths: &[usize]) -> usize {
    widths.iter().sum::<usize>() + 2 * widths.len().saturating_sub(1)
}

fn write_table_row(
    writer: &mut dyn Write,
    cells: &[String],
    columns: &[Column],
    layout: &TableLayout,
) -> Result<(), CliError> {
    for (position, (index, width)) in layout.indices.iter().zip(&layout.widths).enumerate() {
        if position > 0 {
            writer.write_all(b"  ").map_err(CliError::Stdout)?;
        }
        let column = &columns[*index];
        let escaped = escape_cell(&cells[*index]);
        let cell = truncate(&escaped, *width, column.truncation);
        let padding = " ".repeat(width.saturating_sub(display_width(&cell)));
        match column.alignment {
            Alignment::Left => write!(writer, "{cell}{padding}").map_err(CliError::Stdout)?,
            Alignment::Right => write!(writer, "{padding}{cell}").map_err(CliError::Stdout)?,
        }
    }
    writer.write_all(b"\n").map_err(CliError::Stdout)
}

pub(crate) fn truncate(value: &str, width: usize, strategy: Truncation) -> String {
    if display_width(value) <= width {
        return value.to_owned();
    }
    if width <= 1 {
        return "…".to_owned();
    }
    match strategy {
        Truncation::Tail => truncate_tail(value, width),
        Truncation::Middle => truncate_middle(value, width),
    }
}

pub(crate) fn display_width(value: &str) -> usize {
    UnicodeWidthStr::width(value)
}

fn truncate_tail(value: &str, width: usize) -> String {
    let mut rendered = String::new();
    let mut used = 0;
    for grapheme in value.graphemes(true) {
        let grapheme_width = display_width(grapheme);
        if used + grapheme_width > width - 1 {
            break;
        }
        rendered.push_str(grapheme);
        used += grapheme_width;
    }
    rendered.push('…');
    rendered
}

fn truncate_middle(value: &str, width: usize) -> String {
    let graphemes = value.graphemes(true).collect::<Vec<_>>();
    let available = width - 1;
    let left_limit = available.div_ceil(2);
    let right_limit = available / 2;
    let mut left = String::new();
    let mut left_width = 0;
    let mut split = 0;
    for (index, grapheme) in graphemes.iter().enumerate() {
        let grapheme_width = display_width(grapheme);
        if left_width + grapheme_width > left_limit {
            break;
        }
        left.push_str(grapheme);
        left_width += grapheme_width;
        split = index + 1;
    }
    let mut right = Vec::new();
    let mut right_width = 0;
    for grapheme in graphemes[split..].iter().rev() {
        let grapheme_width = display_width(grapheme);
        if right_width + grapheme_width > right_limit {
            break;
        }
        right.push(*grapheme);
        right_width += grapheme_width;
    }
    right.reverse();
    format!("{left}…{}", right.concat())
}

fn escape_cell(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '\t' => escaped.push_str("\\t"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            character if character.is_control() && u32::from(character) <= 0xff => {
                use std::fmt::Write as _;
                let _ = write!(escaped, "\\x{:02X}", u32::from(character));
            }
            character if character.is_control() => {
                use std::fmt::Write as _;
                let _ = write!(escaped, "\\u{{{:X}}}", u32::from(character));
            }
            character => escaped.push(character),
        }
    }
    escaped
}
