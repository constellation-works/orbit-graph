#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]

//! Change-explorer library built on the public `orbit_graph` API.
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
//! - [`service`] serves these over a loopback HTTP service scoped to one
//!   repository and one pair of revisions.
//!
//! The evidence contract, service surface, and UI layout this crate implements
//! are recorded in `docs/design/change-explorer.md`.
//!
//! The crate never reads Orbit control-plane state and never executes content
//! from the repository under inspection.

pub mod cache;
pub mod changes;
pub mod evidence;
pub mod filters;
pub mod report;
pub mod service;
pub mod snapshot;
mod status;
