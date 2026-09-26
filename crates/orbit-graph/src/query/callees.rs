//! Outbound call-edge query.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::extract::Selector;
use rusqlite::{Connection, params};

use crate::{
    CalleeEdge, CalleeOpts, CalleeReport, Graph, GraphError, RefConfidence, RefKind, SymbolSpan,
    resolve_symbol_span,
};

pub(crate) fn run(
    graph: &Graph,
    sel: &Selector,
    opts: &CalleeOpts,
) -> Result<CalleeReport, GraphError> {
    if opts.kind.is_some_and(|kind| kind != RefKind::Call) {
        return Ok(CalleeReport::default());
    }
    let resolved = graph.with_read_connection(|conn| {
        let Some(symbol) = resolve_symbol_span(conn, sel)? else {
            return Ok(None);
        };
        let mut defined = HashMap::new();
        let mut hidden_unresolved = 0;
        let mut edges = Vec::new();
        for edge in edges_for_symbol(conn, &symbol)? {
            let confidence = RefConfidence::from_db(edge.confidence.as_str())?;
            if !confidence.visible_at_floor(opts.confidence) {
                continue;
            }
            if opts.hide_unresolved
                && edge.target_qualified.is_none()
                && !has_indexed_definition(conn, &mut defined, edge.target_name.as_str())?
            {
                hidden_unresolved += 1;
                continue;
            }
            edges.push((edge, confidence));
        }
        Ok(Some((symbol, edges, hidden_unresolved)))
    })?;
    let Some((symbol, edges, hidden_unresolved)) = resolved else {
        return Ok(CalleeReport::default());
    };

    Ok(CalleeReport {
        callees: materialize_edges(
            graph.worktree_root.as_path(),
            symbol.file_path.as_str(),
            edges,
        )?,
        hidden_unresolved,
    })
}

/// Symbol kinds a call expression can target. A same-named `type_alias`
/// (Rust's associated `type Err`), module, or heading is not a definition of
/// the called name.
const CALLABLE_KINDS: &str = "'function', 'method', 'class', 'struct'";

/// Whether any indexed callable symbol is named `name`, memoized per query.
fn has_indexed_definition(
    conn: &Connection,
    memo: &mut HashMap<String, bool>,
    name: &str,
) -> Result<bool, GraphError> {
    if let Some(defined) = memo.get(name) {
        return Ok(*defined);
    }
    let defined = conn
        .prepare_cached(
            format!(
                "SELECT EXISTS (SELECT 1 FROM symbols WHERE name = ?1 AND kind IN ({CALLABLE_KINDS}))"
            )
            .as_str(),
        )
        .and_then(|mut stmt| stmt.query_row(params![name], |row| row.get::<_, bool>(0)))
        .map_err(|source| GraphError::sqlite("look up callee definition", source))?;
    memo.insert(name.to_string(), defined);
    Ok(defined)
}

pub(crate) fn edges_for_symbol(
    conn: &Connection,
    symbol: &SymbolSpan,
) -> Result<Vec<StoredCalleeEdge>, GraphError> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT target_name, target_qualified, confidence, from_span_start
             FROM refs
             WHERE from_file = ?1
               AND from_span_start >= ?2
               AND from_span_end <= ?3
               AND kind = 'call'
             ORDER BY from_span_start, id",
        )
        .map_err(|source| GraphError::sqlite("prepare callees query", source))?;

    stmt.query_map(
        params![symbol.file_path, symbol.span_start, symbol.span_end],
        |row| {
            Ok(StoredCalleeEdge {
                target_name: row.get(0)?,
                target_qualified: row.get(1)?,
                confidence: row.get(2)?,
                from_span: row.get(3)?,
            })
        },
    )
    .map_err(|source| GraphError::sqlite("execute callees query", source))?
    .collect::<Result<Vec<_>, _>>()
    .map_err(|source| GraphError::sqlite("collect callees edges", source))
}

#[derive(Debug, Clone)]
pub(crate) struct StoredCalleeEdge {
    pub(crate) target_name: String,
    pub(crate) target_qualified: Option<String>,
    pub(crate) confidence: String,
    pub(crate) from_span: i64,
}

fn materialize_edges(
    worktree_root: &Path,
    file_path: &str,
    edges: Vec<(StoredCalleeEdge, RefConfidence)>,
) -> Result<Vec<CalleeEdge>, GraphError> {
    if edges.is_empty() {
        return Ok(Vec::new());
    }

    let source_path = super::contained_worktree_source(worktree_root, file_path)?;
    let bytes = fs::read(source_path.as_path()).map_err(|source| {
        GraphError::io(
            "read source file for graph callee line",
            source_path,
            source,
        )
    })?;
    let lines = LineIndex::new(bytes);
    edges
        .into_iter()
        .map(|(edge, confidence)| {
            let from_span = usize::try_from(edge.from_span).map_err(|source| {
                GraphError::invalid_data("compute graph callee line", source.to_string())
            })?;
            Ok(CalleeEdge {
                target_name: edge.target_name,
                target_qualified: edge.target_qualified,
                confidence,
                line: lines.line_for(from_span),
            })
        })
        .collect()
}

struct LineIndex {
    byte_len: usize,
    line_starts: Vec<usize>,
}

impl LineIndex {
    fn new(bytes: Vec<u8>) -> Self {
        let mut line_starts = vec![0];
        for (index, byte) in bytes.iter().enumerate() {
            if *byte == b'\n' {
                line_starts.push(index + 1);
            }
        }
        Self {
            byte_len: bytes.len(),
            line_starts,
        }
    }

    fn line_for(&self, byte_offset: usize) -> usize {
        let capped = byte_offset.min(self.byte_len);
        self.line_starts
            .partition_point(|line_start| *line_start <= capped)
    }
}
