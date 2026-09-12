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

/// A stable JSON document and its optional human/NDJSON projections.
#[derive(Debug)]
pub struct CommandOutput {
    pub(crate) document: Value,
    pub(crate) view: View,
    pub(crate) ndjson_records: Option<Vec<Value>>,
}

impl CommandOutput {
    /// Preserve the JSON document while a command awaits a dedicated view.
    // Every command currently supplies a view; this constructor is the
    // documented boundary for the ones that do not, and is exercised by the
    // output tests.
    #[allow(dead_code)]
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
