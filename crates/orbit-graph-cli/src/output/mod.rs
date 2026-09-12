//! Shared terminal output policy and rendering boundary.
//!
//! Commands return a JSON document plus a [`View`]. The document is the stable
//! machine contract; the view describes the human rendering without reading
//! terminal state or writing to a process stream. [`OutputSink`] resolves that
//! process state once and is the only value a renderer needs.

pub mod json;
pub mod payload;
pub mod pipe;
pub mod render;
pub mod sink;
pub mod table;

#[cfg(test)]
mod tests;

pub use payload::{CommandOutput, View, ViewBlock};
pub use render::{emit, emit_error};
pub use sink::{OutputSink, install_format_argument, requested_format, requested_format_from_args};
pub use table::{Column, TableView};
