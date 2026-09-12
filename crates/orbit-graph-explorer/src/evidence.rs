//! Relationship evidence and candidate tests for one changed symbol.
//!
//! Two products, both scoped to exactly one snapshot:
//!
//! - [`EvidenceReport`] — bounded multi-hop **inbound** evidence paths: ordered
//!   edge chains from an affected symbol back to the queried symbol, each edge
//!   carrying its relationship kind, evidence category, confidence, snapshot
//!   SHA, and source location. A `fallback` block returned by `refs` renders as
//!   [`EvidenceCategory::HeuristicMatch`] with its note attached, never merged
//!   into the primary result.
//! - [`EntryPointReport`] — affected symbols that are entry points under the
//!   disclosed rules in [`ENTRY_POINT_RULES`], each carrying the rule that
//!   fired and the shortest evidence path back to the queried symbol.
//! - [`CandidateTests`] — tests with a defensible connection to the symbol,
//!   from the three disclosed sources and never anything else: a call path from
//!   a test-classified file, an import relationship from a test file, or a
//!   naming/file heuristic. The three sources are computed independently, so a
//!   heuristic candidate is never promoted to a call-path candidate.
//!
//! Neither product is a claim about execution. Static reachability is
//! *potential* impact: not proof of execution, not proof of coverage, and not a
//! severity judgment. When no path is found,
//! [`EvidenceReport::no_path_reasons`] states the reasons a path could be
//! missing, because "no path in the indexed evidence" is a weaker claim than
//! "no relationship exists".
//!
//! Traversal is directed: [`orbit_graph::ImpactDirection::Inbound`] establishes
//! the bounded inbound node set and the typed
//! [`orbit_graph::ImpactOrigin`] labelling, and each hop's edges come from the
//! `refs` query for that node, which is what carries a file, a line, a
//! reference kind, and a confidence. A node the index could only attribute to
//! its file is labelled `file` and is never presented as a symbol.
//!
//! Three bounds apply to every query — traversal depth, a node cap, and a
//! wall-clock budget — and each one that is reached is reported through
//! [`EvidenceReport::truncated_by`] and [`EvidenceReport::bounds_hit`] with the
//! bound's value. Nothing after a bound is presented as complete. A node
//! already on the current path is never re-entered, so a cyclic call graph
//! terminates with no repeated node in any path.
//!
//! A path is only as strong as its weakest edge: [`EvidencePath::category`] is
//! the weakest category on the chain, so a resolved call reached through a
//! name-only hop is never rendered as resolved.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use orbit_graph::{
    Confidence, DEFAULT_IMPACT_DEPTH, DEFAULT_SHOW_MAX_BYTES, IMPACT_NODE_CAP, ImpactDirection,
    ImpactOrigin, OverviewFormat, RefConfidence, RefEntry, RefKind, RefOpts, RelationEntry,
    Selector,
};
use serde::Serialize;
use thiserror::Error;

use crate::changes::{ChangedSymbols, OutOfScopeEntry};
use crate::filters::{FilterLog, FilterReason, FilterSet, FilteredOut, language_of};
use crate::snapshot::{Comparison, Snapshot, SnapshotSide};

/// Schema version of the evidence and candidate-test payloads.
pub const EVIDENCE_SCHEMA_VERSION: u32 = 1;

/// Traversal depth used when a request does not name one.
pub const EVIDENCE_DEPTH: u8 = DEFAULT_IMPACT_DEPTH;

/// Per-request wall-clock budget, in milliseconds, used when a launch does not
/// name one.
///
/// A traversal that runs out of budget reports `truncated_by: "time_budget"`
/// rather than returning a partial result as if it were complete.
pub const DEFAULT_TIME_BUDGET_MS: u64 = 5_000;

/// Largest traversal depth a request may ask for.
///
/// Depth is a bound, and a bound a caller can raise without limit is not a
/// bound; the cap keeps one request from walking an entire index.
pub const MAX_EVIDENCE_DEPTH: u8 = 10;

/// Standing reasons a relationship can be absent from the indexed evidence.
///
/// The index is a static, syntax-driven tree-sitter graph, not a compiler or a
/// language server. These reasons accompany every empty result so "no path
/// found" is never read as "no relationship exists".
const STANDING_NO_PATH_REASONS: &[&str] = &[
    "The index is syntax-driven: dynamic dispatch, reflection, runtime imports, and calls written \
     inside macro invocations produce no edge.",
    "Generated and macro-expanded code is indexed as written, not as expanded.",
];

/// Failure surface of evidence collection.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum EvidenceError {
    /// The supplied selector could not be parsed.
    #[error("{0}")]
    Selector(String),
    /// A query against one snapshot index failed.
    #[error("{operation} for the {side} snapshot: {reason}")]
    Query {
        /// Operation being performed.
        operation: &'static str,
        /// Snapshot side the query belonged to.
        side: SnapshotSide,
        /// Failure reason.
        reason: String,
    },
}

/// One of the design document's evidence categories.
///
/// Every edge and every candidate test carries exactly one, and the category
/// survives into the UI and the export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EvidenceCategory {
    /// A call edge whose target resolved to a qualified symbol.
    ResolvedCall,
    /// A syntactic reference at a known file and line in the named snapshot.
    ObservedReference,
    /// A module or import edge between files.
    ImportRelationship,
    /// A name-only or same-module association, including every `fuzzy_name`
    /// result and every fallback block. Weak: may be a different symbol that
    /// happens to share a name.
    HeuristicMatch,
}

impl EvidenceCategory {
    /// Stable label used in reports and user-facing output.
    pub fn label(self) -> &'static str {
        match self {
            Self::ResolvedCall => "resolved_call",
            Self::ObservedReference => "observed_reference",
            Self::ImportRelationship => "import_relationship",
            Self::HeuristicMatch => "heuristic_match",
        }
    }
}

/// Classify one reference or relation into an evidence category.
///
/// A `fuzzy_name` result is a heuristic match whatever its kind: it is a
/// name-only association and may be an unrelated symbol sharing the name.
/// Otherwise a resolved call is a `resolved_call`, an import or use edge is an
/// `import_relationship`, and anything else is an `observed_reference`.
fn categorize(kind: RefKind, confidence: RefConfidence) -> EvidenceCategory {
    if confidence == RefConfidence::FuzzyName {
        return EvidenceCategory::HeuristicMatch;
    }
    match kind {
        RefKind::Call
            if matches!(
                confidence,
                RefConfidence::Exact | RefConfidence::ImportResolved
            ) =>
        {
            EvidenceCategory::ResolvedCall
        }
        RefKind::Use => EvidenceCategory::ImportRelationship,
        _ => EvidenceCategory::ObservedReference,
    }
}

/// Stable label of a confidence level.
pub fn confidence_label(confidence: RefConfidence) -> &'static str {
    match confidence {
        RefConfidence::Exact => "exact",
        RefConfidence::ImportResolved => "import_resolved",
        RefConfidence::SameModule => "same_module",
        RefConfidence::FuzzyName => "fuzzy_name",
    }
}

/// Parse a confidence floor from its stable label, accepting the CLI spellings.
pub fn parse_confidence(label: &str) -> Option<RefConfidence> {
    match label {
        "exact" => Some(RefConfidence::Exact),
        "import_resolved" | "import" => Some(RefConfidence::ImportResolved),
        "same_module" => Some(RefConfidence::SameModule),
        "fuzzy_name" | "fuzzy" => Some(RefConfidence::FuzzyName),
        _ => None,
    }
}

/// Stable label of a reference or relation kind.
pub fn kind_label(kind: RefKind) -> &'static str {
    match kind {
        RefKind::Call => "call",
        RefKind::Type => "type",
        RefKind::Use => "use",
        RefKind::TraitBound => "trait_bound",
        RefKind::Impl => "impl",
        RefKind::Extends => "extends",
        RefKind::Implements => "implements",
    }
}

/// Where an edge was observed in source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidenceSource {
    /// Snapshot-relative source file. Always present.
    pub file: String,
    /// One-based source line, or `None` when the site could not be attributed
    /// to a line.
    pub line: Option<usize>,
}

/// Whether an endpoint is an indexed symbol or a file-attributed call site.
///
/// Mirrors [`orbit_graph::ImpactOrigin`]: a call site that lies outside every
/// indexed symbol span is attributed to its file, and is never presented as a
/// symbol.
fn origin_label(origin: ImpactOrigin) -> &'static str {
    match origin {
        ImpactOrigin::Symbol => "symbol",
        ImpactOrigin::File => "file",
    }
}

