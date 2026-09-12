use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;

use clap::{Args, Subcommand};
use serde_json::Value;

use orbit_graph::{DeliveryImport, GraphError, HistoryIndex};

use super::{CliError, CommandContext, display_value, json_value};
use crate::output::{Column, CommandOutput, TableView, View, ViewBlock};

#[derive(Debug, Args)]
pub struct HistoryCommand {
    #[command(subcommand)]
    command: HistorySubcommand,
}

impl HistoryCommand {
    pub(crate) fn run(&self, context: &CommandContext) -> Result<serde_json::Value, CliError> {
        self.command.run(context)
    }

    pub(crate) fn output(&self, document: Value) -> CommandOutput {
        let rows = match self.command {
            HistorySubcommand::Import(_) => vec![
                ("operation", "import".to_owned()),
                ("delivery", display_value(&document["delivery_id"])),
                ("inserted", display_value(&document["inserted"])),
                ("files", display_value(&document["files"])),
                ("symbols", display_value(&document["symbols"])),
            ],
            HistorySubcommand::Sync(_) => sync_rows("sync", &document),
            HistorySubcommand::Status(_) => vec![
                ("repository", display_value(&document["repository"])),
                ("branch", display_value(&document["landing_branch"])),
                ("database", display_value(&document["database_path"])),
                ("schema version", display_value(&document["schema_version"])),
                (
                    "extractor version",
                    display_value(&document["extractor_version"]),
                ),
                ("cursor", display_value(&document["cursor"])),
                ("bootstrap tip", display_value(&document["bootstrap_tip"])),
                (
                    "resume from",
                    display_value(&document["bootstrap_resume_from"]),
                ),
                ("complete", display_value(&document["complete"])),
                ("deliveries", display_value(&document["deliveries"])),
                (
                    "verified deliveries",
                    display_value(&document["verified_deliveries"]),
                ),
                (
                    "git-only deliveries",
                    display_value(&document["git_only_deliveries"]),
                ),
                (
                    "task associations",
                    display_value(&document["task_associations"]),
                ),
            ],
            HistorySubcommand::Rebuild(_) => {
                let mut rows = vec![
                    ("operation", "rebuild".to_owned()),
                    (
                        "removed deliveries",
                        display_value(&document["removed_deliveries"]),
                    ),
                ];
                rows.extend(sync_rows("rebuild sync", &document["sync"]));
                rows
            }
        };
        CommandOutput::with_view(
            document,
            View::Blocks(vec![ViewBlock::table(detail_table(rows))]),
        )
    }
}

fn sync_rows(operation: &'static str, document: &Value) -> Vec<(&'static str, String)> {
    vec![
        ("operation", operation.to_owned()),
        (
            "commits indexed",
            display_value(&document["commits_indexed"]),
        ),
        (
            "deliveries inserted",
            display_value(&document["deliveries_inserted"]),
        ),
        ("cursor before", display_value(&document["cursor_before"])),
        ("cursor after", display_value(&document["cursor_after"])),
        ("snapshot tip", display_value(&document["snapshot_tip"])),
        ("resume from", display_value(&document["resume_from"])),
        ("complete", display_value(&document["complete"])),
    ]
}

fn detail_table(rows: Vec<(&str, String)>) -> TableView {
    let mut table = TableView::new(vec![Column::text("field"), Column::text("value")]);
    for (field, value) in rows {
        table.push_row([field.to_owned(), value]);
    }
    table
}

#[derive(Debug, Subcommand)]
enum HistorySubcommand {
    /// Import a verified public JSON delivery envelope.
    Import(HistoryImportCommand),
    /// Incrementally sync first-parent Git history with weaker Git-only evidence.
    Sync(HistorySyncCommand),
    /// Report scope versions, cursor, and record counts.
    Status(HistoryStatusCommand),
    /// Atomically rebuild the scope from first-parent Git history.
    Rebuild(HistoryRebuildCommand),
}

impl HistorySubcommand {
    fn run(&self, context: &CommandContext) -> Result<serde_json::Value, CliError> {
        match self {
            Self::Import(command) => command.run(context),
            Self::Sync(command) => command.run(context),
            Self::Status(command) => command.run(context),
            Self::Rebuild(command) => command.run(context),
        }
    }
}

#[derive(Debug, Args)]
struct HistoryImportCommand {
    /// JSON envelope path, or `-` for stdin.
    #[arg(long, default_value = "-")]
    input: PathBuf,
}

impl HistoryImportCommand {
    fn run(&self, context: &CommandContext) -> Result<serde_json::Value, CliError> {
        let bytes = read_input(self.input.as_path())?;
        let delivery: DeliveryImport =
            serde_json::from_slice(bytes.as_slice()).map_err(|error| {
                CliError::Graph(GraphError::invalid_data(
                    "decode delivery import JSON",
                    error.to_string(),
                ))
            })?;
        let index = HistoryIndex::open(context.worktree_root(), delivery.landing_branch.as_str())?;
        json_value(index.import(delivery)?)
    }
}

#[derive(Debug, Args)]
struct HistorySyncCommand {
    /// Landing branch to follow by its first-parent chain.
    #[arg(long)]
    branch: String,
    /// Maximum commits to traverse in this atomic operation.
    #[arg(long)]
    limit: Option<usize>,
}

impl HistorySyncCommand {
    fn run(&self, context: &CommandContext) -> Result<serde_json::Value, CliError> {
        let index = HistoryIndex::open(context.worktree_root(), self.branch.as_str())?;
        json_value(index.sync(self.limit)?)
    }
}

#[derive(Debug, Args)]
struct HistoryStatusCommand {
    /// Landing branch scope to inspect.
    #[arg(long)]
    branch: String,
}

impl HistoryStatusCommand {
    fn run(&self, context: &CommandContext) -> Result<serde_json::Value, CliError> {
        let index = HistoryIndex::open(context.worktree_root(), self.branch.as_str())?;
        json_value(index.status()?)
    }
}

#[derive(Debug, Args)]
struct HistoryRebuildCommand {
    /// Landing branch scope to rebuild.
    #[arg(long)]
    branch: String,
    /// Maximum commits to traverse in this atomic operation.
    #[arg(long)]
    limit: Option<usize>,
}

impl HistoryRebuildCommand {
    fn run(&self, context: &CommandContext) -> Result<serde_json::Value, CliError> {
        let index = HistoryIndex::open(context.worktree_root(), self.branch.as_str())?;
        json_value(index.rebuild(self.limit)?)
    }
}

fn read_input(path: &std::path::Path) -> Result<Vec<u8>, CliError> {
    if path == std::path::Path::new("-") {
        let mut bytes = Vec::new();
        io::stdin().read_to_end(&mut bytes).map_err(|source| {
            CliError::Graph(GraphError::io(
                "read delivery import from stdin",
                path,
                source,
            ))
        })?;
        Ok(bytes)
    } else {
        fs::read(path).map_err(|source| {
            CliError::Graph(GraphError::io("read delivery import file", path, source))
        })
    }
}
