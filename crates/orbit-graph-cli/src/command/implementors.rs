use clap::Args;
use orbit_graph::Selector;
use serde_json::{Value, json};

use super::{CliError, CommandContext, json_value};
use crate::output::{Column, CommandOutput, TableView, View, ViewBlock};

#[derive(Debug, Args)]
pub struct ImplementorsCommand {
    /// Trait selector (`symbol:<file>#<Trait>:trait` or `module:<path>`). The
    /// trait's trailing name segment is matched against impl sites.
    selector: String,
}

pub(crate) fn output(document: Value) -> CommandOutput {
    let trait_name = super::display_value(&document["trait_name"]);
    let implementors = document["implementors"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut table = TableView::new(vec![
        Column::text("type"),
        Column::text("trait"),
        Column::fixed("kind"),
        Column::path("file"),
    ]);
    for item in &implementors {
        table.push_row([
            super::display_value(&item["type_qualified"]),
            super::display_value(&item["trait_matched"]),
            super::display_value(&item["kind"]),
            super::display_value(&item["file"]),
        ]);
    }
    let records = std::iter::once(json!({
        "record_type": "implementors_context",
        "context": {"trait_name": trait_name.clone()}
    }))
    .chain(
        implementors
            .into_iter()
            .map(|implementor| json!({"record_type": "implementor", "implementor": implementor})),
    )
    .collect();
    CommandOutput::with_view(
        document,
        View::Blocks(vec![ViewBlock::table(table.with_empty_message(format!(
            "no implementors found for {trait_name}"
        )))]),
    )
    .with_ndjson_records(records)
}

impl ImplementorsCommand {
    pub(crate) fn run(&self, context: &CommandContext) -> Result<serde_json::Value, CliError> {
        let graph = context.open_graph()?;
        let selector = self.selector.parse::<Selector>()?;
        json_value(graph.implementors(&selector)?)
    }
}
