//! What a command hands back: the stable JSON document plus the human view of
//! the same records.
//!
//! A command body builds one of these and returns it. It does not choose a
//! format, does not read terminal state, and does not write to a process
//! stream; [`crate::output::render`] projects the payload into the mode the
//! sink resolved.

use serde_json::Value;

use crate::output::table::TableView;

/// A command's human rendering, kept separate from its stable JSON document.
#[derive(Debug)]
pub enum View {
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

/// A stable JSON document and its optional human/NDJSON projections.
#[derive(Debug)]
pub struct CommandOutput {
    pub(crate) document: Value,
    pub(crate) view: View,
    /// Optional source-only projection for plain output.
    pub(crate) plain_source: Option<String>,
    pub(crate) ndjson_records: Option<Vec<Value>>,
    /// Diagnostics written to stderr after the records, in every mode.
    pub(crate) notices: Vec<String>,
}

impl CommandOutput {
    /// Attach a human view to a stable JSON document.
    pub fn with_view(document: Value, view: View) -> Self {
        Self {
            document,
            view,
            plain_source: None,
            ndjson_records: None,
            notices: Vec::new(),
        }
    }

    /// Use only this source text in plain mode, preserving its layout and
    /// final newline while escaping terminal controls.
    #[must_use]
    pub fn with_plain_source(mut self, source: String) -> Self {
        self.plain_source = Some(source);
        self
    }

    /// Define the complete values that become NDJSON records.
    #[must_use]
    pub fn with_ndjson_records(mut self, records: Vec<Value>) -> Self {
        self.ndjson_records = Some(records);
        self
    }

    /// Add a stderr diagnostic, written in every output mode, such as how many
    /// rows a default filter omitted. The JSON document should carry the same
    /// fact as a field.
    #[must_use]
    pub fn with_notice(mut self, notice: impl Into<String>) -> Self {
        self.notices.push(notice.into());
        self
    }
}
