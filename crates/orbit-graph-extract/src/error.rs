use std::path::PathBuf;

/// Failure raised while extracting history from a Git repository.
///
/// Language extractors never fail: a file they cannot parse yields empty
/// rows. Only the Git-backed history extraction returns this error.
#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    /// Supplied or discovered history data was invalid.
    #[error("{operation}: {reason}")]
    InvalidData {
        /// Operation being performed.
        operation: &'static str,
        /// Validation failure.
        reason: String,
    },
    /// A filesystem operation failed.
    #[error("{operation} at {}", path.display())]
    Io {
        /// Operation being performed.
        operation: &'static str,
        /// Filesystem path involved in the failed operation.
        path: PathBuf,
        /// Underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// A Git object, tree or diff operation failed.
    #[error("{operation}")]
    Git {
        /// Operation being performed.
        operation: &'static str,
        /// Underlying libgit2 failure.
        #[source]
        source: git2::Error,
    },
}

impl ExtractError {
    pub(crate) fn invalid_data(operation: &'static str, reason: impl Into<String>) -> Self {
        Self::InvalidData {
            operation,
            reason: reason.into(),
        }
    }

    pub(crate) fn io(
        operation: &'static str,
        path: impl Into<PathBuf>,
        source: std::io::Error,
    ) -> Self {
        Self::Io {
            operation,
            path: path.into(),
            source,
        }
    }
}
