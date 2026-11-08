//! Shared helpers for language extractors (dedup, path normalize, string filter).
//! Moved from rust.rs to avoid intra-crate duplication >50 LOC across c/markdown/config.
//! See task ORB-00305 comments for rationale.

use std::cell::Cell;
use std::ops::ControlFlow;
use std::path::Path;
use std::time::{Duration, Instant};

use tree_sitter::{Node, ParseOptions, Parser, Tree};

use crate::{RawImport, RawRef, RawRelation, RawSymbol};

/// Most native frames one file's AST walk may push.
///
/// This is the ~1–2k descent cap. It binds when each frame is tiny. The
/// stack budget below binds first on larger frames, which is what keeps a
/// walk inside a rayon worker's default 2 MiB stack.
pub(crate) const MAX_WALK_DEPTH: usize = 1536;

/// Most stack, measured from the first walker frame, one file's walk may use.
///
/// Rayon workers in pass 1 keep the default 2 MiB stack. Stopping around
/// 512 KiB of walker frames leaves room for the parse and the extractor
/// entry that are already on that stack. Real source stays far under this;
/// a generated nest of tens of thousands of nodes used to overflow the
/// worker and abort the process (`SIGSEGV` is not a panic).
pub(crate) const MAX_WALK_STACK: usize = 512 * 1024;

thread_local! {
    /// Deadline for tree-sitter parses on this thread, set by
    /// [`with_extraction_limits`].
    static PARSE_DEADLINE: Cell<Option<Instant>> = const { Cell::new(None) };
    /// Whether a parse on this thread was cancelled by [`PARSE_DEADLINE`].
    static PARSE_DEADLINE_EXCEEDED: Cell<bool> = const { Cell::new(false) };
    /// Walker frames currently on this thread. `0` means no walk is active.
    static WALK_DEPTH: Cell<usize> = const { Cell::new(0) };
    /// Stack address of the first walker frame, or `0` when no walk is active.
    static WALK_ORIGIN: Cell<usize> = const { Cell::new(0) };
    /// Whether a walk on this thread stopped at [`MAX_WALK_DEPTH`] or
    /// [`MAX_WALK_STACK`].
    static WALK_DEPTH_EXCEEDED: Cell<bool> = const { Cell::new(false) };
}

/// What [`with_extraction_limits`] observed while the extractor ran.
///
/// Either flag means the returned rows are not a successful extraction. The
/// caller records the file as a failure and does not store them, the same
/// way a cancelled parse is handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtractionLimits {
    /// A tree-sitter parse was cancelled at the deadline.
    pub parse_deadline_exceeded: bool,
    /// An AST walk stopped because it passed the depth cap or stack budget.
    pub walk_depth_exceeded: bool,
}

/// Runs `extract` with this thread's parse deadline and AST-walk budget.
///
/// A cancelled parse yields no tree, and a walk that passes the budget
/// returns without descending further. Either way the extractor's rows are
/// incomplete: the caller must treat the file as an extraction failure
/// rather than store them.
pub fn with_extraction_limits<T>(
    timeout: Duration,
    extract: impl FnOnce() -> T,
) -> (T, ExtractionLimits) {
    let _scope = ExtractionLimitScope::enter(Instant::now().checked_add(timeout));
    let value = extract();
    (
        value,
        ExtractionLimits {
            parse_deadline_exceeded: PARSE_DEADLINE_EXCEEDED.get(),
            walk_depth_exceeded: WALK_DEPTH_EXCEEDED.get(),
        },
    )
}

/// Runs `extract` with every tree-sitter parse on this thread bounded by
/// `timeout`, and reports whether any parse was cancelled by it.
///
/// The AST-walk budget is enforced too, but this function only reports the
/// parse deadline. Callers that store rows use [`with_extraction_limits`] so
/// a walk that stopped early is not stored as a successful file.
///
/// A cancelled parse yields no tree, so the extractor returns empty rows; the
/// caller must treat an exceeded deadline as an extraction failure rather
/// than store those rows.
pub fn with_parse_timeout<T>(timeout: Duration, extract: impl FnOnce() -> T) -> (T, bool) {
    let (value, limits) = with_extraction_limits(timeout, extract);
    (value, limits.parse_deadline_exceeded)
}

