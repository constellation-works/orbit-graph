//! Inbound reference and relation query.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::extract::Selector;
use rusqlite::{Connection, Row, params};

use crate::{
    Graph, GraphError, RefConfidence, RefEntry, RefFallback, RefKind, RefOpts, RefResult,
    RefTarget, RelationEntry,
};

/// Maximum characters kept in a reference's one-line [`RefEntry::snippet`].
pub(crate) const REF_SNIPPET_MAX_CHARS: usize = 160;

const CONFIDENCE_EXACT: &str = "exact";
const CONFIDENCE_IMPORT_RESOLVED: &str = "import_resolved";
const CONFIDENCE_SAME_MODULE: &str = "same_module";
const CONFIDENCE_FUZZY_NAME: &str = "fuzzy_name";

pub(crate) fn run(graph: &Graph, sel: &Selector, opts: &RefOpts) -> Result<RefResult, GraphError> {
    let mut rows = RowContext {
        lines: LineCache::new(graph.worktree_root.as_path()),
        enclosing: EnclosingSymbols::default(),
    };
    graph.with_read_connection(|conn| {
        let target = resolve_target(conn, sel)?;
        let Some(qualified) = target.output.qualified.as_deref() else {
            return Ok(empty_result(target.output));
        };

        let mut skipped_low_confidence = 0;
        let refs = if should_query_refs(opts.kind) {
            query_refs(
                conn,
                target.symbol_id,
                qualified,
                target.output.name.as_str(),
                opts,
                &mut rows,
                &mut skipped_low_confidence,
            )?
        } else {
            Vec::new()
        };
        let relations = if should_query_relations(opts.kind) {
            query_relations(
                conn,
                qualified,
                opts,
                &mut rows.lines,
                &mut skipped_low_confidence,
            )?
        } else {
            Vec::new()
        };

        let fallback = maybe_fuzzy_fallback(
            conn,
            target.symbol_id,
            qualified,
            target.output.name.as_str(),
            opts,
            &refs,
            &mut rows,
        )?;

        Ok(RefResult {
            target: target.output,
            refs,
            relations,
            skipped_low_confidence,
            fallback_used: fallback.is_some(),
            fallback,
        })
    })
}

/// When the precise floor yields no textual refs, re-query at the `fuzzy_name`
/// floor so cross-crate call sites routed through `pub use` re-exports (which
/// resolve only by name, with `target_qualified = NULL`) are still surfaced.
///
/// Returns `None` when refs were already found, when the caller already asked
/// for the fuzzy floor (the matches are in `refs` directly), or when no
/// lower-confidence match exists. See ORB-00383.
fn maybe_fuzzy_fallback(
    conn: &Connection,
    symbol_id: Option<i64>,
    qualified: &str,
    target_name: &str,
    opts: &RefOpts,
    refs: &[RefEntry],
    rows: &mut RowContext,
) -> Result<Option<RefFallback>, GraphError> {
    if !refs.is_empty()
        || opts.confidence == RefConfidence::FuzzyName
        || !should_query_refs(opts.kind)
    {
        return Ok(None);
    }

    let fallback_opts = RefOpts {
        confidence: RefConfidence::FuzzyName,
        kind: opts.kind,
    };
    let mut skipped = 0;
    let fallback_refs = query_refs(
        conn,
        symbol_id,
        qualified,
        target_name,
        &fallback_opts,
        rows,
        &mut skipped,
    )?;
    if fallback_refs.is_empty() {
        return Ok(None);
    }

    let note = format!(
        "No references resolved at the `{}` confidence floor; showing {} match(es) found at the \
         `fuzzy_name` fallback floor. These are name-only matches and may include unrelated \
         symbols sharing the name — check each entry's `confidence`.",
        confidence_label(opts.confidence),
        fallback_refs.len(),
    );
    Ok(Some(RefFallback {
        confidence: RefConfidence::FuzzyName,
        refs: fallback_refs,
        note,
    }))
}