/// One edge of an evidence path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidenceEdge {
    /// Referencing endpoint, as `<path>#<symbol>` when the reference lies
    /// inside an indexed symbol span and as `<path>` when it does not.
    pub from: String,
    /// Canonical selector for the referencing endpoint, copyable as text.
    pub from_selector: String,
    /// Whether the referencing endpoint is a symbol or a file-attributed site.
    pub from_origin: String,
    /// Referenced endpoint, as `<path>#<symbol>`.
    pub to: String,
    /// Canonical selector for the referenced endpoint.
    pub to_selector: String,
    /// `RefKind` value behind this edge.
    pub relationship: String,
    /// Evidence category.
    pub category: EvidenceCategory,
    /// `orbit_graph` confidence label for this edge.
    pub confidence: String,
    /// Snapshot side this edge was observed in.
    pub snapshot: String,
    /// Immutable commit SHA of that snapshot.
    pub commit_sha: String,
    /// Where the edge was observed.
    pub source: EvidenceSource,
    /// Qualification attached to this edge, such as a fallback block's note.
    pub note: Option<String>,
}

/// One endpoint of an evidence path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EndpointRef {
    /// Canonical selector for the endpoint.
    pub selector: String,
    /// Snapshot the selector resolves in.
    pub snapshot: String,
    /// Display label: `<path>#<symbol>`, or `<path>` for a file-attributed
    /// endpoint.
    pub label: String,
    /// `symbol` or `file`, from [`orbit_graph::ImpactOrigin`].
    pub origin: String,
}

impl EndpointRef {
    fn symbol(selector: &str, snapshot: &str, label: &str) -> Self {
        Self {
            selector: selector.to_string(),
            snapshot: snapshot.to_string(),
            label: label.to_string(),
            origin: origin_label(ImpactOrigin::Symbol).to_string(),
        }
    }
}

/// A path from an affected symbol back to the queried symbol.
///
/// `edges` is ordered outermost hop first: `edges[0].from` is the affected
/// symbol and the last edge points at the queried symbol, so a reader walks the
/// chain in the direction the change propagates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidencePath {
    /// Payload schema version.
    pub schema_version: u32,
    /// Stable identifier of this path within its report.
    pub path_id: String,
    /// Affected endpoint.
    pub from: EndpointRef,
    /// Queried symbol.
    pub to: EndpointRef,
    /// Number of edges on the chain.
    pub distance: usize,
    /// Weakest category on the chain: a path is only as strong as its weakest
    /// edge.
    pub category: EvidenceCategory,
    /// Whether this path was cut by a bound.
    pub truncated: bool,
    /// Which bound cut it, when `truncated`.
    pub truncated_by: Option<String>,
    /// Edges of the path, outermost hop first.
    pub edges: Vec<EvidenceEdge>,
}

/// A bound the traversal reached, and the value of that bound.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BoundHit {
    /// Stable bound label: `depth`, `impact_node_cap`, or `time_budget`.
    pub bound: String,
    /// The bound's value, in its own unit.
    pub value: u64,
}

/// Bounds a traversal runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvidenceBounds {
    /// Maximum number of hops from the queried symbol.
    pub depth: u8,
    /// Maximum number of nodes, and of paths, a traversal may return.
    pub node_cap: usize,
    /// Wall-clock budget for one traversal, in milliseconds.
    pub time_budget_ms: u64,
}

impl Default for EvidenceBounds {
    fn default() -> Self {
        Self {
            depth: EVIDENCE_DEPTH,
            node_cap: IMPACT_NODE_CAP,
            time_budget_ms: DEFAULT_TIME_BUDGET_MS,
        }
    }
}

impl EvidenceBounds {
    /// Effective depth: `0` selects the default, and the value is capped by
    /// [`MAX_EVIDENCE_DEPTH`].
    pub fn effective_depth(&self) -> u8 {
        let depth = if self.depth == 0 {
            EVIDENCE_DEPTH
        } else {
            self.depth
        };
        depth.min(MAX_EVIDENCE_DEPTH)
    }

    /// Effective node cap: at least one node.
    pub fn effective_node_cap(&self) -> usize {
        self.node_cap.max(1)
    }

    /// Effective wall-clock budget.
    ///
    /// `0` is a real value: a traversal with no budget answers nothing and
    /// reports the budget as the bound that stopped it, rather than quietly
    /// running anyway.
    pub fn budget(&self) -> Duration {
        Duration::from_millis(self.time_budget_ms)
    }
}

/// One evidence request: a confidence floor, bounds, and presentation filters.
#[derive(Debug, Clone)]
pub struct EvidenceQuery<'a> {
    /// Confidence floor applied to every hop.
    pub min_confidence: Confidence,
    /// Bounds in force.
    pub bounds: EvidenceBounds,
    /// Presentation filters. They change what the response shows, never the
    /// evidence underneath it.
    pub filters: FilterSet,
    /// Changed-symbol slice, used by the `change_kind` filter and to report
    /// which sides carry evidence for the queried symbol.
    pub changes: Option<&'a ChangedSymbols>,
}

impl EvidenceQuery<'_> {
    /// A query at `min_confidence` with default bounds and no filters.
    pub fn new(min_confidence: Confidence) -> Self {
        Self {
            min_confidence,
            bounds: EvidenceBounds::default(),
            filters: FilterSet::default(),
            changes: None,
        }
    }
}

/// Bounds and filters in force for one query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QueryOptions {
    /// Traversal depth in force.
    pub depth: u8,
    /// Traversal direction. Always `inbound`: this is caller evidence.
    pub direction: String,
    /// Confidence floor applied to the query.
    pub min_confidence: String,
    /// Reference-kind filter, when one was applied.
    pub kind: Option<String>,
    /// Node cap in force.
    pub node_cap: usize,
    /// Wall-clock budget in force, in milliseconds.
    pub time_budget_ms: u64,
    /// Byte bound applied to any source excerpt read while answering.
    pub source_max_bytes: usize,
    /// `language` filter, when one was applied.
    pub language: Option<String>,
    /// `change_kind` filter, when one was applied.
    pub change_kind: Vec<String>,
    /// `scope` path-prefix filter, when one was applied.
    pub scope: Option<String>,
}

impl QueryOptions {
    /// The bounds and filters `query` puts in force, as the payload states
    /// them.
    pub fn new(query: &EvidenceQuery<'_>) -> Self {
        Self {
            depth: query.bounds.effective_depth(),
            direction: "inbound".to_string(),
            min_confidence: confidence_label(query.min_confidence).to_string(),
            kind: None,
            node_cap: query.bounds.effective_node_cap(),
            time_budget_ms: query
                .bounds
                .budget()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
            source_max_bytes: DEFAULT_SHOW_MAX_BYTES,
            language: query.filters.language.clone(),
            change_kind: query.filters.change_kind.clone(),
            scope: query.filters.scope.clone(),
        }
    }
}

/// What the core inbound `impact` query reported for the same symbol.
///
/// Recorded alongside the paths so a reader can see the bounded node set the
/// traversal was built on, and whether that set was itself truncated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImpactSummary {
    /// Traversal direction: always `inbound`.
    pub direction: String,
    /// Number of symbols the core query reached.
    pub visited_nodes: usize,
    /// Whether the core query stopped at `IMPACT_NODE_CAP`.
    pub truncated: bool,
    /// The core query's node cap.
    pub node_cap: usize,
    /// Note attached to a name-only fallback set, when the precise floor
    /// reached nothing.
    pub fallback_note: Option<String>,
}

/// Bounded multi-hop inbound evidence for one symbol in one snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidenceReport {
    /// Payload schema version.
    pub schema_version: u32,
    /// Queried symbol.
    pub target: EndpointRef,
    /// Immutable commit SHA of the queried snapshot.
    pub commit_sha: String,
    /// Whether the selector resolved to an indexed symbol on this side.
    pub resolved: bool,
    /// Qualified name of the resolved target, when it resolved.
    pub resolved_qualified: Option<String>,
    /// Bounds and filters in force.
    pub query_options: QueryOptions,
    /// What the core inbound `impact` query reported.
    pub impact: ImpactSummary,
    /// One path per affected endpoint, strongest category first.
    pub paths: Vec<EvidencePath>,
    /// Candidate rows the confidence floor excluded at the queried symbol.
    pub skipped_low_confidence: usize,
    /// Edges skipped because the endpoint already appears on the path, which is
    /// how a cyclic call graph terminates.
    pub cycles_pruned: usize,
    /// Whether any bound cut this result.
    pub truncated: bool,
    /// Which bound cut it first, when `truncated`.
    pub truncated_by: Option<String>,
    /// Every bound the traversal reached, with its value.
    pub bounds_hit: Vec<BoundHit>,
    /// What the presentation filters removed, and why.
    pub filtered_out: Vec<FilteredOut>,
    /// Changed-symbol status of the queried selector, when it is in the slice.
    pub change_status: Option<String>,
    /// Snapshots that carry evidence for the queried symbol.
    pub evidence_sides: Vec<String>,
    /// Why a path could be missing. Always populated when `paths` is empty.
    pub no_path_reasons: Vec<String>,
}

/// One disclosed entry-point rule.
///
/// The rules are data, listed in every entry-point response, so the UI can show
/// exactly what was applied. There is no probabilistic scoring: a symbol is an
/// entry point because a named rule fired, or it is not one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct EntryPointRule {
    /// Stable rule identifier.
    pub id: &'static str,
    /// What the rule checks, in plain language.
    pub description: &'static str,
}