/// Restores the thread's previous limit state on drop, including unwind.
struct ExtractionLimitScope {
    deadline: Option<Instant>,
    parse_exceeded: bool,
    walk_depth: usize,
    walk_origin: usize,
    walk_exceeded: bool,
}

impl ExtractionLimitScope {
    fn enter(deadline: Option<Instant>) -> Self {
        Self {
            deadline: PARSE_DEADLINE.replace(deadline),
            parse_exceeded: PARSE_DEADLINE_EXCEEDED.replace(false),
            walk_depth: WALK_DEPTH.replace(0),
            walk_origin: WALK_ORIGIN.replace(0),
            walk_exceeded: WALK_DEPTH_EXCEEDED.replace(false),
        }
    }
}

impl Drop for ExtractionLimitScope {
    fn drop(&mut self) {
        PARSE_DEADLINE.set(self.deadline);
        PARSE_DEADLINE_EXCEEDED.set(self.parse_exceeded);
        WALK_DEPTH.set(self.walk_depth);
        WALK_ORIGIN.set(self.walk_origin);
        WALK_DEPTH_EXCEEDED.set(self.walk_exceeded);
    }
}

/// One walker frame. Depth and the stack origin return to their previous
/// values when this drops, including on unwind.
#[must_use = "the walker frame lasts until the guard drops"]
pub(crate) struct WalkGuard;

impl Drop for WalkGuard {
    fn drop(&mut self) {
        let depth = WALK_DEPTH.get().saturating_sub(1);
        WALK_DEPTH.set(depth);
        if depth == 0 {
            WALK_ORIGIN.set(0);
        }
    }
}

/// Occupies one walker frame.
///
/// `None` means this descent would pass [`MAX_WALK_DEPTH`] or
/// [`MAX_WALK_STACK`]. The caller returns without walking children, and the
/// file is an extraction failure.
pub(crate) fn enter_walk() -> Option<WalkGuard> {
    let here = stack_pointer();
    let origin = WALK_ORIGIN.get();
    let depth = WALK_DEPTH.get();
    if origin != 0 && (depth >= MAX_WALK_DEPTH || stack_used(origin, here) > MAX_WALK_STACK) {
        WALK_DEPTH_EXCEEDED.set(true);
        return None;
    }
    if origin == 0 {
        WALK_ORIGIN.set(here);
    }
    WALK_DEPTH.set(depth + 1);
    Some(WalkGuard)
}

/// Address of a local, so successive walker frames can see how far the
/// stack has moved. Linux stacks grow downward.
fn stack_pointer() -> usize {
    let marker = 0u8;
    std::hint::black_box(&marker as *const u8).addr()
}

fn stack_used(origin: usize, here: usize) -> usize {
    origin.saturating_sub(here)
}

/// Stops the current function before it descends past the walk budget.
///
/// `walk_limit!()` is for functions that return `()`. `walk_limit!(expr)`
/// returns `expr` instead (for example `None` or the node already in hand).
macro_rules! walk_limit {
    () => {
        let _walk_limit = match $crate::languages::common::enter_walk() {
            Some(guard) => guard,
            None => return,
        };
    };
    ($fallback:expr) => {
        let _walk_limit = match $crate::languages::common::enter_walk() {
            Some(guard) => guard,
            None => return $fallback,
        };
    };
}
pub(crate) use walk_limit;

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
            .then_with(|| left.source_symbol.cmp(&right.source_symbol))
            .then_with(|| left.reexport_module.cmp(&right.reexport_module))
    });
    imports.dedup_by(|left, right| {
        left.from_file == right.from_file
            && left.target_path == right.target_path
            && left.target_symbol == right.target_symbol
            && left.source_symbol == right.source_symbol
            && left.reexport_module == right.reexport_module
    });
}

/// Tree-sitter kinds whose text is not code, for dotted-call scans.
///
/// `with_holes` literals still contain real calls. Their span is excluded,
/// then each `holes` child is restored and walked so a nested literal stays
/// excluded.
#[derive(Clone, Copy)]
pub(crate) struct NonCodeKinds {
    pub opaque: &'static [&'static str],
    pub with_holes: &'static [&'static str],
    pub holes: &'static [&'static str],
}

