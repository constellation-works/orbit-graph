//! The one place a command's records reach the process streams.
//!
//! `main` resolves the sink, dispatches, and hands the returned
//! [`CommandOutput`] here. Records go to stdout in the resolved mode; every
//! diagnostic — empty-state lines, dropped-column notices, and failures — goes
//! to stderr.

use std::io::{self, Write};

use crate::command::CliError;
use crate::output::json::{ErrorPayload, write_json};
use crate::output::payload::{CommandOutput, View, ViewBlock};
use crate::output::sink::{OutputMode, OutputSink};
use crate::output::table::emit_table;

/// Render a command output to the two process streams selected by the contract.
pub fn emit(
    output: &CommandOutput,
    sink: OutputSink,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<(), CliError> {
    match sink.mode() {
        OutputMode::Json => write_json(stdout, &output.document, sink.is_tty()),
        OutputMode::Ndjson => {
            if let Some(records) = &output.ndjson_records {
                for record in records {
                    write_json(stdout, record, false)?;
                    stdout.flush().map_err(CliError::Stdout)?;
                }
                Ok(())
            } else {
                write_json(stdout, &output.document, false)
            }
        }
        OutputMode::Table | OutputMode::Plain => match &output.view {
            View::Document => write_json(stdout, &output.document, true),
            View::Blocks(blocks) => emit_blocks(blocks, sink, stdout, stderr),
        },
    }
}

fn emit_blocks(
    blocks: &[ViewBlock],
    sink: OutputSink,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<(), CliError> {
    for block in blocks {
        match block {
            ViewBlock::Text(text) => {
                stdout
                    .write_all(text.as_bytes())
                    .map_err(CliError::Stdout)?;
                if !text.ends_with('\n') {
                    stdout.write_all(b"\n").map_err(CliError::Stdout)?;
                }
            }
            ViewBlock::Table(table) => emit_table(table, sink, stdout, stderr)?,
        }
    }
    stdout.flush().map_err(CliError::Stdout)
}

/// Write a command failure to stderr in the sink's selected protocol.
pub fn emit_error(error: &CliError, sink: OutputSink) {
    let mut stderr = io::stderr().lock();
    if sink.structured_errors() {
        let _ = serde_json::to_writer(&mut stderr, &ErrorPayload::from(error));
        let _ = stderr.write_all(b"\n");
    } else if let CliError::Clap(error) = error {
        let _ = write!(stderr, "{error}");
    } else {
        let _ = writeln!(stderr, "error: {error}");
    }
    let _ = stderr.flush();
}
