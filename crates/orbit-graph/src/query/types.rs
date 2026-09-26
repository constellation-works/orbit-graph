use std::collections::BTreeMap;

use rusqlite::{Connection, params};
use serde::Serialize;

use crate::{GraphError, Selector};

/// Internal result of resolving a Selector to a symbol's file and span for
/// span-containment queries like callees.
#[derive(Debug, Clone)]
pub(crate) struct SymbolSpan {
    pub(crate) file_path: String,
    pub(crate) span_start: i64,
    pub(crate) span_end: i64,
}

/// Resolve a Selector to a single symbol's (file_path, span) if it exists in
/// the graph. Returns None for selectors that do not map to a stored symbol
/// (including non-Symbol variants and unknown names). Used by read queries
/// that then perform containment or adjacency lookups.
pub(crate) fn resolve_symbol_span(
    conn: &Connection,
    sel: &Selector,
) -> Result<Option<SymbolSpan>, GraphError> {
    let Selector::Symbol { path, symbol, kind } = sel else {
        return Ok(None);
    };

    // Match on either short name or qualified; apply kind filter when provided.
    // Paths in DB are normalized (slash-separated, relative to worktree).
    // An exact qualified match wins over a short-name match, so a printed
    // selector such as `#helper` names the top-level `helper`, not a nested
    // `inner::helper` that happens to have a lower id (STD-01 §R32).
    let mut sql = String::from(
        "SELECT file_path, span_start, span_end FROM symbols
         WHERE file_path = ?1 AND (name = ?2 OR qualified = ?2)",
    );
    let has_kind = !kind.trim().is_empty();
    if has_kind {
        sql.push_str(" AND kind = ?3");
    }
    sql.push_str(" ORDER BY CASE WHEN qualified = ?2 THEN 0 ELSE 1 END, id LIMIT 1");

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|source| GraphError::sqlite("prepare symbol resolve for query", source))?;

    let row = if has_kind {
        stmt.query_row(params![path, symbol, kind.trim()], |r| {
            Ok(SymbolSpan {
                file_path: r.get(0)?,
                span_start: r.get(1)?,
                span_end: r.get(2)?,
            })
        })
    } else {
        stmt.query_row(params![path, symbol], |r| {
            Ok(SymbolSpan {
                file_path: r.get(0)?,
                span_start: r.get(1)?,
                span_end: r.get(2)?,
            })
        })
    };

    match row {
        Ok(s) => Ok(Some(s)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(source) => Err(GraphError::sqlite("resolve symbol for query", source)),
    }
}

/// Reference query options for [`crate::Graph::refs`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefOpts {
    /// Minimum confidence included in returned results.
    pub confidence: RefConfidence,
    /// Optional kind filter for textual refs or structural relations.
    pub kind: Option<RefKind>,
}

/// Confidence floor and output value for graph references.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RefConfidence {
    /// Same file, unambiguous match on name and qualified path.
    Exact,
    /// Cross-file reference resolved through an explicit import.
    ImportResolved,
    /// Cross-file reference resolved within the same module namespace.
    #[default]
    SameModule,
    /// Name-only match with ambiguous or weak resolution.
    FuzzyName,
}

/// Filterable reference and relation kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RefKind {
    /// Function or method call reference.
    Call,
    /// Type usage reference.
    Type,
    /// Import or use-statement reference.
    Use,
    /// Trait-bound reference.
    TraitBound,
    /// Implementation relation.
    Impl,
    /// Inheritance relation.
    Extends,
    /// Interface implementation relation.
    Implements,
}

/// Reference query result returned by [`crate::Graph::refs`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefResult {
    /// Resolved target symbol, or the unresolved selector target.
    pub target: RefTarget,
    /// Textual references anchored to a source file and span.
    pub refs: Vec<RefEntry>,
    /// Structural relations whose destination is the target symbol.
    pub relations: Vec<RelationEntry>,
    /// Number of candidate rows excluded by the confidence floor.
    pub skipped_low_confidence: usize,
    /// Whether `fallback` is present: `true` means the requested floor found
    /// no textual `refs` and the rows under `fallback.refs` are name-only
    /// matches. Always serialized, so a reader never has to infer it from a
    /// missing key.
    pub fallback_used: bool,
    /// Lower-confidence references surfaced because the precise floor found no
    /// textual `refs`. Present only when the precise result was empty and a
    /// lower-confidence (`fuzzy_name`) match exists — see [`RefFallback`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback: Option<RefFallback>,
}

