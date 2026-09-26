//! Bounded change report: the JSON export.
//!
//! A report answers what
//! changed, what it affects, and what tests are plausibly connected in
//! one self-contained, deterministic document per the "Exported change
//! report" contract in `docs/design/change-explorer.md`. Two rules the doc
//! states for the export apply throughout this module:
//!
//! - **Never the whole repository.** Only the changed symbols in scope (all,
//!   by default, or an explicit selection) are queried, and every cited
//!   source location is either a bounded excerpt or a `file:line-span@sha`
//!   reference, never a full-repository dump.
//! - **Self-describing.** Both SHAs, the extractor and store schema versions,
//!   the index identity, the query options, and the complete
//!   truncated/unsupported/excluded scope are always present, even when
//!   empty, so a reader never has to guess what was left out.
//!
//! Two deliberate extensions beyond the design document's literal example JSON:
//!
//! - `entry_points` and `unresolved` are new top-level fields. The doc's
//!   example predates Milestone 3's entry-point rules
//!   ([`crate::evidence::ENTRY_POINT_RULES`]); the acceptance criteria for
//!   this task require both explicitly.
//! - The doc's example shows one report-wide `source_rendering` string.
//!   This report keeps that field (naming the requested excerpt mode) but
//!   also marks every individual cited location `embedded` or `reference`,
//!   because a single mode name cannot say which specific location has
//!   repository content embedded when a file could not be read.
//!
//! Every JSON-facing type here is a plain `#[derive(Serialize)]` struct, never
//! a `serde_json::Value` map, so field order is the struct's declaration
//! order and does not depend on `serde_json`'s (unspecified, feature-gated)
//! map ordering. That, plus a caller-pinnable `generated_at`, is what makes
//! two exports of the same inputs byte-identical.

use std::collections::BTreeSet;
use std::time::{SystemTime, UNIX_EPOCH};

use orbit_graph::{
    Confidence, DEFAULT_SHOW_MAX_BYTES, EXTRACTOR_VERSION, STORE_SCHEMA_VERSION, Selector,
};
use serde::Serialize;
use thiserror::Error;

use crate::changes::{
    ChangeStatus, ChangedSymbol, ChangedSymbols, ChangesError, FileChangeKind, OutOfScopeEntry,
    Pairing, PairingEvidence, SymbolRef, UncertainCandidate,
};
use crate::evidence::{
    CandidateSource, CandidateTest, EndpointRef, EntryPointReport, EvidenceBounds,
    EvidenceCategory, EvidenceCollector, EvidenceDirection, EvidenceEdge, EvidenceError,
    EvidencePath, EvidenceQuery, QueryOptions,
};
use crate::filters::{FilterSet, FilteredOut};
use crate::snapshot::{
    Comparison, ExclusionReason, Snapshot, SnapshotSide, WorkingTreeChange, WorkingTreeState,
};

/// Schema version of the exported report.
///
/// Independent of `orbit-graph`'s crate version and of `EXTRACTOR_VERSION`,
/// per the design doc's "Minimal data contract".
pub const REPORT_SCHEMA_VERSION: u32 = 1;

/// Lines shown on each side of a cited line in [`ExcerptMode::Controlled`].
///
/// A controlled excerpt is therefore at most `2 * EXCERPT_RADIUS_LINES + 1`
/// lines: the cited line, this many lines above it, and this many below.
pub const EXCERPT_RADIUS_LINES: usize = 5;

/// How much source text a report embeds at each cited location.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExcerptMode {
    /// Every cited location is a `file:line-span@sha` reference. No source
    /// text is embedded anywhere in the report.
    None,
    /// Each cited location gets a bounded excerpt: at most
    /// [`EXCERPT_RADIUS_LINES`] lines on each side of the cited line, or the
    /// symbol's own declared body for a changed-symbol or entry-point
    /// location. A location with no known line falls back to a reference.
    #[default]
    Controlled,
    /// Each cited location embeds the entire bounded read of its file (up to
    /// `DEFAULT_SHOW_MAX_BYTES`), not just a window around the cited line.
    FullSpan,
}

impl ExcerptMode {
    /// Stable label used in reports and command-line parsing.
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Controlled => "controlled",
            Self::FullSpan => "full-span",
        }
    }

    /// Parse a mode from its stable label.
    pub fn parse(label: &str) -> Option<Self> {
        match label {
            "none" => Some(Self::None),
            "controlled" => Some(Self::Controlled),
            "full-span" | "full_span" | "fullspan" => Some(Self::FullSpan),
            _ => None,
        }
    }
}

/// What to include in one report, and how to render its cited source.
#[derive(Debug, Clone, Default)]
pub struct ReportOptions {
    /// Changed-symbol selectors to report on. Empty selects every changed
    /// symbol, which is the default: a report never silently narrows itself.
    pub selection: Vec<String>,
    /// Presentation filters applied to the analysis.
    pub filters: FilterSet,
    /// Confidence floor applied to every evidence and entry-point query.
    pub min_confidence: Confidence,
    /// Bounds applied to every evidence and entry-point query.
    pub bounds: EvidenceBounds,
    /// How much source text to embed at each cited location.
    pub excerpts: ExcerptMode,
    /// Pin `generated_at` to this value instead of the wall clock, so a test
    /// can compare two exports byte-for-byte.
    pub generated_at: Option<String>,
    /// Emit the repository's absolute host path. Off by default: a report is
    /// often shared outside the machine that produced it.
    pub include_absolute_paths: bool,
}

