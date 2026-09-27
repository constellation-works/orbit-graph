//! Stable failure codes for the Orbit plugin envelope.

use std::fmt::{Display, Formatter};

use orbit_graph::GraphError;
use serde_json::{Value, json};

use crate::command::report_graph_error;

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
