use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;

use clap::{Args, Subcommand};

use crate::{DeliveryImport, GraphError, HistoryIndex};

use super::{CliError, CommandContext, json_value};

#[derive(Debug, Args)]
pub struct HistoryCommand {
    #[command(subcommand)]
    command: HistorySubcommand,
}

impl HistoryCommand {
    pub(crate) fn run(&self, context: &CommandContext) -> Result<serde_json::Value, CliError> {
        self.command.run(context)
    }
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