/// Failure surface of report generation.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ReportError {
    /// Computing the changed-symbol slice failed.
    #[error(transparent)]
    Changes(#[from] ChangesError),
    /// Computing evidence, entry points, or candidate tests failed.
    #[error(transparent)]
    Evidence(#[from] EvidenceError),
}

/// A bounded window of source text embedded at one cited location.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceExcerpt {
    /// First line of the excerpt. `1`-based.
    ///
    /// For a symbol-scoped excerpt (a changed symbol or an entry point), this
    /// is `1`: the excerpt is the symbol's own bounded text, not a window
    /// into the whole file, so its line numbers are relative to the returned
    /// snippet rather than to the file.
    pub start_line: usize,
    /// Last line of the excerpt, inclusive.
    pub end_line: usize,
    /// The excerpt text itself.
    pub text: String,
    /// Whether the underlying read was itself truncated by
    /// `DEFAULT_SHOW_MAX_BYTES`, independent of the excerpt window.
    pub truncated: bool,
}

/// How one cited location is rendered: embedded source or a reference a
/// reader must resolve against the repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "rendering", rename_all = "snake_case")]
pub enum LocationEvidence {
    /// A bounded excerpt is embedded in the report.
    Embedded {
        /// The embedded text.
        excerpt: SourceExcerpt,
    },
    /// No source text is embedded; `reference` names exactly where to look.
    Reference {
        /// `file:line-span@sha`, `file:line@sha`, or `file@sha` when no line
        /// is known.
        reference: String,
    },
}

/// One evidence edge, with its cited source location rendered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReportEdge {
    /// Every field of the underlying evidence edge.
    #[serde(flatten)]
    pub edge: EvidenceEdge,
    /// How this edge's source location is rendered.
    #[serde(flatten)]
    pub location: LocationEvidence,
}

/// One evidence path, with every edge's source location rendered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReportEvidencePath {
    /// Payload schema version, from the underlying evidence path.
    pub schema_version: u32,
    /// Stable identifier of this path within the query that produced it.
    pub path_id: String,
    /// Affected endpoint.
    pub from: EndpointRef,
    /// Queried symbol.
    pub to: EndpointRef,
    /// Number of edges on the chain.
    pub distance: usize,
    /// Weakest category on the chain.
    pub category: EvidenceCategory,
    /// Whether this path was cut by a bound.
    pub truncated: bool,
    /// Which bound cut it, when `truncated`.
    pub truncated_by: Option<String>,
    /// Edges of the path, outermost hop first, each with its rendered
    /// location.
    pub edges: Vec<ReportEdge>,
}

/// One entry point, associated with the changed symbol it was found for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReportEntryPoint {
    /// The changed symbol this entry point was found for.
    pub queried: EndpointRef,
    /// The entry point itself, with its own location rendered.
    pub node: EndpointRef,
    /// Rendered location of the entry point's own declaration.
    #[serde(flatten)]
    pub node_location: LocationEvidence,
    /// Rule that fired, by identifier.
    pub rule: String,
    /// What that rule checks.
    pub rule_description: String,
    /// Every rule that fired for this symbol, in priority order.
    pub rules: Vec<String>,
    /// Hops from the entry point to the queried symbol.
    pub distance: usize,
    /// Weakest evidence category on `path`, mirroring `path.category` at the
    /// top level.
    pub category: EvidenceCategory,
    /// Shortest evidence path from this entry point to the queried symbol.
    pub path: ReportEvidencePath,
    /// What the rule observed.
    pub note: Option<String>,
}

/// One candidate test, associated with the changed symbol it supports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReportCandidateTest {
    /// The changed symbol this candidate was computed against.
    pub queried: EndpointRef,
    /// The test symbol or test file.
    pub test: EndpointRef,
    /// Rendered location of the candidate's own source.
    #[serde(flatten)]
    pub location: LocationEvidence,
    /// Which disclosed source produced this candidate.
    pub source: CandidateSource,
    /// Fixture-corpus spelling of `source`.
    pub label: String,
    /// Evidence category backing the candidate.
    pub category: EvidenceCategory,
    /// Identifier of the evidence path behind a `call_path` candidate.
    pub path_id: Option<String>,
    /// Changed symbols this candidate was computed against.
    pub changed_symbols: Vec<String>,
    /// Whether a bound cut the search that produced this candidate.
    pub truncated: bool,
    /// Why this candidate is plausible.
    pub note: Option<String>,
}

/// Candidate tests across every queried changed symbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReportCandidateTests {
    /// Payload schema version.
    pub schema_version: u32,
    /// Candidates, ordered by queried symbol then source strength.
    pub candidates: Vec<ReportCandidateTest>,
    /// Paths this comparison did not index, so a candidate living in one of
    /// them could not be found.
    pub unsupported_scope: Vec<OutOfScopeEntry>,
    /// Whether a bound cut the search behind any candidate here.
    pub truncated: bool,
}

/// One occurrence of a changed symbol, with its declaration's location
/// rendered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReportSymbolLocation {
    /// Selector, snapshot, and commit SHA of the occurrence.
    #[serde(flatten)]
    pub symbol: SymbolRef,
    /// Rendered location of the symbol's own declaration.
    #[serde(flatten)]
    pub location: LocationEvidence,
}