/// Records `.identifier(` names in `range_start..range_end`.
///
/// A match whose `.` or name lies in a comment or string/character literal is
/// dropped. Interpolation holes stay eligible, which keeps a real dotted call
/// written inside a template.
pub(crate) fn collect_dotted_calls(
    source: &str,
    range_start: usize,
    range_end: usize,
    scope: Node,
    non_code: NonCodeKinds,
    mut push: impl FnMut(String, usize, usize),
) {
    let excluded = non_code_ranges(scope, non_code);
    let bytes = source.as_bytes();
    let mut index = range_start;
    while index < range_end {
        if let Some(end) = excluded_end(&excluded, index) {
            index = end;
            continue;
        }
        if bytes.get(index) != Some(&b'.') {
            index += 1;
            continue;
        }

        let mut name_start = index + 1;
        while name_start < range_end && bytes[name_start].is_ascii_whitespace() {
            name_start += 1;
        }
        if name_start >= range_end || !is_ident_start(bytes[name_start]) {
            index += 1;
            continue;
        }

        let mut name_end = name_start + 1;
        while name_end < range_end && is_ident_continue(bytes[name_end]) {
            name_end += 1;
        }

        let mut paren = name_end;
        while paren < range_end && bytes[paren].is_ascii_whitespace() {
            paren += 1;
        }
        if bytes.get(paren) == Some(&b'(')
            && let Some(name) = source.get(name_start..name_end)
            && (name_start..name_end).all(|byte| excluded_end(&excluded, byte).is_none())
        {
            push(name.to_string(), name_start, name_end);
        }
        index = name_end;
    }
}

fn non_code_ranges(scope: Node, non_code: NonCodeKinds) -> Vec<(usize, usize)> {
    let mut excluded = Vec::new();
    collect_non_code(scope, non_code, &mut excluded);
    merge_ranges(&mut excluded);
    excluded
}

fn collect_non_code(node: Node, non_code: NonCodeKinds, excluded: &mut Vec<(usize, usize)>) {
    walk_limit!();
    let kind = node.kind();
    if kind_listed(kind, non_code.opaque) {
        excluded.push((node.start_byte(), node.end_byte()));
        return;
    }
    if kind_listed(kind, non_code.with_holes) {
        excluded.push((node.start_byte(), node.end_byte()));
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if kind_listed(child.kind(), non_code.holes) {
                punch(excluded, child.start_byte(), child.end_byte());
                collect_non_code(child, non_code, excluded);
            }
        }
        return;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_non_code(child, non_code, excluded);
    }
}

fn punch(excluded: &mut Vec<(usize, usize)>, start: usize, end: usize) {
    if start >= end {
        return;
    }
    let mut next = Vec::with_capacity(excluded.len() + 1);
    for (left, right) in excluded.drain(..) {
        if right <= start || left >= end {
            next.push((left, right));
            continue;
        }
        if left < start {
            next.push((left, start));
        }
        if end < right {
            next.push((end, right));
        }
    }
    *excluded = next;
}

fn merge_ranges(ranges: &mut Vec<(usize, usize)>) {
    ranges.retain(|(start, end)| start < end);
    ranges.sort_unstable();
    let mut merged = Vec::with_capacity(ranges.len());
    for (start, end) in ranges.iter().copied() {
        if let Some((_, last_end)) = merged.last_mut()
            && start <= *last_end
        {
            *last_end = (*last_end).max(end);
        } else {
            merged.push((start, end));
        }
    }
    *ranges = merged;
}

fn excluded_end(ranges: &[(usize, usize)], index: usize) -> Option<usize> {
    let mut low = 0;
    let mut high = ranges.len();
    while low < high {
        let mid = low + (high - low) / 2;
        let (start, end) = ranges[mid];
        if index < start {
            high = mid;
        } else if index >= end {
            low = mid + 1;
        } else {
            return Some(end);
        }
    }
    None
}

fn kind_listed(kind: &str, kinds: &[&str]) -> bool {
    kinds.contains(&kind)
}

fn is_ident_start(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphabetic()
}

fn is_ident_continue(byte: u8) -> bool {
    is_ident_start(byte) || byte.is_ascii_digit()
}
