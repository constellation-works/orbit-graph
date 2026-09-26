//! Stable failure codes for the Orbit plugin envelope.

use std::fmt::{Display, Formatter};

use crate::GraphError;

/// Machine-readable class of a plugin tool failure.
///
/// The string form is the `error.code` of the v2 response envelope and is part
/// of the plugin contract: callers and conformance goldens match on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ToolErrorCode {
    /// The request was refused before any repository or index was read: an
    /// unknown tool, undecodable input, an unsupported `schema_version`, or a
    /// field outside its documented bounds.
    InvalidRequest,
    /// The explicitly routed `repository` does not exist or is not a Git
    /// repository.
    RepositoryUnavailable,
    /// Any other graph, index, Git, or Orbit-callback failure.
    GraphError,
    /// A query tool found no published code-graph index for the repository;
    /// the `graph_sync` maintenance operation builds one.
    IndexMissing,
    /// The published code-graph index was built by another extractor or
    /// schema version; a full `graph_sync` replaces it.
    IndexIncompatible,
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
        }
    }
}

impl Display for ToolErrorCode {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A plugin tool failure: a stable [`ToolErrorCode`] plus the graph error
/// that explains it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolError {
    code: ToolErrorCode,
    source: GraphError,
}

impl ToolError {
    /// A request refused before any repository or index was read.
    pub fn invalid_request(operation: &'static str, reason: impl Into<String>) -> Self {
        Self {
            code: ToolErrorCode::InvalidRequest,
            source: GraphError::invalid_data(operation, reason),
        }
    }

    /// A routed repository that could not be opened.
    pub fn repository_unavailable(source: GraphError) -> Self {
        Self {
            code: ToolErrorCode::RepositoryUnavailable,
            source,
        }
    }

    /// Any other graph failure.
    pub fn graph(source: GraphError) -> Self {
        Self {
            code: ToolErrorCode::GraphError,
            source,
        }
    }

    /// A query that needs a code-graph index that has not been built.
    pub fn index_missing(reason: impl Into<String>) -> Self {
        Self {
            code: ToolErrorCode::IndexMissing,
            source: GraphError::invalid_data("read code-graph index", reason),
        }
    }

    /// A query against an index from another extractor or schema version.
    pub fn index_incompatible(reason: impl Into<String>) -> Self {
        Self {
            code: ToolErrorCode::IndexIncompatible,
            source: GraphError::invalid_data("read code-graph index", reason),
        }
    }

    /// The stable envelope code.
    pub const fn code(&self) -> ToolErrorCode {
        self.code
    }

    /// The underlying graph error.
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
