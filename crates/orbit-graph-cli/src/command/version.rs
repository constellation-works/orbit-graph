use clap::Args;
use orbit_graph::{EXTRACTOR_VERSION, STORE_SCHEMA_VERSION};
use serde::Serialize;
use serde_json::Value;

use super::{CliError, display_value, json_value};
use crate::output::{Column, CommandOutput, TableView, View, ViewBlock};

#[derive(Debug, Args)]
pub struct VersionCommand;

impl VersionCommand {
    pub(crate) fn run(&self) -> Result<serde_json::Value, CliError> {
        json_value(VersionOutput {
            crate_version: env!("CARGO_PKG_VERSION"),
            extractor_version: EXTRACTOR_VERSION,
            store_schema_version: STORE_SCHEMA_VERSION,
        })
    }
}

#[derive(Debug, Serialize)]
struct VersionOutput {
    crate_version: &'static str,
    extractor_version: u32,
    store_schema_version: u32,
}

pub(crate) fn output(document: Value) -> CommandOutput {
    let mut table = TableView::new(vec![
        Column::text("crate version"),
        Column::number("extractor version"),
        Column::number("store schema version"),
    ]);
    table.push_row([
        display_value(&document["crate_version"]),
        display_value(&document["extractor_version"]),
        display_value(&document["store_schema_version"]),
    ]);
    CommandOutput::with_view(document, View::Blocks(vec![ViewBlock::table(table)]))
}
