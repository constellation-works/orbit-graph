//! Relationship evidence and candidate tests for one changed symbol.
//!
//! Two products, both scoped to exactly one snapshot:
//!
//! - [`EvidenceReport`] — the direct (depth 1) inbound references and
//!   structural relations of a symbol, each mapped to one of the design
//!   document's evidence categories and carrying its confidence, reference
//!   kind, snapshot SHA, and source location. A `fallback` block returned by
//!   `refs` renders as [`EvidenceCategory::HeuristicMatch`] with its note
//!   attached, never merged into the primary result.
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
//! Depth is fixed at [`EVIDENCE_DEPTH`] in this milestone. Multi-hop traversal
//! waits for a direction option on the core `impact` query (gap G4 in the
//! design document): `impact` is today an undirected neighbourhood that mixes
//! the origin's own callees in with its callers, which cannot be rendered as
//! caller evidence without misstating it.

use std::collections::{BTreeMap, BTreeSet};

use orbit_graph::{
    Confidence, DEFAULT_SHOW_MAX_BYTES, OverviewFormat, RefConfidence, RefEntry, RefKind, RefOpts,
    RelationEntry, Selector,
};
use serde::Serialize;
use thiserror::Error;

use crate::changes::OutOfScopeEntry;
use crate::snapshot::{Comparison, Snapshot, SnapshotSide};

/// Schema version of the evidence and candidate-test payloads.
pub const EVIDENCE_SCHEMA_VERSION: u32 = 1;

/// Traversal depth this milestone supports: direct inbound references only.
pub const EVIDENCE_DEPTH: u8 = 1;

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

/// One edge of an evidence path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidenceEdge {
    /// Referencing endpoint, as `<path>#<symbol>` when the reference lies
    /// inside an indexed symbol span and as `<path>` when it does not.
    pub from: String,
    /// Canonical selector for the referencing endpoint, copyable as text.
    pub from_selector: String,
    /// Referenced endpoint, as `<path>#<symbol>`.
    pub to: String,
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
}

/// A depth-1 path from a referencing site to the queried symbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidencePath {
    /// Payload schema version.
    pub schema_version: u32,
    /// Stable identifier of this path within its report.
    pub path_id: String,
    /// Referencing endpoint.
    pub from: EndpointRef,
    /// Queried symbol.
    pub to: EndpointRef,
    /// Whether this path was cut by a bound.
    pub truncated: bool,
    /// Which bound cut it, when `truncated`.
    pub truncated_by: Option<String>,
    /// Edges of the path: exactly one at this depth.
    pub edges: Vec<EvidenceEdge>,
}

/// Bounds in force for one evidence query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QueryOptions {
    /// Traversal depth. Always [`EVIDENCE_DEPTH`] in this milestone.
    pub depth: u8,
    /// Confidence floor applied to the query.
    pub min_confidence: String,
    /// Reference-kind filter, when one was applied.
    pub kind: Option<String>,
    /// Byte bound applied to any source excerpt read while answering.
    pub source_max_bytes: usize,
}

impl QueryOptions {
    fn new(min_confidence: RefConfidence) -> Self {
        Self {
            depth: EVIDENCE_DEPTH,
            min_confidence: confidence_label(min_confidence).to_string(),
            kind: None,
            source_max_bytes: DEFAULT_SHOW_MAX_BYTES,
        }
    }
}

