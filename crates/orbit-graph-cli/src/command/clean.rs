use clap::Args;
use orbit_graph::{CleanItem, clean_old_databases, plan_clean_old_databases};
use serde::Serialize;
use serde_json::{Value, json};

use super::{CliError, CommandContext, display_value, json_value};
use crate::output::{Column, CommandOutput, TableView, View, ViewBlock};

/// Report obsolete graph databases, and delete them only with `--confirm`.
///
/// A cleanup command defaults to a report (STD-01 §R5): without `--confirm`
/// nothing is written, created or removed.
#[derive(Debug, Args)]
pub struct CleanCommand {
    /// Delete the files the report lists. Without it, `clean` only reports.
    #[arg(long)]
    confirm: bool,
}

impl CleanCommand {
    pub(crate) fn run(&self, context: &CommandContext) -> Result<serde_json::Value, CliError> {
        let report = if self.confirm {
            clean_old_databases(context.worktree_root.as_path())?
        } else {
            plan_clean_old_databases(context.worktree_root.as_path())?
        };
        json_value(CleanOutput {
            graph_dir: report.graph_dir.display().to_string(),
            would_delete: report
                .would_delete
                .iter()
                .map(CleanOutputItem::from)
                .collect(),
            kept: report.kept.iter().map(CleanOutputItem::from).collect(),
            applied: report.applied,
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
    would_delete: Vec<CleanOutputItem>,
    kept: Vec<CleanOutputItem>,
    applied: bool,
    deleted: Vec<String>,
}

#[derive(Debug, Serialize)]
struct CleanOutputItem {
    path: String,
    reason: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

impl From<&CleanItem> for CleanOutputItem {
    fn from(item: &CleanItem) -> Self {
        Self {
            path: item.path.display().to_string(),
            reason: serde_json::to_value(item.reason).unwrap_or(Value::Null),
            detail: item.detail.clone(),
        }
    }
}

pub(crate) fn output(document: Value) -> CommandOutput {
    let graph_dir = display_value(&document["graph_dir"]);
    let applied = document["applied"].as_bool().unwrap_or(false);
    let would_delete = document["would_delete"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let kept = document["kept"].as_array().cloned().unwrap_or_default();
    let deleted = document["deleted"].as_array().cloned().unwrap_or_default();
    let mut summary = TableView::new(vec![
        Column::text("graph directory"),
        Column::text("applied"),
        Column::number("kept"),
        Column::number(if applied { "deleted" } else { "would delete" }),
    ]);
    summary.push_row([
        graph_dir.clone(),
        applied.to_string(),
        kept.len().to_string(),
        would_delete.len().to_string(),
    ]);
    let action = if applied { "deleted" } else { "would_delete" };
    let mut paths = TableView::new(vec![
        Column::text("action"),
        Column::text("reason"),
        Column::text("path"),
    ]);
    for item in &would_delete {
        paths.push_row([
            action.to_string(),
            display_value(&item["reason"]),
            display_value(&item["path"]),
        ]);
    }
    for item in &kept {
        paths.push_row([
            "kept".to_string(),
            display_value(&item["reason"]),
            display_value(&item["path"]),
        ]);
    }
    let view = View::Blocks(vec![
        ViewBlock::table(summary),
        ViewBlock::table(paths.with_empty_message("no graph databases besides the active one")),
    ]);
    let mut records = vec![json!({
        "record_type": "clean_context",
        "context": {"graph_dir": graph_dir, "applied": applied}
    })];
    records.extend(
        deleted
            .into_iter()
            .map(|path| json!({"record_type": "deleted_database", "path": path})),
    );
    if !applied {
        records.extend(would_delete.iter().map(|item| {
            json!({
                "record_type": "would_delete_database",
                "path": item["path"],
                "reason": item["reason"],
            })
        }));
    }
    records.extend(kept.iter().map(|item| {
        let mut record = json!({
            "record_type": "kept_database",
            "path": item["path"],
            "reason": item["reason"],
        });
        if let Some(detail) = item.get("detail") {
            record["detail"] = detail.clone();
        }
        record
    }));
    let output = CommandOutput::with_view(document, view).with_ndjson_records(records);
    if applied || would_delete.is_empty() {
        output
    } else {
        output.with_notice(format!(
            "dry run: nothing was deleted; pass `--confirm` to delete the {} listed file(s)",
            would_delete.len()
        ))
    }
}
