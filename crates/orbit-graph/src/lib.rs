// A library prints nothing: diagnostics go through `tracing` (STD-02 §R15).
#![deny(clippy::print_stderr, clippy::print_stdout)]
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]

//! SQLite-backed source-code graph store and query API.
//!
//! This crate owns the durable graph database path contract, sync policy, and
//! public query surface. It can be embedded as a library or used through the
//! `orbit-graph` JSON command-line interface.

mod clean;
mod db_path;
mod error;
mod evaluation;
mod graph;
mod live_evaluation;
mod lock;
pub mod plugin;
mod query;
mod recommend;
mod store;
mod sync;

#[cfg(test)]
mod tests;

// `docs/usage.md`, included so `cargo test --doc` compiles its Rust example
// against this API and a stale example fails the build (STD-04 §R14).
#[cfg(doctest)]
#[doc = include_str!("../../../docs/usage.md")]
mod usage_doc {}

/// Confidence floor used by reference and traversal queries.
pub use RefConfidence as Confidence;
pub use clean::{
    CleanItem, CleanReason, CleanReport, clean_old_databases, plan_clean_old_databases,
};
pub use db_path::{
    GraphDbPath, resolve_db_path, resolve_db_path_for_commit, resolve_worktree_db_path,
};
pub use error::GraphError;
pub use graph::Graph;
pub(crate) use graph::open_read_connection;
pub use query::types::{
    CalleeEdge, CalleeOpts, CalleeReport, DepEdge, DepsResult, ImpactDirection, ImpactEntry,
    ImpactFallback, ImpactOrigin, ImpactResult, Implementor, ImplementorsResult, OverviewFile,
    OverviewFormat, OverviewResult, OverviewSymbol, ProgramName, ProgramNameSource, RefConfidence,
    RefEntry, RefFallback, RefKind, RefOpts, RefResult, RefTarget, RelationEntry,
    RuntimeInvocation, RuntimeInvocationSymbol, TraceNode, TraceResult,
};
pub(crate) use query::types::{SymbolSpan, resolve_symbol_span};
pub use sync::report::{
    SyncFailure, SyncMode, SyncObserver, SyncOutcome, SyncPhase, SyncPolicy, SyncProgress,
    SyncReport, SyncSkip,
};

#[doc(inline)]
pub use orbit_graph_extract::history::{
    CHANGE_EXTRACTOR_VERSION, CurrentRevisionResolution, CurrentSymbolStatus,
    DEFAULT_HISTORY_SYNC_LIMIT, DELIVERY_IMPORT_SCHEMA_VERSION, DeliveredChange, DeliveryEvidence,
    DeliveryImport, FileChange, FileChangeKind, FileFallbackReason, Provenance, RevisionSide,
    SymbolAttribution, SymbolChange, SymbolIdentity, SymbolMatchConfidence, TaskAssociation,
    TaskTextAvailability, TemporalFact, TemporalStatus,
};
#[doc(inline)]
pub use orbit_graph_extract::{Selector, SelectorParseError};
pub use store::history::{
    HISTORY_INDEX_SCHEMA_VERSION, HistoryImportReport, HistoryIndex, HistoryRebuildReport,
    HistoryStatus, HistorySyncReport,
};

pub use evaluation::{
    EVALUATION_CORPUS_SCHEMA_VERSION, EVALUATION_SCHEMA_VERSION, EvaluationCorpus,
    EvaluationReport, evaluate_corpus,
};
pub use live_evaluation::{LiveGitEvaluation, LiveGitReport, evaluate_live_git};
pub use query::{
    DEFAULT_SEARCH_LIMIT, DEFAULT_SHOW_MAX_BYTES, Match, NodeMetadata, NodeView, SearchKind,
    SearchQuery, SearchResult, SourceSpan,
};
pub use recommend::{
    DEFAULT_RECOMMENDATION_LIMIT, HybridTaskHit, Recommendation, RecommendationAssociation,
    RecommendationCounts, RecommendationEngine, RecommendationFallback, RecommendationFreshness,
    RecommendationFreshnessStatus, RecommendationInput, RecommendationLevel, RecommendationReason,
    RecommendationRequest, RecommendationResult, RecommendationVariant,
};

