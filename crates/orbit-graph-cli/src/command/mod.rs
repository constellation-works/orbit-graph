//! Argument declarations and dispatch for every `orbit-graph` subcommand.
//!
//! [`Cli`] is the parsed command line. Each subcommand lives in its own
//! module, parses its arguments, makes one library call through a
//! [`CommandContext`] and returns a JSON payload; `crate::output` renders it.
//! [`CliError`] is the CLI's one surface error type.

use std::env;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use orbit_graph::{Graph, GraphError, SelectorParseError, SyncPolicy};
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use crate::output::CommandOutput;

mod callees;
mod clean;
mod db_path;
mod deps;
mod evaluate;
mod history;
mod impact;
mod implementors;
mod overview;
mod recommend;
mod refs;
mod search;
mod show;
mod sync;
mod trace;
mod version;

const TOP_LEVEL_HELP_TEMPLATE: &str = "\
{name}

{about}

{usage-heading} {usage}

Explore code:
  overview    Summarize indexed files and symbols
  search      Search indexed symbols, strings, and configuration keys
  show        Show source and metadata for a graph selector

Follow relationships:
  refs          List references to a symbol
  callees       List outbound calls from a function or command
  implementors  Find implementations of a trait
  deps          List source-level imports for a file or directory
  trace         Trace outbound calls from a discovered CLI command handler
  impact        Traverse the bounded graph around a selector

Recommendations and history:
  recommend  Recommend current change destinations from historical evidence
  history    Inspect and maintain historical delivery evidence
  evaluate   Run chronological or live leakage-safe recommendation evaluation

Index and utilities:
  sync     Update or rebuild the source graph index
  db-path  Print the current graph database path
  clean    Report obsolete graph databases; delete them with --confirm
  version  Print crate, extractor, and store schema versions

Other:
  help     Print this message or the help of the given subcommand(s)

Options:
{options}

Run `orbit-graph <COMMAND> --help` for command-specific options.

Output:
  On a terminal, results are a headed table. When stdout is piped or
  redirected they are headerless tab-separated rows, one per record, in the
  table's column order. Pass `--format table` for a header, or `--format json`
  or `--format ndjson` for named fields (the stable machine contract).

Examples:
  orbit-graph overview
  orbit-graph search parser --kind symbol
  orbit-graph refs symbol:src/lib.rs#entry:function
";

#[derive(Debug, Parser)]
#[command(
    name = "orbit-graph",
    about = "Index and query a source-code graph",
    help_template = TOP_LEVEL_HELP_TEMPLATE
)]
pub struct Cli {
    #[command(subcommand)]
    pub(crate) command: Command,
}

impl Cli {
    /// Run the parsed subcommand and pair its JSON payload with the renderer
    /// the output layer uses for it.
    pub fn run(&self) -> Result<CommandOutput, CliError> {
        let document = self.command.run()?;
        Ok(self.command.output(document))
    }
}

impl Command {
    fn output(&self, document: Value) -> CommandOutput {
        match self {
            Self::Overview(_) => overview::output(document),
            Self::Search(_) => search::output(document),
            Self::Show(_) => show::output(document),
            Self::Refs(_) => refs::output(document),
            Self::Callees(_) => callees::output(document),
            Self::Implementors(_) => implementors::output(document),
            Self::Deps(_) => deps::output(document),
            Self::Trace(_) => trace::output(document),
            Self::Impact(_) => impact::output(document),
            Self::Recommend(_) => recommend::output(document),
            Self::History(command) => command.output(document),
            Self::Evaluate(_) => evaluate::output(document),
            Self::Sync(_) => sync::output(document),
            Self::DbPath(_) => db_path::output(document),
            Self::Clean(_) => clean::output(document),
            Self::Version(_) => version::output(document),
        }
    }

    /// Dispatch this subcommand against a freshly discovered worktree context
    /// and return the JSON payload the caller is expected to emit.
    pub fn run(&self) -> Result<Value, CliError> {
        let context = CommandContext::from_current_dir()?;
        self.run_with_context(&context)
    }