fn confidence_label(confidence: RefConfidence) -> &'static str {
    match confidence {
        RefConfidence::Exact => CONFIDENCE_EXACT,
        RefConfidence::ImportResolved => CONFIDENCE_IMPORT_RESOLVED,
        RefConfidence::SameModule => CONFIDENCE_SAME_MODULE,
        RefConfidence::FuzzyName => CONFIDENCE_FUZZY_NAME,
    }
}

fn resolve_target(conn: &Connection, sel: &Selector) -> Result<QueryTarget, GraphError> {
    let Selector::Symbol { path, symbol, kind } = sel else {
        return Ok(QueryTarget {
            output: RefTarget {
                name: sel.path().to_string(),
                qualified: None,
            },
            symbol_id: None,
        });
    };

    let mut stmt = conn
        .prepare_cached(
            "SELECT id, name, qualified FROM symbols
             WHERE file_path = ?1
               AND kind = ?2
               AND (name = ?3 OR qualified = ?3)
             ORDER BY CASE WHEN qualified = ?3 THEN 0 ELSE 1 END, id
             LIMIT 1",
        )
        .map_err(|source| GraphError::sqlite("prepare refs target resolution", source))?;
    let result = stmt.query_row(params![path, kind, symbol], |row| {
        Ok(QueryTarget {
            symbol_id: Some(row.get(0)?),
            output: RefTarget {
                name: row.get(1)?,
                qualified: Some(row.get(2)?),
            },
        })
    });

    match result {
        Ok(target) => Ok(target),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(QueryTarget {
            output: RefTarget {
                name: symbol.clone(),
                qualified: None,
            },
            symbol_id: None,
        }),
        Err(source) => Err(GraphError::sqlite("resolve refs target symbol", source)),
    }
}

fn empty_result(target: RefTarget) -> RefResult {
    RefResult {
        target,
        refs: Vec::new(),
        relations: Vec::new(),
        skipped_low_confidence: 0,
        fallback_used: false,
        fallback: None,
    }
}

fn should_query_refs(kind: Option<RefKind>) -> bool {
    kind.is_none_or(RefKind::is_textual)
}

fn should_query_relations(kind: Option<RefKind>) -> bool {
    kind.is_none_or(RefKind::is_structural)
}