/// Lower-confidence references surfaced when the precise floor returned no refs.
///
/// Cross-crate call sites routed through `pub use` re-exports resolve only at
/// `fuzzy_name` (name-only) confidence, which the default `same_module` floor
/// excludes. When the precise `refs` list is empty, the query falls back to the
/// fuzzy floor so a genuinely-referenced public API does not look unreferenced.
/// These matches are name-only and may include unrelated symbols sharing the name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefFallback {
    /// Confidence floor used to produce the fallback references (`fuzzy_name`).
    pub confidence: RefConfidence,
    /// Fallback references, each labelled with its own resolution confidence.
    pub refs: Vec<RefEntry>,
    /// Human-readable explanation of why these lower-confidence refs are shown.
    pub note: String,
}

/// Target metadata included in a [`RefResult`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefTarget {
    /// Short symbol name requested or resolved.
    pub name: String,
    /// Fully-qualified symbol name used as the graph query key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qualified: Option<String>,
}

/// Textual reference entry returned by [`crate::Graph::refs`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefEntry {
    /// Source file containing the reference.
    pub file: String,
    /// One-based source line containing the reference.
    pub line: usize,
    /// Textual reference kind.
    pub kind: RefKind,
    /// Resolution confidence for this reference.
    pub confidence: RefConfidence,
    /// `symbol:` selector of the innermost indexed symbol enclosing the
    /// reference (the caller, for a call), or `None` when the reference lies
    /// outside every symbol span, such as a top-level statement.
    pub from_selector: Option<String>,
    /// The trimmed source line containing the reference, cut to at most 160
    /// characters with a trailing `…`.
    pub snippet: String,
}

/// Structural relation entry returned by [`crate::Graph::refs`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RelationEntry {
    /// Qualified source symbol for the relation.
    pub from: String,
    /// Structural relation kind.
    pub kind: RefKind,
    /// Source file defining the relation.
    pub file: String,
    /// One-based source line defining the relation.
    pub line: usize,
    /// Resolution confidence for this relation.
    pub confidence: RefConfidence,
}

/// Outbound call edge returned by [`crate::Graph::callees`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CalleeEdge {
    /// Short target name as written in the call site.
    pub target_name: String,
    /// Resolved qualified name (authoritative when present); None for fuzzy/unresolved.
    pub target_qualified: Option<String>,
    /// Resolution confidence assigned to the call edge.
    pub confidence: RefConfidence,
    /// One-based source line containing the call site.
    pub line: usize,
}

/// Options for [`crate::Graph::callees_with_options`].
///
/// # Examples
///
/// ```
/// use std::fs;
///
/// use orbit_graph::{CalleeOpts, Confidence, Graph, Selector, SyncMode, SyncPolicy};
///
/// let dir = tempfile::tempdir()?;
/// fs::create_dir_all(dir.path().join("src"))?;
/// fs::write(
///     dir.path().join("src/lib.rs"),
///     "pub fn helper() -> i32 { 1 }\npub fn entry() -> i32 { helper() }\n",
/// )?;
///
/// let graph = Graph::open(dir.path(), SyncPolicy::Manual)?;
/// graph.sync(SyncMode::Full)?;
///
/// let selector: Selector = "symbol:src/lib.rs#entry:function".parse()?;
/// let opts = CalleeOpts {
///     confidence: Confidence::Exact,
///     kind: None,
///     hide_unresolved: false,
/// };
/// let callees = graph.callees_with_options(&selector, &opts)?;
/// assert_eq!(callees.len(), 1);
/// assert_eq!(callees[0].target_name, "helper");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CalleeOpts {
    /// Minimum confidence included in returned edges.
    pub confidence: RefConfidence,
    /// Optional edge-kind filter. Since this query returns calls, any non-call
    /// kind produces an empty result.
    pub kind: Option<RefKind>,
    /// Omit unresolved call edges (no `target_qualified`) whose call name has
    /// no indexed callable definition (a `function`, `method`, `class`, or
    /// `struct` of that name) anywhere in the graph, such as standard-library
    /// or prelude calls (`map_err`, `Ok`, `to_string`). They are counted in
    /// [`CalleeReport::hidden_unresolved`] instead.
    pub hide_unresolved: bool,
}

impl CalleeOpts {
    /// Return options that preserve the unfiltered [`crate::Graph::callees`] behavior.
    pub const fn all() -> Self {
        Self {
            confidence: RefConfidence::FuzzyName,
            kind: None,
            hide_unresolved: false,
        }
    }
}

/// Outbound call edges plus what [`CalleeOpts::hide_unresolved`] omitted,
/// returned by [`crate::Graph::callees_report`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CalleeReport {
    /// Returned call edges, in call-site order.
    pub callees: Vec<CalleeEdge>,
    /// Unresolved edges with no indexed definition that were omitted; `0`
    /// unless [`CalleeOpts::hide_unresolved`] is set.
    pub hidden_unresolved: usize,
}