    /// Dispatch this subcommand against an explicit worktree context.
    ///
    /// Split out from [`Command::run`] so tests can supply a fixture
    /// [`CommandContext`] directly instead of relying on the process-wide
    /// current directory.
    pub(crate) fn run_with_context(&self, context: &CommandContext) -> Result<Value, CliError> {
        match self {
            Command::Sync(command) => command.run(context),
            Command::Search(command) => command.run(context),
            Command::Show(command) => command.run(context),
            Command::Refs(command) => command.run(context),
            Command::Callees(command) => command.run(context),
            Command::Impact(command) => command.run(context),
            Command::Recommend(command) => command.run(context),
            Command::History(command) => command.run(context),
            Command::Trace(command) => command.run(context),
            Command::Overview(command) => command.run(context),
            Command::Implementors(command) => command.run(context),
            Command::Deps(command) => command.run(context),
            Command::Evaluate(command) => command.run(context),
            Command::Version(command) => command.run(),
            Command::DbPath(command) => command.run(context),
            Command::Clean(command) => command.run(context),
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Update or rebuild the source graph index.
    Sync(sync::SyncCommand),
    /// Search indexed symbols, strings, and configuration keys.
    Search(search::SearchCommand),
    /// Show source and metadata for a graph selector.
    Show(show::ShowCommand),
    /// List references to a symbol.
    Refs(refs::RefsCommand),
    /// List outbound calls from a function or command.
    Callees(callees::CalleesCommand),
    /// Traverse the bounded graph around a selector.
    Impact(impact::ImpactCommand),
    /// Recommend current change destinations from historical evidence.
    Recommend(recommend::RecommendCommand),
    /// Inspect and maintain historical delivery evidence.
    History(history::HistoryCommand),
    /// Run chronological or live leakage-safe recommendation evaluation.
    Evaluate(evaluate::EvaluateCommand),
    /// Trace outbound calls from a discovered CLI command handler.
    Trace(trace::TraceCommand),
    /// Summarize indexed files and symbols.
    Overview(overview::OverviewCommand),
    /// Find implementations of a trait.
    Implementors(implementors::ImplementorsCommand),
    /// List source-level imports for a file or directory.
    Deps(deps::DepsCommand),
    /// Print the current graph database path.
    DbPath(db_path::DbPathCommand),
    /// Report obsolete graph databases; delete them with --confirm.
    Clean(clean::CleanCommand),
    /// Print crate, extractor, and store schema versions.
    Version(version::VersionCommand),
}

/// The worktree a subcommand runs against.
pub(crate) struct CommandContext {
    worktree_root: PathBuf,
}

impl CommandContext {
    fn from_current_dir() -> Result<Self, CliError> {
        let current_dir = env::current_dir().map_err(CliError::CurrentDir)?;
        let worktree_root = git2::Repository::discover(current_dir.as_path())
            .ok()
            .and_then(|repo| repo.workdir().map(PathBuf::from))
            .unwrap_or(current_dir);
        Ok(Self { worktree_root })
    }

    /// Build a context pinned to an explicit worktree root, bypassing the
    /// process-wide current directory. Test-only: production callers always
    /// discover the worktree from cwd via [`CommandContext::from_current_dir`].
    #[cfg(test)]
    pub(crate) fn for_worktree(worktree_root: PathBuf) -> Self {
        Self { worktree_root }
    }

    /// Open the worktree's existing graph index for a query: nothing is
    /// created, initialized or deleted (STD-01 §R31), and an unsynced
    /// worktree fails with `index_missing`.
    pub(crate) fn open_graph(&self) -> Result<Graph, CliError> {
        Graph::open_existing_read_only(self.worktree_root.as_path()).map_err(CliError::Graph)
    }

    /// Open, creating when needed, the worktree's graph index for `sync`.
    pub(crate) fn open_graph_for_sync(&self) -> Result<Graph, CliError> {
        Graph::open(self.worktree_root.as_path(), SyncPolicy::Manual).map_err(CliError::Graph)
    }

    pub(crate) fn worktree_root(&self) -> &std::path::Path {
        self.worktree_root.as_path()
    }
}

pub(crate) fn json_value<T: Serialize>(value: T) -> Result<Value, CliError> {
    serde_json::to_value(value).map_err(CliError::Json)
}

pub(crate) fn display_value(value: &Value) -> String {
    match value {
        Value::Null => "-".to_owned(),
        Value::String(value) => value.clone(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        value => serde_json::to_string(value).unwrap_or_else(|_| "-".to_owned()),
    }
}

/// Every failure the CLI reports; [`CliError::code`] is its stable
/// machine-readable name.
#[derive(Debug, Error)]
pub enum CliError {
    /// clap rejected the command line.
    #[error(transparent)]
    Clap(clap::Error),
    /// The process working directory could not be read.
    #[error("failed to determine current directory: {0}")]
    CurrentDir(std::io::Error),
    /// Standard input could not be read.
    #[error("failed to read stdin: {0}")]
    Stdin(std::io::Error),
    /// The graph library failed.
    #[error(transparent)]
    Graph(#[from] GraphError),
    /// An Orbit plugin tool request failed.
    #[error(transparent)]
    Tool(orbit_graph::plugin::ToolError),
    /// A graph selector argument did not parse.
    #[error(transparent)]
    Selector(#[from] SelectorParseError),
    /// A payload could not be serialized to JSON.
    #[error("failed to serialize JSON: {0}")]
    Json(serde_json::Error),
    /// Writing to stdout failed.
    #[error("failed to write JSON to stdout: {0}")]
    Stdout(std::io::Error),
    /// Writing to stderr failed.
    #[error("failed to write diagnostics to stderr: {0}")]
    Stderr(std::io::Error),
}

impl CliError {
    /// The stable error code written to the JSON error payload.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Clap(_) => "argument_error",
            Self::CurrentDir(_) => "current_dir_error",
            Self::Stdin(_) => "stdin_error",
            Self::Graph(GraphError::IndexMissing { .. }) => "index_missing",
            Self::Graph(GraphError::IndexIncompatible { .. }) => "index_incompatible",
            Self::Graph(_) => "graph_error",
            Self::Tool(error) => error.code().as_str(),
            Self::Selector(_) => "selector_parse_error",
            Self::Json(_) => "json_error",
            Self::Stdout(_) => "stdout_error",
            Self::Stderr(_) => "stderr_error",
        }
    }

    /// The underlying reason, when the error carries one worth reporting
    /// separately from the message.
    pub fn details(&self) -> Option<&str> {
        match self {
            Self::Graph(GraphError::InvalidData { reason, .. }) => Some(reason.as_str()),
            Self::Graph(GraphError::Io { reason, .. }) => Some(reason.as_str()),
            Self::Graph(GraphError::Sqlite { reason, .. }) => Some(reason.as_str()),
            Self::Graph(GraphError::IndexMissing { .. } | GraphError::IndexIncompatible { .. }) => {
                None
            }
            Self::Graph(GraphError::Unimplemented) => None,
            _ => None,
        }
    }

    /// Whether this error is a closed stdout pipe, which is a successful stop.
    pub fn is_broken_pipe(&self) -> bool {
        crate::output::pipe::is_broken_pipe(self)
    }
}
