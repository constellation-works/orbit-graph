//! Selector resolution and bounded source reads.

use std::str;

use orbit_graph_extract::Selector;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde::ser::{SerializeStruct, Serializer};

use crate::{Graph, GraphError};

/// Default maximum source bytes returned by [`Graph::show`].
///
/// The value keeps a single result comfortably inside agent context while
/// still covering typical functions, files, and command handlers.
pub const DEFAULT_SHOW_MAX_BYTES: usize = 64 * 1024;

/// Source and metadata view returned by [`Graph::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeView {
    /// Source bytes from the resolved span, bounded by the caller's budget.
    pub bytes: Vec<u8>,
    /// Metadata for the resolved source span.
    pub metadata: NodeMetadata,
}

/// `source` is the UTF-8 text, or `null` when the bytes are not UTF-8; then
/// `source_bytes` carries them losslessly. `source_encoding` names which one
/// is set (`utf-8` or `bytes`), so the field types never depend on content
/// (STD-01 §R11).
impl Serialize for NodeView {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("NodeView", 4)?;
        match str::from_utf8(self.bytes.as_slice()) {
            Ok(source) => {
                state.serialize_field("source", source)?;
                state.serialize_field("source_bytes", &None::<&[u8]>)?;
                state.serialize_field("source_encoding", "utf-8")?;
            }
            Err(_) => {
                state.serialize_field("source", &None::<&str>)?;
                state.serialize_field("source_bytes", self.bytes.as_slice())?;
                state.serialize_field("source_encoding", "bytes")?;
            }
        }
        state.serialize_field("metadata", &self.metadata)?;
        state.end()
    }
}

/// Metadata for a [`NodeView`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NodeMetadata {
    /// Workspace-relative source path.
    pub file: String,
    /// Full resolved source span before byte-budget truncation.
    pub span: SourceSpan,
    /// Resolved node kind.
    pub kind: String,
    /// Display name when one exists, otherwise `null`.
    pub name: Option<String>,
    /// Qualified symbol name when one exists, otherwise `null`.
    pub qualified: Option<String>,
    /// Whether the returned source is shorter than the resolved source span.
    pub truncated: bool,
}

/// Byte span in a source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SourceSpan {
    /// Inclusive byte start.
    pub start: usize,
    /// Exclusive byte end.
    pub end: usize,
}

pub(crate) fn run(
    graph: &Graph,
    selector: &Selector,
    max_bytes: usize,
) -> Result<Option<NodeView>, GraphError> {
    let Some(resolved) = graph.with_read_connection(|conn| resolve_selector(conn, selector))?
    else {
        return Ok(None);
    };
    materialize_view(graph, resolved, max_bytes).map(Some)
}

fn resolve_selector(
    conn: &Connection,
    selector: &Selector,
) -> Result<Option<ResolvedView>, GraphError> {
    match selector {
        Selector::Symbol { path, symbol, kind } => resolve_symbol(conn, path, symbol, kind),
        Selector::File { path } => resolve_file(conn, path),
        Selector::Module { qualified } => resolve_module(conn, qualified),
        Selector::Command { name } => resolve_command(conn, name),
        Selector::Dir { .. } => Ok(None),
    }
}

fn resolve_symbol(
    conn: &Connection,
    path: &str,
    symbol: &str,
    kind: &str,
) -> Result<Option<ResolvedView>, GraphError> {
    conn.query_row(
        "SELECT file_path, span_start, span_end, kind, name, qualified
         FROM symbols
         WHERE file_path = ?1
           AND kind = ?3
           AND (name = ?2 OR qualified = ?2)
         ORDER BY CASE WHEN qualified = ?2 THEN 0 WHEN name = ?2 THEN 1 ELSE 2 END, id
         LIMIT 1",
        params![path, symbol, kind],
        resolved_symbol_from_row,
    )
    .optional()
    .map_err(|source| GraphError::sqlite("resolve graph symbol selector", source))
}

fn resolve_file(conn: &Connection, path: &str) -> Result<Option<ResolvedView>, GraphError> {
    conn.query_row(
        "SELECT path, 0, byte_len, 'file', path, NULL
         FROM files
         WHERE path = ?1",
        params![path],
        resolved_symbol_from_row,
    )
    .optional()
    .map_err(|source| GraphError::sqlite("resolve graph file selector", source))
}

fn resolve_module(conn: &Connection, qualified: &str) -> Result<Option<ResolvedView>, GraphError> {
    conn.query_row(
        "SELECT file_path, span_start, span_end, kind, name, qualified
         FROM symbols
         WHERE kind = 'module'
           AND (qualified = ?1 OR name = ?1)
         ORDER BY CASE WHEN qualified = ?1 THEN 0 WHEN name = ?1 THEN 1 ELSE 2 END, id
         LIMIT 1",
        params![qualified],
        resolved_symbol_from_row,
    )
    .optional()
    .map_err(|source| GraphError::sqlite("resolve graph module selector", source))
}

fn resolve_command(conn: &Connection, name: &str) -> Result<Option<ResolvedView>, GraphError> {
    conn.query_row(
        "SELECT COALESCE(s.file_path, c.file_path) AS file_path,
                COALESCE(s.span_start, c.span_start) AS span_start,
                COALESCE(s.span_end, f.byte_len) AS span_end,
                'command' AS kind,
                c.name AS name,
                s.qualified AS qualified
         FROM commands c
         JOIN files f ON f.path = c.file_path
         LEFT JOIN symbols s ON s.id = c.handler_symbol
         WHERE c.name = ?1
         ORDER BY c.name
         LIMIT 1",
        params![name],
        resolved_symbol_from_row,
    )
    .optional()
    .map_err(|source| GraphError::sqlite("resolve graph command selector", source))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedView {
    file: String,
    span_start: i64,
    span_end: i64,
    kind: String,
    name: Option<String>,
    qualified: Option<String>,
}

fn resolved_symbol_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ResolvedView> {
    Ok(ResolvedView {
        file: row.get(0)?,
        span_start: row.get(1)?,
        span_end: row.get(2)?,
        kind: row.get(3)?,
        name: row.get(4)?,
        qualified: row.get(5)?,
    })
}

fn materialize_view(
    graph: &Graph,
    resolved: ResolvedView,
    max_bytes: usize,
) -> Result<NodeView, GraphError> {
    let source_path =
        super::contained_worktree_source(graph.worktree_root.as_path(), resolved.file.as_str())?;
    let window = super::source::read_show_window(
        source_path.as_path(),
        "read source file for graph show",
        "read source span for graph show",
        resolved.span_start,
        resolved.span_end,
        max_bytes,
        resolved.file.as_str(),
    )?;

    Ok(NodeView {
        bytes: window.bytes,
        metadata: NodeMetadata {
            file: resolved.file,
            span: SourceSpan {
                start: window.start,
                end: window.end,
            },
            kind: resolved.kind,
            name: resolved.name,
            qualified: resolved.qualified,
            truncated: window.truncated,
        },
    })
}

#[cfg(test)]
#[path = "tests/show.rs"]
mod tests;
