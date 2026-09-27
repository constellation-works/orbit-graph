//! JSON and NDJSON bytes, and the structured error object.
//!
//! Both machine modes write through [`write_json`], so the trailing newline and
//! flush behavior of a document and of one NDJSON record cannot drift apart.

use std::io::{self, Write};

use serde::Serialize;
use serde_json::Value;

use crate::command::CliError;

/// Write one JSON value, followed by a newline, and flush the writer.
pub(crate) fn write_json(
    writer: &mut dyn Write,
    value: &Value,
    pretty: bool,
) -> Result<(), CliError> {
    if pretty {
        serde_json::to_writer_pretty(&mut *writer, value).map_err(CliError::Json)?;
    } else {
        serde_json::to_writer(&mut *writer, value).map_err(CliError::Json)?;
    }
    writer.write_all(b"\n").map_err(CliError::Stdout)?;
    writer.flush().map_err(CliError::Stdout)
}

/// Write one compact Orbit plugin protocol response line to stdout.
///
/// The plugin protocol is separate from terminal rendering: its response and
/// error envelopes always go to stdout, whatever the output mode.
pub(crate) fn write_plugin_response<T: Serialize>(value: &T) -> Result<(), CliError> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, value).map_err(CliError::Json)?;
    stdout.write_all(b"\n").map_err(CliError::Stdout)?;
    stdout.flush().map_err(CliError::Stdout)
}

/// The machine-mode failure object written to stderr (STD-01 §R19): the
/// message under `error` and its stable `snake_case` `code`.
#[derive(Debug, Serialize)]
pub(crate) struct ErrorPayload {
    error: String,
    code: &'static str,
}

impl From<&CliError> for ErrorPayload {
    fn from(error: &CliError) -> Self {
        Self {
            error: error.to_string(),
            code: error.code(),
        }
    }
}