/// Every entry-point rule this crate applies, in priority order.
pub const ENTRY_POINT_RULES: &[EntryPointRule] = &[
    EntryPointRule {
        id: "main_function",
        description: "A function or method named `main`: the process entry point of a Rust, Go, \
                      Java, C, or C# program, and a conventional one elsewhere.",
    },
    EntryPointRule {
        id: "cli_command_handler",
        description: "A CLI command handler the core command machinery resolves: a `command:` \
                      selector formed from the symbol's name and its file's module path resolves \
                      to this symbol in this snapshot, and `trace` roots that command's call tree \
                      at it. Commands the extractor never discovered cannot fire this rule.",
    },
    EntryPointRule {
        id: "crate_root_public_item",
        description: "A public function or type declared at a crate root or package initializer \
                      (`lib.rs`, `main.rs`, `__init__.py`, `index.js`, `index.ts`): the surface \
                      another package can call.",
    },
    EntryPointRule {
        id: "test_function",
        description: "A test function: a symbol in a test-classified path, or one carrying a \
                      `test`/`spec` affix. These also feed the candidate-test list.",
    },
];

/// One affected symbol that a disclosed rule classified as an entry point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EntryPoint {
    /// The entry point itself.
    pub node: EndpointRef,
    /// Rule that fired, by identifier.
    pub rule: String,
    /// What that rule checks.
    pub rule_description: String,
    /// Every rule that fired for this symbol, in priority order.
    pub rules: Vec<String>,
    /// Hops from the entry point to the queried symbol; `0` when the queried
    /// symbol is itself an entry point.
    pub distance: usize,
    /// Shortest evidence path from this entry point to the queried symbol.
    pub path: EvidencePath,
    /// What the rule observed, such as the command name that resolved.
    pub note: Option<String>,
}

/// Entry points among the symbols affected by one changed symbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EntryPointReport {
    /// Payload schema version.
    pub schema_version: u32,
    /// Queried symbol.
    pub target: EndpointRef,
    /// Immutable commit SHA of the queried snapshot.
    pub commit_sha: String,
    /// Bounds and filters in force.
    pub query_options: QueryOptions,
    /// The rules that were applied, so a reader can see the whole rule set.
    pub rules: Vec<EntryPointRule>,
    /// Entry points found, shortest path first.
    pub entry_points: Vec<EntryPoint>,
    /// Whether a bound cut the traversal behind this result.
    pub truncated: bool,
    /// Which bound cut it first, when `truncated`.
    pub truncated_by: Option<String>,
    /// Every bound the traversal reached, with its value.
    pub bounds_hit: Vec<BoundHit>,
    /// What the presentation filters removed, and why.
    pub filtered_out: Vec<FilteredOut>,
    /// Why no entry point was found, when none was.
    pub no_entry_point_reasons: Vec<String>,
}

/// Which disclosed source produced a candidate test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CandidateSource {
    /// A static call from a test-classified file or symbol to the symbol.
    CallPath,
    /// An import edge from a test file to the symbol's file or module.
    ImportRelationship,
    /// A naming or file-location signal only.
    NamingHeuristic,
}

impl CandidateSource {
    /// Stable label, matching the design document's `source` vocabulary.
    pub fn label(self) -> &'static str {
        match self {
            Self::CallPath => "call_path",
            Self::ImportRelationship => "import_relationship",
            Self::NamingHeuristic => "naming_heuristic",
        }
    }

    /// Fixture-corpus spelling of the same source, as used by the
    /// `candidate_tests[].category` field of `expected.json`.
    pub fn corpus_label(self) -> &'static str {
        match self {
            Self::CallPath => "call-path",
            Self::ImportRelationship => "import",
            Self::NamingHeuristic => "naming-heuristic",
        }
    }
}

/// One candidate test for a changed symbol.
///
/// A candidate is never presented as coverage, and the list is never described
/// as "the tests for this change".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CandidateTest {
    /// The test symbol, or the test file when no symbol could be attributed.
    pub test: EndpointRef,
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

/// Candidate tests for one symbol in one snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CandidateTests {
    /// Payload schema version.
    pub schema_version: u32,
    /// Queried symbol.
    pub target: EndpointRef,
    /// Immutable commit SHA of the queried snapshot.
    pub commit_sha: String,
    /// Bounds in force.
    pub query_options: QueryOptions,
    /// Candidates, ordered by source strength then selector.
    pub candidates: Vec<CandidateTest>,
    /// Paths in this comparison the extractor did not index, so a candidate
    /// living in one of them could not be found.
    pub unsupported_scope: Vec<OutOfScopeEntry>,
    /// Whether a bound cut the search.
    pub truncated: bool,
}

/// One indexed symbol's byte span within its file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SymbolSpan {
    name: String,
    kind: String,
    qualified: String,
    start: usize,
    end: usize,
}

/// Per-file information needed to attribute a source line to a symbol.
///
/// `line_starts` is derived from a source read bounded by
/// [`DEFAULT_SHOW_MAX_BYTES`], so a reference past that bound has no line
/// offset and is attributed to its file rather than to a symbol. Symbol spans
/// come from each symbol's own metadata and are exact.
#[derive(Debug, Clone, Default)]
struct FileIndex {
    line_starts: Vec<usize>,
    symbols: Vec<SymbolSpan>,
}

/// Collects evidence for one snapshot of a comparison.
///
/// Holds a small per-file cache, so repeated queries against the same snapshot
/// do not re-read the same source spans.
pub struct EvidenceCollector<'a> {
    comparison: &'a Comparison,
    side: SnapshotSide,
    indexed_files: Vec<String>,
    symbols_by_file: BTreeMap<String, Vec<IndexedSymbol>>,
    file_cache: BTreeMap<String, FileIndex>,
}

/// One indexed symbol, as the snapshot's overview reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct IndexedSymbol {
    name: String,
    kind: String,
    qualified: String,
}

impl<'a> EvidenceCollector<'a> {
    /// Build a collector for one side of `comparison`.
    pub fn new(comparison: &'a Comparison, side: SnapshotSide) -> Result<Self, EvidenceError> {
        let overview = comparison
            .snapshot(side)
            .graph()
            .overview(None, OverviewFormat::Full)
            .map_err(|error| EvidenceError::Query {
                operation: "list indexed files",
                side,
                reason: error.to_string(),
            })?;

        let mut indexed_files = Vec::new();
        let mut symbols_by_file = BTreeMap::new();
        for file in overview.files {
            let symbols: Vec<IndexedSymbol> = file
                .symbols
                .into_iter()
                .map(|symbol| IndexedSymbol {
                    name: symbol.name,
                    kind: symbol.kind,
                    qualified: symbol.qualified,
                })
                .collect();
            symbols_by_file.insert(file.path.clone(), symbols);
            indexed_files.push(file.path);
        }
        indexed_files.sort();

        Ok(Self {
            comparison,
            side,
            indexed_files,
            symbols_by_file,
            file_cache: BTreeMap::new(),
        })
    }

    /// Which side this collector queries.
    pub fn side(&self) -> SnapshotSide {
        self.side
    }

    /// Indexed files of this snapshot, in path order.
    pub fn indexed_files(&self) -> &[String] {
        self.indexed_files.as_slice()
    }

    fn snapshot(&self) -> &Snapshot {
        self.comparison.snapshot(self.side)
    }

    fn side_label(&self) -> String {
        self.side.label().to_string()
    }

    fn commit_sha(&self) -> String {
        self.snapshot().commit_sha().to_string()
    }