fn query_refs(
    conn: &Connection,
    symbol_id: Option<i64>,
    qualified: &str,
    target_name: &str,
    opts: &RefOpts,
    context: &mut RowContext,
    skipped_low_confidence: &mut usize,
) -> Result<Vec<RefEntry>, GraphError> {
    let include_fuzzy_name = opts.confidence == RefConfidence::FuzzyName;
    let sql = match (opts.kind, include_fuzzy_name) {
        (Some(_), true) => {
            "SELECT from_file, from_span_start, from_span_end, kind, confidence
             FROM refs
             WHERE kind = ?3
               AND (
                   target_symbol_hint = ?1
                   OR (target_symbol_hint IS NULL AND target_qualified = ?2)
                   OR (confidence = 'fuzzy_name' AND target_name = ?4)
               )
             ORDER BY from_file, from_span_start, id"
        }
        (Some(_), false) => {
            "SELECT from_file, from_span_start, from_span_end, kind, confidence
             FROM refs
             WHERE (target_symbol_hint = ?1 OR (target_symbol_hint IS NULL AND target_qualified = ?2)) AND kind = ?3
             ORDER BY from_file, from_span_start, id"
        }
        (None, true) => {
            "SELECT from_file, from_span_start, from_span_end, kind, confidence
             FROM refs
             WHERE target_symbol_hint = ?1
                OR (target_symbol_hint IS NULL AND target_qualified = ?2)
                OR (confidence = 'fuzzy_name' AND target_name = ?3
                    AND kind <> 'runtime_invocation')
             ORDER BY from_file, from_span_start, id"
        }
        (None, false) => {
            "SELECT from_file, from_span_start, from_span_end, kind, confidence
             FROM refs
             WHERE target_symbol_hint = ?1 OR (target_symbol_hint IS NULL AND target_qualified = ?2)
             ORDER BY from_file, from_span_start, id"
        }
    };
    let mut stmt = conn
        .prepare_cached(sql)
        .map_err(|source| GraphError::sqlite("prepare refs lookup", source))?;
    let rows = match (opts.kind, include_fuzzy_name) {
        (Some(kind), true) => stmt
            .query_map(
                params![symbol_id, qualified, kind.as_str(), target_name],
                row_to_ref_row,
            )
            .map_err(|source| GraphError::sqlite("query refs by target", source))?
            .collect::<Result<Vec<_>, _>>(),
        (Some(kind), false) => stmt
            .query_map(params![symbol_id, qualified, kind.as_str()], row_to_ref_row)
            .map_err(|source| GraphError::sqlite("query refs by target", source))?
            .collect::<Result<Vec<_>, _>>(),
        (None, true) => stmt
            .query_map(params![symbol_id, qualified, target_name], row_to_ref_row)
            .map_err(|source| GraphError::sqlite("query refs by target", source))?
            .collect::<Result<Vec<_>, _>>(),
        (None, false) => stmt
            .query_map(params![symbol_id, qualified], row_to_ref_row)
            .map_err(|source| GraphError::sqlite("query refs by target", source))?
            .collect::<Result<Vec<_>, _>>(),
    }
    .map_err(|source| GraphError::sqlite("collect refs lookup rows", source))?;

    let mut entries = Vec::with_capacity(rows.len());
    for row in rows {
        let kind = RefKind::from_db(row.kind.as_str())?;
        let confidence = RefConfidence::from_db(row.confidence.as_str())?;
        if !confidence.visible_at_floor(opts.confidence) {
            *skipped_low_confidence += 1;
            continue;
        }
        let from_selector = context.enclosing.selector_for(
            conn,
            row.file.as_str(),
            row.span_start,
            row.span_end,
        )?;
        entries.push(RefEntry {
            line: context.lines.line_for(row.file.as_str(), row.span_start)?,
            snippet: context
                .lines
                .snippet_for(row.file.as_str(), row.span_start)?,
            file: row.file,
            kind,
            confidence,
            from_selector,
        });
    }
    Ok(entries)
}

struct QueryTarget {
    output: RefTarget,
    symbol_id: Option<i64>,
}

fn query_relations(
    conn: &Connection,
    qualified: &str,
    opts: &RefOpts,
    line_cache: &mut LineCache,
    skipped_low_confidence: &mut usize,
) -> Result<Vec<RelationEntry>, GraphError> {
    let sql = match opts.kind {
        Some(_) => {
            "SELECT from_qualified, kind, def_file, def_span_start, confidence
             FROM relations
             WHERE to_qualified = ?1 AND kind = ?2
             ORDER BY def_file, def_span_start, id"
        }
        None => {
            "SELECT from_qualified, kind, def_file, def_span_start, confidence
             FROM relations
             WHERE to_qualified = ?1
             ORDER BY def_file, def_span_start, id"
        }
    };
    let mut stmt = conn
        .prepare_cached(sql)
        .map_err(|source| GraphError::sqlite("prepare relations lookup", source))?;
    let rows = match opts.kind {
        Some(kind) => stmt
            .query_map(params![qualified, kind.as_str()], row_to_relation_row)
            .map_err(|source| GraphError::sqlite("query relations by target", source))?
            .collect::<Result<Vec<_>, _>>(),
        None => stmt
            .query_map(params![qualified], row_to_relation_row)
            .map_err(|source| GraphError::sqlite("query relations by target", source))?
            .collect::<Result<Vec<_>, _>>(),
    }
    .map_err(|source| GraphError::sqlite("collect relations lookup rows", source))?;

    let mut entries = Vec::with_capacity(rows.len());
    for row in rows {
        let kind = RefKind::from_db(row.kind.as_str())?;
        let confidence = RefConfidence::from_db(row.confidence.as_str())?;
        if !confidence.visible_at_floor(opts.confidence) {
            *skipped_low_confidence += 1;
            continue;
        }
        entries.push(RelationEntry {
            from: row.from,
            kind,
            line: line_cache.line_for(row.file.as_str(), row.span_start)?,
            file: row.file,
            confidence,
        });
    }
    Ok(entries)
}