/// Bounded impact result returned by [`crate::Graph::impact`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImpactResult {
    /// Traversal direction used to build this result.
    #[serde(skip_serializing_if = "ImpactDirection::is_both")]
    pub direction: ImpactDirection,
    /// Impacted symbols in breadth-first order from the origin.
    pub touched: Vec<ImpactEntry>,
    /// Whether traversal stopped because [`crate::IMPACT_NODE_CAP`] was reached.
    pub truncated: bool,
    /// Number of impacted symbols returned in `touched`.
    pub visited_nodes: usize,
    /// Whether `fallback` is present: `true` means the requested floor reached
    /// no node and `fallback.touched` holds name-only matches. Always
    /// serialized.
    pub fallback_used: bool,
    /// Lower-confidence impact surfaced because the precise floor found no
    /// touched nodes. Present only when a lower-confidence (`fuzzy_name`) match
    /// exists; see [`ImpactFallback`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback: Option<ImpactFallback>,
}

/// Lower-confidence impact surfaced when the precise floor returned no nodes.
///
/// `fuzzy_name` edges are name-only and may include unrelated symbols sharing
/// the same short name. The top-level [`ImpactResult`] remains the result for
/// the requested confidence floor; this fallback is an explicit hint that a
/// lower-confidence blast radius exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImpactFallback {
    /// Confidence floor used to produce the fallback impact (`fuzzy_name`).
    pub confidence: RefConfidence,
    /// Impacted symbols found at the fallback floor.
    pub touched: Vec<ImpactEntry>,
    /// Whether fallback traversal stopped because [`crate::IMPACT_NODE_CAP`] was reached.
    pub truncated: bool,
    /// Number of impacted symbols returned in `touched`.
    pub visited_nodes: usize,
    /// Human-readable explanation of why these lower-confidence nodes are shown.
    pub note: String,
}

/// A symbol reached by [`crate::Graph::impact`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImpactEntry {
    /// Qualified symbol name reached by the traversal.
    pub qualified_name: String,
    /// Whether `qualified_name` names a symbol or a file-attributed call site.
    #[serde(skip_serializing_if = "ImpactOrigin::is_symbol")]
    pub origin: ImpactOrigin,
    /// Breadth-first distance from the origin symbol.
    pub distance: usize,
    /// Edge kind used for the prior hop into this symbol.
    pub edge_kind: RefKind,
    /// Selector that `show`, `refs`, `callees`, or `impact` accept for this
    /// node: `symbol:<file>#<qualified>:<kind>` for an indexed symbol, or
    /// `file:<file>` for a file-attributed call site. `None` when the name
    /// has no indexed definition, such as a trait from another crate.
    pub selector: Option<String>,
    /// Workspace-relative file of the node, when indexed.
    pub file: Option<String>,
    /// One-based line: the symbol's definition line, or for a file-attributed
    /// node the first call site that reached it.
    pub line: Option<usize>,
}

/// Direction followed by an impact traversal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImpactDirection {
    /// Follow callers and reverse structural dependencies.
    Inbound,
    /// Follow callees and forward structural dependencies.
    Outbound,
    /// Follow both directions, preserving the historical behavior.
    #[default]
    Both,
}

impl ImpactDirection {
    fn is_both(&self) -> bool {
        *self == Self::Both
    }
}

/// Origin represented by an [`ImpactEntry`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImpactOrigin {
    /// An indexed symbol node.
    #[default]
    Symbol,
    /// A call site attributed to its file because it lies outside symbol spans.
    File,
}

impl ImpactOrigin {
    fn is_symbol(&self) -> bool {
        *self == Self::Symbol
    }
}

/// Command trace result returned by [`crate::Graph::trace`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TraceResult {
    /// Root command handler node, or `None` when the command is unknown.
    pub root: Option<TraceNode>,
    /// Whether traversal stopped because [`crate::TRACE_NODE_CAP`] was reached.
    pub truncated: bool,
    /// Number of nodes returned in the trace tree, including the root.
    pub visited_nodes: usize,
}

impl TraceResult {
    pub(crate) fn empty() -> Self {
        Self {
            root: None,
            truncated: false,
            visited_nodes: 0,
        }
    }
}

/// A node in a command-handler call tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TraceNode {
    /// Short name as written at the call site, or the handler symbol name for the root.
    pub name: String,
    /// Resolved qualified symbol name when the call target was resolved.
    pub qualified_name: Option<String>,
    /// Resolver confidence for the edge into this node; `None` for the root.
    pub confidence: Option<String>,
    /// Nested callees reached from this symbol.
    pub children: Vec<TraceNode>,
}

