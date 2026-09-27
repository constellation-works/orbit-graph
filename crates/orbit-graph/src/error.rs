use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Details of a history version contract that needs a rebuild.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionMismatchDetails {
    /// Key in `history_meta` that differs.
    pub key: String,
    /// Stored value.
    pub found: String,
    /// Value required by this binary.
    pub expected: String,
    /// History database containing the mismatch.
    pub path: PathBuf,
    /// Runnable recovery command.
    pub command: String,
}

/// The underlying failure of a [`GraphError`], shared so the error stays
/// `Clone` without rendering its source to text.
pub type ErrorSource = Arc<dyn std::error::Error + Send + Sync + 'static>;

/// A source that is only known as a message, such as a SQLite constraint the
/// graph checked itself.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct Message(String);

/// Graph crate error surface.
///
/// Each variant is one failure class; [`GraphError::class`] names it for
/// callers that translate it into their own codes (STD-02 §R10, §R12).
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum GraphError {
    /// A filesystem operation failed.
    #[error("{operation} at {}: {source}", path.display())]
    Io {
        /// Operation being performed.
        operation: &'static str,
        /// Filesystem path involved in the failed operation.
        path: PathBuf,
        /// The I/O failure.
        #[source]
        source: Arc<std::io::Error>,
    },
    /// A SQLite operation failed.
    #[error("{operation}: {source}")]
    Sqlite {
        /// Operation being performed.
        operation: &'static str,
        /// The SQLite failure.
        #[source]
        source: ErrorSource,
    },
    /// A Git object, reference, revision walk or repository operation failed.
    #[error("{operation}: {source}")]
    Git {
        /// Operation being performed.
        operation: &'static str,
        /// The libgit2 failure.
        #[source]
        source: ErrorSource,
    },
    /// The caller's input was refused before it was acted on: a selector of
    /// the wrong kind, a bound out of range, an empty query.
    #[error("{operation}: {reason}")]
    InvalidInput {
        /// Operation that refused the input.
        operation: &'static str,
        /// The input field that was refused.
        field: &'static str,
        /// Why it was refused.
        reason: String,
    },
    /// Something the caller named does not exist: a revision, a branch.
    #[error("{operation}: {what} was not found")]
    NotFound {
        /// Operation that looked for it.
        operation: &'static str,
        /// What was looked for, as the caller named it.
        what: String,
    },
    /// Stored, discovered or received data was invalid, or a request could
    /// not be satisfied from it.
    #[error("{operation}: {reason}")]
    InvalidData {
        /// Operation being performed.
        operation: &'static str,
        /// Validation failure.
        reason: String,
    },
    /// A wait or a subprocess outlived its deadline.
    #[error("{operation}: timed out after {} ms; {detail}", after.as_millis())]
    Timeout {
        /// Operation that timed out.
        operation: &'static str,
        /// The deadline that passed.
        after: Duration,
        /// What was being waited for, and on whom.
        detail: String,
    },
    /// Orbit answered a call with a structured refusal.
    #[error("{tool} refused: {code}: {message}")]
    OrbitRefused {
        /// The Orbit tool that refused.
        tool: String,
        /// Orbit's own refusal code, such as `policy_denied`.
        code: String,
        /// Orbit's message.
        message: String,
    },
    /// A subprocess failed without a structured refusal.
    #[error("{operation}: {status}: {stderr}")]
    Subprocess {
        /// Operation that ran the subprocess.
        operation: &'static str,
        /// How it ended: `exit N` or `signal N`.
        status: String,
        /// Its (bounded) standard error.
        stderr: String,
    },
    /// A read found no index where it looked; nothing was created.
    #[error("{reason}")]
    IndexMissing {
        /// Path of the index that does not exist yet.
        path: PathBuf,
        /// Actionable message naming the command that builds the index.
        reason: String,
    },
    /// An index exists but its stored schema identity is not the one this
    /// binary reads, so it is neither read nor written.
    #[error("{reason}")]
    IndexIncompatible {
        /// Path of the incompatible index.
        path: PathBuf,
        /// Actionable message naming the stored and expected identities.
        reason: String,
    },
    /// A state directory or file was refused before anything was read or
    /// written through it: a symlink, something other than the expected
    /// directory or regular file, another user's file, or repository content
    /// tracked by Git (`STD-05 §R6`, `§R7`, `§R9`).
    #[error("refusing orbit-graph state path {}: {reason}", path.display())]
    UnsafeStatePath {
        /// The path that was refused.
        path: PathBuf,
        /// Why it was refused.
        reason: String,
    },
    /// A history index uses an older extractor or import contract.
    #[error(
        "history index {} has {key}={found}; expected {expected}; run `{command}`",
        .0.path.display(),
        key = .0.key,
        found = .0.found,
        expected = .0.expected,
        command = .0.command,
    )]
    VersionMismatch(Box<VersionMismatchDetails>),
}