    /// Bounded multi-hop inbound evidence for `selector`.
    ///
    /// The core `impact` query in [`ImpactDirection::Inbound`] establishes the
    /// bounded node set and the typed origin labelling; each hop's edges come
    /// from `refs`, which is what carries a file, a line, a reference kind, and
    /// a confidence. Every bound reached is reported, and no result after a
    /// bound is presented as complete.
    pub fn evidence(
        &mut self,
        selector: &str,
        query: &EvidenceQuery<'_>,
    ) -> Result<EvidenceReport, EvidenceError> {
        let parsed = parse_selector(selector)?;
        let started = Instant::now();
        let depth = query.bounds.effective_depth();
        let node_cap = query.bounds.effective_node_cap();
        let budget = query.bounds.budget();
        let commit_sha = self.commit_sha();
        let side_label = self.side_label();
        let target_label = endpoint_label(&parsed);
        let target = EndpointRef {
            selector: selector.to_string(),
            snapshot: side_label.clone(),
            label: target_label.clone(),
            // A non-symbol selector addresses a file, not an indexed symbol,
            // and is labelled as one rather than promoted.
            origin: origin_label(match parsed {
                Selector::Symbol { .. } => ImpactOrigin::Symbol,
                _ => ImpactOrigin::File,
            })
            .to_string(),
        };

        let impact = self
            .snapshot()
            .graph()
            .impact_with_direction(
                &parsed,
                depth,
                query.min_confidence,
                ImpactDirection::Inbound,
            )
            .map_err(|error| EvidenceError::Query {
                operation: "query inbound impact",
                side: self.side,
                reason: error.to_string(),
            })?;
        let mut origins: BTreeMap<String, ImpactOrigin> = BTreeMap::new();
        for entry in &impact.touched {
            origins.insert(entry.qualified_name.clone(), entry.origin);
        }
        let impact_summary = ImpactSummary {
            direction: "inbound".to_string(),
            visited_nodes: impact.visited_nodes,
            truncated: impact.truncated,
            node_cap: IMPACT_NODE_CAP,
            fallback_note: impact
                .fallback
                .as_ref()
                .map(|fallback| fallback.note.clone()),
        };

        let mut queue: VecDeque<Frontier> = VecDeque::new();
        queue.push_back(Frontier {
            selector: selector.to_string(),
            label: target_label.clone(),
            path: Vec::new(),
        });
        // Nodes already expanded, the queried symbol included. A node is
        // expanded once, on its shortest chain, which is what keeps the
        // traversal polynomial and makes the reported path the shortest one.
        let mut expanded: BTreeSet<String> = BTreeSet::from([selector.to_string()]);
        let mut paths: Vec<EvidencePath> = Vec::new();
        let mut cycles_pruned = 0usize;
        let mut skipped_low_confidence = 0usize;
        let mut resolved = false;
        let mut resolved_qualified = None;
        let mut hit_depth = false;
        let mut hit_node_cap = false;
        let mut hit_time_budget = false;

        'traversal: while let Some(frontier) = queue.pop_front() {
            if started.elapsed() >= budget {
                hit_time_budget = true;
                break;
            }
            let hop = frontier.path.len();
            if hop >= usize::from(depth) {
                hit_depth = true;
                continue;
            }

            let inbound = self.inbound_edges(
                frontier.selector.as_str(),
                frontier.label.as_str(),
                query.min_confidence,
                commit_sha.as_str(),
                &origins,
            )?;
            if hop == 0 {
                resolved = inbound.resolved;
                resolved_qualified = inbound.qualified.clone();
                skipped_low_confidence = inbound.skipped_low_confidence;
            }

            let mut on_path: BTreeSet<&str> = BTreeSet::from([frontier.selector.as_str()]);
            on_path.insert(selector);
            for edge in &frontier.path {
                on_path.insert(edge.from_selector.as_str());
            }

            for edge in inbound.edges {
                if on_path.contains(edge.from_selector.as_str()) {
                    // Re-entering a node already on this path would repeat it,
                    // so a mutually recursive call graph terminates here.
                    cycles_pruned += 1;
                    continue;
                }
                if paths.len() >= node_cap {
                    hit_node_cap = true;
                    break 'traversal;
                }

                let from_origin = edge.from_origin.clone();
                let from_selector = edge.from_selector.clone();
                let from_label = edge.from.clone();
                let mut chain = Vec::with_capacity(frontier.path.len() + 1);
                chain.push(edge);
                chain.extend(frontier.path.iter().cloned());
                let distance = chain.len();
                let category = chain
                    .iter()
                    .map(|edge| edge.category)
                    .max()
                    .unwrap_or(EvidenceCategory::HeuristicMatch);
                paths.push(EvidencePath {
                    schema_version: EVIDENCE_SCHEMA_VERSION,
                    path_id: String::new(),
                    from: EndpointRef {
                        selector: from_selector.clone(),
                        snapshot: side_label.clone(),
                        label: from_label.clone(),
                        origin: from_origin.clone(),
                    },
                    to: target.clone(),
                    distance,
                    category,
                    truncated: false,
                    truncated_by: None,
                    edges: chain.clone(),
                });

                let is_symbol = from_origin == origin_label(ImpactOrigin::Symbol);
                if is_symbol && distance >= usize::from(depth) {
                    // A symbol left unexpanded because the depth bound stopped
                    // here. Whether it has callers of its own is unknown, and
                    // the bound report says exactly that.
                    hit_depth = true;
                }
                let expandable = is_symbol && distance < usize::from(depth);
                if expandable && !expanded.contains(from_selector.as_str()) {
                    if expanded.len() >= node_cap {
                        hit_node_cap = true;
                        break 'traversal;
                    }
                    expanded.insert(from_selector.clone());
                    queue.push_back(Frontier {
                        selector: from_selector,
                        label: from_label,
                        path: chain,
                    });
                }
            }
        }

        // Strongest evidence first, then shortest chain, so the first rows a
        // reader sees are the ones that claim the most.
        paths.sort_by(|left, right| {
            left.category
                .cmp(&right.category)
                .then(left.distance.cmp(&right.distance))
                .then(
                    left.edges
                        .first()
                        .map(|edge| (edge.source.file.clone(), edge.source.line))
                        .cmp(
                            &right
                                .edges
                                .first()
                                .map(|edge| (edge.source.file.clone(), edge.source.line)),
                        ),
                )
                .then(left.from.selector.cmp(&right.from.selector))
        });

        let mut log = FilterLog::default();
        log.record_count(FilterReason::Confidence, skipped_low_confidence);
        let paths = self.apply_filters(paths, query, &mut log);

        let mut paths = paths;
        for (index, path) in paths.iter_mut().enumerate() {
            path.path_id = format!("p-{}", index + 1);
        }

        let mut bounds_hit = Vec::new();
        if hit_time_budget {
            bounds_hit.push(BoundHit {
                bound: "time_budget".to_string(),
                value: budget.as_millis().try_into().unwrap_or(u64::MAX),
            });
        }
        if hit_node_cap {
            bounds_hit.push(BoundHit {
                bound: "impact_node_cap".to_string(),
                value: node_cap as u64,
            });
        }
        if hit_depth {
            bounds_hit.push(BoundHit {
                bound: "depth".to_string(),
                value: u64::from(depth),
            });
        }
        let truncated_by = bounds_hit.first().map(|hit| hit.bound.clone());
        let truncated = truncated_by.is_some();

        let (change_status, evidence_sides) = self.side_context(selector, query.changes);
        let no_path_reasons = if paths.is_empty() {
            self.no_path_reasons(
                resolved,
                skipped_low_confidence,
                query.min_confidence,
                bounds_hit.as_slice(),
            )
        } else {
            Vec::new()
        };