fn row_to_ref_row(row: &Row<'_>) -> rusqlite::Result<StoredRefRow> {
    Ok(StoredRefRow {
        file: row.get(0)?,
        span_start: row.get(1)?,
        span_end: row.get(2)?,
        kind: row.get(3)?,
        confidence: row.get(4)?,
    })
}

fn row_to_relation_row(row: &Row<'_>) -> rusqlite::Result<StoredRelationRow> {
    Ok(StoredRelationRow {
        from: row.get(0)?,
        kind: row.get(1)?,
        file: row.get(2)?,
        span_start: row.get(3)?,
        confidence: row.get(4)?,
    })
}

impl RefConfidence {
    pub(crate) fn from_db(value: &str) -> Result<Self, GraphError> {
        match value {
            CONFIDENCE_EXACT => Ok(Self::Exact),
            CONFIDENCE_IMPORT_RESOLVED => Ok(Self::ImportResolved),
            CONFIDENCE_SAME_MODULE => Ok(Self::SameModule),
            CONFIDENCE_FUZZY_NAME => Ok(Self::FuzzyName),
            other => Err(GraphError::invalid_data(
                "parse graph ref confidence",
                format!("unknown confidence `{other}`"),
            )),
        }
    }

    pub(crate) fn visible_at_floor(self, floor: Self) -> bool {
        self.rank() <= floor.rank()
    }

    fn rank(self) -> u8 {
        match self {
            Self::Exact => 1,
            Self::ImportResolved => 2,
            Self::SameModule => 3,
            Self::FuzzyName => 4,
        }
    }
}

impl RefKind {
    pub(crate) fn from_db(value: &str) -> Result<Self, GraphError> {
        match value {
            "call" => Ok(Self::Call),
            "type" => Ok(Self::Type),
            "use" => Ok(Self::Use),
            "trait_bound" => Ok(Self::TraitBound),
            "impl" => Ok(Self::Impl),
            "extends" => Ok(Self::Extends),
            "implements" => Ok(Self::Implements),
            other => Err(GraphError::invalid_data(
                "parse graph ref kind",
                format!("unknown ref kind `{other}`"),
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Call => "call",
            Self::Type => "type",
            Self::Use => "use",
            Self::TraitBound => "trait_bound",
            Self::Impl => "impl",
            Self::Extends => "extends",
            Self::Implements => "implements",
        }
    }

    fn is_textual(self) -> bool {
        matches!(self, Self::Call | Self::Type | Self::Use | Self::TraitBound)
    }

    fn is_structural(self) -> bool {
        matches!(self, Self::Impl | Self::Extends | Self::Implements)
    }
}

struct StoredRefRow {
    file: String,
    span_start: i64,
    span_end: i64,
    kind: String,
    confidence: String,
}

struct StoredRelationRow {
    from: String,
    kind: String,
    file: String,
    span_start: i64,
    confidence: String,
}

/// Per-query caches that turn a stored reference row into a [`RefEntry`].
struct RowContext<'a> {
    lines: LineCache<'a>,
    enclosing: EnclosingSymbols,
}

/// Per-file symbol spans, loaded once per file, for attributing a reference
/// to the innermost symbol whose span encloses it.
#[derive(Default)]
struct EnclosingSymbols {
    files: BTreeMap<String, Vec<SymbolSpanRow>>,
}

struct SymbolSpanRow {
    qualified: String,
    kind: String,
    span_start: i64,
    span_end: i64,
}

impl EnclosingSymbols {
    /// Selector for the innermost symbol in `file` whose span contains
    /// `[start, end)`, or `None` when the reference lies outside every symbol
    /// (for example a top-level statement).
    fn selector_for(
        &mut self,
        conn: &Connection,
        file: &str,
        start: i64,
        end: i64,
    ) -> Result<Option<String>, GraphError> {
        if !self.files.contains_key(file) {
            let mut stmt = conn
                .prepare_cached(
                    "SELECT qualified, kind, span_start, span_end FROM symbols
                     WHERE file_path = ?1
                     ORDER BY id",
                )
                .map_err(|source| GraphError::sqlite("prepare enclosing symbol lookup", source))?;
            let rows = stmt
                .query_map(params![file], |row| {
                    Ok(SymbolSpanRow {
                        qualified: row.get(0)?,
                        kind: row.get(1)?,
                        span_start: row.get(2)?,
                        span_end: row.get(3)?,
                    })
                })
                .map_err(|source| GraphError::sqlite("query enclosing symbols", source))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|source| GraphError::sqlite("collect enclosing symbols", source))?;
            self.files.insert(file.to_string(), rows);
        }
        let innermost = self.files.get(file).and_then(|symbols| {
            symbols
                .iter()
                .filter(|symbol| symbol.span_start <= start && symbol.span_end >= end)
                .min_by_key(|symbol| symbol.span_end - symbol.span_start)
        });
        Ok(innermost.map(|symbol| {
            super::symbol_selector(file, symbol.qualified.as_str(), symbol.kind.as_str())
        }))
    }
}

