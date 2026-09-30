// Unit tests use unwrap/expect for fixture setup; production call sites remain linted.
// Test fixtures write files with `fs::write`, which `clippy.toml` bans from
// shipped code in favour of `orbit_graph::atomic_write` (STD-03 §R5).
#![cfg_attr(
    test,
    allow(clippy::expect_used, clippy::unwrap_used, clippy::disallowed_methods)
)]

//! Command-line interface for the standalone graph index.

use std::ffi::OsString;
use std::io::{self, Read};
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use clap::{CommandFactory, FromArgMatches};
use serde::Serialize;
use serde_json::{Value, json};

use crate::command::{Cli, CliError};
use crate::output::json::write_plugin_response;
use crate::output::{
    OutputSink, emit_error, emit_help, emit_notice, emit_to_process, init_logging,
    install_format_argument, requested_format_from_args, requested_output,
};
use crate::plugin::{
    ORBIT_TIMEOUT_ENV, PLUGIN_STATE_ENV, PluginConfig, PluginEnvironment, ToolError,
};
use orbit_graph::{DEFAULT_LOCK_TIMEOUT, RuntimeConfig, SyncFaultPoint};

mod command;
mod output;
mod plugin;

#[cfg(test)]
mod tests;

/// The deprecation warning for the former `overview --format summary|full`
/// detail spelling (STD-01 §R35).
const LEGACY_DETAIL_WARNING: &str =
    "warning: `overview --format summary|full` is deprecated; use `overview --detail summary|full`";

/// `ORBIT_GRAPH_LOCK_TIMEOUT_MS`: how long a graph or history lock is
/// waited for, in whole milliseconds; `0` tries once.
const LOCK_TIMEOUT_ENV: &str = "ORBIT_GRAPH_LOCK_TIMEOUT_MS";
/// `ORBIT_GRAPH_FAULT_INJECT`: test hook naming a sync point that aborts
/// the process.
const FAULT_INJECT_ENV: &str = "ORBIT_GRAPH_FAULT_INJECT";

/// Bound the complete plugin request before JSON decoding, including its
/// context and config. Tool inputs contain selectors and query parameters,
/// never source files or graph data (STD-03 §R22).
const MAX_PLUGIN_REQUEST_BYTES: u64 = 1024 * 1024;