        Ok(EvidenceReport {
            schema_version: EVIDENCE_SCHEMA_VERSION,
            target,
            commit_sha,
            resolved,
            resolved_qualified,
            query_options: QueryOptions::new(query),
            impact: impact_summary,
            paths,
            skipped_low_confidence,
            cycles_pruned,
            truncated,
            truncated_by,
            bounds_hit,
            filtered_out: log.into_filtered_out(),
            change_status,
            evidence_sides,
            no_path_reasons,
        })
    }

    /// Entry points among the symbols affected by `selector`.
    ///
    /// Runs the same bounded inbound traversal and classifies each affected
    /// symbol — and the queried symbol itself — with the disclosed rules in
    /// [`ENTRY_POINT_RULES`]. Each result carries the rule that fired and the
    /// shortest evidence path back to the queried symbol.
    pub fn entry_points(
        &mut self,
        selector: &str,
        query: &EvidenceQuery<'_>,
    ) -> Result<EntryPointReport, EvidenceError> {
        let report = self.evidence(selector, query)?;

        // Shortest path per affected endpoint: an entry point is reported once,
        // with the least evidence needed to connect it.
        let mut shortest: BTreeMap<String, EvidencePath> = BTreeMap::new();
        for path in &report.paths {
            let entry = shortest.entry(path.from.selector.clone());
            match entry {
                std::collections::btree_map::Entry::Vacant(slot) => {
                    slot.insert(path.clone());
                }
                std::collections::btree_map::Entry::Occupied(mut slot) => {
                    if path.distance < slot.get().distance {
                        slot.insert(path.clone());
                    }
                }
            }
        }

        let mut candidates: Vec<(String, EvidencePath)> = Vec::new();
        candidates.push((
            selector.to_string(),
            EvidencePath {
                schema_version: EVIDENCE_SCHEMA_VERSION,
                path_id: "p-0".to_string(),
                from: report.target.clone(),
                to: report.target.clone(),
                distance: 0,
                category: EvidenceCategory::ObservedReference,
                truncated: false,
                truncated_by: None,
                edges: Vec::new(),
            },
        ));
        candidates.extend(shortest);

        let mut entry_points = Vec::new();
        for (node_selector, path) in candidates {
            if path.from.origin != origin_label(ImpactOrigin::Symbol) {
                // A file-attributed call site is not a symbol, so no rule about
                // symbols can fire for it.
                continue;
            }
            let Some(address) = symbol_address(node_selector.as_str()) else {
                continue;
            };
            let fired = self.classify_entry_point(&address)?;
            let Some((rule, note)) = fired.first().cloned() else {
                continue;
            };
            entry_points.push(EntryPoint {
                node: path.from.clone(),
                rule: rule.id.to_string(),
                rule_description: rule.description.to_string(),
                rules: fired.iter().map(|(rule, _)| rule.id.to_string()).collect(),
                distance: path.distance,
                path,
                note,
            });
        }
        entry_points.sort_by(|left, right| {
            left.distance
                .cmp(&right.distance)
                .then(left.rule.cmp(&right.rule))
                .then(left.node.selector.cmp(&right.node.selector))
        });

        let mut no_entry_point_reasons = Vec::new();
        if entry_points.is_empty() {
            no_entry_point_reasons.push(
                "No affected symbol matched a disclosed entry-point rule. The rules are listed in \
                 this response; a symbol reached only through evidence a rule does not cover is \
                 not classified, and no score is assigned in place of a rule."
                    .to_string(),
            );
            no_entry_point_reasons.extend(report.no_path_reasons.iter().cloned());
        }

        Ok(EntryPointReport {
            schema_version: EVIDENCE_SCHEMA_VERSION,
            target: report.target,
            commit_sha: report.commit_sha,
            query_options: report.query_options,
            rules: ENTRY_POINT_RULES.to_vec(),
            entry_points,
            truncated: report.truncated,
            truncated_by: report.truncated_by,
            bounds_hit: report.bounds_hit,
            filtered_out: report.filtered_out,
            no_entry_point_reasons,
        })
    }

    /// Apply the presentation filters, recording what each one removed.
    fn apply_filters(
        &self,
        paths: Vec<EvidencePath>,
        query: &EvidenceQuery<'_>,
        log: &mut FilterLog,
    ) -> Vec<EvidencePath> {
        if query.filters.is_empty() {
            return paths;
        }
        let mut kept = Vec::with_capacity(paths.len());
        for path in paths {
            let file = path
                .edges
                .first()
                .map(|edge| edge.source.file.clone())
                .unwrap_or_default();
            if !query.filters.language_admits(file.as_str()) {
                log.record(FilterReason::Language, path.from.selector.as_str());
                continue;
            }
            if !query.filters.scope_admits(file.as_str()) {
                log.record(FilterReason::Scope, path.from.selector.as_str());
                continue;
            }
            if !query.filters.change_kind.is_empty() {
                let status = change_status_of(query.changes, path.from.selector.as_str());
                if !query.filters.change_kind_admits(status.as_deref()) {
                    log.record(FilterReason::ChangeKind, path.from.selector.as_str());
                    continue;
                }
            }
            kept.push(path);
        }
        kept
    }

    /// The changed-symbol status of `selector` and the sides that carry
    /// evidence for it.
    fn side_context(
        &self,
        selector: &str,
        changes: Option<&ChangedSymbols>,
    ) -> (Option<String>, Vec<String>) {
        let Some(changes) = changes else {
            return (None, vec![self.side.label().to_string()]);
        };
        let Some(entry) = changes.entries_for(selector).first().copied() else {
            return (None, vec![self.side.label().to_string()]);
        };
        (
            Some(entry.status.label().to_string()),
            entry.supporting_snapshots.clone(),
        )
    }

    /// Inbound references, structural relations, and the name-only fallback for
    /// one node, as evidence edges.
    fn inbound_edges(
        &mut self,
        node_selector: &str,
        node_label: &str,
        min_confidence: Confidence,
        commit_sha: &str,
        origins: &BTreeMap<String, ImpactOrigin>,
    ) -> Result<InboundEdges, EvidenceError> {
        let parsed = parse_selector(node_selector)?;
        let side_label = self.side_label();
        let result = self
            .snapshot()
            .refs(
                &parsed,
                &RefOpts {
                    confidence: min_confidence,
                    kind: None,
                },
            )
            .map_err(|error| EvidenceError::Query {
                operation: "query inbound references",
                side: self.side,
                reason: error.to_string(),
            })?;

        let mut edges = Vec::new();
        for entry in result.refs.clone() {
            edges.push(self.ref_edge(
                &entry,
                node_selector,
                node_label,
                commit_sha,
                None,
                origins,
            )?);
        }
        for relation in &result.relations {
            edges.push(relation_edge(
                relation,
                node_selector,
                node_label,
                side_label.as_str(),
                commit_sha,
                origins,
            ));
        }
        if let Some(fallback) = result.fallback.clone() {
            for entry in fallback.refs {
                // A fallback block is name-only: it renders as a heuristic
                // match with its note attached, never merged into the primary
                // result.
                let mut edge = self.ref_edge(
                    &entry,
                    node_selector,
                    node_label,
                    commit_sha,
                    Some(fallback.note.as_str()),
                    origins,
                )?;
                edge.category = EvidenceCategory::HeuristicMatch;
                edges.push(edge);
            }
        }

        edges.sort_by(|left, right| {
            left.category
                .cmp(&right.category)
                .then(left.source.file.cmp(&right.source.file))
                .then(left.source.line.cmp(&right.source.line))
                .then(left.relationship.cmp(&right.relationship))
        });

        Ok(InboundEdges {
            edges,
            resolved: result.target.qualified.is_some(),
            qualified: result.target.qualified,
            skipped_low_confidence: result.skipped_low_confidence,
        })
    }

    /// Which disclosed entry-point rules fire for `address`, in priority order.
    fn classify_entry_point(
        &mut self,
        address: &SymbolAddress,
    ) -> Result<Vec<(EntryPointRule, Option<String>)>, EvidenceError> {
        let mut fired = Vec::new();
        for rule in ENTRY_POINT_RULES {
            let note = match rule.id {
                "main_function" => (address.name == "main"
                    && is_callable_kind(address.kind.as_str()))
                .then(|| format!("`{}` is named `main`", address.name)),
                "cli_command_handler" => self
                    .command_handler_for(address)?
                    .map(|command| format!("`command:{command}` resolves to this symbol")),
                "crate_root_public_item" => {
                    if root_module_kind(address.path.as_str()).is_some()
                        && is_public_item_kind(address.kind.as_str())
                        && self.declaration_is_public(address)?
                    {
                        Some(format!(
                            "public `{}` declared in the {} {}",
                            address.kind,
                            root_module_kind(address.path.as_str()).unwrap_or("root"),
                            address.path
                        ))
                    } else {
                        None
                    }
                }
                "test_function" => (is_test_path(address.path.as_str())
                    || normalize_test_name(address.name.as_str()) != address.name)
                    .then(|| {
                        format!(
                            "`{}` is a test symbol by path or naming convention",
                            address.name
                        )
                    }),
                _ => None,
            };
            if let Some(note) = note {
                fired.push((*rule, Some(note)));
            }
        }
        Ok(fired)
    }

    /// The command name the core command machinery resolves to `address`, when
    /// there is one.
    ///
    /// The public API exposes no way to list the commands an index discovered,
    /// so candidate names are formed from the symbol's own name and its file's
    /// module path and probed with the `command:` selector. A candidate counts
    /// only when the command resolves to exactly this symbol, in this file, and
    /// `trace` roots the command's call tree at it.
    fn command_handler_for(
        &mut self,
        address: &SymbolAddress,
    ) -> Result<Option<String>, EvidenceError> {
        if !is_callable_kind(address.kind.as_str()) {
            return Ok(None);
        }
        let Some(qualified) = self.qualified_name_of(address) else {
            return Ok(None);
        };
        for candidate in command_candidates(address) {
            let selector = Selector::Command {
                name: candidate.clone(),
            };
            let view = self
                .snapshot()
                .graph()
                .show(&selector, DEFAULT_SHOW_MAX_BYTES)
                .map_err(|error| EvidenceError::Query {
                    operation: "resolve command selector",
                    side: self.side,
                    reason: error.to_string(),
                })?;
            let Some(view) = view else {
                continue;
            };
            if view.metadata.file != address.path
                || view.metadata.qualified.as_deref() != Some(qualified.as_str())
            {
                continue;
            }
            let trace = self
                .snapshot()
                .graph()
                .trace(candidate.as_str(), 1, RefConfidence::FuzzyName)
                .map_err(|error| EvidenceError::Query {
                    operation: "trace command handler",
                    side: self.side,
                    reason: error.to_string(),
                })?;
            if trace.root.is_some() {
                return Ok(Some(candidate));
            }
        }
        Ok(None)
    }

    /// Whether the declaration of `address` is public in its own language.
    ///
    /// Read from the symbol's own source span: Rust and JavaScript/TypeScript
    /// declare visibility in the declaration, and the languages that do not use
    /// a leading underscore by convention.
    fn declaration_is_public(&mut self, address: &SymbolAddress) -> Result<bool, EvidenceError> {
        let selector = Selector::Symbol {
            path: address.path.clone(),
            symbol: address.name.clone(),
            kind: address.kind.clone(),
        };
        let view = self
            .snapshot()
            .graph()
            .show(&selector, DEFAULT_SHOW_MAX_BYTES)
            .map_err(|error| EvidenceError::Query {
                operation: "read entry-point declaration",
                side: self.side,
                reason: error.to_string(),
            })?;
        let Some(view) = view else {
            return Ok(false);
        };
        let text = String::from_utf8_lossy(view.bytes.as_slice()).into_owned();
        let declaration = text
            .lines()
            .find(|line| {
                let trimmed = line.trim_start();
                !trimmed.is_empty()
                    && !trimmed.starts_with("//")
                    && !trimmed.starts_with('#')
                    && !trimmed.starts_with('@')
            })
            .unwrap_or_default()
            .trim_start()
            .to_string();
        Ok(match language_of(address.path.as_str()) {
            Some("rust") => declaration.starts_with("pub"),
            Some("javascript") | Some("typescript") => declaration.starts_with("export"),
            _ => !address.name.starts_with('_'),
        })
    }

    fn qualified_name_of(&self, address: &SymbolAddress) -> Option<String> {
        self.symbols_by_file
            .get(address.path.as_str())?
            .iter()
            .find(|symbol| symbol.name == address.name && symbol.kind == address.kind)
            .map(|symbol| symbol.qualified.clone())
    }

    /// Candidate tests for `selector` from the three disclosed sources.
    ///
    /// Each source is computed independently: a test reached by a naming
    /// heuristic is emitted as a `naming_heuristic` candidate whether or not it
    /// also appears under `call_path`, so a heuristic candidate is never
    /// promoted.
    pub fn candidate_tests(
        &mut self,
        selector: &str,
        query: &EvidenceQuery<'_>,
        unsupported_scope: Vec<OutOfScopeEntry>,
    ) -> Result<CandidateTests, EvidenceError> {
        let parsed = parse_selector(selector)?;
        let commit_sha = self.commit_sha();
        let side_label = self.side_label();
        let (symbol_path, symbol_name) = match &parsed {
            Selector::Symbol { path, symbol, .. } => (path.clone(), symbol.clone()),
            other => (other.path().to_string(), String::new()),
        };

        let mut candidates = Vec::new();
        let call_paths = self.call_path_candidates(selector, query)?;
        let truncated = call_paths.1;
        candidates.extend(call_paths.0);
        candidates.extend(self.import_candidates(
            selector,
            symbol_path.as_str(),
            symbol_name.as_str(),
        )?);
        candidates.extend(self.naming_candidates(
            selector,
            symbol_path.as_str(),
            symbol_name.as_str(),
        ));

        candidates.sort_by(|left, right| {
            left.source
                .cmp(&right.source)
                .then(left.test.selector.cmp(&right.test.selector))
        });
        candidates.dedup_by(|left, right| {
            left.source == right.source && left.test.selector == right.test.selector
        });

        Ok(CandidateTests {
            schema_version: EVIDENCE_SCHEMA_VERSION,
            target: EndpointRef::symbol(
                selector,
                side_label.as_str(),
                endpoint_label(&parsed).as_str(),
            ),
            commit_sha,
            query_options: QueryOptions::new(query),
            candidates,
            unsupported_scope,
            truncated,
        })
    }

    /// Source 1: a static call path from a test-classified file.
    ///
    /// Restricted to chains whose every edge is a call: an import or type
    /// reference from a test file is a weaker claim and belongs to the import
    /// source below, so promoting one here would overstate it. A multi-hop
    /// chain qualifies — the test still reaches the symbol by calls — and the
    /// note names the distance so no reader mistakes it for a direct call.
    fn call_path_candidates(
        &mut self,
        selector: &str,
        query: &EvidenceQuery<'_>,
    ) -> Result<(Vec<CandidateTest>, bool), EvidenceError> {
        let report = self.evidence(selector, query)?;
        let mut candidates = Vec::new();
        for path in &report.paths {
            let Some(edge) = path.edges.first() else {
                continue;
            };
            if !is_test_path(edge.source.file.as_str())
                || path
                    .edges
                    .iter()
                    .any(|edge| edge.relationship != kind_label(RefKind::Call))
            {
                continue;
            }
            candidates.push(CandidateTest {
                test: path.from.clone(),
                source: CandidateSource::CallPath,
                label: CandidateSource::CallPath.corpus_label().to_string(),
                category: path.category,
                path_id: Some(path.path_id.clone()),
                changed_symbols: vec![selector.to_string()],
                truncated: report.truncated,
                note: Some(format!(
                    "{} reference at {}:{}; {} hop(s) to the changed symbol",
                    edge.relationship,
                    edge.source.file,
                    edge.source
                        .line
                        .map(|line| line.to_string())
                        .unwrap_or_else(|| "?".to_string()),
                    path.distance,
                )),
            });
        }
        Ok((candidates, report.truncated))
    }

    /// Source 2: an import edge from a test file to the symbol's module.
    fn import_candidates(
        &mut self,
        selector: &str,
        symbol_path: &str,
        symbol_name: &str,
    ) -> Result<Vec<CandidateTest>, EvidenceError> {
        let module_keys = module_keys(symbol_path);
        let side_label = self.side_label();
        let test_files: Vec<String> = self
            .indexed_files
            .iter()
            .filter(|path| is_test_path(path.as_str()) && path.as_str() != symbol_path)
            .cloned()
            .collect();

        let mut candidates = Vec::new();
        for file in test_files {
            let file_selector = Selector::File { path: file.clone() };
            let deps = self
                .snapshot()
                .graph()
                .deps(&file_selector)
                .map_err(|error| EvidenceError::Query {
                    operation: "query test-file imports",
                    side: self.side,
                    reason: error.to_string(),
                })?;
            let matched = deps.imports.iter().find(|edge| {
                edge.target_symbol.as_deref() == Some(symbol_name)
                    || module_keys.contains(&import_key(edge.target_path.as_str()))
            });
            let Some(matched) = matched else {
                continue;
            };
            candidates.push(CandidateTest {
                test: EndpointRef {
                    selector: format!("file:{file}"),
                    snapshot: side_label.clone(),
                    label: file.clone(),
                    origin: origin_label(ImpactOrigin::File).to_string(),
                },
                source: CandidateSource::ImportRelationship,
                label: CandidateSource::ImportRelationship
                    .corpus_label()
                    .to_string(),
                category: EvidenceCategory::ImportRelationship,
                path_id: None,
                changed_symbols: vec![selector.to_string()],
                truncated: false,
                note: Some(format!(
                    "imports `{}`; an import is a file-level relationship, not a call",
                    matched.target_path
                )),
            });
        }
        Ok(candidates)
    }

    /// Source 3: a naming or file-location signal only.
    ///
    /// Never upgraded by a call or import edge found elsewhere: the doc forbids
    /// promoting a heuristic candidate.
    fn naming_candidates(
        &self,
        selector: &str,
        symbol_path: &str,
        symbol_name: &str,
    ) -> Vec<CandidateTest> {
        let side_label = self.side_label();
        let target_stem = file_stem(symbol_path);
        let mut candidates = Vec::new();

        for file in &self.indexed_files {
            if file.as_str() == symbol_path || !is_test_path(file.as_str()) {
                continue;
            }
            let stem = normalize_test_name(file_stem(file.as_str()).as_str());
            if !target_stem.is_empty() && stem == target_stem {
                candidates.push(CandidateTest {
                    test: EndpointRef {
                        selector: format!("file:{file}"),
                        snapshot: side_label.clone(),
                        label: file.clone(),
                        origin: origin_label(ImpactOrigin::File).to_string(),
                    },
                    source: CandidateSource::NamingHeuristic,
                    label: CandidateSource::NamingHeuristic.corpus_label().to_string(),
                    category: EvidenceCategory::HeuristicMatch,
                    path_id: None,
                    changed_symbols: vec![selector.to_string()],
                    truncated: false,
                    note: Some(format!("file name matches {symbol_path}")),
                });
            }

            if symbol_name.is_empty() {
                continue;
            }
            let Some(symbols) = self.symbols_by_file.get(file.as_str()) else {
                continue;
            };
            for symbol in symbols {
                let (name, kind) = (symbol.name.clone(), symbol.kind.clone());
                let normalized = normalize_test_name(name.as_str());
                let matches = normalized == symbol_name
                    || normalized.starts_with(format!("{symbol_name}_").as_str());
                if !matches {
                    continue;
                }
                candidates.push(CandidateTest {
                    test: EndpointRef::symbol(
                        format!("symbol:{file}#{name}:{kind}").as_str(),
                        side_label.as_str(),
                        format!("{file}#{name}").as_str(),
                    ),
                    source: CandidateSource::NamingHeuristic,
                    label: CandidateSource::NamingHeuristic.corpus_label().to_string(),
                    category: EvidenceCategory::HeuristicMatch,
                    path_id: None,
                    changed_symbols: vec![selector.to_string()],
                    truncated: false,
                    note: Some(format!(
                        "test name `{name}` resembles `{symbol_name}`; name similarity only, no \
                         call or import edge is asserted"
                    )),
                });
            }
        }
        candidates
    }

    fn no_path_reasons(
        &self,
        resolved: bool,
        skipped_low_confidence: usize,
        min_confidence: RefConfidence,
        bounds_hit: &[BoundHit],
    ) -> Vec<String> {
        let mut reasons = Vec::new();
        if !resolved {
            reasons.push(format!(
                "The selector did not resolve to an indexed symbol in the {} snapshot, so no \
                 inbound reference could be attributed to it.",
                self.side.label()
            ));
        }
        if skipped_low_confidence > 0 {
            reasons.push(format!(
                "{skipped_low_confidence} candidate reference(s) were excluded by the `{}` \
                 confidence floor.",
                confidence_label(min_confidence)
            ));
        }
        let excluded = self.snapshot().materialization().excluded.len();
        if excluded > 0 {
            reasons.push(format!(
                "{excluded} tree entr(ies) were excluded from this snapshot and contribute no \
                 evidence."
            ));
        }
        for hit in bounds_hit {
            reasons.push(format!(
                "The traversal stopped at the `{}` bound ({}), so this result is not a complete \
                 set of inbound paths.",
                hit.bound, hit.value
            ));
        }
        reasons.extend(STANDING_NO_PATH_REASONS.iter().map(|text| text.to_string()));
        reasons
    }

    fn ref_edge(
        &mut self,
        entry: &RefEntry,
        to_selector: &str,
        to_label: &str,
        commit_sha: &str,
        note: Option<&str>,
        origins: &BTreeMap<String, ImpactOrigin>,
    ) -> Result<EvidenceEdge, EvidenceError> {
        let from = self.attribute(entry.file.as_str(), entry.line)?;
        // A site the index could only attribute to its file stays a file
        // endpoint, and the core query's own `ImpactOrigin` labelling wins when
        // it disagrees: it is the authority on what is a symbol node.
        let origin = match from.qualified.as_deref() {
            Some(qualified) => origins
                .get(qualified)
                .copied()
                .unwrap_or(ImpactOrigin::Symbol),
            None => ImpactOrigin::File,
        };
        Ok(EvidenceEdge {
            from: from.label,
            from_selector: from.selector,
            from_origin: origin_label(origin).to_string(),
            to: to_label.to_string(),
            to_selector: to_selector.to_string(),
            relationship: kind_label(entry.kind).to_string(),
            category: categorize(entry.kind, entry.confidence),
            confidence: confidence_label(entry.confidence).to_string(),
            snapshot: self.side_label(),
            commit_sha: commit_sha.to_string(),
            source: EvidenceSource {
                file: entry.file.clone(),
                line: Some(entry.line),
            },
            note: note.map(str::to_string),
        })
    }

    /// Name the indexed symbol whose span contains `line` in `file`.
    ///
    /// Falls back to the bare file path when no indexed symbol span contains
    /// the line: a call site outside every symbol span is real evidence, and
    /// the design document requires `source.file` to be present even when a
    /// containing symbol is not.
    fn attribute(&mut self, file: &str, line: usize) -> Result<Attribution, EvidenceError> {
        self.ensure_file_index(file)?;
        let unattributed = Attribution {
            label: file.to_string(),
            selector: format!("file:{file}"),
            qualified: None,
        };
        let Some(index) = self.file_cache.get(file) else {
            return Ok(unattributed);
        };
        let Some(offset) = index.line_starts.get(line.saturating_sub(1)).copied() else {
            return Ok(unattributed);
        };
        let containing = index
            .symbols
            .iter()
            .filter(|symbol| symbol.start <= offset && offset < symbol.end)
            .min_by_key(|symbol| symbol.end.saturating_sub(symbol.start));
        Ok(match containing {
            Some(symbol) => Attribution {
                label: format!("{file}#{}", symbol.name),
                selector: format!("symbol:{file}#{}:{}", symbol.name, symbol.kind),
                qualified: Some(symbol.qualified.clone()),
            },
            None => unattributed,
        })
    }

    fn ensure_file_index(&mut self, file: &str) -> Result<(), EvidenceError> {
        if self.file_cache.contains_key(file) {
            return Ok(());
        }
        let mut index = FileIndex::default();

        let file_selector = Selector::File {
            path: file.to_string(),
        };
        let view = self
            .snapshot()
            .graph()
            .show(&file_selector, DEFAULT_SHOW_MAX_BYTES)
            .map_err(|error| EvidenceError::Query {
                operation: "read referencing file",
                side: self.side,
                reason: error.to_string(),
            })?;
        if let Some(view) = view {
            index.line_starts = line_starts(view.bytes.as_slice());
        }

        let symbols = self.symbols_by_file.get(file).cloned().unwrap_or_default();
        for symbol in symbols {
            let (name, kind, qualified) = (symbol.name, symbol.kind, symbol.qualified);
            let selector = Selector::Symbol {
                path: file.to_string(),
                symbol: name.clone(),
                kind: kind.clone(),
            };
            let view = self
                .snapshot()
                .graph()
                .show(&selector, DEFAULT_SHOW_MAX_BYTES)
                .map_err(|error| EvidenceError::Query {
                    operation: "read referencing symbol span",
                    side: self.side,
                    reason: error.to_string(),
                })?;
            let Some(view) = view else {
                continue;
            };
            index.symbols.push(SymbolSpan {
                name,
                kind,
                qualified,
                start: view.metadata.span.start,
                end: view.metadata.span.end,
            });
        }

        self.file_cache.insert(file.to_string(), index);
        Ok(())
    }
}

