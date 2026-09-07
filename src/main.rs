#![allow(missing_docs)]

//! JSON command-line interface for the standalone graph index.

use std::io::{self, Write};
use std::process::ExitCode;

use clap::Parser;
use orbit_graph::cli::{Cli, CliError};
use serde::Serialize;
use serde_json::json;
use tracing_subscriber::EnvFilter;

fn main() -> ExitCode {
    init_tracing();

    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) if error.exit_code() == 0 => {
            let _ = error.print();
            return ExitCode::SUCCESS;
        }
        Err(error) => return report_error(&CliError::Clap(error)),
    };

    match cli.run().and_then(|output| write_json_to_stdout(&output)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => report_error(&error),
    }
}

fn report_error(error: &CliError) -> ExitCode {
    let _ = write_json_to_stderr(&ErrorPayload::from(error));
    ExitCode::FAILURE
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("off"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(io::stderr)
        .with_target(false)
        .without_time()
        .try_init();
}

fn write_json_to_stdout<T: Serialize>(value: &T) -> Result<(), CliError> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, value).map_err(CliError::Json)?;
    stdout.write_all(b"\n").map_err(CliError::Stdout)?;
    stdout.flush().map_err(CliError::Stdout)
}

fn write_json_to_stderr<T: Serialize>(value: &T) -> io::Result<()> {
    let mut stderr = io::stderr().lock();
    serde_json::to_writer(&mut stderr, value)?;
    stderr.write_all(b"\n")?;
    stderr.flush()
}

#[derive(Debug, Serialize)]
struct ErrorPayload<'a> {
    error: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<&'a str>,
}

impl<'a> From<&'a CliError> for ErrorPayload<'a> {
    fn from(error: &'a CliError) -> Self {
        Self {
            error: json!({
                "code": error.code(),
                "message": error.to_string(),
            }),
            details: error.details(),
        }
    }
}
