//! Process settings the composition layer resolves once and hands to the
//! library before its first call (STD-02 §R3).
//!
//! The library reads no environment variable, argument or other process
//! input itself: the `orbit-graph` CLI resolves these values from its
//! environment at start-up and installs them with [`install_runtime`]. An
//! embedder that installs nothing gets [`RuntimeConfig::default`].

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

/// Default wait for a graph lock before failing (`STD-03 §R7`).
pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(30);

/// A point where a sync aborts the process, for tests that interrupt a sync
/// through the real `orbit-graph` binary. An abort stands in for a kill.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncFaultPoint {
    /// Abort once pass 1 has committed.
    AfterPass1,
    /// Abort halfway through pass 2's refs, before its commit.
    MidPass2,
}

impl SyncFaultPoint {
    /// The point `name` selects (`after-pass1`, `mid-pass2`); any other name
    /// selects none.
    pub fn from_name(name: &str) -> Option<Self> {
        [Self::AfterPass1, Self::MidPass2]
            .into_iter()
            .find(|point| point.name() == name)
    }

    /// The name that selects this point.
    pub const fn name(self) -> &'static str {
        match self {
            Self::AfterPass1 => "after-pass1",
            Self::MidPass2 => "mid-pass2",
        }
    }
}

/// Resolved process settings.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// How long a graph or history lock is waited for; zero tries once.
    pub lock_timeout: Duration,
    /// The executable name written into lock holder records.
    pub program_name: String,
    /// Where temporary evaluation checkouts are created. `None` leaves the
    /// choice to the `tempfile` crate's platform default; the CLI always
    /// sets it.
    pub temp_dir: Option<PathBuf>,
    /// Test hook: the sync point that aborts the process.
    #[doc(hidden)]
    pub sync_fault: Option<SyncFaultPoint>,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            lock_timeout: DEFAULT_LOCK_TIMEOUT,
            program_name: "orbit-graph".to_string(),
            temp_dir: None,
            sync_fault: None,
        }
    }
}

static RUNTIME: OnceLock<RuntimeConfig> = OnceLock::new();

/// Install the process settings. Only the first call takes effect; a later
/// one returns its configuration back unused.
pub fn install_runtime(config: RuntimeConfig) -> Result<(), RuntimeConfig> {
    RUNTIME.set(config)
}

/// The installed settings, or the defaults when none were installed.
pub(crate) fn runtime() -> &'static RuntimeConfig {
    RUNTIME.get_or_init(RuntimeConfig::default)
}