fn relation_edge(
    relation: &RelationEntry,
    to_selector: &str,
    to_label: &str,
    side_label: &str,
    commit_sha: &str,
    origins: &BTreeMap<String, ImpactOrigin>,
) -> EvidenceEdge {
    let origin = origins
        .get(relation.from.as_str())
        .copied()
        .unwrap_or(ImpactOrigin::Symbol);
    EvidenceEdge {
        from: relation.from.clone(),
        // A structural relation names its source by qualified symbol, not by a
        // file anchor, so no canonical selector can be formed from it alone.
        from_selector: format!("module:{}", relation.from),
        from_origin: origin_label(origin).to_string(),
        to: to_label.to_string(),
        to_selector: to_selector.to_string(),
        relationship: kind_label(relation.kind).to_string(),
        category: categorize(relation.kind, relation.confidence),
        confidence: confidence_label(relation.confidence).to_string(),
        snapshot: side_label.to_string(),
        commit_sha: commit_sha.to_string(),
        source: EvidenceSource {
            file: relation.file.clone(),
            line: Some(relation.line),
        },
        note: None,
    }
}

/// One node waiting to be expanded, with the chain that reached it.
struct Frontier {
    selector: String,
    label: String,
    /// Chain from this node back to the queried symbol, outermost hop first.
    path: Vec<EvidenceEdge>,
}

