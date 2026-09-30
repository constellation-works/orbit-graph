//! One bounded answer to "what does this change affect, and which tests
//! should run?", shaped for agents.
//!
//! [`analyse`] opens a comparison (two revisions, or a base and the working
//! tree), builds the [`report`](crate::report) over it under explicit bounds
//! and an optional wall-clock budget, and regroups the result per changed
//! symbol: the callers that reach it, the entry points behind those callers,
//! and the labelled candidate tests to run. Every caller, entry point, and
//! test carries its source, its evidence category, its confidence, and the
//! evidence path behind it when one exists.
//!
//! Every input is validated before anything is materialized or indexed
//! (STD-02 §R34). Every bound that cuts a list is recorded in `truncation`,
//! and a budget that runs out yields a document marked `complete: false`,
//! never a hang and never a silently shorter answer.
//!
//! The payload contract is recorded in `docs/design/changes-command/`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use orbit_graph::{Confidence, Selector};
use serde::Serialize;
use thiserror::Error;

use crate::changes::{
    ChangeStatus, FileChangeKind, OutOfScopeEntry, Pairing, PairingEvidence, SymbolRef,
    UncertainCandidate,
};
use crate::evidence::{
    CandidateSource, DEFAULT_TIME_BUDGET_MS, EVIDENCE_DEPTH, EndpointRef, EvidenceBounds,
    EvidenceCategory, MAX_EVIDENCE_DEPTH, confidence_label,
};
use crate::filters::{FilterSet, FilteredOut};
use crate::report::{
    ComparisonView, ExcerptMode, ReportChangedSymbol, ReportDeadline, ReportError,
    ReportEvidencePath, ReportLimits, ReportOptions, TruncationFlag, UnanalysedReason,
    UnresolvedArea, build_report_with_selection,
};
use crate::selection::ResolvedSelection;
use crate::snapshot::{
    BuildStatus, Comparison, ComparisonOptions, ComparisonOutcome, ComparisonProgress, DefaultBase,
    SnapshotError, SnapshotSide, default_base,
};

#[cfg(test)]
mod tests;

/// Schema version of [`ChangesDocument`]. Independent of the report's
/// `schema_version`, which this document does not embed.
pub const CHANGES_SCHEMA_VERSION: u32 = 1;

/// Default cap on changed symbols analysed per call.
pub const DEFAULT_MAX_SYMBOLS: usize = 50;
/// Default cap on callers kept per changed symbol.
pub const DEFAULT_MAX_CALLERS: usize = 10;
/// Default cap on entry points kept per changed symbol.
pub const DEFAULT_MAX_ENTRY_POINTS: usize = 5;
/// Default cap on candidate tests kept per changed symbol.
pub const DEFAULT_MAX_TESTS: usize = 10;
/// Default cap on nodes, and on paths, one traversal may return.
pub const DEFAULT_NODE_CAP: usize = 200;

/// Confidence of a candidate test backed by a file-level import edge rather
/// than an evidence path.
pub const CONFIDENCE_FILE_IMPORT: &str = "file_import";
/// Confidence of a candidate test associated by name only (a naming
/// heuristic, or a program name a test starts).
pub const CONFIDENCE_NAME_ONLY: &str = "name_only";

/// The inclusive range one numeric bound accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundRange {
    /// Stable bound name, as spelled in the request and in errors.
    pub name: &'static str,
    /// Smallest accepted value.
    pub min: u64,
    /// Largest accepted value.
    pub max: u64,
}

/// Accepted range of [`ChangesBounds::depth`].
pub const DEPTH_RANGE: BoundRange = BoundRange {
    name: "depth",
    min: 1,
    max: MAX_EVIDENCE_DEPTH as u64,
};
/// Accepted range of [`ChangesBounds::node_cap`].
pub const NODE_CAP_RANGE: BoundRange = BoundRange {
    name: "node_cap",
    min: 1,
    max: 2_000,
};
/// Accepted range of [`ChangesBounds::query_budget_ms`].
pub const QUERY_BUDGET_RANGE: BoundRange = BoundRange {
    name: "query_budget_ms",
    min: 100,
    max: 60_000,
};
/// Accepted range of [`ChangesBounds::max_symbols`].
pub const MAX_SYMBOLS_RANGE: BoundRange = BoundRange {
    name: "max_symbols",
    min: 1,
    max: 1_000,
};
/// Accepted range of [`ChangesBounds::max_callers`].
pub const MAX_CALLERS_RANGE: BoundRange = BoundRange {
    name: "max_callers",
    min: 1,
    max: 500,
};
/// Accepted range of [`ChangesBounds::max_entry_points`].
pub const MAX_ENTRY_POINTS_RANGE: BoundRange = BoundRange {
    name: "max_entry_points",
    min: 1,
    max: 100,
};
/// Accepted range of [`ChangesBounds::max_tests`].
pub const MAX_TESTS_RANGE: BoundRange = BoundRange {
    name: "max_tests",
    min: 1,
    max: 500,
};
/// Accepted range of [`ChangesBounds::budget_ms`], when set.
pub const BUDGET_RANGE: BoundRange = BoundRange {
    name: "budget_ms",
    min: 1_000,
    max: 3_600_000,
};