fn main() -> ExitCode {
    init_logging();

    let mut args: Vec<OsString> = std::env::args_os().collect();
    // The library reads no process input: its settings are resolved here,
    // once, and installed before anything runs (STD-02 §R3).
    let runtime = resolve_runtime(
        args.first().map(OsString::as_os_str),
        std::env::var_os(LOCK_TIMEOUT_ENV),
        std::env::var_os(FAULT_INJECT_ENV),
    );
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
        return run_external_tool(tool_name.as_str(), runtime);
    }
    match runtime {
        Ok(runtime) => install_runtime(runtime),
        Err(error) => {
            emit_error(
                &error,
                OutputSink::from_process(requested_format_from_args(&args)),
            );
            return process_exit_code(error.exit_code());
        }
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

/// Set by the plugin launcher (`bin/orbit-graph`) when the manifest's named
/// `--allow-unbound-backend` override ran this executable without a recorded
/// SHA-256; the value is the executable's path. Every response then carries
/// it as `backend_override`, so an override call is never indistinguishable
/// from a bound one (STD-05 §R4). It only labels the response: it grants
/// nothing.
const BACKEND_OVERRIDE_ENV: &str = "ORBIT_GRAPH_BACKEND_OVERRIDE";

/// Resolve the library's process settings from `argv[0]` and the raw
/// values of [`LOCK_TIMEOUT_ENV`] and [`FAULT_INJECT_ENV`]; a malformed
/// value is refused naming its variable (STD-02 §R28).
fn resolve_runtime(
    program: Option<&std::ffi::OsStr>,
    lock_timeout: Option<OsString>,
    sync_fault: Option<OsString>,
) -> Result<RuntimeConfig, CliError> {
    let program_name = program
        .and_then(|program| Path::new(program).file_name())
        .map_or_else(
            || "orbit-graph".to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
    let lock_timeout = match lock_timeout {
        None => DEFAULT_LOCK_TIMEOUT,
        Some(value) => value
            .to_str()
            .and_then(|value| value.parse::<u64>().ok())
            .map(Duration::from_millis)
            .ok_or_else(|| {
                CliError::Usage(format!(
                    "{LOCK_TIMEOUT_ENV}={value:?} is not a whole number of milliseconds; set \
                     it to a non-negative integer (0 tries the lock once) or unset it"
                ))
            })?,
    };
    let sync_fault = match sync_fault {
        None => None,
        Some(value) => Some(
            value
                .to_str()
                .and_then(SyncFaultPoint::from_name)
                .ok_or_else(|| {
                    CliError::Usage(format!(
                        "{FAULT_INJECT_ENV}={value:?} names no sync fault point; use {} or {}",
                        SyncFaultPoint::AfterPass1.name(),
                        SyncFaultPoint::MidPass2.name()
                    ))
                })?,
        ),
    };
    Ok(RuntimeConfig {
        lock_timeout,
        program_name,
        temp_dir: Some(std::env::temp_dir()),
        sync_fault,
    })
}

fn install_runtime(runtime: RuntimeConfig) {
    // `main` is the only caller and runs once, so nothing was installed
    // before; a second install would be ignored, not half-applied.
    let _ = orbit_graph::install_runtime(runtime);
}

fn run_external_tool(environment_tool: &str, runtime: Result<RuntimeConfig, CliError>) -> ExitCode {
    let backend_override = std::env::var(BACKEND_OVERRIDE_ENV)
        .ok()
        .filter(|path| !path.is_empty());
    // A malformed setting is the request's environment, refused in the
    // envelope's own vocabulary.
    let runtime = runtime.map_err(|error| {
        CliError::Tool(ToolError::invalid_request(
            "read plugin environment",
            error.to_string(),
        ))
    });
    let environment = runtime.map(install_runtime).and_then(|()| {
        PluginEnvironment::resolve(
            std::env::var_os(PLUGIN_STATE_ENV),
            std::env::var_os(ORBIT_TIMEOUT_ENV),
        )
        .map_err(CliError::Tool)
    });
    let environment = match environment {
        Ok(environment) => environment,
        Err(error) => return report_plugin_error(&error, backend_override.as_deref()),
    };
    let mut input = Vec::new();
    if let Err(source) = io::stdin()
        .lock()
        .take(MAX_PLUGIN_REQUEST_BYTES + 1)
        .read_to_end(&mut input)
    {
        return report_plugin_error(&CliError::Stdin(source), backend_override.as_deref());
    }
    if input.len() as u64 > MAX_PLUGIN_REQUEST_BYTES {
        return report_plugin_error(
            &invalid_request(
                "read Orbit plugin request",
                format!(
                    "request exceeds {MAX_PLUGIN_REQUEST_BYTES} bytes; reduce the input or context"
                ),
            ),
            backend_override.as_deref(),
        );
    }
    match decode_external_tool_request(environment_tool, input.as_slice())
        .and_then(|(tool_name, input, config)| {
            crate::plugin::execute_external_tool(
                tool_name.as_str(),
                input.as_slice(),
                &config,
                &environment,
            )
            .map_err(CliError::Tool)
        })
        .and_then(|output| {
            write_plugin_response(&with_backend_override(
                json!({"ok": true, "output": output}),
                backend_override.as_deref(),
            ))
        }) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if error.is_broken_pipe() => ExitCode::SUCCESS,
        Err(error) => report_plugin_error(&error, backend_override.as_deref()),
    }
}

fn report_plugin_error(error: &CliError, backend_override: Option<&str>) -> ExitCode {
    let payload = serde_json::to_value(ErrorPayload::from(error)).unwrap_or_else(|_| {
        json!({"ok": false, "error": {
            "code": error.code(),
            "message": error.to_string(),
            "retryable": error.retryable(),
        }})
    });
    let _ = write_plugin_response(&with_backend_override(payload, backend_override));
    ExitCode::SUCCESS
}

/// Add `backend_override` beside `ok` when the launcher ran this executable
/// under the unbound-backend override.
fn with_backend_override(mut response: Value, backend_override: Option<&str>) -> Value {
    if let (Some(path), Some(fields)) = (backend_override, response.as_object_mut()) {
        fields.insert("backend_override".to_string(), json!(path));
    }
    response
}

fn decode_external_tool_request(
    environment_tool: &str,
    request: &[u8],
) -> Result<(String, Vec<u8>, PluginConfig), CliError> {
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
        let Some(tool) = crate::plugin::find_tool(envelope_tool) else {
            return Err(CliError::Tool(ToolError::invalid_request(
                "select Orbit external tool",
                format!("unsupported envelope tool {envelope_tool:?}"),
            )));
        };
        let mut input = value["input"].clone();
        if tool.needs_repository
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
        // The effective `[plugins.graph]` section: `branch` defaults the
        // landing branch of calls that name none.
        let (config, ignored) =
            PluginConfig::from_context(value.pointer("/context/config")).map_err(CliError::Tool)?;
        for key in ignored {
            emit_notice(&format!(
                "warning: ignoring plugin config key {key:?}, which this orbit-graph does not read"
            ));
        }
        let input = serde_json::to_vec(&input).map_err(CliError::Json)?;
        return Ok((envelope_tool.to_string(), input, config));
    }

    let tool_name = environment_tool;
    if crate::plugin::find_tool(tool_name).is_none() {
        return Err(CliError::Tool(ToolError::invalid_request(
            "select Orbit external tool",
            format!("unsupported ORBIT_TOOL_NAME {tool_name:?}"),
        )));
    }
    emit_notice(
        "warning: bare Orbit plugin requests are deprecated; send the v2 tool/input envelope",
    );
    Ok((
        tool_name.to_string(),
        request.to_vec(),
        PluginConfig::default(),
    ))
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
    /// For an Orbit refusal, the refusing tool and Orbit's own code.
    #[serde(skip_serializing_if = "Option::is_none")]
    orbit: Option<Value>,
}

impl<'a> From<&'a CliError> for ErrorPayload<'a> {
    fn from(error: &'a CliError) -> Self {
        Self {
            ok: false,
            error: ErrorDetails {
                code: error.code(),
                message: error.to_string(),
                retryable: error.retryable(),
                orbit: error.graph_error().and_then(crate::plugin::orbit_detail),
            },
        }
    }
}