/// Extractor/storage compatibility version embedded in graph database names.
///
/// Bump this when extractor output or storage expectations change
/// incompatibly. Older graph DB files then become invisible to the active
/// graph handle and are removed by the next `orbit-graph sync` or `clean --confirm`
/// whose lock on them is free; a newer version's files are never removed.
///
/// A bump also changes the committed `direct-call` sample export, which
/// records this number: regenerate it in the same change with
/// `UPDATE_GOLDENS=1 cargo test -p orbit-graph-explorer --test derived_artifacts --locked`.
// L-0052: FTS population invariants require a fresh DB when old indexes may be empty.
/// Version 5 rebuilds stored refs so qualified cross-file and explicit-import
/// resolution records stable symbol hints.
///
/// Version 6 rebuilds stored refs again: the Rust and Python call extractors
/// now recurse into method-chain receivers (previously dropped) and no
/// longer emit a chain receiver's full source text as a bogus callee name
/// for turbofish method calls (`x.collect::<Vec<_>>()`).
///
/// Version 7 rebuilds stored refs once more: Rust `impl` blocks are indexed
/// under the implemented type's name, and pass 2 now excludes them from ref
/// candidate lookups, so a type with any impl block resolves cross-file
/// references instead of degrading every one of them to `fuzzy_name`.
///
/// Version 8 rebuilds stored refs once more: a method call whose receiver type
/// the extractor cannot determine (`args.execute()`) is recorded with its
/// receiver, and pass 2 no longer resolves such a call by short name alone, so
/// a dispatcher no longer reports its own dispatch lines as `exact` inbound
/// references to itself.
///
/// Version 9 includes nested Rust and Python function definitions, so their
/// symbols and call ownership are present in rebuilt snapshots.
///
/// Version 10 rebuilds stored refs once more: calls on an impl's literal `self`
/// receiver and `Self::method()` now retain that impl type, so they can resolve
/// to the intended method rather than joining the name-only fallback.
///
/// Version 11 records `runtime_invocation` refs for Python `subprocess` and
/// Rust `Command::new` / `Command::cargo_bin` calls that name the program they
/// start, so [`Graph::runtime_invocations`] has rows to report.
///
/// Version 12 adds indexes on the store's foreign-key child columns and on
/// `imports(from_file)`, which reference resolution and per-file rewrites
/// look up; the stored rows are unchanged.
///
/// Version 13 rebuilds stored refs once more: Rust calls written inside macro
/// invocation arguments (`assert!(f(x))`, `vec![g(y)]`, `format!("{}", h())`)
/// are recovered from the macro's token tree, and a function passed by name
/// as a call argument (`.map(skill_link_roots)`) is recorded as a call.
///
/// Version 14 rebuilds stored refs once more: a Rust method call on a local
/// binding whose type is spelled (a typed parameter or `let`, or a
/// `T::new(..)`/`T { .. }` initialiser) records `<T>::method`, and pass 2
/// resolves `<T>::m`, `<T as Trait>::m`, and scoped `T::m(..)` targets against
/// that type's inherent, trait-impl, or trait-declared member instead of by
/// short name.
///
/// Version 15 stores each ref's resolution inputs (`extracted_qualified`,
/// `unresolved_receiver`, `spelled_path`), so an incremental sync can
/// re-resolve refs in unchanged files whose target was defined, removed or
/// renamed elsewhere.
///
/// Version 16 rebuilds stored refs so cross-file resolution only chooses
/// symbols from the ref's language.
///
/// Version 17 skips files larger than 4 MiB and files whose tree-sitter parse
/// exceeds its per-file deadline, so neither gets rows.
///
/// Version 18 makes pass 2's commit the only point where a file becomes
/// current, so an interrupted sync is repaired by the next one. It also
/// rebuilds indexes that earlier interrupted syncs left with files marked
/// current but missing their refs.
pub const EXTRACTOR_VERSION: u32 = 18;

/// SQLite schema version used by the graph store.
///
/// # Examples
///
/// ```
/// use orbit_graph::STORE_SCHEMA_VERSION;
///
/// assert!(STORE_SCHEMA_VERSION >= 1);
/// ```
pub const STORE_SCHEMA_VERSION: u32 = store::schema::SCHEMA_VERSION;

/// Default graph distance used by callers that do not supply `--depth`.
pub const DEFAULT_IMPACT_DEPTH: u8 = 3;

/// Default call-tree distance used by command traces when depth is omitted.
pub const DEFAULT_TRACE_DEPTH: u8 = 5;

/// Maximum number of impacted symbols returned by bounded traversals.
pub const IMPACT_NODE_CAP: usize = 200;

/// Maximum number of trace nodes returned by command traces.
pub const TRACE_NODE_CAP: usize = 200;