/// Every bound on one analysis. Each one that cuts a list is recorded in
/// [`ChangesDocument::truncation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ChangesBounds {
    /// Maximum hops from a changed symbol to a caller or entry point.
    pub depth: u8,
    /// Maximum nodes, and paths, one traversal may return.
    pub node_cap: usize,
    /// Wall-clock budget of one traversal, in milliseconds.
    pub query_budget_ms: u64,
    /// Maximum changed symbols analysed, in changed-symbol order.
    pub max_symbols: usize,
    /// Maximum callers kept per changed symbol, strongest first.
    pub max_callers: usize,
    /// Maximum entry points kept per changed symbol, nearest first.
    pub max_entry_points: usize,
    /// Maximum candidate tests kept per changed symbol, strongest first.
    pub max_tests: usize,
    /// Wall-clock budget for the whole call, indexing included, in
    /// milliseconds. `None` sets no deadline.
    pub budget_ms: Option<u64>,
}

impl Default for ChangesBounds {
    fn default() -> Self {
        Self {
            depth: EVIDENCE_DEPTH,
            node_cap: DEFAULT_NODE_CAP,
            query_budget_ms: DEFAULT_TIME_BUDGET_MS,
            max_symbols: DEFAULT_MAX_SYMBOLS,
            max_callers: DEFAULT_MAX_CALLERS,
            max_entry_points: DEFAULT_MAX_ENTRY_POINTS,
            max_tests: DEFAULT_MAX_TESTS,
            budget_ms: None,
        }
    }
}

impl ChangesBounds {
    /// Reject any bound outside its accepted range. Nothing is clamped
    /// (STD-01 §R29).
    pub fn validate(&self) -> Result<(), AnalysisError> {
        let checks = [
            (DEPTH_RANGE, Some(u64::from(self.depth))),
            (NODE_CAP_RANGE, Some(self.node_cap as u64)),
            (QUERY_BUDGET_RANGE, Some(self.query_budget_ms)),
            (MAX_SYMBOLS_RANGE, Some(self.max_symbols as u64)),
            (MAX_CALLERS_RANGE, Some(self.max_callers as u64)),
            (MAX_ENTRY_POINTS_RANGE, Some(self.max_entry_points as u64)),
            (MAX_TESTS_RANGE, Some(self.max_tests as u64)),
            (BUDGET_RANGE, self.budget_ms),
        ];
        for (range, value) in checks {
            let Some(value) = value else { continue };
            if value < range.min || value > range.max {
                return Err(AnalysisError::InvalidBound {
                    name: range.name,
                    value,
                    min: range.min,
                    max: range.max,
                });
            }
        }
        Ok(())
    }
}

/// Which two trees to compare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangesRange {
    /// Two committed revisions, `<base>..<head>`.
    Revisions {
        /// Base ref as supplied.
        base: String,
        /// Head ref as supplied.
        head: String,
    },
    /// A base revision and the uncommitted working tree. `None` selects the
    /// [`default_base`].
    WorkingTree {
        /// Base ref as supplied, when one was.
        base: Option<String>,
    },
}

impl ChangesRange {
    /// Parse `<base>..<head>` (two revisions) or `<base>` (the working tree
    /// against `base`). The three-dot form is rejected rather than read as
    /// something it does not mean here.
    pub fn parse(text: &str) -> Result<Self, AnalysisError> {
        let invalid = |reason: &str| AnalysisError::InvalidRange {
            range: text.to_string(),
            reason: reason.to_string(),
        };
        if text.trim().is_empty() {
            return Err(invalid(
                "the range is empty; pass <base>..<head>, or <base> to compare the working tree",
            ));
        }
        if text.contains("...") {
            return Err(invalid(
                "the three-dot form is not supported; pass <base>..<head> with an explicit \
                 base, such as the merge base",
            ));
        }
        match text.split_once("..") {
            None => Ok(Self::WorkingTree {
                base: Some(text.to_string()),
            }),
            Some((base, head)) => {
                if base.is_empty() || head.is_empty() {
                    return Err(invalid("both sides of `..` must name a revision"));
                }
                if head.contains("..") {
                    return Err(invalid("a range has exactly one `..`"));
                }
                Ok(Self::Revisions {
                    base: base.to_string(),
                    head: head.to_string(),
                })
            }
        }
    }
}

/// One change-analysis request.
#[derive(Debug, Clone)]
pub struct ChangesRequest {
    /// Any path inside the repository's working tree.
    pub repository: PathBuf,
    /// The trees to compare.
    pub range: ChangesRange,
    /// Changed-symbol selectors to analyse. Empty analyses every changed
    /// symbol, up to [`ChangesBounds::max_symbols`].
    pub selection: Vec<String>,
    /// Presentation filters.
    pub filters: FilterSet,
    /// Confidence floor applied to every hop.
    pub min_confidence: Confidence,
    /// Bounds in force.
    pub bounds: ChangesBounds,
    /// Where snapshot trees and indexes are cached.
    pub cache: ComparisonOptions,
}

/// Failure surface of [`analyse`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AnalysisError {
    /// The range argument is malformed.
    #[error("invalid range `{range}`: {reason}")]
    InvalidRange {
        /// The range as supplied.
        range: String,
        /// What is wrong with it.
        reason: String,
    },
    /// A bound is outside its accepted range.
    #[error("{name} must be between {min} and {max}, got {value}")]
    InvalidBound {
        /// Bound name.
        name: &'static str,
        /// Value supplied.
        value: u64,
        /// Smallest accepted value.
        min: u64,
        /// Largest accepted value.
        max: u64,
    },
    /// A selection entry is not a symbol selector.
    #[error("invalid selection `{selector}`: {reason}")]
    InvalidSelection {
        /// The entry as supplied.
        selector: String,
        /// What is wrong with it.
        reason: String,
    },
    /// Resolving, materializing, or indexing a snapshot failed.
    #[error(transparent)]
    Snapshot(#[from] SnapshotError),
    /// Computing changed symbols or evidence failed.
    #[error(transparent)]
    Report(#[from] ReportError),
}

/// Why a document is incomplete.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Incomplete {
    /// `indexing` when the budget ran out before both snapshots were ready,
    /// `analysis` when it ran out between changed symbols.
    pub phase: String,
    /// The bound that stopped the call.
    pub bound: String,
    /// That bound's value.
    pub value: u64,
    /// What a caller can do about it.
    pub message: String,
}

