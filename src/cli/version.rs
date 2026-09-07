use crate::EXTRACTOR_VERSION;
use clap::Args;
use serde::Serialize;
use serde_json::Value;

use super::output::{Column, CommandOutput, TableView, View, ViewBlock};
use super::{CliError, display_value, json_value};

#[derive(Debug, Args)]
pub struct VersionCommand;

impl VersionCommand {
    pub(crate) fn run(&self) -> Result<serde_json::Value, CliError> {
        json_value(VersionOutput {
            crate_version: env!("CARGO_PKG_VERSION"),
            extractor_version: EXTRACTOR_VERSION,
        })
    }
}

#[derive(Debug, Serialize)]
struct VersionOutput {
    crate_version: &'static str,
    extractor_version: u32,
}

pub(crate) fn output(document: Value) -> CommandOutput {
    let mut table = TableView::new(vec![
        Column::text("crate version"),
        Column::number("extractor version"),
    ]);
    table.push_row([
        display_value(&document["crate_version"]),
        display_value(&document["extractor_version"]),
    ]);
    CommandOutput::with_view(document, View::Blocks(vec![ViewBlock::table(table)]))
}