/// The inbound edges of one node, plus what the query said about the node.
struct InboundEdges {
    edges: Vec<EvidenceEdge>,
    resolved: bool,
    qualified: Option<String>,
    skipped_low_confidence: usize,
}

/// A symbol selector split into the parts the entry-point rules read.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SymbolAddress {
    path: String,
    name: String,
    kind: String,
}

/// Split `symbol:<path>#<name>:<kind>` into its parts.
///
/// Any other selector form returns `None`: the entry-point rules are about
/// symbols, and a file-attributed endpoint is not one.
fn symbol_address(selector: &str) -> Option<SymbolAddress> {
    match selector.parse::<Selector>().ok()? {
        Selector::Symbol { path, symbol, kind } => Some(SymbolAddress {
            path,
            name: symbol,
            kind,
        }),
        _ => None,
    }
}

/// The changed-symbol status of `selector`, when the slice carries it.
fn change_status_of(changes: Option<&ChangedSymbols>, selector: &str) -> Option<String> {
    changes?
        .entries_for(selector)
        .first()
        .map(|entry| entry.status.label().to_string())
}

/// Whether a symbol kind can be an invocable entry point.
fn is_callable_kind(kind: &str) -> bool {
    matches!(kind, "function" | "method" | "fn" | "func" | "procedure")
}

/// Whether a symbol kind is an item a package can expose.
fn is_public_item_kind(kind: &str) -> bool {
    is_callable_kind(kind)
        || matches!(
            kind,
            "class" | "struct" | "enum" | "trait" | "interface" | "type" | "module"
        )
}

/// Whether `path` is a crate root or package initializer, and which.
fn root_module_kind(path: &str) -> Option<&'static str> {
    let file = path.rsplit('/').next().unwrap_or(path);
    match file {
        "lib.rs" | "main.rs" => Some("crate root"),
        "__init__.py" => Some("package initializer"),
        "index.js" | "index.jsx" | "index.ts" | "index.tsx" | "index.mjs" => {
            Some("package entry module")
        }
        _ => None,
    }
}