/// The query that produced a document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangesQuery {
    /// Changed-symbol selectors requested; empty means every changed symbol.
    pub selection: Vec<String>,
    /// Confidence floor label.
    pub min_confidence: String,
    /// Bounds in force.
    pub bounds: ChangesBounds,
}

/// Counts over the document.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ChangesSummary {
    /// Changed symbols after filters, whether analysed or not.
    pub changed_symbols: usize,
    /// Changed symbols analysed (listed in `symbols`).
    pub analysed_symbols: usize,
    /// Changed symbols a bound left unanalysed (listed in `not_analysed`).
    pub not_analysed_symbols: usize,
    /// Changed symbols the selection excluded.
    pub not_selected_symbols: usize,
    /// Callers kept, over every analysed symbol.
    pub callers: usize,
    /// Entry points kept, over every analysed symbol.
    pub entry_points: usize,
    /// Distinct candidate tests in `tests`.
    pub candidate_tests: usize,
    /// Changed paths the extractor did not index.
    pub out_of_scope_paths: usize,
}

/// Wall-clock cost of the call.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ChangesTimings {
    /// Resolving, materializing, and indexing both snapshots.
    pub prepare_ms: u64,
    /// Building evidence over the prepared snapshots.
    pub analysis_ms: u64,
    /// The whole call.
    pub total_ms: u64,
    /// Base snapshot cache outcome: `hit`, `miss`, or `disabled`.
    pub base_cache: Option<String>,
    /// Head snapshot cache outcome. Always `disabled` for the working tree.
    pub head_cache: Option<String>,
}

/// A caller that reaches a changed symbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AffectedCaller {
    /// The calling symbol, or file for a file-attributed reference.
    pub caller: EndpointRef,
    /// Snapshots the caller was found in.
    pub snapshots: Vec<String>,
    /// Hops from the caller to the changed symbol.
    pub distance: usize,
    /// `call_path` when every hop is a call, `import_relationship` when a
    /// hop is an import, `reference_path` otherwise, and `changed_symbol` at
    /// distance 0, where the changed symbol is itself the endpoint.
    pub source: String,
    /// Weakest evidence category on the path.
    pub category: EvidenceCategory,
    /// Weakest `orbit_graph` confidence on the path.
    pub confidence: String,
    /// The evidence path, caller first.
    pub evidence: ReportEvidencePath,
}

/// An entry point that reaches a changed symbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AffectedEntryPoint {
    /// The entry point.
    pub entry_point: EndpointRef,
    /// Rule that classified it as an entry point.
    pub rule: String,
    /// Every rule that fired, in priority order.
    pub rules: Vec<String>,
    /// Snapshots the entry point was found in.
    pub snapshots: Vec<String>,
    /// Hops from the entry point to the changed symbol.
    pub distance: usize,
    /// Same vocabulary as [`AffectedCaller::source`].
    pub source: String,
    /// Weakest evidence category on the path.
    pub category: EvidenceCategory,
    /// Weakest `orbit_graph` confidence on the path.
    pub confidence: String,
    /// The evidence path, entry point first.
    pub evidence: ReportEvidencePath,
    /// What the rule observed.
    pub note: Option<String>,
}

/// A candidate test for one changed symbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LabelledTest {
    /// The test symbol, or test file.
    pub test: EndpointRef,
    /// Snapshots the candidate was found in.
    pub snapshots: Vec<String>,
    /// Which disclosed source produced it.
    pub source: CandidateSource,
    /// Evidence category backing it.
    pub category: EvidenceCategory,
    /// Weakest `orbit_graph` confidence on its evidence path;
    /// [`CONFIDENCE_FILE_IMPORT`] or [`CONFIDENCE_NAME_ONLY`] when no path
    /// backs it.
    pub confidence: String,
    /// The evidence path behind a `call_path` candidate.
    pub evidence: Option<ReportEvidencePath>,
    /// Whether a bound cut the search that produced it.
    pub truncated: bool,
    /// Why it is plausible.
    pub note: Option<String>,
}

/// One analysed changed symbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SymbolAnalysis {
    /// Primary selector: head-side when the symbol exists in head.
    pub selector: String,
    /// How the symbol changed.
    pub status: ChangeStatus,
    /// Which pairing rung established its identity.
    pub pairing: Pairing,
    /// What evidence supports that rung.
    pub pairing_evidence: PairingEvidence,
    /// Base-side occurrence.
    pub base: Option<SymbolRef>,
    /// Head-side occurrence.
    pub head: Option<SymbolRef>,
    /// How Git classified the containing file.
    pub file_change: FileChangeKind,
    /// Every candidate partner of an `uncertain` pairing.
    pub uncertain_candidates: Vec<UncertainCandidate>,
    /// Qualification of this entry.
    pub note: Option<String>,
    /// Callers, strongest and nearest first.
    pub callers: Vec<AffectedCaller>,
    /// Distinct callers found before `max_callers` applied.
    pub callers_found: usize,
    /// Entry points, nearest first.
    pub entry_points: Vec<AffectedEntryPoint>,
    /// Distinct entry points found before `max_entry_points` applied.
    pub entry_points_found: usize,
    /// Candidate tests, strongest source first.
    pub candidate_tests: Vec<LabelledTest>,
    /// Distinct candidate tests found before `max_tests` applied.
    pub candidate_tests_found: usize,
}

