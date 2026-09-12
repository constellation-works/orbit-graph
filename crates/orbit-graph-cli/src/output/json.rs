//! JSON and NDJSON bytes, and the structured error envelope.
//!
//! Both machine modes write through [`write_json`], so the trailing newline and
//! flush behavior of a document and of one NDJSON record cannot drift apart.

use std::io::Write;

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

/// The stable failure envelope emitted in the two machine modes.
#[derive(Debug, Serialize)]
pub(crate) struct ErrorPayload<'a> {
    error: ErrorBody,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<&'a str>,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
}

impl<'a> From<&'a CliError> for ErrorPayload<'a> {
    fn from(error: &'a CliError) -> Self {
        Self {
            error: ErrorBody {
                code: error.code(),
                message: error.to_string(),
            },
            details: error.details(),
        }
    }
}
