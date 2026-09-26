use clap::Args;
use orbit_graph::resolve_worktree_db_path;
use serde::Serialize;
use serde_json::Value;

use super::{CliError, CommandContext, display_value, json_value};
use crate::output::{Column, CommandOutput, TableView, View, ViewBlock};

#[derive(Debug, Args)]
pub struct DbPathCommand;

impl DbPathCommand {
    pub(crate) fn run(&self, context: &CommandContext) -> Result<serde_json::Value, CliError> {
        // The would-be path: resolving it creates nothing (STD-01 §R31).
        let db_path = resolve_worktree_db_path(context.worktree_root())?;
        json_value(DbPathOutput {
            path: db_path.path().display().to_string(),
            branch: db_path.branch().to_string(),
            extractor_version: db_path.extractor_version(),
            exists: db_path.path().is_file(),
        })
    }
}

#[derive(Debug, Serialize)]
struct DbPathOutput {
    path: String,
    branch: String,
    extractor_version: u32,
    /// Whether `orbit-graph sync` has built the database yet.
    exists: bool,
}

pub(crate) fn output(document: Value) -> CommandOutput {
    let mut table = TableView::new(vec![
        Column::text("path"),
        Column::text("branch"),
        Column::number("extractor version"),
        Column::text("exists"),
    ]);
    table.push_row([
        display_value(&document["path"]),
        display_value(&document["branch"]),
        display_value(&document["extractor_version"]),
        display_value(&document["exists"]),
    ]);
    CommandOutput::with_view(document, View::Blocks(vec![ViewBlock::table(table)]))
}