/// A changed symbol a bound left unanalysed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NotAnalysed {
    /// Primary selector.
    pub selector: String,
    /// How the symbol changed.
    pub status: ChangeStatus,
    /// The bound: `max_symbols` or `time_budget_ms`.
    pub reason: String,
}

/// One candidate test across every analysed symbol: the test-selection view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TestSelection {
    /// The test symbol, or test file.
    pub test: EndpointRef,
    /// Strongest source across the symbols it was found for.
    pub source: CandidateSource,
    /// Category of that strongest candidate.
    pub category: EvidenceCategory,
    /// Confidence of that strongest candidate.
    pub confidence: String,
    /// Changed symbols it is a candidate for.
    pub changed_symbols: Vec<String>,
}

/// The agent-facing change analysis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangesDocument {
    /// Payload schema version: [`CHANGES_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// When the document was generated (RFC 3339, UTC).
    pub generated_at: String,
    /// `false` when a budget stopped the call; see `incomplete`.
    pub complete: bool,
    /// Why the call stopped early, when it did.
    pub incomplete: Option<Incomplete>,
    /// The resolved comparison; `None` only when indexing did not finish.
    pub comparison: Option<ComparisonView>,
    /// How the base was chosen when the caller named none.
    pub default_base: Option<DefaultBase>,
    /// The query in force.
    pub query: ChangesQuery,
    /// Counts over this document.
    pub summary: ChangesSummary,
    /// Analysed changed symbols, in changed-symbol order.
    pub symbols: Vec<SymbolAnalysis>,
    /// Changed symbols a bound left unanalysed.
    pub not_analysed: Vec<NotAnalysed>,
    /// Distinct candidate tests across every analysed symbol, strongest
    /// source first.
    pub tests: Vec<TestSelection>,
    /// Selection entries that matched no changed symbol.
    pub unmatched_selection: Vec<String>,
    /// Queries that produced no result, and why.
    pub unresolved: Vec<UnresolvedArea>,
    /// Changed paths the extractor did not index.
    pub out_of_scope: Vec<OutOfScopeEntry>,
    /// What presentation filters removed.
    pub filtered_out: Vec<FilteredOut>,
    /// Whether any bound cut anything in this document.
    pub truncated: bool,
    /// Every bound reached, deduplicated.
    pub truncation: Vec<TruncationFlag>,
    /// Wall-clock cost.
    pub timings: ChangesTimings,
    /// Human-readable qualifications: uncommitted state, cache notes.
    pub notices: Vec<String>,
}

impl ChangesDocument {
    /// Shrink this document until it serializes to at most `max_bytes`,
    /// recording every cut; returns whether it fits.
    ///
    /// Per-symbol lists are halved first, weakest entries dropped, down to one
    /// entry each; then analysed symbols move to `not_analysed`, last first;
    /// then the long standing lists (`not_analysed`, `unresolved`,
    /// `out_of_scope`) are halved. Every step adds a `max_response_bytes`
    /// flag naming what it cut, so a caller never mistakes a fitted document
    /// for the whole answer.
    pub fn fit_to_bytes(&mut self, max_bytes: usize) -> bool {
        let bound = |what: &str| TruncationFlag {
            what: what.to_string(),
            bound: "max_response_bytes".to_string(),
            value: max_bytes as u64,
        };
        let mut flags = Vec::new();
        loop {
            let size = serde_json::to_vec(&*self).map_or(usize::MAX, |bytes| bytes.len());
            if size <= max_bytes {
                break;
            }
            let longest = self
                .symbols
                .iter()
                .flat_map(|symbol| {
                    [
                        symbol.callers.len(),
                        symbol.entry_points.len(),
                        symbol.candidate_tests.len(),
                    ]
                })
                .max()
                .unwrap_or(0);
            if longest > 1 {
                let cap = longest / 2;
                for symbol in &mut self.symbols {
                    symbol.callers.truncate(cap);
                    symbol.entry_points.truncate(cap);
                    symbol.candidate_tests.truncate(cap);
                }
                flags.push(bound("symbols[].lists"));
            } else if !self.symbols.is_empty() {
                let keep = self.symbols.len() / 2;
                for symbol in self.symbols.drain(keep..) {
                    self.not_analysed.push(NotAnalysed {
                        selector: symbol.selector,
                        status: symbol.status,
                        reason: "max_response_bytes".to_string(),
                    });
                }
                flags.push(bound("symbols"));
            } else {
                let longest = [
                    self.not_analysed.len(),
                    self.unresolved.len(),
                    self.out_of_scope.len(),
                ]
                .into_iter()
                .max()
                .unwrap_or(0);
                if longest == 0 {
                    break;
                }
                let cap = longest / 2;
                self.not_analysed.truncate(cap);
                self.unresolved.truncate(cap);
                self.out_of_scope.truncate(cap);
                flags.push(bound("not_analysed,unresolved,out_of_scope"));
            }
            self.tests = test_selection(&self.symbols);
            self.summary.analysed_symbols = self.symbols.len();
            self.summary.not_analysed_symbols = self.not_analysed.len();
            self.summary.callers = self.symbols.iter().map(|symbol| symbol.callers.len()).sum();
            self.summary.entry_points = self
                .symbols
                .iter()
                .map(|symbol| symbol.entry_points.len())
                .sum();
            self.summary.candidate_tests = self.tests.len();
        }
        if !flags.is_empty() {
            self.truncation.extend(flags);
            self.truncation.sort();
            self.truncation.dedup();
            self.truncated = true;
        }
        serde_json::to_vec(&*self).is_ok_and(|bytes| bytes.len() <= max_bytes)
    }
}

