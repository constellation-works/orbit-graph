//! CLI and plugin failure types and the one graph-error translator.
//!
//! Foundation module for `command`, `output` and `plugin` (STD-02 §R1): it
//! owns [`CliError`], the [`report_graph_error`] translator, the plugin
//! [`ToolError`] mapping and [`PLUGIN_SCHEMA_VERSION`]. Nothing here imports
//! from those three modules, so they can all depend on it.

use std::fmt::{Display, Formatter};
use std::io::ErrorKind;

use orbit_graph::{GraphError, GraphErrorClass, SelectorParseError};
use orbit_graph_changes::{AnalysisError, SnapshotError};
use serde_json::{Value, json};
use thiserror::Error;

/// Version of every external-tool request and response envelope.
pub const PLUGIN_SCHEMA_VERSION: u32 = 1;

/// Machine-readable class of a plugin tool failure.
///
/// The string form is the `error.code` of the v2 response envelope and is part
/// of the plugin contract: callers and conformance goldens match on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolErrorCode {
    /// The request was refused before it was acted on: an unknown tool,
    /// undecodable input, an unsupported `schema_version`, a field outside its
    /// documented bounds, or a selector of the wrong kind. For `changes`, also
    /// a revision that does not resolve, or no default base, refused before
    /// indexing.
    InvalidRequest,
    /// The explicitly routed `repository` does not exist or is not a Git
    /// repository.
    RepositoryUnavailable,
    /// Any other graph, index, Git, or Orbit-callback failure.
    GraphError,
    /// A query tool found no published code-graph index for the repository
    /// (the `graph_sync` maintenance operation builds one), or `status` or
    /// `recommend` found no history index (`history_sync` builds one).
    IndexMissing,
    /// The published code-graph index was built by another extractor or
    /// schema version; a full `graph_sync` replaces it.
    IndexIncompatible,
    /// Something the request named, such as a revision, does not exist.
    NotFound,
    /// A lock wait or an Orbit callback outlived its deadline; retryable.
    Timeout,
    /// Orbit refused a callback; the envelope carries Orbit's own code.
    OrbitRefused,
    /// A state path was refused as unsafe to read or write through.
    UnsafeStatePath,
    /// The history index was built by another extractor or import contract.
    VersionMismatch,
}

impl ToolErrorCode {
    /// The envelope spelling of this code.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::RepositoryUnavailable => "repository_unavailable",
            Self::GraphError => "graph_error",
            Self::IndexMissing => "index_missing",
            Self::IndexIncompatible => "index_incompatible",
            Self::NotFound => "not_found",
            Self::Timeout => "timeout",
            Self::OrbitRefused => "orbit_refused",
            Self::UnsafeStatePath => "unsafe_state_path",
            Self::VersionMismatch => "version_mismatch",
        }
    }
}

impl Display for ToolErrorCode {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A plugin tool failure: a stable [`ToolErrorCode`], whether a retry can
/// succeed, and the graph error that explains it.
#[derive(Debug, Clone)]
pub struct ToolError {
    code: ToolErrorCode,
    retryable: bool,
    source: GraphError,
}

impl ToolError {
    /// A request refused before any repository or index was read.
    pub fn invalid_request(operation: &'static str, reason: impl Into<String>) -> Self {
        Self::fixed(
            ToolErrorCode::InvalidRequest,
            GraphError::invalid_input(operation, "request", reason),
        )
    }

    /// A routed repository that could not be opened.
    pub fn repository_unavailable(source: GraphError) -> Self {
        Self::fixed(ToolErrorCode::RepositoryUnavailable, source)
    }

    /// Any other graph failure, classified by the one translator.
    pub fn graph(source: GraphError) -> Self {
        let reported = report_graph_error(&source);
        Self {
            code: reported.plugin_code,
            retryable: reported.retryable,
            source,
        }
    }

    /// A query that needs a code-graph index that has not been built.
    pub fn index_missing(reason: impl Into<String>) -> Self {
        Self::fixed(
            ToolErrorCode::IndexMissing,
            GraphError::invalid_data("read code-graph index", reason),
        )
    }

    /// A read-only tool that needs a history index that has not been built.
    pub(crate) fn history_index_missing(reason: impl Into<String>) -> Self {
        Self::fixed(
            ToolErrorCode::IndexMissing,
            GraphError::invalid_data("read history index", reason),
        )
    }

