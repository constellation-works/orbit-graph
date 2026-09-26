use std::time::Duration;

use clap::Args;
use orbit_graph::{GraphError, SyncFailure, SyncMode, SyncReport, SyncSkip};
use serde::Serialize;
use serde_json::Value;

use super::{CliError, CommandContext, display_value, json_value};
use crate::output::{Column, CommandOutput, TableView, View, ViewBlock};

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
        // Isolated failures are reported, not fatal, while anything was
        // indexed (STD-02 §R32); with nothing indexed the sync did not happen
        // (STD-01 §R30).
        if !report.failed.is_empty() && report.files_indexed == 0 {
            return Err(CliError::Graph(nothing_indexed(&report)));
        }
        json_value(SyncOutput::from(report))
    }
}

fn nothing_indexed(report: &SyncReport) -> GraphError {
    let first = report
        .failed
        .first()
        .map(|failure| {
            format!(
                "; first: {} ({}: {})",
                failure.path, failure.operation, failure.message
            )
        })
        .unwrap_or_default();
    GraphError::invalid_data(
        "sync graph",
        format!(
            "indexed no file: {} path(s) failed{first}",
            report.failed.len()
        ),
    )
}

#[derive(Debug, Serialize)]
struct SyncOutput {
    files_indexed: usize,
    files_changed: usize,
    files_removed: usize,
    duration_ms: u128,
    /// The graph database this sync wrote.
    database_path: String,
    /// The branch the graph database indexes.
    branch: String,
    failed: Entries<FailedEntry>,
    skipped: Entries<SkippedEntry>,
}

impl From<SyncReport> for SyncOutput {
    fn from(report: SyncReport) -> Self {
        Self {
            files_indexed: report.files_indexed,
            files_changed: report.files_changed,
            files_removed: report.files_removed,
            duration_ms: duration_millis(report.duration),
            database_path: report.database_path.display().to_string(),
            branch: report.branch,
            failed: Entries::new(report.failed.into_iter().map(FailedEntry::from).collect()),
            skipped: Entries::new(report.skipped.into_iter().map(SkippedEntry::from).collect()),
        }
    }
}

/// A count and the entries it counts.
#[derive(Debug, Serialize)]
struct Entries<T> {
    count: usize,
    entries: Vec<T>,
}

impl<T> Entries<T> {
    fn new(entries: Vec<T>) -> Self {
        Self {
            count: entries.len(),
            entries,
        }
    }
}

#[derive(Debug, Serialize)]
struct FailedEntry {
    path: String,
    operation: String,
    error_kind: String,
    message: String,
}

impl From<SyncFailure> for FailedEntry {
    fn from(failure: SyncFailure) -> Self {
        Self {
            path: failure.path,
            operation: failure.operation,
            error_kind: failure.error_kind,
            message: failure.message,
        }
    }
}

#[derive(Debug, Serialize)]
struct SkippedEntry {
    path: String,
    reason: String,
}

impl From<SyncSkip> for SkippedEntry {
    fn from(skip: SyncSkip) -> Self {
        Self {
            path: skip.path,
            reason: skip.reason,
        }
    }
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
        Column::number("failed"),
        Column::number("skipped"),
        Column::text("branch"),
        Column::path("database"),
    ]);
    table.push_row([
        display_value(&document["files_indexed"]),
        display_value(&document["files_changed"]),
        display_value(&document["files_removed"]),
        display_value(&document["duration_ms"]),
        display_value(&document["failed"]["count"]),
        display_value(&document["skipped"]["count"]),
        display_value(&document["branch"]),
        display_value(&document["database_path"]),
    ]);
    let mut blocks = vec![ViewBlock::table(table)];
    let failed = entries(&document["failed"]);
    if !failed.is_empty() {
        let mut table = TableView::new(vec![
            Column::path("failed path"),
            Column::text("operation"),
            Column::text("error kind"),
        ]);
        for entry in failed {
            table.push_row([
                display_value(&entry["path"]),
                display_value(&entry["operation"]),
                display_value(&entry["error_kind"]),
            ]);
        }
        blocks.push(ViewBlock::table(table));
    }
    let skipped = entries(&document["skipped"]);
    if !skipped.is_empty() {
        let mut table = TableView::new(vec![Column::path("skipped path"), Column::text("reason")]);
        for entry in skipped {
            table.push_row([
                display_value(&entry["path"]),
                display_value(&entry["reason"]),
            ]);
        }
        blocks.push(ViewBlock::table(table));
    }
    let failed_count = failed.len();
    let skipped_count = skipped.len();
    let mut output = CommandOutput::with_view(document, View::Blocks(blocks));
    if failed_count > 0 {
        output = output.with_notice(format!(
            "warning: {failed_count} path(s) could not be read or extracted and were not \
             indexed; see `failed` in --format json"
        ));
    }
    if skipped_count > 0 {
        output = output.with_notice(format!(
            "note: {skipped_count} file(s) larger than the 4 MiB byte cap were skipped; \
             see `skipped` in --format json"
        ));
    }
    output
}

fn entries(section: &Value) -> &[Value] {
    section["entries"].as_array().map_or(&[], Vec::as_slice)
}