/// Validate `request` without touching the repository beyond reading it.
pub fn validate(request: &ChangesRequest) -> Result<(), AnalysisError> {
    request.bounds.validate()?;
    for selector in &request.selection {
        match selector.parse::<Selector>() {
            Ok(Selector::Symbol { .. }) => {}
            Ok(_) => {
                return Err(AnalysisError::InvalidSelection {
                    selector: selector.clone(),
                    reason: "expected a symbol selector, symbol:<path>#<name>:<kind>".to_string(),
                });
            }
            Err(error) => {
                return Err(AnalysisError::InvalidSelection {
                    selector: selector.clone(),
                    reason: error.to_string(),
                });
            }
        }
    }
    Ok(())
}

/// Cancels a comparison build once the call's deadline passes.
struct DeadlineProgress {
    deadline: Option<Instant>,
}

impl ComparisonProgress for DeadlineProgress {
    fn on_status(&self, _side: SnapshotSide, _status: &BuildStatus) {}

    fn is_cancelled(&self) -> bool {
        self.deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
    }
}

/// Run one change analysis.
///
/// Inputs are validated, and both refs resolved, before anything is
/// materialized or indexed. With [`ChangesBounds::budget_ms`] set, the call
/// returns by that deadline (plus at most one in-flight file or query) with
/// whatever was complete, marked `complete: false`.
pub fn analyse(request: &ChangesRequest) -> Result<ChangesDocument, AnalysisError> {
    let started = Instant::now();
    validate(request)?;
    let deadline = request
        .bounds
        .budget_ms
        .map(|budget| started + Duration::from_millis(budget));
    let progress = DeadlineProgress { deadline };

    let (outcome, default_base) = match &request.range {
        ChangesRange::Revisions { base, head } => (
            Comparison::open_with_progress(
                request.repository.as_path(),
                base,
                head,
                &request.cache,
                &progress,
            )?,
            None,
        ),
        ChangesRange::WorkingTree { base } => {
            let (base_ref, chosen) = match base {
                Some(base) => (base.clone(), None),
                None => {
                    let chosen = default_base(request.repository.as_path())?;
                    (chosen.merge_base.clone(), Some(chosen))
                }
            };
            (
                Comparison::open_working_tree(
                    request.repository.as_path(),
                    base_ref.as_str(),
                    &request.cache,
                    &progress,
                )?,
                chosen,
            )
        }
    };

    let query = ChangesQuery {
        selection: request.selection.clone(),
        min_confidence: confidence_label(request.min_confidence).to_string(),
        bounds: request.bounds,
    };
    let comparison = match outcome {
        ComparisonOutcome::Ready(comparison) => comparison,
        ComparisonOutcome::Cancelled => {
            let budget_ms = request.bounds.budget_ms.unwrap_or_default();
            return Ok(ChangesDocument {
                schema_version: CHANGES_SCHEMA_VERSION,
                generated_at: crate::report::now_rfc3339(),
                complete: false,
                incomplete: Some(Incomplete {
                    phase: "indexing".to_string(),
                    bound: "budget_ms".to_string(),
                    value: budget_ms,
                    message: "the budget ran out before both snapshots were indexed; no \
                              changed symbols were computed. Retry with a larger budget_ms: \
                              committed snapshots already cached are reused"
                        .to_string(),
                }),
                comparison: None,
                default_base,
                query,
                summary: ChangesSummary::default(),
                symbols: Vec::new(),
                not_analysed: Vec::new(),
                tests: Vec::new(),
                unmatched_selection: Vec::new(),
                unresolved: Vec::new(),
                out_of_scope: Vec::new(),
                filtered_out: Vec::new(),
                truncated: true,
                truncation: vec![TruncationFlag {
                    what: "comparison".to_string(),
                    bound: "budget_ms".to_string(),
                    value: budget_ms,
                }],
                timings: ChangesTimings {
                    total_ms: elapsed_ms(started),
                    prepare_ms: elapsed_ms(started),
                    ..ChangesTimings::default()
                },
                notices: Vec::new(),
            });
        }
    };
    let prepared = Instant::now();

    let options = ReportOptions {
        selection: request.selection.clone(),
        filters: request.filters.clone(),
        min_confidence: request.min_confidence,
        bounds: EvidenceBounds {
            depth: request.bounds.depth,
            node_cap: request.bounds.node_cap,
            time_budget_ms: request.bounds.query_budget_ms,
        },
        excerpts: ExcerptMode::None,
        generated_at: None,
        include_absolute_paths: false,
    };
    let limits = ReportLimits {
        skip_outbound: true,
        max_symbols: Some(request.bounds.max_symbols),
        deadline: deadline.map(|at| ReportDeadline {
            at,
            budget_ms: request.bounds.budget_ms.unwrap_or_default(),
        }),
    };
    let selection =
        ResolvedSelection::resolve(&comparison, &request.selection).map_err(ReportError::from)?;
    let built = build_report_with_selection(&comparison, &options, &limits, &selection)?;
    let finished = Instant::now();

    let mut document = regroup(built.report, built.unanalysed, &selection, query);
    document.default_base = default_base;
    document.timings = ChangesTimings {
        prepare_ms: duration_ms(prepared.duration_since(started)),
        analysis_ms: duration_ms(finished.duration_since(prepared)),
        total_ms: elapsed_ms(started),
        base_cache: Some(comparison.base().cache_outcome().label().to_string()),
        head_cache: Some(comparison.head().cache_outcome().label().to_string()),
    };
    if let Some(note) = comparison.cache_note() {
        document.notices.push(note.to_string());
    }
    if let Some(note) = comparison.head().cache_note() {
        document.notices.push(note.to_string());
    }
    Ok(document)
}

