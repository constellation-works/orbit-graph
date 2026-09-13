//! Live per-side build status shared between the background indexing thread
//! and every request handler in [`crate::service`].
//!
//! [`crate::snapshot::Comparison::open_with_progress`] reports what it knows
//! as it happens: phase transitions and per-file counts. It does not know
//! wall-clock timing or how a caller wants a caught failure to look, so this
//! module adds both on top: [`StatusBoard`] tracks `started_at` itself and
//! folds a caught `Err` into [`crate::snapshot::BuildState::Failed`] with the
//! reason attached.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::snapshot::{BuildState, BuildStatus, ComparisonProgress, SnapshotSide};

/// One side's status, as last reported, plus timing the build side itself
/// does not track.
#[derive(Debug, Clone)]
struct SideReport {
    status: BuildStatus,
    started_at: Option<Instant>,
    started_at_unix_ms: Option<u64>,
    /// Elapsed time as of the side's last terminal state (`ready`, `failed`,
    /// or `cancelled`). Frozen there rather than kept live, so `elapsed_ms`
    /// reports how long the build took, not how long ago it finished.
    finished_ms: Option<u64>,
    error: Option<String>,
}

impl SideReport {
    fn pending() -> Self {
        Self {
            status: pending_status(),
            started_at: None,
            started_at_unix_ms: None,
            finished_ms: None,
            error: None,
        }
    }

    fn elapsed_ms(&self) -> u64 {
        self.started_at
            .map(|started_at| u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0)
    }

    fn to_json(&self) -> Value {
        let elapsed_ms = self.finished_ms.unwrap_or_else(|| self.elapsed_ms());
        json!({
            "state": self.status.state.label(),
            "files_seen": self.status.files_seen,
            "files_indexed": self.status.files_indexed,
            "files_ignored": self.status.files_ignored,
            "unsupported_constructs": self.status.unsupported_constructs,
            "languages": self.status.languages,
            "started_at": self.started_at_unix_ms,
            "elapsed_ms": elapsed_ms,
            "error": self.error,
        })
    }
}

fn pending_status() -> BuildStatus {
    BuildStatus {
        state: BuildState::Pending,
        files_seen: 0,
        files_indexed: 0,
        files_ignored: 0,
        unsupported_constructs: 0,
        languages: Vec::new(),
    }
}

/// Board shared by the indexer thread and every request handler.
///
/// Every method takes `&self` and locks internally: the board is read from
/// request-handling threads while the indexer thread writes to it
/// concurrently.
pub(crate) struct StatusBoard {
    base: Mutex<SideReport>,
    head: Mutex<SideReport>,
}

impl StatusBoard {
    pub(crate) fn new() -> Self {
        Self {
            base: Mutex::new(SideReport::pending()),
            head: Mutex::new(SideReport::pending()),
        }
    }

    /// Reset both sides to pending with a fresh start time, for a new build
    /// attempt: the first one, or a restart after cancellation.
    pub(crate) fn reset(&self) {
        let started_at = Instant::now();
        let started_at_unix_ms = Some(unix_millis_now());
        for slot in [&self.base, &self.head] {
            let mut report = slot.lock().unwrap_or_else(PoisonError::into_inner);
            *report = SideReport {
                status: pending_status(),
                started_at: Some(started_at),
                started_at_unix_ms,
                finished_ms: None,
                error: None,
            };
        }
    }

    fn slot(&self, side: SnapshotSide) -> &Mutex<SideReport> {
        match side {
            SnapshotSide::Base => &self.base,
            SnapshotSide::Head => &self.head,
        }
    }

    pub(crate) fn set_status(&self, side: SnapshotSide, status: &BuildStatus) {
        let mut report = self
            .slot(side)
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        report.status = status.clone();
        if matches!(status.state, BuildState::Ready | BuildState::Cancelled) {
            report.finished_ms = Some(report.elapsed_ms());
        }
    }

    /// Record a build failure. `side`, when known, narrows which side failed;
    /// `None` marks both, since some failures — the repository itself
    /// becoming unreadable, for example — are not attributable to one side.
    pub(crate) fn mark_failed(&self, side: Option<SnapshotSide>, reason: &str) {
        match side {
            Some(side) => self.mark_side_failed(side, reason),
            None => {
                self.mark_side_failed(SnapshotSide::Base, reason);
                self.mark_side_failed(SnapshotSide::Head, reason);
            }
        }
    }

    fn mark_side_failed(&self, side: SnapshotSide, reason: &str) {
        let mut report = self
            .slot(side)
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        report.status.state = BuildState::Failed;
        report.error = Some(reason.to_string());
        report.finished_ms = Some(report.elapsed_ms());
    }

    /// Record that cancellation stopped the build, for every side that had
    /// not already reached `ready`.
    ///
    /// `Comparison::open_with_progress` reports `Cancelled` only through its
    /// `Result`, not through a per-side status: a side cancelled before its
    /// first status report — cancellation requested before materialization
    /// even started, for example — would otherwise stay `pending` forever.
    pub(crate) fn mark_cancelled(&self) {
        for slot in [&self.base, &self.head] {
            let mut report = slot.lock().unwrap_or_else(PoisonError::into_inner);
            if !matches!(report.status.state, BuildState::Ready) {
                report.status.state = BuildState::Cancelled;
                report.finished_ms = Some(report.elapsed_ms());
            }
        }
    }

    pub(crate) fn to_json(&self) -> Value {
        json!({
            "base": self.base.lock().unwrap_or_else(PoisonError::into_inner).to_json(),
            "head": self.head.lock().unwrap_or_else(PoisonError::into_inner).to_json(),
        })
    }
}

fn unix_millis_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Bridges [`ComparisonProgress`] to a [`StatusBoard`] and an [`AtomicBool`]
/// cancellation flag shared with request handlers.
pub(crate) struct ServiceProgress {
    board: Arc<StatusBoard>,
    cancel: Arc<AtomicBool>,
}

impl ServiceProgress {
    pub(crate) fn new(board: Arc<StatusBoard>, cancel: Arc<AtomicBool>) -> Self {
        Self { board, cancel }
    }
}

impl ComparisonProgress for ServiceProgress {
    fn on_status(&self, side: SnapshotSide, status: &BuildStatus) {
        self.board.set_status(side, status);
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}