/// One changed symbol, with both occurrences' locations rendered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReportChangedSymbol {
    /// How the symbol changed.
    pub status: ChangeStatus,
    /// Which pairing rung established its identity.
    pub pairing: Pairing,
    /// What evidence supports that rung.
    pub pairing_evidence: PairingEvidence,
    /// Base-side occurrence, when the symbol exists in base.
    pub base: Option<ReportSymbolLocation>,
    /// Head-side occurrence, when the symbol exists in head.
    pub head: Option<ReportSymbolLocation>,
    /// Snapshots that support this entry.
    pub supporting_snapshots: Vec<String>,
    /// How Git classified the containing file.
    pub file_change: FileChangeKind,
    /// Base-side path of the containing file, when it exists in base.
    pub base_path: Option<String>,
    /// Head-side path of the containing file, when it exists in head.
    pub head_path: Option<String>,
    /// Every candidate partner, populated only for `uncertain` entries.
    pub uncertain_candidates: Vec<UncertainCandidate>,
    /// Human-readable qualification of this entry.
    pub note: Option<String>,
}

/// The changed-symbol slice as the report presents it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReportChangedSymbols {
    /// Payload schema version.
    pub schema_version: u32,
    /// Changed symbols in the report's selection, in the same order
    /// [`ChangedSymbols::compute`] produces.
    pub symbols: Vec<ReportChangedSymbol>,
    /// Changed paths the extractor did not index.
    pub out_of_scope: Vec<OutOfScopeEntry>,
    /// What the report's selection and presentation filters removed, and why.
    pub filtered_out: Vec<FilteredOut>,
}

/// One requested ref and the immutable commit it resolved to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RevisionRef {
    /// Ref exactly as supplied by the caller.
    pub requested_ref: String,
    /// Immutable commit SHA it resolved to.
    pub commit_sha: String,
}

/// One uncommitted working-tree entry, as the report presents it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkingTreeEntryView {
    /// Repository-relative, slash-separated path.
    pub path: String,
    /// Observed change kind.
    pub change: String,
}

/// Uncommitted state of the working tree, as the report presents it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkingTreeView {
    /// Whether any uncommitted change was observed.
    pub dirty: bool,
    /// Human-readable dirty notice, or `None` when clean.
    pub notice: Option<String>,
    /// Whether `entries` was truncated by `DIRTY_ENTRY_CAP`.
    pub truncated: bool,
    /// Observed changes, with the same bounded collection policy.
    pub entries: Vec<WorkingTreeEntryView>,
}

fn working_tree_view(state: &WorkingTreeState) -> WorkingTreeView {
    WorkingTreeView {
        dirty: state.dirty,
        notice: state.notice(),
        truncated: state.truncated,
        entries: state
            .entries
            .iter()
            .map(|entry| WorkingTreeEntryView {
                path: entry.path.clone(),
                change: change_label(entry.change).to_string(),
            })
            .collect(),
    }
}

fn change_label(change: WorkingTreeChange) -> &'static str {
    change.label()
}

/// One tree entry a snapshot deliberately did not materialize.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExcludedEntryView {
    /// Slash-separated path inside the commit tree.
    pub path: String,
    /// Stable exclusion-reason label.
    pub reason: String,
    /// Snapshot the exclusion belongs to.
    pub snapshot: String,
}

/// One side of the resolved comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ComparisonSnapshotView {
    /// `base` or `head`.
    pub side: String,
    /// Ref exactly as supplied by the caller.
    pub requested_ref: String,
    /// Immutable commit SHA this snapshot indexes.
    pub commit_sha: String,
    /// Number of files indexed in this snapshot.
    pub files_indexed: usize,
    /// Blobs materialized into this snapshot's tree.
    pub files_written: usize,
    /// `orbit_graph::EXTRACTOR_VERSION` used to build this snapshot.
    pub extractor_version: u32,
    /// Entries deliberately not materialized.
    pub excluded: Vec<ExcludedEntryView>,
}

/// The resolved comparison, verbatim per the design doc's "Resolved
/// comparison" contract, except that `repository` is omitted unless
/// [`ReportOptions::include_absolute_paths`] is set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ComparisonView {
    /// Payload schema version.
    pub schema_version: u32,
    /// Absolute repository path, present only when the caller opted in.
    pub repository: Option<String>,
    /// Comparison mode. Always `direct_base_head` in this milestone.
    pub mode: String,
    /// Base revision as requested and as resolved.
    pub base: RevisionRef,
    /// Head revision as requested and as resolved.
    pub head: RevisionRef,
    /// Effective base SHA: equal to `base.commit_sha` in `direct_base_head`
    /// mode, which never rewrites the caller's stated base.
    pub effective_base_sha: String,
    /// Uncommitted state of the working tree when the comparison was opened.
    pub working_tree: WorkingTreeView,
    /// Both snapshots, base then head.
    pub snapshots: Vec<ComparisonSnapshotView>,
}

/// Per-side index identity, so a reader can tell which index format produced
/// this report's evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IndexIdentityView {
    /// `orbit_graph::EXTRACTOR_VERSION` used to build both snapshots.
    pub extractor_version: u32,
    /// `orbit_graph::STORE_SCHEMA_VERSION` used to build both snapshots.
    pub store_schema_version: u32,
    /// Workspace-shared version of `orbit_graph` and `orbit-graph-changes`
    /// (both crates share one workspace version).
    pub orbit_graph_version: String,
    /// This binary's own version.
    pub explorer_version: String,
    /// Stable digest identifying the base snapshot's index: a BLAKE3 hash of
    /// its commit SHA, extractor version, store schema version, and indexed
    /// file count — not a hash of the on-disk database file, whose internal
    /// byte layout is not guaranteed stable across otherwise-identical
    /// builds.
    pub base_index_identity: String,
    /// The same digest for the head snapshot.
    pub head_index_identity: String,
}