fn elapsed_ms(started: Instant) -> u64 {
    duration_ms(started.elapsed())
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Rank of an `orbit_graph` confidence label, strongest first. An unknown
/// label ranks weakest, so it is never presented as stronger than it is.
fn confidence_rank(label: &str) -> u8 {
    match label {
        "exact" => 0,
        "import_resolved" => 1,
        "same_module" => 2,
        "fuzzy_name" => 3,
        _ => 4,
    }
}

/// Weakest confidence on a path's edges. A path with no edge is the changed
/// symbol itself, which nothing had to resolve: `exact`.
fn weakest_confidence(path: &ReportEvidencePath) -> String {
    path.edges
        .iter()
        .map(|edge| edge.edge.confidence.as_str())
        .max_by_key(|label| confidence_rank(label))
        .unwrap_or("exact")
        .to_string()
}

/// Source label of a path: how the caller reaches the changed symbol.
fn path_source(path: &ReportEvidencePath) -> &'static str {
    if path.edges.is_empty() {
        // Distance 0: the changed symbol is itself the entry point.
        "changed_symbol"
    } else if path
        .edges
        .iter()
        .all(|edge| edge.edge.relationship == "call")
    {
        "call_path"
    } else if path
        .edges
        .iter()
        .any(|edge| edge.edge.relationship == "use")
    {
        "import_relationship"
    } else {
        "reference_path"
    }
}

/// Keep the stronger of two entries for the same key, merging snapshots.
fn merge_snapshot(snapshots: &mut Vec<String>, snapshot: &str) {
    if !snapshots.iter().any(|seen| seen == snapshot) {
        snapshots.push(snapshot.to_string());
        snapshots.sort();
    }
}

/// Regroup a flattened report per changed symbol, applying the per-symbol
/// caps and recording each one that cuts a list.
pub(crate) fn regroup(
    report: crate::report::ExportedReport,
    unanalysed: Vec<(String, UnanalysedReason)>,
    selection: &ResolvedSelection,
    query: ChangesQuery,
) -> ChangesDocument {
    let bounds = query.bounds;
    let mut truncation: Vec<TruncationFlag> = report.scope.truncated.clone();
    let unanalysed_reasons: BTreeMap<&str, UnanalysedReason> = unanalysed
        .iter()
        .map(|(selector, reason)| (selector.as_str(), *reason))
        .collect();

    let all_selectors = report.changed_symbols.symbols.iter().flat_map(|symbol| {
        [
            (SnapshotSide::Base, symbol.base.as_ref()),
            (SnapshotSide::Head, symbol.head.as_ref()),
        ]
        .into_iter()
        .filter_map(|(side, occurrence)| {
            occurrence.map(|occurrence| (side, occurrence.symbol.selector.as_str()))
        })
    });
    let unmatched_selection = selection.unmatched(all_selectors);
    let selected = |symbol: &ReportChangedSymbol| {
        selection.selects(
            symbol
                .base
                .as_ref()
                .map(|occurrence| occurrence.symbol.selector.as_str()),
            symbol
                .head
                .as_ref()
                .map(|occurrence| occurrence.symbol.selector.as_str()),
        )
    };

    let mut symbols = Vec::new();
    let mut not_analysed = Vec::new();
    let mut not_selected = 0;
    for symbol in &report.changed_symbols.symbols {
        let primary = primary_selector(symbol);
        if !selected(symbol) {
            not_selected += 1;
            continue;
        }
        if let Some(reason) = unanalysed_reasons.get(primary.as_str()) {
            not_analysed.push(NotAnalysed {
                selector: primary,
                status: symbol.status,
                reason: reason.bound().to_string(),
            });
            continue;
        }
        let occurrences: Vec<(&str, &str)> = [symbol.base.as_ref(), symbol.head.as_ref()]
            .into_iter()
            .flatten()
            .map(|occurrence| {
                (
                    occurrence.symbol.selector.as_str(),
                    occurrence.symbol.snapshot.as_str(),
                )
            })
            .collect();
        let queried = |endpoint: &EndpointRef| {
            occurrences.contains(&(endpoint.selector.as_str(), endpoint.snapshot.as_str()))
        };

        let (callers, callers_found) = collect_callers(&report, &queried, bounds.max_callers);
        if callers_found > callers.len() {
            truncation.push(TruncationFlag {
                what: format!("callers:{primary}"),
                bound: "max_callers".to_string(),
                value: bounds.max_callers as u64,
            });
        }
        let (entry_points, entry_points_found) =
            collect_entry_points(&report, &queried, bounds.max_entry_points);
        if entry_points_found > entry_points.len() {
            truncation.push(TruncationFlag {
                what: format!("entry_points:{primary}"),
                bound: "max_entry_points".to_string(),
                value: bounds.max_entry_points as u64,
            });
        }
        let (candidate_tests, candidate_tests_found) =
            collect_tests(&report, &queried, bounds.max_tests);
        if candidate_tests_found > candidate_tests.len() {
            truncation.push(TruncationFlag {
                what: format!("candidate_tests:{primary}"),
                bound: "max_tests".to_string(),
                value: bounds.max_tests as u64,
            });
        }

        symbols.push(SymbolAnalysis {
            selector: primary,
            status: symbol.status,
            pairing: symbol.pairing,
            pairing_evidence: symbol.pairing_evidence,
            base: symbol
                .base
                .as_ref()
                .map(|occurrence| occurrence.symbol.clone()),
            head: symbol
                .head
                .as_ref()
                .map(|occurrence| occurrence.symbol.clone()),
            file_change: symbol.file_change,
            uncertain_candidates: symbol.uncertain_candidates.clone(),
            note: symbol.note.clone(),
            callers,
            callers_found,
            entry_points,
            entry_points_found,
            candidate_tests,
            candidate_tests_found,
        });
    }

    let tests = test_selection(&symbols);
    if report.candidate_tests.truncated {
        truncation.push(TruncationFlag {
            what: "candidate_tests".to_string(),
            bound: "traversal".to_string(),
            value: bounds.node_cap as u64,
        });
    }
    truncation.sort();
    truncation.dedup();

    let stopped_by_budget = unanalysed
        .iter()
        .any(|(_, reason)| *reason == UnanalysedReason::TimeBudget);
    let incomplete = stopped_by_budget.then(|| Incomplete {
        phase: "analysis".to_string(),
        bound: "budget_ms".to_string(),
        value: bounds.budget_ms.unwrap_or_default(),
        message: "the budget ran out before every selected changed symbol was analysed; the \
                  rest are listed in not_analysed. Retry with a larger budget_ms, or select \
                  them with symbols"
            .to_string(),
    });

    let mut notices = Vec::new();
    if let Some(notice) = report.comparison.working_tree.notice.clone() {
        notices.push(notice);
    }

    ChangesDocument {
        schema_version: CHANGES_SCHEMA_VERSION,
        generated_at: report.generated_at,
        complete: incomplete.is_none(),
        incomplete,
        default_base: None,
        summary: ChangesSummary {
            changed_symbols: report.changed_symbols.symbols.len(),
            analysed_symbols: symbols.len(),
            not_analysed_symbols: not_analysed.len(),
            not_selected_symbols: not_selected,
            callers: symbols.iter().map(|symbol| symbol.callers.len()).sum(),
            entry_points: symbols.iter().map(|symbol| symbol.entry_points.len()).sum(),
            candidate_tests: tests.len(),
            out_of_scope_paths: report.changed_symbols.out_of_scope.len(),
        },
        comparison: Some(report.comparison),
        query,
        symbols,
        not_analysed,
        tests,
        unmatched_selection,
        unresolved: report.unresolved,
        out_of_scope: report.changed_symbols.out_of_scope,
        filtered_out: report.changed_symbols.filtered_out,
        truncated: !truncation.is_empty(),
        truncation,
        timings: ChangesTimings::default(),
        notices,
    }
}

