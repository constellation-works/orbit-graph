use std::time::Duration;

use crate::SyncMode;
use clap::Args;
use serde::Serialize;
use serde_json::Value;

use super::output::{Column, CommandOutput, TableView, View, ViewBlock};
use super::{CliError, CommandContext, display_value, json_value};

#[derive(Debug, Args)]
pub struct SyncCommand {
    #[arg(long)]
    full: bool,
}

impl SyncCommand {
    pub(crate) fn run(&self, context: &CommandContext) -> Result<serde_json::Value, CliError> {
        let graph = context.open_graph()?;
        let report = graph.sync(if self.full {
            SyncMode::Full
        } else {
            SyncMode::Auto
        })?;
        json_value(SyncOutput {
            files_indexed: report.files_indexed,
            files_changed: report.files_changed,
            files_removed: report.files_removed,
            duration_ms: duration_millis(report.duration),
        })
    }
}

#[derive(Debug, Serialize)]
struct SyncOutput {
    files_indexed: usize,
    files_changed: usize,
    files_removed: usize,
    duration_ms: u128,
}

fn duration_millis(duration: Duration) -> u128 {
    duration.as_millis()
}

pub(crate) fn output(document: Value) -> CommandOutput {
    let mut table = TableView::new(vec![
        Column::number("files indexed"),
        Column::number("changed"),
        Column::number("removed"),
        Column::number("duration (ms)"),
    ]);
    table.push_row([
        display_value(&document["files_indexed"]),
        display_value(&document["files_changed"]),
        display_value(&document["files_removed"]),
        display_value(&document["duration_ms"]),
    ]);
    CommandOutput::with_view(document, View::Blocks(vec![ViewBlock::table(table)]))
}
