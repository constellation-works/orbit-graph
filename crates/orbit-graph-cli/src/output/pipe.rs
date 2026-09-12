//! A closed stdout is a normal way for a command to end, not a failure.
//!
//! `orbit-graph search foo | head -1` closes the read end after one line, and
//! the next write fails with `EPIPE`. The process exits `0` silently instead of
//! reporting an error, so `set -o pipefail` does not turn a consumer that read
//! what it wanted into a failed script.

use std::io::ErrorKind;

use crate::command::CliError;

/// Whether this error is a closed stdout pipe, which is a successful stop.
pub(crate) fn is_broken_pipe(error: &CliError) -> bool {
    match error {
        CliError::Json(error) => error.io_error_kind() == Some(ErrorKind::BrokenPipe),
        CliError::Stdout(error) => error.kind() == ErrorKind::BrokenPipe,
        _ => false,
    }
}
