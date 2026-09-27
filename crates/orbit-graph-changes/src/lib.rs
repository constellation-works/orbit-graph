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
//! - [`snapshot`] resolves two refs to immutable commits and indexes each one
//!   in isolation, so every answer is attributable to exactly one revision.
//! - [`cache`] keeps those materialized trees and indexes on disk, keyed by
//!   `(commit SHA, EXTRACTOR_VERSION, STORE_SCHEMA_VERSION)`, so a second
//!   launch reuses them instead of re-indexing.
//! - [`changes`] pairs symbols across those two snapshots and produces the
//!   changed-symbol payload.
//! - [`evidence`] turns one changed symbol into bounded multi-hop inbound
//!   evidence paths, entry points, and labelled candidate tests.
//! - [`filters`] applies presentation filters and states, in every filtered
//!   response, what they removed and why.
//! - [`report`] exports the whole analysis of one comparison as one payload.
//! - [`analysis`] regroups that analysis per changed symbol, under explicit
//!   bounds and an optional wall-clock budget, for agents choosing what to
//!   review and which tests to run.
//!
//! The evidence contract and snapshot semantics are recorded in
//! `docs/design/change-explorer.md`; the agent-facing contract in
//! `docs/design/changes-command/`.
//!
//! The crate never reads Orbit control-plane state and never executes content
//! from the repository under inspection.

pub mod analysis;
pub mod cache;
pub mod changes;
pub mod evidence;
pub mod filters;
pub mod report;
pub mod snapshot;