fn index_identity_digest(snapshot: &Snapshot) -> String {
    let identity = snapshot.index_identity();
    let material = format!(
        "{}|{}|{}|{}",
        snapshot.commit_sha(),
        identity.extractor_version,
        identity.store_schema_version,
        snapshot.files_indexed(),
    );
    format!("blake3:{}", blake3::hash(material.as_bytes()).to_hex())
}

/// Bounds, filters, and excerpt policy in force for a report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReportQueryOptions {
    /// Bounds and filters shared with the live evidence and entry-point
    /// routes.
    #[serde(flatten)]
    pub evidence: QueryOptions,
    /// Excerpt mode requested for this report.
    pub excerpts: String,
    /// Lines shown on each side of a cited line in `controlled` mode.
    pub excerpt_radius_lines: usize,
    /// Changed-symbol selectors requested. Empty means every changed symbol.
    pub selection: Vec<String>,
}

/// One bound a query reached, and what it applied to.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct TruncationFlag {
    /// What was truncated: a queried symbol and the kind of query, or a
    /// standing bound such as the dirty-entry cap.
    pub what: String,
    /// Stable bound label.
    pub bound: String,
    /// The bound's value, in its own unit.
    pub value: u64,
}

/// Why one queried area has no result, beyond a bound being reached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnresolvedArea {
    /// Selector that was queried.
    pub selector: String,
    /// Snapshot it was queried in.
    pub snapshot: String,
    /// `evidence` or `entry_points`.
    pub kind: String,
    /// Reasons a result could be missing.
    pub reasons: Vec<String>,
}

/// The complete disclosed scope of a report: every bound reached, every path
/// the extractor never indexed, and every tree entry a snapshot excluded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReportScope {
    /// Every bound reached anywhere in this report, deduplicated.
    pub truncated: Vec<TruncationFlag>,
    /// Changed paths the extractor did not index.
    pub unsupported: Vec<OutOfScopeEntry>,
    /// Tree entries either snapshot deliberately did not materialize.
    pub excluded: Vec<ExcludedEntryView>,
}

/// The complete exported change report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExportedReport {
    /// Payload schema version.
    pub schema_version: u32,
    /// When this report was generated. Pinnable via
    /// [`ReportOptions::generated_at`] so two exports of the same inputs
    /// compare byte-for-byte.
    pub generated_at: String,
    /// The resolved comparison payload.
    pub comparison: ComparisonView,
    /// Per-side index identity.
    pub index_identity: IndexIdentityView,
    /// Bounds, filters, and excerpt policy in force.
    pub query_options: ReportQueryOptions,
    /// The changed-symbol slice in scope for this report.
    pub changed_symbols: ReportChangedSymbols,
    /// Inbound evidence paths for every queried changed symbol, flattened.
    pub evidence_paths: Vec<ReportEvidencePath>,
    /// Outbound (callee) evidence paths for every queried changed symbol,
    /// flattened: `edges[0].from` is the changed symbol and the last edge's
    /// `to` is the reached callee, mirroring `evidence_paths` with the arrow
    /// reversed.
    pub outbound_paths: Vec<ReportEvidencePath>,
    /// Entry points for every queried changed symbol, flattened.
    pub entry_points: Vec<ReportEntryPoint>,
    /// Candidate tests for every queried changed symbol.
    pub candidate_tests: ReportCandidateTests,
    /// Areas where no result could be produced, beyond a bound being reached.
    pub unresolved: Vec<UnresolvedArea>,
    /// Every truncation flag, out-of-scope path, and excluded tree entry in
    /// this report.
    pub scope: ReportScope,
    /// `reference` when [`ExcerptMode::None`] was requested, `excerpt`
    /// otherwise. Per-location rendering is also marked on every cited
    /// location; this field states the report-wide policy that produced it.
    pub source_rendering: String,
}

/// Read a snapshot's source lines for excerpt windows, caching one read per
/// `(snapshot, file)` pair queried during a single report build.
/// Cache key for one snapshot side's file lines.
type LineCacheKey = (&'static str, String);
/// Cached lines of a file, and whether the underlying read was truncated.
type LineCacheValue = Option<(Vec<String>, bool)>;

/// State accumulated while rendering cited locations across one report: a
/// per-`(side, file)` line cache, so repeated citations of the same file
/// read it once, and every bound reached along the way.
struct RenderState<'a> {
    comparison: &'a Comparison,
    cache: std::collections::BTreeMap<LineCacheKey, LineCacheValue>,
    truncation: BTreeSet<TruncationFlag>,
}

impl<'a> RenderState<'a> {
    fn new(comparison: &'a Comparison) -> Self {
        Self {
            comparison,
            cache: std::collections::BTreeMap::new(),
            truncation: BTreeSet::new(),
        }
    }

    /// Lines of `file` in `side`, and whether the underlying read was
    /// truncated by `DEFAULT_SHOW_MAX_BYTES`.
    fn lines(&mut self, side: SnapshotSide, file: &str) -> LineCacheValue {
        let key = (side.label(), file.to_string());
        if !self.cache.contains_key(&key) {
            let snapshot = self.comparison.snapshot(side);
            let value = read_file_lines(snapshot, file);
            self.cache.insert(key.clone(), value);
        }
        self.cache.get(&key).cloned().flatten()
    }

    fn snapshot(&self, side: SnapshotSide) -> &'a Snapshot {
        self.comparison.snapshot(side)
    }

    fn note_truncation(&mut self, what: impl Into<String>, bound: impl Into<String>, value: u64) {
        self.truncation.insert(TruncationFlag {
            what: what.into(),
            bound: bound.into(),
            value,
        });
    }

    fn into_truncation(self) -> Vec<TruncationFlag> {
        self.truncation.into_iter().collect()
    }
}