pub(crate) struct LineCache<'a> {
    worktree_root: &'a Path,
    files: BTreeMap<String, LineIndex>,
}

impl<'a> LineCache<'a> {
    pub(crate) fn new(worktree_root: &'a Path) -> Self {
        Self {
            worktree_root,
            files: BTreeMap::new(),
        }
    }

    pub(crate) fn line_for(&mut self, file: &str, byte_offset: i64) -> Result<usize, GraphError> {
        let offset = checked_offset(file, byte_offset)?;
        Ok(self.index(file)?.line_for(offset))
    }

    /// The trimmed source line containing `byte_offset`, bounded to
    /// [`REF_SNIPPET_MAX_CHARS`] characters with a trailing `…` when cut.
    pub(crate) fn snippet_for(
        &mut self,
        file: &str,
        byte_offset: i64,
    ) -> Result<String, GraphError> {
        let offset = checked_offset(file, byte_offset)?;
        Ok(self.index(file)?.snippet_for(offset, REF_SNIPPET_MAX_CHARS))
    }

    fn index(&mut self, file: &str) -> Result<&LineIndex, GraphError> {
        if !self.files.contains_key(file) {
            let path = super::contained_worktree_source(self.worktree_root, file)?;
            let bytes = fs::read(path.as_path()).map_err(|source| {
                GraphError::io("read source file for graph ref line", path, source)
            })?;
            self.files.insert(file.to_string(), LineIndex::new(bytes));
        }
        self.files.get(file).ok_or_else(|| {
            GraphError::invalid_data(
                "compute graph ref line",
                format!("line index missing for {file} after load"),
            )
        })
    }
}

fn checked_offset(file: &str, byte_offset: i64) -> Result<usize, GraphError> {
    if byte_offset < 0 {
        return Err(GraphError::invalid_data(
            "compute graph ref line",
            format!("negative byte offset {byte_offset} for {file}"),
        ));
    }
    usize::try_from(byte_offset)
        .map_err(|source| GraphError::invalid_data("compute graph ref line", source.to_string()))
}

struct LineIndex {
    bytes: Vec<u8>,
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
        Self { bytes, line_starts }
    }

    fn line_for(&self, byte_offset: usize) -> usize {
        let capped = byte_offset.min(self.bytes.len());
        self.line_starts
            .partition_point(|line_start| *line_start <= capped)
    }

    fn snippet_for(&self, byte_offset: usize, max_chars: usize) -> String {
        let line = self.line_for(byte_offset);
        let start = self.line_starts[line - 1];
        let end = self
            .line_starts
            .get(line)
            .map_or(self.bytes.len(), |next| next.saturating_sub(1));
        let text = String::from_utf8_lossy(&self.bytes[start..end.max(start)]);
        let text = text.trim();
        let mut snippet: String = text.chars().take(max_chars).collect();
        if text.chars().nth(max_chars).is_some() {
            snippet.push('…');
        }
        snippet
    }
}
