//! Declares process environment keys and their documentation without importing sibling units.
//!
//! Each variable the CLI reads is declared once here with its meaning, default
//! value, and overridden flag (STD-01 §R25). `scripts/check-env-vars.sh`
//! generates the `## Environment` table in `docs/usage.md` from these
//! declarations and fails when the table, the constants, or production reads
//! disagree.

/// Orbit external-tool plugin protocol signal; when set, stdin is read as an
/// Orbit plugin JSON request and no CLI arguments are permitted.
///
/// Default: unset
/// Overridden flag: none
pub const TOOL_NAME_ENV: &str = "ORBIT_TOOL_NAME";

/// Executable path set by the plugin launcher under `--allow-unbound-backend`;
/// attaches `backend_override` to every plugin JSON response.
///
/// Default: unset
/// Overridden flag: none
pub const BACKEND_OVERRIDE_ENV: &str = "ORBIT_GRAPH_BACKEND_OVERRIDE";

/// Lock wait timeout for graph and history databases in whole milliseconds (`0`
/// tries once).
///
/// Default: `30000`
/// Overridden flag: none
pub const LOCK_TIMEOUT_ENV: &str = "ORBIT_GRAPH_LOCK_TIMEOUT_MS";

/// Test hook naming a sync fault point (`after-pass1` or `mid-pass2`) that
/// aborts the process during `sync`.
///
/// Default: unset
/// Overridden flag: none
pub const FAULT_INJECT_ENV: &str = "ORBIT_GRAPH_FAULT_INJECT";

/// Plugin state root directory granted to plugin tools for the code-graph index
/// and snapshot cache.
///
/// Default: unset
/// Overridden flag: none
pub const PLUGIN_STATE_ENV: &str = "ORBIT_PLUGIN_STATE";

/// Per-call timeout in whole seconds (`1`–`60`) for nested Orbit callbacks.
///
/// Default: `10`
/// Overridden flag: none
pub const ORBIT_TIMEOUT_ENV: &str = "GRAPH_ORBIT_TIMEOUT_SECONDS";

/// Terminal column width override (positive integer) used only when stdout is
/// a terminal.
///
/// Default: terminal width (`TIOCGWINSZ`, else `0`)
/// Overridden flag: none
pub const COLUMNS_ENV: &str = "COLUMNS";

/// Standalone output mode override (`auto`, `table`, `json`, or `ndjson`);
/// unrecognized values fall back to `auto`.
///
/// Default: `auto`
/// Overridden flag: `--format`, `--json`
pub const FORMAT_ENV: &str = "ORBIT_GRAPH_FORMAT";

/// Tracing diagnostic log filter written to stderr (for example
/// `orbit_graph=debug`).
///
/// Default: `warn`
/// Overridden flag: none
pub const RUST_LOG_ENV: &str = "RUST_LOG";

/// Every documented environment variable constant, in usage-table order.
#[cfg(test)]
pub const ENV_VARS: &[&str] = &[
    TOOL_NAME_ENV,
    BACKEND_OVERRIDE_ENV,
    LOCK_TIMEOUT_ENV,
    FAULT_INJECT_ENV,
    PLUGIN_STATE_ENV,
    ORBIT_TIMEOUT_ENV,
    COLUMNS_ENV,
    FORMAT_ENV,
    RUST_LOG_ENV,
];
