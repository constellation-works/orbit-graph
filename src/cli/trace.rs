use crate::{DEFAULT_TRACE_DEPTH, RefConfidence};
use clap::{Args, ValueEnum};
use serde_json::{Value, json};

use super::output::{Column, CommandOutput, TableView, View, ViewBlock};
use super::{CliError, CommandContext, json_value};

#[derive(Debug, Args)]
pub struct TraceCommand {
    command_name: String,
    #[arg(long, default_value_t = DEFAULT_TRACE_DEPTH)]
    depth: u8,
    /// Minimum resolution confidence floor (default: same_module).
    ///
    /// The default follows precise edges only. Cross-crate edges routed
    /// through `pub use` re-exports resolve at `fuzzy_name`; pass
    /// `--confidence fuzzy` to follow them while tracing.
    #[arg(long, value_enum, default_value_t = ConfidenceArg::SameModule)]
    confidence: ConfidenceArg,
}

pub(crate) fn output(document: Value) -> CommandOutput {
    let mut table = TableView::new(vec![
        Column::number("depth"),
        Column::text("name"),
        Column::text("qualified"),
        Column::fixed("confidence"),
        Column::text("traversal"),
    ]);
    if let Some(root) = document.get("root").filter(|value| !value.is_null()) {
        append_trace_rows(&mut table, root, 0, &[]);
    }
    let mut context = document.clone();
    let root = context
        .as_object_mut()
        .and_then(|object| object.remove("root"));
    let mut records = vec![json!({"record_type": "trace_context", "context": context})];
    if let Some(root) = root.filter(|value| !value.is_null()) {
        records.push(json!({"record_type": "trace_root", "root": root}));
    }
    CommandOutput::with_view(
        document,
        View::Blocks(vec![ViewBlock::table(
            table.with_empty_message("command handler was not found in the graph"),
        )]),
    )
    .with_ndjson_records(records)
}

fn append_trace_rows(table: &mut TableView, node: &Value, depth: usize, ancestors: &[String]) {
    let name = super::display_value(&node["name"]);
    let mut traversal = ancestors.to_vec();
    traversal.push(name.clone());
    table.push_row([
        depth.to_string(),
        name,
        super::display_value(&node["qualified_name"]),
        super::display_value(&node["confidence"]),
        traversal.join(" > "),
    ]);
    if let Some(children) = node["children"].as_array() {
        for child in children {
            append_trace_rows(table, child, depth + 1, &traversal);
        }
    }
}

impl TraceCommand {
    pub(crate) fn run(&self, context: &CommandContext) -> Result<serde_json::Value, CliError> {
        let graph = context.open_graph()?;
        json_value(graph.trace(
            normalize_command_selector(self.command_name.as_str()),
            self.depth,
            self.confidence.into_graph(),
        )?)
    }
}

fn normalize_command_selector(command: &str) -> &str {
    command
        .trim()
        .strip_prefix("command:")
        .map(str::trim)
        .unwrap_or_else(|| command.trim())
}

#[derive(Debug, Clone, Copy, ValueEnum)]
#[clap(rename_all = "snake_case")]
enum ConfidenceArg {
    Exact,
    #[value(alias = "import_resolved")]
    Import,
    SameModule,
    #[value(alias = "fuzzy_name")]
    Fuzzy,
}

impl ConfidenceArg {
    fn into_graph(self) -> RefConfidence {
        match self {
            Self::Exact => RefConfidence::Exact,
            Self::Import => RefConfidence::ImportResolved,
            Self::SameModule => RefConfidence::SameModule,
            Self::Fuzzy => RefConfidence::FuzzyName,
        }
    }
}
