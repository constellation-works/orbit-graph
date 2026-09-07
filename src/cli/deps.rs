use crate::Selector;
use clap::Args;
use serde_json::{Value, json};

use super::output::{Column, CommandOutput, TableView, View, ViewBlock};
use super::{CliError, CommandContext, json_value};

#[derive(Debug, Args)]
pub struct DepsCommand {
    /// File or directory selector (`file:…` or `dir:…`) whose outbound module/
    /// import edges to list. Reports source-level imports, not Cargo crate edges.
    selector: String,
}

pub(crate) fn output(document: Value) -> CommandOutput {
    let scope = super::display_value(&document["scope"]);
    let imports = document["imports"].as_array().cloned().unwrap_or_default();
    let mut table = TableView::new(vec![
        Column::path("from file"),
        Column::path("target path"),
        Column::text("symbol"),
    ]);
    for import in &imports {
        table.push_row([
            super::display_value(&import["from_file"]),
            super::display_value(&import["target_path"]),
            super::display_value(&import["target_symbol"]),
        ]);
    }
    let records = std::iter::once(json!({
        "record_type": "deps_context",
        "context": {"scope": scope.clone()}
    }))
    .chain(
        imports
            .into_iter()
            .map(|import| json!({"record_type": "import", "import": import})),
    )
    .collect();
    CommandOutput::with_view(
        document,
        View::Blocks(vec![ViewBlock::table(table.with_empty_message(format!(
            "no source imports found under {scope}"
        )))]),
    )
    .with_ndjson_records(records)
}

impl DepsCommand {
    pub(crate) fn run(&self, context: &CommandContext) -> Result<serde_json::Value, CliError> {
        let graph = context.open_graph()?;
        let selector = self.selector.parse::<Selector>()?;
        json_value(graph.deps(&selector)?)
    }
}
