use clap::Args;
use orbit_graph::clean_old_databases;
use serde::Serialize;
use serde_json::{Value, json};

use super::{CliError, CommandContext, display_value, json_value};
use crate::output::{Column, CommandOutput, TableView, View, ViewBlock};

#[derive(Debug, Args)]
pub struct CleanCommand;

impl CleanCommand {
    pub(crate) fn run(&self, context: &CommandContext) -> Result<serde_json::Value, CliError> {
        let report = clean_old_databases(context.worktree_root.as_path())?;
        json_value(CleanOutput {
            graph_dir: report.graph_dir.display().to_string(),
            deleted: report
                .deleted
                .iter()
                .map(|path| path.display().to_string())
                .collect(),
        })
    }
}

#[derive(Debug, Serialize)]
struct CleanOutput {
    graph_dir: String,
    deleted: Vec<String>,
}

pub(crate) fn output(document: Value) -> CommandOutput {
    let graph_dir = display_value(&document["graph_dir"]);
    let deleted = document["deleted"].as_array().cloned().unwrap_or_default();
    let mut summary = TableView::new(vec![
        Column::text("graph directory"),
        Column::number("deleted"),
    ]);
    summary.push_row([graph_dir.clone(), deleted.len().to_string()]);
    let mut paths = TableView::new(vec![Column::text("deleted path")]);
    for path in &deleted {
        paths.push_row([display_value(path)]);
    }
    let view = View::Blocks(vec![
        ViewBlock::table(summary),
        ViewBlock::table(paths.with_empty_message("no obsolete graph databases")),
    ]);
    let mut records = vec![json!({
        "record_type": "clean_context",
        "context": {"graph_dir": graph_dir}
    })];
    records.extend(
        deleted
            .into_iter()
            .map(|path| json!({"record_type": "deleted_database", "path": path})),
    );
    CommandOutput::with_view(document, view).with_ndjson_records(records)
}
