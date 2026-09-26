use std::path::PathBuf;
use std::time::Duration;

/// Sync mode requested by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncMode {
    /// Incremental sync driven by file metadata and content hashes.
    Auto,
    /// Full sync that rehashes and re-extracts all indexable files.
    Full,
}

/// Policy controlling whether reads refresh the graph before querying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncPolicy {
    /// Never auto-sync; callers invoke [`crate::Graph::sync`] explicitly.
    Manual,
    /// Sync inline on every query.
    OnRead,
    /// Sync inline only if the last successful sync is older than `window`.
    Windowed {
        /// Maximum age of the last successful sync before reads refresh.
        window: Duration,
    },
    /// Run an initial sync at open, then keep the index fresh with a background watcher.
    Watch {
        /// Event coalescing window before a background sync starts.
        debounce: Duration,
    },
}

/// Summary returned after a graph sync completes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncReport {
    /// Number of files present in the graph after sync.
    pub files_indexed: usize,
    /// Number of files inserted or refreshed by this sync.
    pub files_changed: usize,
    /// Number of files removed from the graph by this sync.
    pub files_removed: usize,
    /// Wall-clock duration spent syncing.
    pub duration: Duration,
    /// Paths this sync could not read or extract, one entry each. The sync
    /// isolated them and indexed everything else; an already indexed path
    /// among them keeps its previous rows.
    pub failed: Vec<SyncFailure>,
    /// Paths this sync deliberately did not index, such as files larger than
    /// the 4 MiB byte cap.
    pub skipped: Vec<SyncSkip>,
    /// The graph database this sync wrote.
    pub database_path: PathBuf,
    /// The branch the graph database indexes.
    pub branch: String,
}

/// A path a sync could not read or extract. See [`SyncReport::failed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncFailure {
    /// Worktree-relative path, `/`-separated.
    pub path: String,
    /// What the sync was doing, such as `scan directory` or
    /// `read file for content hash`.
    pub operation: String,
    /// Stable class of the error: `permission_denied`, `not_found`, `io`,
    /// `invalid_data`, `parse_timeout`, `unsupported` or `panic`.
    pub error_kind: String,
    /// The error as reported, for diagnosis.
    pub message: String,
}

/// A path a sync deliberately did not index. See [`SyncReport::skipped`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncSkip {
    /// Worktree-relative path, `/`-separated.
    pub path: String,
    /// Why it was skipped; `oversize` for a file above the byte cap.
    pub reason: String,
}

/// Phase of [`crate::Graph::sync_with_observer`] a [`SyncProgress`] report describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncPhase {
    /// Pass 1: changed files are read, parsed, and written; symbols, imports,
    /// and raw refs are extracted. `units_done`/`units_total` mirror
    /// `files_indexed`/`files_seen` in this phase.
    Extracting,
    /// Pass 2: raw refs extracted from every touched file are resolved
    /// against the confidence ladder and written. `files_seen`,
    /// `files_indexed`, and `current_path` are frozen at pass 1's final
    /// values during this phase; `units_done`/`units_total` count refs
    /// resolved so far and refs to resolve.
    Resolving,
}

impl SyncPhase {
    /// Stable label used by callers that serialize this phase, such as the
    /// change-explorer service.
    pub fn label(self) -> &'static str {
        match self {
            Self::Extracting => "extracting",
            Self::Resolving => "resolving",
        }
    }
}

/// Progress observed while [`crate::Graph::sync_with_observer`] processes files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncProgress {
    /// Phase this report describes.
    pub phase: SyncPhase,
    /// Total files this sync will touch, written or removed. Pass 1 extracts
    /// in bounded chunks, so during [`SyncPhase::Extracting`] this starts as
    /// every changed or removed file and drops by each file whose extraction
    /// fails as its chunk is extracted. Frozen at pass 1's final count once
    /// `phase` is [`SyncPhase::Resolving`].
    pub files_seen: usize,
    /// Files this sync has processed so far; increases monotonically up to
    /// `files_seen`. Frozen at pass 1's final count once `phase` is
    /// [`SyncPhase::Resolving`].
    pub files_indexed: usize,
    /// Path of the file most recently processed, once any has been. Frozen
    /// at pass 1's last file once `phase` is [`SyncPhase::Resolving`].
    pub current_path: Option<String>,
    /// Units processed so far within `phase`; increases monotonically up to
    /// `units_total` and resets at the start of each new phase.
    pub units_done: usize,
    /// Total units `phase` will process.
    pub units_total: usize,
}

/// Observes progress across both passes of [`crate::Graph::sync_with_observer`] and
/// can request cancellation of pass 1.
///
/// The cancel check runs only between files during pass 1 (extraction). A
/// file becomes current only when pass 2 commits its references, so a
/// cancelled sync, which skips pass 2, leaves every file it touched looking
/// unsynced: the next sync, incremental or full, extracts those files again,
/// resolves their references, and re-resolves the references elsewhere that
/// the interrupted sync may have affected.
///
/// Pass 2 (reference resolution) is not cancellable: once pass 1 completes
/// without cancellation, pass 2 resolves every collected ref to completion
/// inside one SQLite transaction before this call returns. A cancellation
/// request that arrives while pass 2 is running has no effect on that sync;
/// it takes effect at the next sync's pass 1.
pub trait SyncObserver: Send + Sync {
    /// Called once before the first file of pass 1 (with the total already
    /// known), again after each file pass 1 touches, and — once pass 1
    /// completes without cancellation — once before pass 2 starts resolving
    /// refs, at a bounded cadence while it resolves them, and once after the
    /// last one.
    fn on_progress(&self, progress: &SyncProgress);
    /// Checked between files during pass 1; once this returns `true`, no
    /// further file in pass 1 starts. Not polled during pass 2.
    fn is_cancelled(&self) -> bool;
}

/// Outcome of a sync run through [`crate::Graph::sync_with_observer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncOutcome {
    /// The sync processed every file it found.
    Completed(SyncReport),
    /// The observer requested cancellation; the report covers exactly the
    /// files pass 1 processed before that point. None of them is current
    /// until a later sync completes (see [`SyncObserver`]).
    Cancelled(SyncReport),
}