/// Depth-1 inbound evidence for one symbol in one snapshot.
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
    /// Bounds in force.
    pub query_options: QueryOptions,
    /// One path per inbound edge, strongest category first.
    pub paths: Vec<EvidencePath>,
    /// Candidate rows the confidence floor excluded.
    pub skipped_low_confidence: usize,
    /// Whether any bound cut this result.
    pub truncated: bool,
    /// Which bound cut it, when `truncated`.
    pub truncated_by: Option<String>,
    /// Why a path could be missing. Always populated when `paths` is empty.
    pub no_path_reasons: Vec<String>,
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
    symbols_by_file: BTreeMap<String, Vec<(String, String)>>,
    file_cache: BTreeMap<String, FileIndex>,
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
            let symbols = file
                .symbols
                .into_iter()
                .map(|symbol| (symbol.name, symbol.kind))
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

    /// Depth-1 inbound evidence for `selector` at `min_confidence`.
    pub fn evidence(
        &mut self,
        selector: &str,
        min_confidence: Confidence,
    ) -> Result<EvidenceReport, EvidenceError> {
        let parsed = parse_selector(selector)?;
        let commit_sha = self.commit_sha();
        let side_label = self.side_label();
        let to_label = endpoint_label(&parsed);

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
            edges.push(self.ref_edge(&entry, to_label.as_str(), commit_sha.as_str(), None)?);
        }
        for relation in &result.relations {
            edges.push(relation_edge(
                relation,
                to_label.as_str(),
                side_label.as_str(),
                commit_sha.as_str(),
            ));
        }
        if let Some(fallback) = result.fallback.clone() {
            for entry in fallback.refs {
                // A fallback block is name-only: it renders as a heuristic
                // match with its note attached, never merged into the primary
                // result.
                let mut edge = self.ref_edge(
                    &entry,
                    to_label.as_str(),
                    commit_sha.as_str(),
                    Some(fallback.note.as_str()),
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

        let resolved = result.target.qualified.is_some();
        let mut paths = Vec::with_capacity(edges.len());
        for (index, edge) in edges.into_iter().enumerate() {
            paths.push(EvidencePath {
                schema_version: EVIDENCE_SCHEMA_VERSION,
                path_id: format!("p-{}", index + 1),
                from: EndpointRef {
                    selector: edge.from_selector.clone(),
                    snapshot: side_label.clone(),
                },
                to: EndpointRef {
                    selector: selector.to_string(),
                    snapshot: side_label.clone(),
                },
                truncated: false,
                truncated_by: None,
                edges: vec![edge],
            });
        }

        let no_path_reasons = if paths.is_empty() {
            self.no_path_reasons(resolved, result.skipped_low_confidence, min_confidence)
        } else {
            Vec::new()
        };

        Ok(EvidenceReport {
            schema_version: EVIDENCE_SCHEMA_VERSION,
            target: EndpointRef {
                selector: selector.to_string(),
                snapshot: side_label,
            },
            commit_sha,
            resolved,
            resolved_qualified: result.target.qualified,
            query_options: QueryOptions::new(min_confidence),
            paths,
            skipped_low_confidence: result.skipped_low_confidence,
            truncated: false,
            truncated_by: None,
            no_path_reasons,
        })
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
        min_confidence: Confidence,
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
        candidates.extend(self.call_path_candidates(selector, min_confidence)?);
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
            target: EndpointRef {
                selector: selector.to_string(),
                snapshot: side_label,
            },
            commit_sha,
            query_options: QueryOptions::new(min_confidence),
            candidates,
            unsupported_scope,
            truncated: false,
        })
    }

    /// Source 1: a static call from a test-classified file.
    ///
    /// Restricted to call edges. An import or type reference from a test file
    /// is a weaker claim and belongs to the import source below, so promoting
    /// one here would overstate it.
    fn call_path_candidates(
        &mut self,
        selector: &str,
        min_confidence: Confidence,
    ) -> Result<Vec<CandidateTest>, EvidenceError> {
        let report = self.evidence(selector, min_confidence)?;
        let side_label = self.side_label();
        let mut candidates = Vec::new();
        for path in &report.paths {
            let Some(edge) = path.edges.first() else {
                continue;
            };
            if !is_test_path(edge.source.file.as_str())
                || edge.relationship != kind_label(RefKind::Call)
            {
                continue;
            }
            candidates.push(CandidateTest {
                test: EndpointRef {
                    selector: edge.from_selector.clone(),
                    snapshot: side_label.clone(),
                },
                source: CandidateSource::CallPath,
                label: CandidateSource::CallPath.corpus_label().to_string(),
                category: edge.category,
                path_id: Some(path.path_id.clone()),
                changed_symbols: vec![selector.to_string()],
                truncated: false,
                note: Some(format!(
                    "{} reference at {}:{}",
                    edge.relationship,
                    edge.source.file,
                    edge.source
                        .line
                        .map(|line| line.to_string())
                        .unwrap_or_else(|| "?".to_string())
                )),
            });
        }
        Ok(candidates)
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
            for (name, kind) in symbols {
                let normalized = normalize_test_name(name.as_str());
                let matches = normalized == symbol_name
                    || normalized.starts_with(format!("{symbol_name}_").as_str());
                if !matches {
                    continue;
                }
                candidates.push(CandidateTest {
                    test: EndpointRef {
                        selector: format!("symbol:{file}#{name}:{kind}"),
                        snapshot: side_label.clone(),
                    },
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
        reasons.extend(STANDING_NO_PATH_REASONS.iter().map(|text| text.to_string()));
        reasons
    }

    fn ref_edge(
        &mut self,
        entry: &RefEntry,
        to_label: &str,
        commit_sha: &str,
        note: Option<&str>,
    ) -> Result<EvidenceEdge, EvidenceError> {
        let from = self.attribute(entry.file.as_str(), entry.line)?;
        Ok(EvidenceEdge {
            from: from.label,
            from_selector: from.selector,
            to: to_label.to_string(),
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
        for (name, kind) in symbols {
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
    to_label: &str,
    side_label: &str,
    commit_sha: &str,
) -> EvidenceEdge {
    EvidenceEdge {
        from: relation.from.clone(),
        // A structural relation names its source by qualified symbol, not by a
        // file anchor, so no canonical selector can be formed from it alone.
        from_selector: format!("module:{}", relation.from),
        to: to_label.to_string(),
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
