#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]

//! Change-explorer library built on the public `orbit_graph` API.
//!
//! Milestone 1 provides the [`snapshot`] module: the immutable base/head
//! snapshot foundation every later milestone queries. The evidence contract,
//! service surface, and UI layout this crate will grow into are recorded in
//! `docs/design/change-explorer.md`.
//!
//! The crate never reads Orbit control-plane state and never executes content
//! from the repository under inspection.

pub mod snapshot;
