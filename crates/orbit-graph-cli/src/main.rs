#![allow(missing_docs)]
// Unit tests use unwrap/expect for fixture setup; production call sites remain linted.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]

//! Command-line interface for the standalone graph index.

use std::io::{self, Read, Write};
use std::process::ExitCode;

use clap::{CommandFactory, FromArgMatches};
use serde::Serialize;
use serde_json::json;
use tracing_subscriber::EnvFilter;

use crate::command::{Cli, CliError};
use crate::output::{
    CommandOutput, OutputSink, emit, emit_error, install_format_argument, requested_format,
    requested_format_from_args,
};

mod command;
mod output;

#[cfg(test)]
mod tests;

fn main() -> ExitCode {
    init_tracing();

    if let Ok(tool_name) = std::env::var("ORBIT_TOOL_NAME")
        && orbit_graph::plugin::recognizes_tool(tool_name.as_str())
    {
        return run_external_tool(tool_name.as_str());
    }

    let mut args: Vec<_> = std::env::args_os().collect();
    if args.len() == 1 {
        args.push("--help".into());
    }
    let fallback_format = requested_format_from_args(&args);
    let matches = match install_format_argument(Cli::command()).try_get_matches_from(args) {
        Ok(matches) => matches,
        Err(error) if error.exit_code() == 0 => {
            let _ = error.print();
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            let exit_code = error.exit_code();
            let sink = OutputSink::from_process(fallback_format);
            emit_error(&CliError::Clap(error), sink);
            return process_exit_code(exit_code);
        }
    };
    let format = requested_format(&matches);
    let cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(error) => {
            let exit_code = error.exit_code();
            let sink = OutputSink::from_process(format);
            emit_error(&CliError::Clap(error), sink);
            return process_exit_code(exit_code);
        }
    };
    let sink = OutputSink::from_process(format);

    match cli.run().and_then(|output| emit_to_process(&output, sink)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if error.is_broken_pipe() => ExitCode::SUCCESS,
        Err(error) => {
            emit_error(&error, sink);
            ExitCode::FAILURE
        }
    }
}

fn run_external_tool(tool_name: &str) -> ExitCode {
    let mut input = Vec::new();
    if let Err(source) = io::stdin().read_to_end(&mut input) {
        return report_plugin_error(&CliError::Stdin(source));
    }
    match orbit_graph::plugin::execute_external_tool(tool_name, input.as_slice())
        .map_err(CliError::Graph)
        .and_then(|output| write_json_to_stdout(&output))
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if error.is_broken_pipe() => ExitCode::SUCCESS,
        Err(error) => report_plugin_error(&error),
    }
}

fn report_plugin_error(error: &CliError) -> ExitCode {
    let _ = write_json_to_stderr(&ErrorPayload::from(error));
    ExitCode::FAILURE
}

fn emit_to_process(output: &CommandOutput, sink: OutputSink) -> Result<(), CliError> {
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    emit(output, sink, &mut stdout, &mut stderr)
}

fn process_exit_code(code: i32) -> ExitCode {
    u8::try_from(code).map_or(ExitCode::FAILURE, ExitCode::from)
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
