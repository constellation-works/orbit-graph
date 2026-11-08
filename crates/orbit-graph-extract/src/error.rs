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
    /// The system clock could not supply a usable current time, so no
    /// timestamp was fabricated (STD-02 §R29).
    #[error("{operation}: {source}")]
    Clock {
        /// Operation being performed.
        operation: &'static str,
        /// Why the system clock failed.
        #[source]
        source: std::time::SystemTimeError,
    },
}

impl ExtractError {
    pub(crate) fn invalid_data(operation: &'static str, reason: impl Into<String>) -> Self {
        Self::InvalidData {
            operation,
            reason: reason.into(),
        }
    }

    pub(crate) fn clock(operation: &'static str, source: std::time::SystemTimeError) -> Self {
        Self::Clock { operation, source }
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
