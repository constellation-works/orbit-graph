use clap::Args;
use serde_json::Value;

use orbit_graph::Selector;

use super::{CliError, CommandContext, json_value};
use crate::output::{Column, CommandOutput, TableView, View, ViewBlock};

#[derive(Debug, Args)]
pub struct CalleesCommand {
    symbol: String,
}

pub(crate) fn output(document: Value) -> CommandOutput {
    let callees = document["callees"].as_array().cloned().unwrap_or_default();
    let mut table = TableView::new(vec![
        Column::text("target"),
        Column::text("call name"),
        Column::fixed("confidence"),
        Column::number("line"),
    ]);
    for edge in &callees {
        table.push_row([
            edge.get("target_qualified")
                .filter(|value| !value.is_null())
                .map_or_else(
                    || super::display_value(&edge["target_name"]),
                    super::display_value,
                ),
            super::display_value(&edge["target_name"]),
            super::display_value(&edge["confidence"]),
            super::display_value(&edge["line"]),
        ]);
    }
    CommandOutput::with_view(
        document,
        View::Blocks(vec![ViewBlock::table(
            table.with_empty_message("symbol has no outbound calls"),
        )]),
    )
    .with_ndjson_records(callees)
}

impl CalleesCommand {
    pub(crate) fn run(&self, context: &CommandContext) -> Result<serde_json::Value, CliError> {
        let graph = context.open_graph()?;
        let selector = self.symbol.parse::<Selector>()?;
        json_value(serde_json::json!({
            "callees": graph.callees(&selector)?,
        }))
    }
}