    /// A query against an index from another extractor or schema version.
    pub fn index_incompatible(reason: impl Into<String>) -> Self {
        Self::fixed(
            ToolErrorCode::IndexIncompatible,
            GraphError::invalid_data("read code-graph index", reason),
        )
    }

    fn fixed(code: ToolErrorCode, source: GraphError) -> Self {
        Self {
            code,
            retryable: false,
            source,
        }
    }

    /// The stable envelope code.
    pub const fn code(&self) -> ToolErrorCode {
        self.code
    }

    /// Whether retrying the same call can succeed.
    pub const fn retryable(&self) -> bool {
        self.retryable
    }

    /// The graph error that explains this failure.
    pub const fn source_error(&self) -> &GraphError {
        &self.source
    }
}

impl From<GraphError> for ToolError {
    fn from(source: GraphError) -> Self {
        Self::graph(source)
    }
}

impl Display for ToolError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.source, f)
    }
}

impl std::error::Error for ToolError {}

/// The envelope `error` object for `error`: its code, message and
/// `retryable`, and for an Orbit refusal, [`orbit_detail`].
pub(crate) fn failure_json(error: &ToolError) -> Value {
    let mut failure = json!({
        "code": error.code().as_str(),
        "message": error.to_string(),
        "retryable": error.retryable(),
    });
    if let Some(orbit) = orbit_detail(error.source_error()) {
        failure["orbit"] = orbit;
    }
    failure
}

/// For an Orbit refusal, the envelope's `error.orbit`: the refusing tool and
/// Orbit's own code, preserved verbatim.
pub(crate) fn orbit_detail(error: &GraphError) -> Option<Value> {
    if let GraphError::OrbitRefused { tool, code, .. } = error {
        return Some(json!({"tool": tool, "code": code}));
    }
    None
}

/// How one graph failure is reported on each surface: its CLI `code`, its
/// plugin envelope `code`, and whether a retry can succeed.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ReportedFailure {
    pub(crate) cli_code: &'static str,
    pub(crate) plugin_code: ToolErrorCode,
    pub(crate) retryable: bool,
}

/// The one translator from a [`GraphError`] to the CLI and plugin codes
/// (STD-02 §R12). It decides from the error's class, never its message
/// (§R10), and `retryable` is true only for a timeout or transient I/O.
pub(crate) fn report_graph_error(error: &GraphError) -> ReportedFailure {
    let (cli_code, plugin_code, retryable) = match error.class() {
        GraphErrorClass::InvalidInput => ("invalid_input", ToolErrorCode::InvalidRequest, false),
        GraphErrorClass::NotFound => ("not_found", ToolErrorCode::NotFound, false),
        GraphErrorClass::Timeout => ("timeout", ToolErrorCode::Timeout, true),
        GraphErrorClass::OrbitRefused => ("orbit_refused", ToolErrorCode::OrbitRefused, false),
        GraphErrorClass::IndexMissing => ("index_missing", ToolErrorCode::IndexMissing, false),
        GraphErrorClass::IndexIncompatible => (
            "index_incompatible",
            ToolErrorCode::IndexIncompatible,
            false,
        ),
        GraphErrorClass::UnsafeStatePath => {
            ("unsafe_state_path", ToolErrorCode::UnsafeStatePath, false)
        }
        GraphErrorClass::VersionMismatch => {
            ("version_mismatch", ToolErrorCode::VersionMismatch, false)
        }
        GraphErrorClass::Io { transient } => ("graph_error", ToolErrorCode::GraphError, transient),
        GraphErrorClass::Git
        | GraphErrorClass::Sqlite
        | GraphErrorClass::InvalidData
        | GraphErrorClass::Subprocess => ("graph_error", ToolErrorCode::GraphError, false),
    };
    ReportedFailure {
        cli_code,
        plugin_code,
        retryable,
    }
}

