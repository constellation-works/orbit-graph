use std::fmt::{Display, Formatter};
use std::path::PathBuf;

/// Graph crate error surface.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum GraphError {
    /// A filesystem operation failed while opening graph storage.
    Io {
        /// Operation being performed.
        operation: &'static str,
        /// Filesystem path involved in the failed operation.
        path: PathBuf,
        /// Source error rendered as text for cloneable error propagation.
        reason: String,
    },
    /// A SQLite operation failed while opening or initializing graph storage.
    Sqlite {
        /// Operation being performed.
        operation: &'static str,
        /// Source error rendered as text for cloneable error propagation.
        reason: String,
    },
    /// Stored or discovered graph data was invalid.
    InvalidData {
        /// Operation being performed.
        operation: &'static str,
        /// Validation failure rendered as text for cloneable error propagation.
        reason: String,
    },
    /// A read found no index where it looked; nothing was created.
    IndexMissing {
        /// Path of the index that does not exist yet.
        path: PathBuf,
        /// Actionable message naming the command that builds the index.
        reason: String,
    },
    /// An index exists but its stored schema identity is not the one this
    /// binary reads, so it is neither read nor written.
    IndexIncompatible {
        /// Path of the incompatible index.
        path: PathBuf,
        /// Actionable message naming the stored and expected identities.
        reason: String,
    },
    /// Placeholder variant until storage, sync, and query errors are defined.
    Unimplemented,
}

impl GraphError {
    /// Build an [`GraphError::Io`] failure for `operation` on `path`.
    ///
    /// The variant is `#[non_exhaustive]`, so this constructor is also how
    /// callers outside the crate — the `orbit-graph` CLI among them — report a
    /// filesystem failure in the graph error vocabulary.
    pub fn io(operation: &'static str, path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            operation,
            path: path.into(),
            reason: source.to_string(),
        }
    }

    pub(crate) fn sqlite(operation: &'static str, source: rusqlite::Error) -> Self {
        Self::sqlite_message(operation, source.to_string())
    }

    pub(crate) fn sqlite_message(operation: &'static str, reason: impl Into<String>) -> Self {
        Self::Sqlite {
            operation,
            reason: reason.into(),
        }
    }

    /// Build a [`GraphError::InvalidData`] failure for `operation`.
    ///
    /// Public for the same reason as [`GraphError::io`]: the variant cannot be
    /// constructed literally outside this crate.
    pub fn invalid_data(operation: &'static str, reason: impl Into<String>) -> Self {
        Self::InvalidData {
            operation,
            reason: reason.into(),
        }
    }
}

impl Display for GraphError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io {
                operation,
                path,
                reason,
            } => write!(f, "{operation} at {}: {reason}", path.display()),
            Self::Sqlite { operation, reason } => write!(f, "{operation}: {reason}"),
            Self::InvalidData { operation, reason } => write!(f, "{operation}: {reason}"),
            Self::IndexMissing { reason, .. } | Self::IndexIncompatible { reason, .. } => {
                f.write_str(reason)
            }
            Self::Unimplemented => f.write_str("graph operation is not implemented"),
        }
    }
}

impl std::error::Error for GraphError {}

/// The one translator from extraction failures into the graph error surface
/// (STD-02 §R12). Each variant keeps the message text it had before extraction
/// moved into its own crate.
impl From<orbit_graph_extract::ExtractError> for GraphError {
    fn from(error: orbit_graph_extract::ExtractError) -> Self {
        use orbit_graph_extract::ExtractError;
        match error {
            ExtractError::InvalidData { operation, reason } => {
                Self::invalid_data(operation, reason)
            }
            ExtractError::Io {
                operation,
                path,
                source,
            } => Self::io(operation, path, source),
            ExtractError::Git { operation, source } => {
                Self::invalid_data(operation, source.to_string())
            }
        }
    }
}