/// Output format for [`crate::Graph::overview`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OverviewFormat {
    /// Aggregate counts plus the highest-symbol files, without per-file symbols.
    Summary,
    /// Aggregate counts plus every in-scope file and its symbols.
    Full,
}

/// Repository shape summary returned by [`crate::Graph::overview`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OverviewResult {
    /// Format used to build this result.
    pub format: OverviewFormat,
    /// Scope path the summary was restricted to, or `None` for the whole worktree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Number of indexed files in scope.
    pub total_files: usize,
    /// Number of indexed symbols in scope.
    pub total_symbols: usize,
    /// File counts keyed by language.
    pub languages: BTreeMap<String, usize>,
    /// Symbol counts keyed by symbol kind.
    pub symbol_kinds: BTreeMap<String, usize>,
    /// Files in scope. In `summary` format these are the top files by symbol
    /// count with empty `symbols`; in `full` format every in-scope file with
    /// its symbols.
    pub files: Vec<OverviewFile>,
}

/// A file entry in an [`OverviewResult`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OverviewFile {
    /// Worktree-relative file path.
    pub path: String,
    /// Detected language.
    pub lang: String,
    /// Number of symbols defined in this file.
    pub symbol_count: usize,
    /// Symbols defined in this file; populated only in `full` format.
    pub symbols: Vec<OverviewSymbol>,
}

/// A symbol entry in an [`OverviewFile`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OverviewSymbol {
    /// Short symbol name.
    pub name: String,
    /// Symbol kind.
    pub kind: String,
    /// Fully-qualified symbol name.
    pub qualified: String,
}

/// Trait implementor result returned by [`crate::Graph::implementors`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImplementorsResult {
    /// Trait name matched against, derived from the selector's trailing segment.
    pub trait_name: String,
    /// Concrete types implementing the trait.
    pub implementors: Vec<Implementor>,
}

/// A single trait implementor entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Implementor {
    /// Qualified name of the implementing type.
    pub type_qualified: String,
    /// Trait reference recorded at the impl site (`relations.to_qualified`).
    pub trait_matched: String,
    /// Structural relation kind (`impl` / `implements`).
    pub kind: RefKind,
    /// File defining the implementation.
    pub file: String,
}

/// Outbound module/import edge result returned by [`crate::Graph::deps`].
///
/// Reports source-level import edges, not the Cargo crate dependency graph that
/// v1 `orbit.graph.deps` returned. Computed by the internal `query::deps`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DepsResult {
    /// Selector scope echoed back.
    pub scope: String,
    /// Outbound import edges in scope.
    pub imports: Vec<DepEdge>,
}

/// A single outbound import edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DepEdge {
    /// Source file that declares the import.
    pub from_file: String,
    /// Imported module path or specifier (language-specific opaque string).
    pub target_path: String,
    /// Imported symbol, or `None` for a whole-module import.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_symbol: Option<String>,
}

/// One runtime-invocation site returned by [`crate::Graph::runtime_invocations`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuntimeInvocation {
    /// Source file containing the invoking call.
    pub file: String,
    /// One-based source line where the invoking call starts.
    pub line: usize,
    /// Program string as written: the first argv element, a shell command's
    /// first token, a Cargo binary name, or the `.py` script an interpreter
    /// launch runs. Opaque; never resolved to a symbol.
    pub program: String,
    /// Innermost indexed symbol whose span contains the call, or `None` when
    /// the call lies outside every symbol span.
    pub symbol: Option<RuntimeInvocationSymbol>,
}

/// Enclosing symbol of a [`RuntimeInvocation`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuntimeInvocationSymbol {
    /// Short symbol name.
    pub name: String,
    /// Symbol kind.
    pub kind: String,
    /// Fully-qualified symbol name.
    pub qualified: String,
}

/// One program name returned by [`crate::Graph::program_names`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ProgramName {
    /// Program name as the manifest declares it.
    pub name: String,
    /// Which manifest entry declared it.
    pub source: ProgramNameSource,
    /// Worktree-relative path of the declaring manifest.
    pub manifest: String,
}

/// Manifest entry kinds that name a program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgramNameSource {
    /// `Cargo.toml` `[package].name`.
    CargoPackage,
    /// `Cargo.toml` `[[bin]].name`, or a `src/bin` target.
    CargoBin,
    /// A `pyproject.toml` `[project.scripts]` or `[tool.poetry.scripts]` key.
    PyprojectScript,
}
