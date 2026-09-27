//! The diagnostic log subscriber.
//!
//! Log lines are diagnostics, so they go to stderr in every mode (STD-01
//! §R12). The default filter is `warn`, so a warning reaches the user without
//! `RUST_LOG`; `RUST_LOG` overrides it. Lines carry no ANSI escapes, because
//! nothing in the output layer styles text (STD-01 §R17).

use std::io;

use tracing_subscriber::EnvFilter;

/// Install the process-wide log subscriber once, before anything logs.
pub fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(io::stderr)
        .with_ansi(false)
        .with_target(false)
        .without_time()
        .try_init();
}