fn primary_selector(symbol: &ReportChangedSymbol) -> String {
    symbol
        .head
        .as_ref()
        .or(symbol.base.as_ref())
        .map(|occurrence| occurrence.symbol.selector.clone())
        .unwrap_or_default()
}

/// Distinct callers of one symbol, strongest and nearest first, capped.
fn collect_callers(
    report: &crate::report::ExportedReport,
    queried: &dyn Fn(&EndpointRef) -> bool,
    cap: usize,
) -> (Vec<AffectedCaller>, usize) {
    let mut by_caller: BTreeMap<String, AffectedCaller> = BTreeMap::new();
    for path in report
        .evidence_paths
        .iter()
        .filter(|path| queried(&path.to))
    {
        let candidate = AffectedCaller {
            caller: path.from.clone(),
            snapshots: vec![path.to.snapshot.clone()],
            distance: path.distance,
            source: path_source(path).to_string(),
            category: path.category,
            confidence: weakest_confidence(path),
            evidence: path.clone(),
        };
        match by_caller.get_mut(path.from.selector.as_str()) {
            Some(existing) => {
                let mut snapshots = existing.snapshots.clone();
                merge_snapshot(&mut snapshots, path.to.snapshot.as_str());
                if caller_key(&candidate) < caller_key(existing) {
                    *existing = candidate;
                }
                existing.snapshots = snapshots;
            }
            None => {
                by_caller.insert(path.from.selector.clone(), candidate);
            }
        }
    }
    let mut callers: Vec<AffectedCaller> = by_caller.into_values().collect();
    callers.sort_by_key(caller_key);
    let found = callers.len();
    callers.truncate(cap);
    (callers, found)
}

fn caller_key(caller: &AffectedCaller) -> (EvidenceCategory, usize, u8, bool, String) {
    (
        caller.category,
        caller.distance,
        confidence_rank(caller.confidence.as_str()),
        caller.evidence.to.snapshot != SnapshotSide::Head.label(),
        caller.caller.selector.clone(),
    )
}

