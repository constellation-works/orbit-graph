use clap::Args;
use serde_json::Value;

use orbit_graph::{CalleeOpts, Selector};

use super::{CliError, CommandContext, json_value};
use crate::output::{Column, CommandOutput, TableView, View, ViewBlock};

#[derive(Debug, Args)]
pub struct CalleesCommand {
    symbol: String,
    /// Also list unresolved calls whose name has no indexed definition.
    ///
    /// By default those calls (typically standard-library and prelude calls
    /// such as `map_err`, `Ok`, or `to_string`) are omitted and counted in the
    /// JSON `hidden_unresolved` field and a stderr note.
    #[arg(long)]
    include_unresolved: bool,
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
    let hidden = document["hidden_unresolved"].as_u64().unwrap_or(0);
    let empty_message = if hidden == 0 {
        "symbol has no outbound calls".to_owned()
    } else {
        "symbol has no outbound calls with an indexed definition".to_owned()
    };
    let output = CommandOutput::with_view(
        document,
        View::Blocks(vec![ViewBlock::table(
            table.with_empty_message(empty_message),
        )]),
    )
    .with_ndjson_records(callees);
    if hidden == 0 {
        output
    } else {
        output.with_notice(format!(
            "{hidden} unresolved call(s) with no indexed definition hidden; \
             pass --include-unresolved to list them"
        ))
    }
}

impl CalleesCommand {
    pub(crate) fn run(&self, context: &CommandContext) -> Result<serde_json::Value, CliError> {
        // Input is validated before the index is opened.
        let selector = self.symbol.parse::<Selector>()?;
        let graph = context.open_graph()?;
        let opts = CalleeOpts {
            hide_unresolved: !self.include_unresolved,
            ..CalleeOpts::all()
        };
        json_value(graph.callees_report(&selector, &opts)?)
    }
}