/// Command names worth probing for the handler at `address`.
///
/// Formed from the symbol's own name and the module path of its file, in the
/// spellings the command extractors produce: a bare name, a hyphenated name,
/// and a module-qualified name. Every candidate is verified against the index
/// before it counts, so a wrong guess costs one point query and nothing else.
fn command_candidates(address: &SymbolAddress) -> Vec<String> {
    let name = address.name.as_str();
    let spellings = [
        name.to_string(),
        name.replace('_', "-"),
        name.replace('_', " "),
    ];

    let mut segments: Vec<String> = Vec::new();
    for segment in address.path.split('/') {
        let stem = segment.split('.').next().unwrap_or(segment);
        if stem.is_empty()
            || matches!(
                stem,
                "src" | "lib" | "main" | "mod" | "tests" | "test" | "app" | "cmd" | "crates"
            )
        {
            continue;
        }
        segments.push(stem.replace('_', "-"));
        if segments.len() >= 3 {
            break;
        }
    }

    let mut candidates: Vec<String> = Vec::new();
    for spelling in &spellings {
        if !spelling.is_empty() {
            candidates.push(spelling.clone());
        }
    }
    for segment in &segments {
        for spelling in &spellings {
            candidates.push(format!("{segment} {spelling}"));
            candidates.push(format!("{segment}-{spelling}"));
        }
    }
    if segments.len() > 1 {
        let prefix = segments.join(" ");
        for spelling in &spellings {
            candidates.push(format!("{prefix} {spelling}"));
        }
    }
    candidates.sort();
    candidates.dedup();
    candidates
}

fn parse_selector(selector: &str) -> Result<Selector, EvidenceError> {
    selector
        .parse::<Selector>()
        .map_err(|error| EvidenceError::Selector(error.to_string()))
}

fn endpoint_label(selector: &Selector) -> String {
    match selector {
        Selector::Symbol { path, symbol, .. } => format!("{path}#{symbol}"),
        other => other.to_string(),
    }
}

/// A referencing site, named for display and addressed by selector.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Attribution {
    label: String,
    selector: String,
    /// Qualified name of the containing symbol, when a symbol contains the
    /// site. `None` is a file-attributed site.
    qualified: Option<String>,
}

fn line_starts(bytes: &[u8]) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (offset, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            starts.push(offset + 1);
        }
    }
    starts
}

/// Whether a path looks like test code.
///
/// Deliberately language-agnostic and file-shaped, matching the conventions the
/// fixture corpus exercises: a `test`/`tests`/`spec`/`specs` path segment, or a
/// file stem that a `test`/`spec` affix marks. This is a heuristic, and every
/// candidate it produces is labelled as one.
pub fn is_test_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let mut segments = lower.split('/').collect::<Vec<_>>();
    let Some(file) = segments.pop() else {
        return false;
    };
    if segments
        .iter()
        .any(|segment| matches!(*segment, "test" | "tests" | "spec" | "specs" | "__tests__"))
    {
        return true;
    }
    let stem = file.split('.').next().unwrap_or(file);
    stem.starts_with("test_")
        || stem.starts_with("test-")
        || stem == "test"
        || stem.ends_with("_test")
        || stem.ends_with("-test")
        || stem.ends_with("_tests")
        || stem.ends_with("test")
        || stem.ends_with("_spec")
        || stem.ends_with("-spec")
        || stem.ends_with("spec")
}

/// Strip the conventional `test`/`spec` affixes from a name.
fn normalize_test_name(name: &str) -> String {
    let mut current = name;
    for prefix in ["test_", "test-", "Test", "spec_", "spec-"] {
        if let Some(rest) = current.strip_prefix(prefix) {
            current = rest;
            break;
        }
    }
    for suffix in [
        "_test", "-test", "_tests", "-tests", "Test", "Tests", "_spec", "-spec", "Spec",
    ] {
        if let Some(rest) = current.strip_suffix(suffix) {
            current = rest;
            break;
        }
    }
    current.to_string()
}

/// File stem of a path, without directories or extension.
fn file_stem(path: &str) -> String {
    let file = path.rsplit('/').next().unwrap_or(path);
    file.split('.').next().unwrap_or(file).to_string()
}

/// Last segment of an import specifier, however the language spells it.
fn import_key(target_path: &str) -> String {
    target_path
        .replace("::", "/")
        .replace('.', "/")
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or(target_path)
        .to_string()
}

/// Keys an import specifier may plausibly use to name `path`.
fn module_keys(path: &str) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    if path.is_empty() {
        return keys;
    }
    let stem = file_stem(path);
    if !stem.is_empty() {
        keys.insert(stem);
    }
    if let Some((directory, _)) = path.rsplit_once('/')
        && let Some(parent) = directory.rsplit('/').next()
        && !parent.is_empty()
    {
        keys.insert(parent.to_string());
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_name_is_always_a_heuristic_match() {
        assert_eq!(
            categorize(RefKind::Call, RefConfidence::FuzzyName),
            EvidenceCategory::HeuristicMatch
        );
        assert_eq!(
            categorize(RefKind::Use, RefConfidence::FuzzyName),
            EvidenceCategory::HeuristicMatch
        );
    }

    #[test]
    fn resolved_calls_require_exact_or_import_resolution() {
        assert_eq!(
            categorize(RefKind::Call, RefConfidence::Exact),
            EvidenceCategory::ResolvedCall
        );
        assert_eq!(
            categorize(RefKind::Call, RefConfidence::ImportResolved),
            EvidenceCategory::ResolvedCall
        );
        assert_eq!(
            categorize(RefKind::Call, RefConfidence::SameModule),
            EvidenceCategory::ObservedReference
        );
    }

    #[test]
    fn imports_and_structural_relations_have_their_own_categories() {
        assert_eq!(
            categorize(RefKind::Use, RefConfidence::SameModule),
            EvidenceCategory::ImportRelationship
        );
        assert_eq!(
            categorize(RefKind::Impl, RefConfidence::Exact),
            EvidenceCategory::ObservedReference
        );
    }

    #[test]
    fn category_labels_match_the_contract() {
        assert_eq!(
            EvidenceCategory::ObservedReference.label(),
            "observed_reference"
        );
        assert_eq!(EvidenceCategory::ResolvedCall.label(), "resolved_call");
        assert_eq!(
            EvidenceCategory::ImportRelationship.label(),
            "import_relationship"
        );
        assert_eq!(EvidenceCategory::HeuristicMatch.label(), "heuristic_match");
    }

    #[test]
    fn candidate_sources_carry_both_spellings() {
        assert_eq!(CandidateSource::CallPath.label(), "call_path");
        assert_eq!(CandidateSource::CallPath.corpus_label(), "call-path");
        assert_eq!(
            CandidateSource::ImportRelationship.label(),
            "import_relationship"
        );
        assert_eq!(CandidateSource::ImportRelationship.corpus_label(), "import");
        assert_eq!(CandidateSource::NamingHeuristic.label(), "naming_heuristic");
        assert_eq!(
            CandidateSource::NamingHeuristic.corpus_label(),
            "naming-heuristic"
        );
    }

    #[test]
    fn confidence_labels_round_trip() {
        for confidence in [
            RefConfidence::Exact,
            RefConfidence::ImportResolved,
            RefConfidence::SameModule,
            RefConfidence::FuzzyName,
        ] {
            assert_eq!(
                parse_confidence(confidence_label(confidence)),
                Some(confidence)
            );
        }
        assert_eq!(
            parse_confidence("import"),
            Some(RefConfidence::ImportResolved)
        );
        assert_eq!(parse_confidence("fuzzy"), Some(RefConfidence::FuzzyName));
        assert_eq!(parse_confidence("nonsense"), None);
    }

    #[test]
    fn test_paths_are_recognized_across_conventions() {
        assert!(is_test_path("tests/test_lib.rs"));
        assert!(is_test_path("tests/add_test.rs"));
        assert!(is_test_path("test_process.py"));
        assert!(is_test_path("src/__tests__/widget.ts"));
        assert!(is_test_path("spec/widget_spec.rb"));
        assert!(!is_test_path("src/lib.rs"));
        assert!(!is_test_path("mod.py"));
        assert!(!is_test_path("formatting/formatter.py"));
    }

    #[test]
    fn test_affixes_are_stripped_once() {
        assert_eq!(normalize_test_name("test_add"), "add");
        assert_eq!(normalize_test_name("add_test"), "add");
        assert_eq!(
            normalize_test_name("test_process_dynamic"),
            "process_dynamic"
        );
        assert_eq!(normalize_test_name("lib"), "lib");
    }

    #[test]
    fn import_keys_use_the_last_specifier_segment() {
        assert_eq!(import_key("mod"), "mod");
        assert_eq!(import_key("formatting.formatter"), "formatter");
        assert_eq!(import_key("orbit_core::scheduler"), "scheduler");
        assert_eq!(import_key("./utils/foo"), "foo");
    }

    #[test]
    fn module_keys_cover_stem_and_parent_directory() {
        let keys = module_keys("formatting/formatter.py");
        assert!(keys.contains("formatter"), "{keys:?}");
        assert!(keys.contains("formatting"), "{keys:?}");
        assert!(module_keys("").is_empty());
    }

    #[test]
    fn line_starts_index_every_line() {
        assert_eq!(line_starts(b"a\nbb\nc"), vec![0, 2, 5]);
        assert_eq!(line_starts(b""), vec![0]);
    }
}