/// Distinct entry points of one symbol, nearest and strongest first, capped.
fn collect_entry_points(
    report: &crate::report::ExportedReport,
    queried: &dyn Fn(&EndpointRef) -> bool,
    cap: usize,
) -> (Vec<AffectedEntryPoint>, usize) {
    let mut by_node: BTreeMap<String, AffectedEntryPoint> = BTreeMap::new();
    for entry in report
        .entry_points
        .iter()
        .filter(|entry| queried(&entry.queried))
    {
        let candidate = AffectedEntryPoint {
            entry_point: entry.node.clone(),
            rule: entry.rule.clone(),
            rules: entry.rules.clone(),
            snapshots: vec![entry.queried.snapshot.clone()],
            distance: entry.distance,
            source: path_source(&entry.path).to_string(),
            category: entry.category,
            confidence: weakest_confidence(&entry.path),
            evidence: entry.path.clone(),
            note: entry.note.clone(),
        };
        match by_node.get_mut(entry.node.selector.as_str()) {
            Some(existing) => {
                let mut snapshots = existing.snapshots.clone();
                merge_snapshot(&mut snapshots, entry.queried.snapshot.as_str());
                if entry_key(&candidate) < entry_key(existing) {
                    *existing = candidate;
                }
                existing.snapshots = snapshots;
            }
            None => {
                by_node.insert(entry.node.selector.clone(), candidate);
            }
        }
    }
    let mut entries: Vec<AffectedEntryPoint> = by_node.into_values().collect();
    entries.sort_by_key(entry_key);
    let found = entries.len();
    entries.truncate(cap);
    (entries, found)
}

fn entry_key(entry: &AffectedEntryPoint) -> (usize, EvidenceCategory, u8, bool, String) {
    (
        entry.distance,
        entry.category,
        confidence_rank(entry.confidence.as_str()),
        entry.evidence.to.snapshot != SnapshotSide::Head.label(),
        entry.entry_point.selector.clone(),
    )
}

/// Distinct candidate tests of one symbol, strongest source first, capped.
fn collect_tests(
    report: &crate::report::ExportedReport,
    queried: &dyn Fn(&EndpointRef) -> bool,
    cap: usize,
) -> (Vec<LabelledTest>, usize) {
    let mut by_test: BTreeMap<String, LabelledTest> = BTreeMap::new();
    for candidate in report
        .candidate_tests
        .candidates
        .iter()
        .filter(|candidate| queried(&candidate.queried))
    {
        let evidence = candidate.path_id.as_ref().and_then(|path_id| {
            report
                .evidence_paths
                .iter()
                .find(|path| {
                    &path.path_id == path_id
                        && path.to.selector == candidate.queried.selector
                        && path.to.snapshot == candidate.queried.snapshot
                })
                .cloned()
        });
        let confidence = match (&evidence, candidate.source) {
            (Some(path), _) => weakest_confidence(path),
            (None, CandidateSource::ImportRelationship) => CONFIDENCE_FILE_IMPORT.to_string(),
            (None, _) => CONFIDENCE_NAME_ONLY.to_string(),
        };
        let labelled = LabelledTest {
            test: candidate.test.clone(),
            snapshots: vec![candidate.queried.snapshot.clone()],
            source: candidate.source,
            category: candidate.category,
            confidence,
            evidence,
            truncated: candidate.truncated,
            note: candidate.note.clone(),
        };
        match by_test.get_mut(candidate.test.selector.as_str()) {
            Some(existing) => {
                let mut snapshots = existing.snapshots.clone();
                merge_snapshot(&mut snapshots, candidate.queried.snapshot.as_str());
                if test_key(&labelled) < test_key(existing) {
                    *existing = labelled;
                }
                existing.snapshots = snapshots;
            }
            None => {
                by_test.insert(candidate.test.selector.clone(), labelled);
            }
        }
    }
    let mut tests: Vec<LabelledTest> = by_test.into_values().collect();
    tests.sort_by_key(test_key);
    let found = tests.len();
    tests.truncate(cap);
    (tests, found)
}

fn test_key(test: &LabelledTest) -> (CandidateSource, EvidenceCategory, u8, String) {
    (
        test.source,
        test.category,
        confidence_rank(test.confidence.as_str()),
        test.test.selector.clone(),
    )
}

/// Distinct kept candidate tests across every analysed symbol.
fn test_selection(symbols: &[SymbolAnalysis]) -> Vec<TestSelection> {
    let mut by_test: BTreeMap<String, TestSelection> = BTreeMap::new();
    for symbol in symbols {
        for test in &symbol.candidate_tests {
            let key = (
                test.source,
                test.category,
                confidence_rank(test.confidence.as_str()),
            );
            match by_test.get_mut(test.test.selector.as_str()) {
                Some(existing) => {
                    if !existing.changed_symbols.contains(&symbol.selector) {
                        existing.changed_symbols.push(symbol.selector.clone());
                    }
                    let existing_key = (
                        existing.source,
                        existing.category,
                        confidence_rank(existing.confidence.as_str()),
                    );
                    if key < existing_key {
                        existing.source = test.source;
                        existing.category = test.category;
                        existing.confidence = test.confidence.clone();
                    }
                }
                None => {
                    by_test.insert(
                        test.test.selector.clone(),
                        TestSelection {
                            test: test.test.clone(),
                            source: test.source,
                            category: test.category,
                            confidence: test.confidence.clone(),
                            changed_symbols: vec![symbol.selector.clone()],
                        },
                    );
                }
            }
        }
    }
    let mut tests: Vec<TestSelection> = by_test.into_values().collect();
    tests.sort_by(|left, right| {
        (
            left.source,
            left.category,
            confidence_rank(left.confidence.as_str()),
            left.test.selector.as_str(),
        )
            .cmp(&(
                right.source,
                right.category,
                confidence_rank(right.confidence.as_str()),
                right.test.selector.as_str(),
            ))
    });
    tests
}