fn read_file_lines(snapshot: &Snapshot, file: &str) -> Option<(Vec<String>, bool)> {
    let selector = Selector::File {
        path: file.to_string(),
    };
    let view = snapshot
        .graph()
        .show(&selector, DEFAULT_SHOW_MAX_BYTES)
        .ok()
        .flatten()?;
    let truncated = view.metadata.truncated;
    let text = String::from_utf8_lossy(view.bytes.as_slice()).into_owned();
    Some((text.lines().map(str::to_string).collect(), truncated))
}

fn side_from_label(label: &str) -> Option<SnapshotSide> {
    match label {
        "base" => Some(SnapshotSide::Base),
        "head" => Some(SnapshotSide::Head),
        _ => None,
    }
}

/// `1`-based inclusive `(start, end)` window of at most
/// `2 * radius + 1` lines around `line`, clamped to `[1, total]`.
fn window(line: usize, total: usize, radius: usize) -> (usize, usize) {
    let total = total.max(1);
    let start = line.saturating_sub(radius).max(1);
    let end = (line + radius).min(total);
    (start.min(end), end)
}

fn reference_pointer(file: &str, span: Option<(usize, usize)>, sha: &str) -> String {
    match span {
        Some((start, end)) if start == end => format!("{file}:{start}@{sha}"),
        Some((start, end)) => format!("{file}:{start}-{end}@{sha}"),
        None => format!("{file}@{sha}"),
    }
}

/// Render a file-and-line cited location: a window of `file` around `line`
/// when one is known, or the whole bounded file in [`ExcerptMode::FullSpan`].
fn render_location(
    state: &mut RenderState<'_>,
    side: SnapshotSide,
    commit_sha: &str,
    file: &str,
    line: Option<usize>,
    mode: ExcerptMode,
    what: &str,
) -> LocationEvidence {
    if matches!(mode, ExcerptMode::None) {
        return LocationEvidence::Reference {
            reference: reference_pointer(file, line.map(|l| (l, l)), commit_sha),
        };
    }
    let Some((lines, file_truncated)) = state.lines(side, file) else {
        return LocationEvidence::Reference {
            reference: reference_pointer(file, line.map(|l| (l, l)), commit_sha),
        };
    };
    let total = lines.len();
    let span = match mode {
        ExcerptMode::FullSpan => Some((1usize, total.max(1))),
        ExcerptMode::Controlled => line.map(|current| window(current, total, EXCERPT_RADIUS_LINES)),
        ExcerptMode::None => unreachable!("handled above"),
    };
    let Some((start, end)) = span else {
        return LocationEvidence::Reference {
            reference: reference_pointer(file, line.map(|l| (l, l)), commit_sha),
        };
    };
    let end = end.min(total.max(start));
    let text = if total == 0 {
        String::new()
    } else {
        lines[start.saturating_sub(1)..end].join("\n")
    };
    let truncated = file_truncated && matches!(mode, ExcerptMode::FullSpan);
    if truncated {
        state.note_truncation(
            what.to_string(),
            "source_max_bytes",
            DEFAULT_SHOW_MAX_BYTES as u64,
        );
    }
    LocationEvidence::Embedded {
        excerpt: SourceExcerpt {
            start_line: start,
            end_line: end,
            text,
            truncated,
        },
    }
}

/// Render a symbol's own declaration as a cited location: the symbol's
/// bounded body, never a file-wide window, because the symbol's own span is
/// already a tighter bound than any line window could give it.
fn render_symbol_location(
    state: &mut RenderState<'_>,
    side: SnapshotSide,
    commit_sha: &str,
    selector: &str,
    mode: ExcerptMode,
    what: &str,
) -> LocationEvidence {
    let Ok(parsed) = selector.parse::<Selector>() else {
        return LocationEvidence::Reference {
            reference: format!("{selector}@{commit_sha}"),
        };
    };
    if matches!(mode, ExcerptMode::None) {
        return LocationEvidence::Reference {
            reference: format!("{}@{commit_sha}", parsed.path()),
        };
    }
    let snapshot = state.snapshot(side);
    let Ok(Some(view)) = snapshot.graph().show(&parsed, DEFAULT_SHOW_MAX_BYTES) else {
        return LocationEvidence::Reference {
            reference: format!("{}@{commit_sha}", parsed.path()),
        };
    };
    let text = String::from_utf8_lossy(view.bytes.as_slice()).into_owned();
    let line_count = text.lines().count().max(1);
    if view.metadata.truncated {
        state.note_truncation(
            what.to_string(),
            "source_max_bytes",
            DEFAULT_SHOW_MAX_BYTES as u64,
        );
    }
    LocationEvidence::Embedded {
        excerpt: SourceExcerpt {
            start_line: 1,
            end_line: line_count,
            text,
            truncated: view.metadata.truncated,
        },
    }
}

fn render_endpoint_location(
    state: &mut RenderState<'_>,
    endpoint: &EndpointRef,
    commit_sha: &str,
    mode: ExcerptMode,
    what: &str,
) -> LocationEvidence {
    let Some(side) = side_from_label(endpoint.snapshot.as_str()) else {
        return LocationEvidence::Reference {
            reference: format!("{}@{commit_sha}", endpoint.label),
        };
    };
    if endpoint.origin == "symbol" {
        render_symbol_location(
            state,
            side,
            commit_sha,
            endpoint.selector.as_str(),
            mode,
            what,
        )
    } else {
        let file = endpoint
            .selector
            .strip_prefix("file:")
            .unwrap_or(endpoint.selector.as_str());
        render_location(state, side, commit_sha, file, None, mode, what)
    }
}