/// Every failure the CLI reports; [`CliError::code`] is its stable
/// machine-readable name.
#[derive(Debug, Error)]
pub enum CliError {
    /// clap rejected the command line.
    #[error(transparent)]
    Clap(clap::Error),
    /// The command line parsed but its combination of arguments or
    /// environment is not usable.
    #[error("{0}")]
    Usage(String),
    /// A selector or name resolved to nothing in the index.
    #[error("{0}")]
    NotFound(String),
    /// Input the CLI read itself, such as a JSON file named by `--input`,
    /// could not be decoded.
    #[error("{operation}: {reason}")]
    Input {
        /// What was being read.
        operation: &'static str,
        /// Why it was refused.
        reason: String,
    },
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
    Tool(ToolError),
    /// Change analysis failed.
    #[error(transparent)]
    Changes(#[from] orbit_graph_changes::AnalysisError),
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
            Self::Clap(_) | Self::Usage(_) => "argument_error",
            Self::NotFound(_) => "not_found",
            Self::Input { .. } => "invalid_input",
            Self::CurrentDir(_) => "current_dir_error",
            Self::Stdin(_) => "stdin_error",
            Self::Graph(error) => report_graph_error(error).cli_code,
            Self::Tool(error) => error.code().as_str(),
            Self::Changes(error) => changes_error_code(error),
            Self::Selector(_) => "selector_parse_error",
            Self::Json(_) => "json_error",
            Self::Stdout(_) => "stdout_error",
            Self::Stderr(_) => "stderr_error",
        }
    }

    /// Whether retrying the same call can succeed: only a timeout or a
    /// transient I/O failure of the graph.
    pub fn retryable(&self) -> bool {
        match self {
            Self::Graph(error) => report_graph_error(error).retryable,
            Self::Tool(error) => error.retryable(),
            Self::Clap(_)
            | Self::Usage(_)
            | Self::NotFound(_)
            | Self::Input { .. }
            | Self::CurrentDir(_)
            | Self::Stdin(_)
            | Self::Changes(_)
            | Self::Selector(_)
            | Self::Json(_)
            | Self::Stdout(_)
            | Self::Stderr(_) => false,
        }
    }

    /// The graph error behind this failure, if there is one.
    pub fn graph_error(&self) -> Option<&GraphError> {
        match self {
            Self::Graph(error) => Some(error),
            Self::Tool(error) => Some(error.source_error()),
            Self::Clap(_)
            | Self::Usage(_)
            | Self::NotFound(_)
            | Self::Input { .. }
            | Self::CurrentDir(_)
            | Self::Stdin(_)
            | Self::Changes(_)
            | Self::Selector(_)
            | Self::Json(_)
            | Self::Stdout(_)
            | Self::Stderr(_) => None,
        }
    }

    /// The process exit code for this failure: `2` for a usage error and `1`
    /// for a command failure (STD-01 §R20).
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Clap(error) => error.exit_code(),
            Self::Usage(_) | Self::Selector(_) => 2,
            Self::Graph(error) if error.class() == GraphErrorClass::InvalidInput => 2,
            Self::Changes(error) if changes_is_usage_error(error) => 2,
            Self::Changes(_)
            | Self::NotFound(_)
            | Self::Input { .. }
            | Self::CurrentDir(_)
            | Self::Stdin(_)
            | Self::Graph(_)
            | Self::Tool(_)
            | Self::Json(_)
            | Self::Stdout(_)
            | Self::Stderr(_) => 1,
        }
    }

    /// Whether this error is a closed stdout pipe, which is a successful stop.
    pub fn is_broken_pipe(&self) -> bool {
        match self {
            CliError::Json(error) => error.io_error_kind() == Some(ErrorKind::BrokenPipe),
            CliError::Stdout(error) => error.kind() == ErrorKind::BrokenPipe,
            CliError::Clap(_)
            | CliError::Usage(_)
            | CliError::NotFound(_)
            | CliError::Input { .. }
            | CliError::CurrentDir(_)
            | CliError::Stdin(_)
            | CliError::Graph(_)
            | CliError::Tool(_)
            | CliError::Changes(_)
            | CliError::Selector(_)
            | CliError::Stderr(_) => false,
        }
    }
}

/// Stable error code of a change-analysis failure.
pub(crate) fn changes_error_code(error: &AnalysisError) -> &'static str {
    match error {
        AnalysisError::InvalidRange { .. }
        | AnalysisError::InvalidBound { .. }
        | AnalysisError::InvalidSelection { .. } => "argument_error",
        AnalysisError::Snapshot(SnapshotError::Revision { .. }) => "revision_not_found",
        AnalysisError::Snapshot(SnapshotError::NoDefaultBase { .. }) => "no_default_base",
        AnalysisError::Snapshot(SnapshotError::Repository { .. }) => "repository_unavailable",
        // `AnalysisError` and `SnapshotError` are `#[non_exhaustive]`: every
        // other failure is a change-analysis failure.
        _ => "changes_error",
    }
}

/// Whether a change-analysis failure is a usage error (exit code 2).
pub(crate) fn changes_is_usage_error(error: &AnalysisError) -> bool {
    changes_error_code(error) == "argument_error"
}
