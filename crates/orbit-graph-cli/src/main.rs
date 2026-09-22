#![allow(missing_docs)]
// Unit tests use unwrap/expect for fixture setup; production call sites remain linted.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]

//! Command-line interface for the standalone graph index.

use std::io::{self, IsTerminal, Read, Write};
use std::process::ExitCode;

use clap::{CommandFactory, FromArgMatches};
use serde::Serialize;
use serde_json::{Value, json};
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

    if let Ok(tool_name) = std::env::var("ORBIT_TOOL_NAME") {
        return run_external_tool(Some(tool_name.as_str()), None);
    }

    let mut args: Vec<_> = std::env::args_os().collect();
    if args.len() == 1 && !io::stdin().is_terminal() {
        let mut input = Vec::new();
        if io::stdin().read_to_end(&mut input).is_ok() && is_external_tool_envelope(&input) {
            return run_external_tool(None, Some(input));
        }
    }
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

fn run_external_tool(environment_tool: Option<&str>, supplied_input: Option<Vec<u8>>) -> ExitCode {
    let mut input = supplied_input.unwrap_or_default();
    if input.is_empty()
        && let Err(source) = io::stdin().read_to_end(&mut input)
    {
        return report_plugin_error(&CliError::Stdin(source));
    }
    match decode_external_tool_request(environment_tool, input.as_slice())
        .and_then(|(tool_name, input)| {
            orbit_graph::plugin::execute_external_tool(tool_name.as_str(), input.as_slice())
                .map_err(CliError::Graph)
        })
        .and_then(|output| write_json_to_stdout(&json!({"ok": true, "output": output})))
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if error.is_broken_pipe() => ExitCode::SUCCESS,
        Err(error) => report_plugin_error(&error),
    }
}

fn report_plugin_error(error: &CliError) -> ExitCode {
    let _ = write_json_to_stdout(&ErrorPayload::from(error));
    ExitCode::SUCCESS
}

fn decode_external_tool_request(
    environment_tool: Option<&str>,
    request: &[u8],
) -> Result<(String, Vec<u8>), CliError> {
    let value: Value = serde_json::from_slice(request).map_err(CliError::Json)?;
    let envelope_tool = value
        .get("tool")
        .and_then(Value::as_str)
        .filter(|_| value.get("input").is_some());
    if let Some(envelope_tool) = envelope_tool {
        if let Some(environment_tool) = environment_tool
            && environment_tool != envelope_tool
        {
            return Err(CliError::Graph(orbit_graph::GraphError::invalid_data(
                "select Orbit external tool",
                format!(
                    "ORBIT_TOOL_NAME {environment_tool:?} does not match envelope tool {envelope_tool:?}"
                ),
            )));
        }
        if !orbit_graph::plugin::recognizes_tool(envelope_tool) {
            return Err(CliError::Graph(orbit_graph::GraphError::invalid_data(
                "select Orbit external tool",
                format!("unsupported envelope tool {envelope_tool:?}"),
            )));
        }
        let input = serde_json::to_vec(&value["input"]).map_err(CliError::Json)?;
        return Ok((envelope_tool.to_string(), input));
    }

    let tool_name = environment_tool.ok_or_else(|| {
        CliError::Graph(orbit_graph::GraphError::invalid_data(
            "select Orbit external tool",
            "request must provide a top-level tool and input",
        ))
    })?;
    if !orbit_graph::plugin::recognizes_tool(tool_name) {
        return Err(CliError::Graph(orbit_graph::GraphError::invalid_data(
            "select Orbit external tool",
            format!("unsupported ORBIT_TOOL_NAME {tool_name:?}"),
        )));
    }
    let _ = writeln!(
        io::stderr().lock(),
        "warning: bare Orbit plugin requests are deprecated; send the v2 tool/input envelope"
    );
    Ok((tool_name.to_string(), request.to_vec()))
}

fn is_external_tool_envelope(input: &[u8]) -> bool {
    serde_json::from_slice::<Value>(input).is_ok_and(|value| {
        value.get("tool").and_then(Value::as_str).is_some() && value.get("input").is_some()
    })
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

#[derive(Debug, Serialize)]
struct ErrorPayload<'a> {
    ok: bool,
    error: ErrorDetails<'a>,
}

#[derive(Debug, Serialize)]
struct ErrorDetails<'a> {
    code: &'a str,
    message: String,
    retryable: bool,
}

impl<'a> From<&'a CliError> for ErrorPayload<'a> {
    fn from(error: &'a CliError) -> Self {
        Self {
            ok: false,
            error: ErrorDetails {
                code: error.code(),
                message: error.to_string(),
                retryable: false,
            },
        }
    }
}