fn render_edge(state: &mut RenderState<'_>, edge: &EvidenceEdge, mode: ExcerptMode) -> ReportEdge {
    let side = side_from_label(edge.snapshot.as_str()).unwrap_or(SnapshotSide::Head);
    let what = format!("evidence_edge:{}@{}", edge.from_selector, edge.snapshot);
    let location = render_location(
        state,
        side,
        edge.commit_sha.as_str(),
        edge.source.file.as_str(),
        edge.source.line,
        mode,
        what.as_str(),
    );
    ReportEdge {
        edge: edge.clone(),
        location,
    }
}

fn render_path(
    state: &mut RenderState<'_>,
    path: &EvidencePath,
    mode: ExcerptMode,
) -> ReportEvidencePath {
    ReportEvidencePath {
        schema_version: path.schema_version,
        path_id: path.path_id.clone(),
        from: path.from.clone(),
        to: path.to.clone(),
        distance: path.distance,
        category: path.category,
        truncated: path.truncated,
        truncated_by: path.truncated_by.clone(),
        edges: path
            .edges
            .iter()
            .map(|edge| render_edge(state, edge, mode))
            .collect(),
    }
}

/// One side of a comparison, with a lazily built [`EvidenceCollector`].
struct Collectors<'a> {
    comparison: &'a Comparison,
    base: Option<EvidenceCollector<'a>>,
    head: Option<EvidenceCollector<'a>>,
}

impl<'a> Collectors<'a> {
    fn new(comparison: &'a Comparison) -> Self {
        Self {
            comparison,
            base: None,
            head: None,
        }
    }

    fn get(&mut self, side: SnapshotSide) -> Result<&mut EvidenceCollector<'a>, ReportError> {
        let slot = match side {
            SnapshotSide::Base => &mut self.base,
            SnapshotSide::Head => &mut self.head,
        };
        if slot.is_none() {
            *slot = Some(EvidenceCollector::new(self.comparison, side)?);
        }
        match slot {
            Some(collector) => Ok(collector),
            // Unreachable: the branch above always fills an empty slot before
            // this match runs. A defensive error, not a panic, is still the
            // right shape for the case the compiler cannot see is impossible.
            None => Err(ReportError::Evidence(EvidenceError::Selector(
                "internal error: evidence collector slot was not initialized".to_string(),
            ))),
        }
    }
}

