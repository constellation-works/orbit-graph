// Unit tests use unwrap/expect for fixture setup; production call sites remain linted.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]

//! Command-line interface for the standalone graph index.

use std::ffi::OsString;
use std::io::{self, Read};
use std::process::ExitCode;

use clap::{CommandFactory, FromArgMatches};
use serde::Serialize;
use serde_json::{Value, json};

use crate::command::{Cli, CliError};
use crate::output::json::write_plugin_response;
use crate::output::{
    OutputSink, emit_error, emit_help, emit_notice, emit_to_process, init_logging,
    install_format_argument, requested_format_from_args, requested_output,
};
use crate::plugin::ToolError;

mod command;
mod output;
mod plugin;

#[cfg(test)]
mod tests;

/// The deprecation warning for the former `overview --format summary|full`
/// detail spelling (STD-01 §R35).
const LEGACY_DETAIL_WARNING: &str =
    "warning: `overview --format summary|full` is deprecated; use `overview --detail summary|full`";

fn main() -> ExitCode {
    init_logging();

    let mut args: Vec<OsString> = std::env::args_os().collect();
    // `ORBIT_TOOL_NAME` is the explicit signal for the Orbit plugin protocol;
    // only then is stdin read as a request envelope. It cannot be combined
    // with command-line arguments: one of the two would be ignored (STD-01
    // §R28).
    if let Ok(tool_name) = std::env::var("ORBIT_TOOL_NAME") {
        if args.len() > 1 {
            let error = CliError::Usage(format!(
                "ORBIT_TOOL_NAME={tool_name:?} selects the Orbit plugin protocol, which reads \
                 its request from stdin and takes no arguments, but arguments were given; \
                 unset ORBIT_TOOL_NAME to run the command, or drop the arguments to run the \
                 plugin tool"
            ));
            emit_error(
                &error,
                OutputSink::from_process(requested_format_from_args(&args)),
            );
            return process_exit_code(error.exit_code());
        }
        return run_external_tool(tool_name.as_str());
    }

    if args.len() == 1 {
        args.push("--help".into());
    }
    let fallback_format = requested_format_from_args(&args);
    let matches = match install_format_argument(Cli::command()).try_get_matches_from(args) {
        Ok(matches) => matches,
        Err(error) if error.exit_code() == 0 => {
            emit_help(&error);
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            let exit_code = error.exit_code();
            let sink = OutputSink::from_process(fallback_format);
            emit_error(&CliError::Clap(error), sink);
            return process_exit_code(exit_code);
        }
    };
    let request = match requested_output(&matches) {
        Ok(request) => request,
        Err(error) => {
            emit_error(&error, OutputSink::from_process(fallback_format));
            return process_exit_code(error.exit_code());
        }
    };
    let sink = OutputSink::from_process(request.format);
    let mut cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(error) => {
            let exit_code = error.exit_code();
            emit_error(&CliError::Clap(error), sink);
            return process_exit_code(exit_code);
        }
    };
    if let Some(detail) = request.legacy_detail {
        emit_notice(LEGACY_DETAIL_WARNING);
        if let Err(error) = cli.apply_legacy_overview_detail(detail) {
            emit_error(&error, sink);
            return process_exit_code(error.exit_code());
        }
    }

    match cli.run().and_then(|output| emit_to_process(&output, sink)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if error.is_broken_pipe() => ExitCode::SUCCESS,
        Err(error) => {
            emit_error(&error, sink);
            process_exit_code(error.exit_code())
        }
    }
}

fn run_external_tool(environment_tool: &str) -> ExitCode {
    let mut input = Vec::new();
    if let Err(source) = io::stdin().read_to_end(&mut input) {
        return report_plugin_error(&CliError::Stdin(source));
    }
    match decode_external_tool_request(environment_tool, input.as_slice())
        .and_then(|(tool_name, input)| {
            crate::plugin::execute_external_tool(tool_name.as_str(), input.as_slice())
                .map_err(CliError::Tool)
        })
        .and_then(|output| write_plugin_response(&json!({"ok": true, "output": output})))
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if error.is_broken_pipe() => ExitCode::SUCCESS,
        Err(error) => report_plugin_error(&error),
    }
}

fn report_plugin_error(error: &CliError) -> ExitCode {
    let _ = write_plugin_response(&ErrorPayload::from(error));
    ExitCode::SUCCESS
}

fn decode_external_tool_request(
    environment_tool: &str,
    request: &[u8],
) -> Result<(String, Vec<u8>), CliError> {
    let value: Value = serde_json::from_slice(request)
        .map_err(|error| invalid_request("decode Orbit plugin request", error.to_string()))?;
    let envelope_tool = value
        .get("tool")
        .and_then(Value::as_str)
        .filter(|_| value.get("input").is_some());
    if let Some(envelope_tool) = envelope_tool {
        if environment_tool != envelope_tool {
            return Err(CliError::Tool(ToolError::invalid_request(
                "select Orbit external tool",
                format!(
                    "ORBIT_TOOL_NAME {environment_tool:?} does not match envelope tool {envelope_tool:?}"
                ),
            )));
        }
        if !crate::plugin::recognizes_tool(envelope_tool) {
            return Err(CliError::Tool(ToolError::invalid_request(
                "select Orbit external tool",
                format!("unsupported envelope tool {envelope_tool:?}"),
            )));
        }
        let mut input = value["input"].clone();
        if !envelope_tool.ends_with(".version")
            && let Some(input) = input.as_object_mut()
            && !input.contains_key("repository")
        {
            let workspace_root = value
                .pointer("/context/workspace_root")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    CliError::Tool(ToolError::invalid_request(
                        "route Orbit plugin repository",
                        "repository is required when context.workspace_root is unavailable",
                    ))
                })?;
            input.insert(
                "repository".to_string(),
                Value::String(workspace_root.to_string()),
            );
        }
        let input = serde_json::to_vec(&input).map_err(CliError::Json)?;
        return Ok((envelope_tool.to_string(), input));
    }

    let tool_name = environment_tool;
    if !crate::plugin::recognizes_tool(tool_name) {
        return Err(CliError::Tool(ToolError::invalid_request(
            "select Orbit external tool",
            format!("unsupported ORBIT_TOOL_NAME {tool_name:?}"),
        )));
    }
    emit_notice(
        "warning: bare Orbit plugin requests are deprecated; send the v2 tool/input envelope",
    );
    Ok((tool_name.to_string(), request.to_vec()))
}

fn invalid_request(operation: &'static str, reason: String) -> CliError {
    CliError::Tool(ToolError::invalid_request(operation, reason))
}

fn process_exit_code(code: i32) -> ExitCode {
    u8::try_from(code).map_or(ExitCode::FAILURE, ExitCode::from)
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
