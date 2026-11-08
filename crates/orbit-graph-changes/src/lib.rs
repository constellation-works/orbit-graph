// A library prints nothing: diagnostics go through `tracing` (STD-02 §R15).
#![deny(clippy::print_stderr, clippy::print_stdout)]
// Test fixtures write files with `fs::write`, which `clippy.toml` bans from
// shipped code in favour of `orbit_graph::atomic_write` (STD-03 §R5).
#![cfg_attr(
    test,
    allow(clippy::expect_used, clippy::unwrap_used, clippy::disallowed_methods)
)]

//! Change-analysis library built on the public `orbit_graph` API.
//!
//! The one public operation is [`analyse`]: it resolves two refs to
//! immutable commits and indexes each one in isolation, so every answer is
//! attributable to exactly one revision. Materialized trees and indexes are
//! kept on disk in a snapshot cache, keyed by `(commit SHA,
//! EXTRACTOR_VERSION, STORE_SCHEMA_VERSION)`, so a second launch reuses them
//! instead of re-indexing. The analysis pairs symbols across the two
//! snapshots, turns each changed symbol into bounded multi-hop inbound
//! evidence paths, entry points, and labelled candidate tests, applies the
//! request's presentation filters — stating, in every filtered response,
//! what they removed and why — and regroups the whole result per changed
//! symbol, under explicit bounds and an optional wall-clock budget, as one
//! [`ChangesDocument`] for agents choosing what to review and which tests
//! to run.
//!
//! Only what the `orbit-graph` CLI consumes is public (STD-02 §R8): the
//! request and document types of [`analyse`] and [`validate`], their error
//! types, and the types those signatures name. Every module is
//! crate-private; the snapshot cache, the changed-symbol pairing, the
//! evidence traversal, and the JSON report export are implementation
//! details, exercised by this crate's own unit tests.
//!
//! The evidence contract and snapshot semantics are recorded in
//! `docs/design/change-explorer.md`; the agent-facing contract in
//! `docs/design/changes-command/`.
//!
//! The crate never reads Orbit control-plane state and never executes content
//! from the repository under inspection.

pub(crate) mod analysis;
pub(crate) mod cache;
pub(crate) mod changes;
pub(crate) mod evidence;
pub(crate) mod filters;
pub(crate) mod report;
mod selection;
pub(crate) mod snapshot;

#[cfg(test)]
mod test_fixtures;

pub use analysis::{
    AffectedCaller, AffectedEntryPoint, AnalysisError, ChangesBounds, ChangesDocument,
    ChangesQuery, ChangesRange, ChangesRequest, ChangesSummary, ChangesTimings,
    DEFAULT_MAX_CALLERS, DEFAULT_MAX_ENTRY_POINTS, DEFAULT_MAX_SYMBOLS, DEFAULT_MAX_TESTS,
    DEFAULT_NODE_CAP, Incomplete, LabelledTest, NotAnalysed, SymbolAnalysis, TestSelection,
    analyse, validate,
};
pub use cache::{CacheError, CleanReason};
pub use changes::{
    ChangeStatus, ChangesError, FileChangeKind, OutOfScopeEntry, OutOfScopeReason, Pairing,
    PairingEvidence, SymbolRef, UncertainCandidate,
};
pub use evidence::{
    CandidateSource, DEFAULT_TIME_BUDGET_MS, EVIDENCE_DEPTH, EndpointRef, EvidenceCategory,
    EvidenceEdge, EvidenceError, EvidenceSource,
};
pub use filters::{FilterSet, FilteredOut};
pub use report::{
    ComparisonSnapshotView, ComparisonView, ExcludedEntryView, LocationEvidence, ReportEdge,
    ReportError, ReportEvidencePath, RevisionRef, SourceExcerpt, TruncationFlag, UnresolvedArea,
    WorkingTreeEntryView, WorkingTreeView,
};
pub use snapshot::{ComparisonOptions, DefaultBase, SnapshotError, SnapshotSide};