/// Build the complete exported report for `comparison` under `options`.
pub fn build_report(
    comparison: &Comparison,
    options: &ReportOptions,
) -> Result<ExportedReport, ReportError> {
    let mode = options.excerpts;
    let generated_at = options.generated_at.clone().unwrap_or_else(now_rfc3339);

    let changed = ChangedSymbols::compute(comparison)?;
    let (changed_for_report, filtered_out) = changed.filtered(&options.filters);

    let selection: BTreeSet<&str> = options.selection.iter().map(String::as_str).collect();
    let selected: Vec<&ChangedSymbol> = changed_for_report
        .symbols
        .iter()
        .filter(|symbol| {
            selection.is_empty()
                || symbol
                    .base
                    .as_ref()
                    .is_some_and(|reference| selection.contains(reference.selector.as_str()))
                || symbol
                    .head
                    .as_ref()
                    .is_some_and(|reference| selection.contains(reference.selector.as_str()))
        })
        .collect();

    let mut state = RenderState::new(comparison);

    if comparison.working_tree().truncated {
        state.note_truncation(
            "working_tree",
            "dirty_entry_cap",
            crate::snapshot::DIRTY_ENTRY_CAP as u64,
        );
    }

    let mut report_symbols = Vec::with_capacity(changed_for_report.symbols.len());
    for symbol in &changed_for_report.symbols {
        report_symbols.push(render_changed_symbol(&mut state, symbol, mode));
    }

    let mut collectors = Collectors::new(comparison);
    let mut evidence_paths = Vec::new();
    let mut outbound_paths = Vec::new();
    let mut entry_points = Vec::new();
    let mut candidates = Vec::new();
    let mut unresolved = Vec::new();
    let mut candidate_tests_truncated = false;
    let mut unsupported_scope = changed.out_of_scope.clone();

    for symbol in &selected {
        for side_label in &symbol.supporting_snapshots {
            let Some(side) = side_from_label(side_label.as_str()) else {
                continue;
            };
            let selector = match side {
                SnapshotSide::Base => symbol.base.as_ref(),
                SnapshotSide::Head => symbol.head.as_ref(),
            };
            let Some(selector) = selector else { continue };
            let selector_text = selector.selector.as_str();

            let query = EvidenceQuery {
                min_confidence: options.min_confidence,
                bounds: options.bounds,
                filters: options.filters.clone(),
                changes: Some(&changed),
                direction: EvidenceDirection::Inbound,
            };
            let outbound_query = EvidenceQuery {
                direction: EvidenceDirection::Outbound,
                ..query.clone()
            };

            let collector = collectors.get(side)?;
            let evidence = collector.evidence(selector_text, &query)?;
            for hit in &evidence.bounds_hit {
                state.note_truncation(
                    format!("evidence:{selector_text}@{side_label}"),
                    hit.bound.clone(),
                    hit.value,
                );
            }
            if !evidence.no_path_reasons.is_empty() {
                unresolved.push(UnresolvedArea {
                    selector: selector_text.to_string(),
                    snapshot: side_label.clone(),
                    kind: "evidence".to_string(),
                    reasons: evidence.no_path_reasons.clone(),
                });
            }
            for path in &evidence.paths {
                evidence_paths.push(render_path(&mut state, path, mode));
            }

            let outbound_evidence = collector.evidence(selector_text, &outbound_query)?;
            for hit in &outbound_evidence.bounds_hit {
                state.note_truncation(
                    format!("outbound_evidence:{selector_text}@{side_label}"),
                    hit.bound.clone(),
                    hit.value,
                );
            }
            if !outbound_evidence.no_path_reasons.is_empty() {
                unresolved.push(UnresolvedArea {
                    selector: selector_text.to_string(),
                    snapshot: side_label.clone(),
                    kind: "outbound_evidence".to_string(),
                    reasons: outbound_evidence.no_path_reasons.clone(),
                });
            }
            for path in &outbound_evidence.paths {
                outbound_paths.push(render_path(&mut state, path, mode));
            }

            let entry_report = collector.entry_points(selector_text, &query)?;
            for hit in &entry_report.bounds_hit {
                state.note_truncation(
                    format!("entry_points:{selector_text}@{side_label}"),
                    hit.bound.clone(),
                    hit.value,
                );
            }
            if !entry_report.no_entry_point_reasons.is_empty() {
                unresolved.push(UnresolvedArea {
                    selector: selector_text.to_string(),
                    snapshot: side_label.clone(),
                    kind: "entry_points".to_string(),
                    reasons: entry_report.no_entry_point_reasons.clone(),
                });
            }
            for entry_point in &entry_report.entry_points {
                entry_points.push(render_entry_point(
                    &mut state,
                    &entry_report,
                    entry_point,
                    mode,
                ));
            }

            let candidate_report =
                collector.candidate_tests(selector_text, &query, unsupported_scope.clone())?;
            candidate_tests_truncated |= candidate_report.truncated;
            for candidate in &candidate_report.candidates {
                candidates.push(render_candidate(
                    &mut state,
                    &entry_report.target,
                    entry_report.commit_sha.as_str(),
                    candidate,
                    mode,
                ));
            }
        }
    }

    candidates.sort_by(|left, right| {
        left.queried
            .selector
            .cmp(&right.queried.selector)
            .then(left.source.cmp(&right.source))
            .then(left.test.selector.cmp(&right.test.selector))
    });
    unsupported_scope.sort_by(|left, right| left.path.cmp(&right.path));
    unresolved.sort_by(|left, right| {
        left.selector
            .cmp(&right.selector)
            .then(left.snapshot.cmp(&right.snapshot))
            .then(left.kind.cmp(&right.kind))
    });

    let comparison_view = comparison_view(comparison, options.include_absolute_paths);
    let index_identity = IndexIdentityView {
        extractor_version: EXTRACTOR_VERSION,
        store_schema_version: STORE_SCHEMA_VERSION,
        orbit_graph_version: env!("CARGO_PKG_VERSION").to_string(),
        explorer_version: env!("CARGO_PKG_VERSION").to_string(),
        base_index_identity: index_identity_digest(comparison.base()),
        head_index_identity: index_identity_digest(comparison.head()),
    };

    let mut excluded = Vec::new();
    for (side, snapshot) in [
        (SnapshotSide::Base, comparison.base()),
        (SnapshotSide::Head, comparison.head()),
    ] {
        for entry in &snapshot.materialization().excluded {
            excluded.push(ExcludedEntryView {
                path: entry.path.clone(),
                reason: exclusion_label(entry.reason).to_string(),
                snapshot: side.label().to_string(),
            });
        }
    }

    let evidence_query_template = EvidenceQuery {
        min_confidence: options.min_confidence,
        bounds: options.bounds,
        filters: options.filters.clone(),
        changes: None,
        direction: EvidenceDirection::Inbound,
    };
    let query_options = ReportQueryOptions {
        evidence: QueryOptions::new(&evidence_query_template),
        excerpts: mode.label().to_string(),
        excerpt_radius_lines: EXCERPT_RADIUS_LINES,
        selection: options.selection.clone(),
    };

    Ok(ExportedReport {
        schema_version: REPORT_SCHEMA_VERSION,
        generated_at,
        comparison: comparison_view,
        index_identity,
        query_options,
        changed_symbols: ReportChangedSymbols {
            schema_version: changed_for_report.schema_version,
            symbols: report_symbols,
            out_of_scope: changed_for_report.out_of_scope.clone(),
            filtered_out,
        },
        evidence_paths,
        outbound_paths,
        entry_points,
        candidate_tests: ReportCandidateTests {
            schema_version: crate::evidence::EVIDENCE_SCHEMA_VERSION,
            candidates,
            unsupported_scope,
            truncated: candidate_tests_truncated,
        },
        unresolved,
        scope: ReportScope {
            truncated: state.into_truncation(),
            unsupported: {
                let mut list = changed.out_of_scope.clone();
                list.sort_by(|left, right| left.path.cmp(&right.path));
                list
            },
            excluded,
        },
        source_rendering: if matches!(mode, ExcerptMode::None) {
            "reference".to_string()
        } else {
            "excerpt".to_string()
        },
    })
}

fn exclusion_label(reason: ExclusionReason) -> &'static str {
    reason.label()
}

