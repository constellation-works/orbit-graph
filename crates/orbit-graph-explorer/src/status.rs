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

    /// Reset both sides to pending, for a new build attempt: the first one,
    /// or a restart after cancellation.
    ///
    /// Neither side gets a start time here: the two sides build sequentially,
    /// so stamping both now would make the second side's `elapsed_ms` count
    /// from the first side's start rather than its own. `set_status` stamps
    /// each side's own `started_at` the first time it actually leaves
    /// `pending`.
    pub(crate) fn reset(&self) {
        for slot in [&self.base, &self.head] {
            let mut report = slot.lock().unwrap_or_else(PoisonError::into_inner);
            *report = SideReport::pending();
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
        // The very first status a side ever receives is the synchronizing
        // `pending()` push both sides get when a comparison starts (see
        // `Comparison::open_with_progress`); that must not stamp a start
        // time, or a side still waiting its turn would start ticking before
        // its own build begins. The side's own start is whenever it first
        // reports something other than `pending`.
        if report.started_at.is_none() && !matches!(status.state, BuildState::Pending) {
            report.started_at = Some(Instant::now());
            report.started_at_unix_ms = Some(unix_millis_now());
        }
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

#[cfg(test)]
mod tests {
    use std::thread::sleep;
    use std::time::Duration;

    use super::*;

    fn materializing() -> BuildStatus {
        BuildStatus {
            state: BuildState::Materializing,
            ..pending_status()
        }
    }

    /// `Comparison::open_with_progress` pushes a synchronizing `pending()`
    /// report for both sides before either side's build begins (see
    /// `open_with_progress` in `snapshot.rs`); that must not be mistaken for
    /// a side starting its build.
    #[test]
    fn a_pending_report_never_starts_the_clock() {
        let board = StatusBoard::new();
        board.reset();
        board.set_status(SnapshotSide::Base, &pending_status());
        sleep(Duration::from_millis(20));

        let json = board.to_json();
        assert!(json["base"]["started_at"].is_null(), "{json}");
        assert_eq!(json["base"]["elapsed_ms"], 0, "{json}");
    }

    /// The two sides build sequentially. Before this fix, `reset` stamped one
    /// `started_at` for both, so the side waiting its turn ticked while
    /// `pending` and then reported the sum of both builds' durations once it
    /// actually started. Each side must instead get its own start time, set
    /// only when that side itself leaves `pending`.
    #[test]
    fn each_side_gets_its_own_start_time_instead_of_sharing_one() {
        let board = StatusBoard::new();
        board.reset();

        board.set_status(SnapshotSide::Base, &materializing());
        sleep(Duration::from_millis(30));
        let mid_build = board.to_json();
        assert_eq!(
            mid_build["head"]["elapsed_ms"], 0,
            "a side still pending must not tick just because the other side started: {mid_build}"
        );
        assert!(mid_build["head"]["started_at"].is_null(), "{mid_build}");
        let base_elapsed_mid_build = mid_build["base"]["elapsed_ms"]
            .as_u64()
            .expect("base elapsed_ms");
        assert!(base_elapsed_mid_build >= 25, "{mid_build}");

        board.set_status(SnapshotSide::Head, &materializing());
        let both_building = board.to_json();
        let base_started = both_building["base"]["started_at"]
            .as_u64()
            .expect("base started_at");
        let head_started = both_building["head"]["started_at"]
            .as_u64()
            .expect("head started_at");
        assert_ne!(
            base_started, head_started,
            "the two sides must not share one started_at: {both_building}"
        );
        let head_elapsed_at_start = both_building["head"]["elapsed_ms"]
            .as_u64()
            .expect("head elapsed_ms");
        assert!(
            head_elapsed_at_start < base_elapsed_mid_build,
            "head's own elapsed time must not include base's build time: {both_building}"
        );
    }

    #[test]
    fn languages_reported_at_ready_are_not_overwritten() {
        let board = StatusBoard::new();
        board.reset();
        board.set_status(
            SnapshotSide::Base,
            &BuildStatus {
                state: BuildState::Ready,
                languages: vec!["rust".to_string(), "markdown".to_string()],
                ..pending_status()
            },
        );

        let json = board.to_json();
        assert_eq!(json["base"]["languages"], json!(["rust", "markdown"]));
    }
}
