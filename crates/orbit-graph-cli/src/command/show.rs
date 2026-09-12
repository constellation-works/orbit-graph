use clap::Args;
use orbit_graph::DEFAULT_SHOW_MAX_BYTES;
use orbit_graph::Selector;
use serde_json::Value;

use super::{CliError, CommandContext, json_value};
use crate::output::{Column, CommandOutput, TableView, View, ViewBlock};

#[derive(Debug, Args)]
pub struct ShowCommand {
    selector: String,
    #[arg(long, default_value_t = DEFAULT_SHOW_MAX_BYTES)]
    max_bytes: usize,
}

pub(crate) fn output(document: Value) -> CommandOutput {
    let mut metadata = TableView::new(vec![
        Column::fixed("kind"),
        Column::text("name"),
        Column::text("qualified"),
        Column::path("file"),
        Column::fixed("span (bytes)"),
        Column::fixed("truncated"),
    ]);
    let mut blocks = Vec::new();
    if let Some(object) = document.as_object() {
        let details = &object["metadata"];
        metadata.push_row([
            super::display_value(&details["kind"]),
            super::display_value(&details["name"]),
            super::display_value(&details["qualified"]),
            super::display_value(&details["file"]),
            format!(
                "{}..{}",
                super::display_value(&details["span"]["start"]),
                super::display_value(&details["span"]["end"])
            ),
            super::display_value(&details["truncated"]),
        ]);
        blocks.push(ViewBlock::table(metadata));
        let source = object.get("source").map_or_else(
            || "Source is not UTF-8; use --format json for the complete byte payload.".to_owned(),
            |source| format!("Source:\n{}", super::display_value(source)),
        );
        blocks.push(ViewBlock::text(source));
    } else {
        blocks.push(ViewBlock::table(
            metadata.with_empty_message("selector did not resolve to indexed source"),
        ));
    }
    CommandOutput::with_view(document.clone(), View::Blocks(blocks))
        .with_ndjson_records(vec![document])
}

impl ShowCommand {
    pub(crate) fn run(&self, context: &CommandContext) -> Result<serde_json::Value, CliError> {
        let graph = context.open_graph()?;
        let selector = self.selector.parse::<Selector>()?;
        json_value(graph.show(&selector, self.max_bytes)?)
    }
}