fn render_changed_symbol(
    state: &mut RenderState<'_>,
    symbol: &ChangedSymbol,
    mode: ExcerptMode,
) -> ReportChangedSymbol {
    let mut render_occurrence = |occurrence: &Option<SymbolRef>| {
        occurrence.as_ref().map(|symbol_ref| {
            let side = side_from_label(symbol_ref.snapshot.as_str()).unwrap_or(SnapshotSide::Head);
            let what = format!(
                "changed_symbol:{}@{}",
                symbol_ref.selector, symbol_ref.snapshot
            );
            let location = render_symbol_location(
                state,
                side,
                symbol_ref.commit_sha.as_str(),
                symbol_ref.selector.as_str(),
                mode,
                what.as_str(),
            );
            ReportSymbolLocation {
                symbol: symbol_ref.clone(),
                location,
            }
        })
    };

    ReportChangedSymbol {
        status: symbol.status,
        pairing: symbol.pairing,
        pairing_evidence: symbol.pairing_evidence,
        base: render_occurrence(&symbol.base),
        head: render_occurrence(&symbol.head),
        supporting_snapshots: symbol.supporting_snapshots.clone(),
        file_change: symbol.file_change,
        base_path: symbol.base_path.clone(),
        head_path: symbol.head_path.clone(),
        uncertain_candidates: symbol.uncertain_candidates.clone(),
        note: symbol.note.clone(),
    }
}

fn render_entry_point(
    state: &mut RenderState<'_>,
    report: &EntryPointReport,
    entry_point: &crate::evidence::EntryPoint,
    mode: ExcerptMode,
) -> ReportEntryPoint {
    let what = format!(
        "entry_point:{}@{}",
        entry_point.node.selector, entry_point.node.snapshot
    );
    let node_location = render_endpoint_location(
        state,
        &entry_point.node,
        report.commit_sha.as_str(),
        mode,
        what.as_str(),
    );
    ReportEntryPoint {
        queried: report.target.clone(),
        node: entry_point.node.clone(),
        node_location,
        rule: entry_point.rule.clone(),
        rule_description: entry_point.rule_description.clone(),
        rules: entry_point.rules.clone(),
        distance: entry_point.distance,
        category: entry_point.category,
        path: render_path(state, &entry_point.path, mode),
        note: entry_point.note.clone(),
    }
}

fn render_candidate(
    state: &mut RenderState<'_>,
    queried: &EndpointRef,
    commit_sha: &str,
    candidate: &CandidateTest,
    mode: ExcerptMode,
) -> ReportCandidateTest {
    let what = format!(
        "candidate_test:{}@{}",
        candidate.test.selector, candidate.test.snapshot
    );
    // Candidate tests do not carry the commit SHA of their own snapshot
    // directly; the test's own snapshot label always matches the queried
    // symbol's snapshot within one `candidate_tests` call, so the caller's
    // commit SHA for that side applies here too.
    let location =
        render_endpoint_location(state, &candidate.test, commit_sha, mode, what.as_str());
    ReportCandidateTest {
        queried: queried.clone(),
        test: candidate.test.clone(),
        location,
        source: candidate.source,
        label: candidate.label.clone(),
        category: candidate.category,
        path_id: candidate.path_id.clone(),
        changed_symbols: candidate.changed_symbols.clone(),
        truncated: candidate.truncated,
        note: candidate.note.clone(),
    }
}

fn comparison_view(comparison: &Comparison, include_absolute_paths: bool) -> ComparisonView {
    let snapshot_view = |side: SnapshotSide| {
        let snapshot = comparison.snapshot(side);
        ComparisonSnapshotView {
            side: side.label().to_string(),
            requested_ref: snapshot.requested_ref().to_string(),
            commit_sha: snapshot.commit_sha().to_string(),
            files_indexed: snapshot.files_indexed(),
            files_written: snapshot.materialization().files_written,
            extractor_version: EXTRACTOR_VERSION,
            excluded: snapshot
                .materialization()
                .excluded
                .iter()
                .map(|entry| ExcludedEntryView {
                    path: entry.path.clone(),
                    reason: exclusion_label(entry.reason).to_string(),
                    snapshot: side.label().to_string(),
                })
                .collect(),
        }
    };

    ComparisonView {
        schema_version: REPORT_SCHEMA_VERSION,
        repository: include_absolute_paths.then(|| comparison.repository().display().to_string()),
        mode: comparison.mode().label().to_string(),
        base: RevisionRef {
            requested_ref: comparison.base().requested_ref().to_string(),
            commit_sha: comparison.base().commit_sha().to_string(),
        },
        head: RevisionRef {
            requested_ref: comparison.head().requested_ref().to_string(),
            commit_sha: comparison.head().commit_sha().to_string(),
        },
        effective_base_sha: comparison.base().commit_sha().to_string(),
        working_tree: working_tree_view(comparison.working_tree()),
        snapshots: vec![
            snapshot_view(SnapshotSide::Base),
            snapshot_view(SnapshotSide::Head),
        ],
    }
}

/// Current wall-clock time as an RFC 3339 UTC timestamp with second
/// precision, with no external date/time dependency.
fn now_rfc3339() -> String {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format_epoch_seconds(elapsed.as_secs())
}

fn format_epoch_seconds(total_seconds: u64) -> String {
    let days = (total_seconds / 86_400) as i64;
    let seconds_of_day = total_seconds % 86_400;
    let (hour, minute, second) = (
        seconds_of_day / 3600,
        (seconds_of_day % 3600) / 60,
        seconds_of_day % 60,
    );
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Howard Hinnant's `civil_from_days`: days since the Unix epoch to a
/// proleptic-Gregorian `(year, month, day)`. Public-domain algorithm; avoids
/// pulling in a date/time crate for one conversion.
fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = (z - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_position = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_position + 2) / 5 + 1) as u32;
    let month = if month_position < 10 {
        month_position + 3
    } else {
        month_position - 9
    } as u32;
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}
