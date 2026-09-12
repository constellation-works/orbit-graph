use clap::Args;
use serde::Serialize;
use serde_json::Value;

use super::{CliError, CommandContext, display_value, json_value};
use crate::output::{Column, CommandOutput, TableView, View, ViewBlock};

#[derive(Debug, Args)]
pub struct DbPathCommand;

impl DbPathCommand {
    pub(crate) fn run(&self, context: &CommandContext) -> Result<serde_json::Value, CliError> {
        let graph = context.open_graph()?;
        let db_path = graph.db_path();
        json_value(DbPathOutput {
            path: db_path.path().display().to_string(),
            branch: db_path.branch().to_string(),
            extractor_version: db_path.extractor_version(),
        })
    }
}

#[derive(Debug, Serialize)]
struct DbPathOutput {
    path: String,
    branch: String,
    extractor_version: u32,
}

pub(crate) fn output(document: Value) -> CommandOutput {
    let mut table = TableView::new(vec![
        Column::text("path"),
        Column::text("branch"),
        Column::number("extractor version"),
    ]);
    table.push_row([
        display_value(&document["path"]),
        display_value(&document["branch"]),
        display_value(&document["extractor_version"]),
    ]);
    CommandOutput::with_view(document, View::Blocks(vec![ViewBlock::table(table)]))
}