/// The failure class of a [`GraphError`], for callers that translate errors
/// into codes of their own. Unlike [`GraphError`] it is exhaustive, so a
/// translator names every class and a new one fails its build (STD-02 §R27).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphErrorClass {
    /// [`GraphError::InvalidInput`].
    InvalidInput,
    /// [`GraphError::NotFound`].
    NotFound,
    /// [`GraphError::Git`].
    Git,
    /// [`GraphError::Io`]; `transient` when the kind is one a retry can clear
    /// (interrupted, would block, timed out).
    Io {
        /// Whether retrying the same call can succeed.
        transient: bool,
    },
    /// [`GraphError::Sqlite`].
    Sqlite,
    /// [`GraphError::InvalidData`].
    InvalidData,
    /// [`GraphError::Timeout`].
    Timeout,
    /// [`GraphError::OrbitRefused`].
    OrbitRefused,
    /// [`GraphError::Subprocess`].
    Subprocess,
    /// [`GraphError::IndexMissing`].
    IndexMissing,
    /// [`GraphError::IndexIncompatible`].
    IndexIncompatible,
    /// [`GraphError::UnsafeStatePath`].
    UnsafeStatePath,
    /// [`GraphError::VersionMismatch`].
    VersionMismatch,
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
            source: Arc::new(source),
        }
    }

    /// Build a [`GraphError::Sqlite`] failure for `operation` from its source.
    ///
    /// Generic so the library's API names no SQLite binding types.
    pub fn sqlite(
        operation: &'static str,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Sqlite {
            operation,
            source: Arc::new(source),
        }
    }

    /// Build a [`GraphError::Sqlite`] failure for `operation` from a rendered
    /// SQLite error, when no error value is at hand.
    pub fn sqlite_message(operation: &'static str, reason: impl Into<String>) -> Self {
        Self::sqlite(operation, Message(reason.into()))
    }

    /// Build a [`GraphError::Git`] failure for `operation` from its source.
    pub fn git(
        operation: &'static str,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Git {
            operation,
            source: Arc::new(source),
        }
    }

    /// Build a [`GraphError::InvalidInput`] refusal of the caller's `field`.
    pub fn invalid_input(
        operation: &'static str,
        field: &'static str,
        reason: impl Into<String>,
    ) -> Self {
        Self::InvalidInput {
            operation,
            field,
            reason: reason.into(),
        }
    }

    /// Build a [`GraphError::NotFound`] failure for `what`.
    pub fn not_found(operation: &'static str, what: impl Into<String>) -> Self {
        Self::NotFound {
            operation,
            what: what.into(),
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

    /// Build a [`GraphError::Timeout`] for `operation` after `after`.
    pub fn timeout(operation: &'static str, after: Duration, detail: impl Into<String>) -> Self {
        Self::Timeout {
            operation,
            after,
            detail: detail.into(),
        }
    }

    /// Build a [`GraphError::OrbitRefused`] for Orbit's refusal of `tool`.
    pub fn orbit_refused(
        tool: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::OrbitRefused {
            tool: tool.into(),
            code: code.into(),
            message: message.into(),
        }
    }

    /// Build a [`GraphError::Subprocess`] failure.
    pub fn subprocess(
        operation: &'static str,
        status: impl Into<String>,
        stderr: impl Into<String>,
    ) -> Self {
        Self::Subprocess {
            operation,
            status: status.into(),
            stderr: stderr.into(),
        }
    }

    /// Build a [`GraphError::UnsafeStatePath`] refusal of `path`.
    pub fn unsafe_state_path(path: impl Into<PathBuf>, reason: impl Into<String>) -> Self {
        Self::UnsafeStatePath {
            path: path.into(),
            reason: reason.into(),
        }
    }

    /// This error's failure class.
    pub fn class(&self) -> GraphErrorClass {
        match self {
            Self::Io { source, .. } => GraphErrorClass::Io {
                transient: is_transient_io(source.kind()),
            },
            Self::Sqlite { .. } => GraphErrorClass::Sqlite,
            Self::Git { .. } => GraphErrorClass::Git,
            Self::InvalidInput { .. } => GraphErrorClass::InvalidInput,
            Self::NotFound { .. } => GraphErrorClass::NotFound,
            Self::InvalidData { .. } => GraphErrorClass::InvalidData,
            Self::Timeout { .. } => GraphErrorClass::Timeout,
            Self::OrbitRefused { .. } => GraphErrorClass::OrbitRefused,
            Self::Subprocess { .. } => GraphErrorClass::Subprocess,
            Self::IndexMissing { .. } => GraphErrorClass::IndexMissing,
            Self::IndexIncompatible { .. } => GraphErrorClass::IndexIncompatible,
            Self::UnsafeStatePath { .. } => GraphErrorClass::UnsafeStatePath,
            Self::VersionMismatch(_) => GraphErrorClass::VersionMismatch,
        }
    }
}

/// I/O kinds a retry of the same call can clear.
fn is_transient_io(kind: std::io::ErrorKind) -> bool {
    use std::io::ErrorKind;
    // `ErrorKind` is a foreign `#[non_exhaustive]` enum; any kind not named
    // here is treated as permanent.
    matches!(
        kind,
        ErrorKind::Interrupted | ErrorKind::WouldBlock | ErrorKind::TimedOut
    )
}

/// The one translator from extraction failures into the graph error surface
/// (STD-02 §R12).
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
            ExtractError::Git { operation, source } => Self::git(operation, source),
        }
    }
}
