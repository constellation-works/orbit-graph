//! Shared helpers for language extractors (dedup, path normalize, string filter).
//! Moved from rust.rs to avoid intra-crate duplication >50 LOC across c/markdown/config.
//! See task ORB-00305 comments for rationale.

use std::cell::Cell;
use std::ops::ControlFlow;
use std::path::Path;
use std::time::{Duration, Instant};

use tree_sitter::{ParseOptions, Parser, Tree};

use crate::{RawImport, RawRef, RawRelation, RawSymbol};

thread_local! {
    /// Deadline for tree-sitter parses on this thread, set by
    /// [`with_parse_timeout`].
    static PARSE_DEADLINE: Cell<Option<Instant>> = const { Cell::new(None) };
    /// Whether a parse on this thread was cancelled by [`PARSE_DEADLINE`].
    static PARSE_DEADLINE_EXCEEDED: Cell<bool> = const { Cell::new(false) };
}

/// Runs `extract` with every tree-sitter parse on this thread bounded by
/// `timeout`, and reports whether any parse was cancelled by it.
///
/// A cancelled parse yields no tree, so the extractor returns empty rows; the
/// caller must treat an exceeded deadline as an extraction failure rather
/// than store those rows.
pub fn with_parse_timeout<T>(timeout: Duration, extract: impl FnOnce() -> T) -> (T, bool) {
    let _scope = ParseDeadlineScope::enter(Instant::now().checked_add(timeout));
    let value = extract();
    (value, PARSE_DEADLINE_EXCEEDED.get())
}

/// Restores the thread's previous deadline state on drop, including unwind.
struct ParseDeadlineScope {
    deadline: Option<Instant>,
    exceeded: bool,
}

impl ParseDeadlineScope {
    fn enter(deadline: Option<Instant>) -> Self {
        Self {
            deadline: PARSE_DEADLINE.replace(deadline),
            exceeded: PARSE_DEADLINE_EXCEEDED.replace(false),
        }
    }
}

impl Drop for ParseDeadlineScope {
    fn drop(&mut self) {
        PARSE_DEADLINE.set(self.deadline);
        PARSE_DEADLINE_EXCEEDED.set(self.exceeded);
    }
}

/// Parses `source`, cancelling the parse once the thread's parse deadline
/// passes. tree-sitter checks progress every hundred or so parse operations.
pub(crate) fn parse_source(parser: &mut Parser, source: &str) -> Option<Tree> {
    let Some(deadline) = PARSE_DEADLINE.get() else {
        return parser.parse(source, None);
    };
    let bytes = source.as_bytes();
    let mut cancelled = false;
    let mut progress = |_: &tree_sitter::ParseState| {
        if Instant::now() >= deadline {
            cancelled = true;
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let tree = parser.parse_with_options(
        &mut |offset, _| bytes.get(offset..).unwrap_or_default(),
        None,
        Some(ParseOptions::new().progress_callback(&mut progress)),
    );
    if cancelled {
        PARSE_DEADLINE_EXCEEDED.set(true);
        return None;
    }
    tree
}

pub(crate) fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Filter for RawString / notable strings per spec §6.2: len>=6, not all ASCII punct, not pure format.
pub(crate) fn is_notable_string(value: &str) -> bool {
    if value.len() < 6 {
        return false;
    }
    let all_punct = value
        .chars()
        .all(|c| c.is_ascii_punctuation() || c.is_whitespace());
    if all_punct {
        return false;
    }
    // crude "pure format string" heuristic: contains {} or %s/%d etc but no letters outside
    let has_letters = value.chars().any(|c| c.is_ascii_alphabetic());
    if !has_letters && (value.contains("{}") || value.contains("%s") || value.contains("%d")) {
        return false;
    }
    true
}

pub(crate) fn dedup_symbols(symbols: &mut Vec<RawSymbol>) {
    symbols.sort_by(|left, right| {
        left.span_start
            .cmp(&right.span_start)
            .then_with(|| left.span_end.cmp(&right.span_end))
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.qualified.cmp(&right.qualified))
    });
    symbols.dedup_by(|left, right| {
        left.file_path == right.file_path
            && left.qualified == right.qualified
            && left.kind == right.kind
            && left.span_start == right.span_start
            && left.span_end == right.span_end
    });
}

pub(crate) fn dedup_refs(refs: &mut Vec<RawRef>) {
    refs.sort_by(|left, right| {
        left.from_span_start
            .cmp(&right.from_span_start)
            .then_with(|| left.from_span_end.cmp(&right.from_span_end))
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.target_name.cmp(&right.target_name))
    });
    refs.dedup_by(|left, right| {
        left.from_file == right.from_file
            && left.from_span_start == right.from_span_start
            && left.from_span_end == right.from_span_end
            && left.target_name == right.target_name
            && left.target_qualified == right.target_qualified
            && left.kind == right.kind
            && left.confidence == right.confidence
    });
}

pub(crate) fn dedup_relations(relations: &mut Vec<RawRelation>) {
    relations.sort_by(|left, right| {
        left.def_span_start
            .cmp(&right.def_span_start)
            .then_with(|| left.from_qualified.cmp(&right.from_qualified))
            .then_with(|| left.to_qualified.cmp(&right.to_qualified))
    });
    relations.dedup_by(|left, right| {
        left.from_qualified == right.from_qualified
            && left.to_qualified == right.to_qualified
            && left.kind == right.kind
            && left.def_file == right.def_file
            && left.def_span_start == right.def_span_start
            && left.def_span_end == right.def_span_end
    });
}

pub(crate) fn dedup_imports(imports: &mut Vec<RawImport>) {
    imports.sort_by(|left, right| {
        left.target_path
            .cmp(&right.target_path)
            .then_with(|| left.target_symbol.cmp(&right.target_symbol))
    });
    imports.dedup_by(|left, right| {
        left.from_file == right.from_file
            && left.target_path == right.target_path
            && left.target_symbol == right.target_symbol
    });
}
