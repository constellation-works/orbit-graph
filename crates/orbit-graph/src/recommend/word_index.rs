//! The `word_index` ranking variant: an inverted index from task wording to
//! the files and symbols those tasks' deliveries changed.
//!
//! The index is built in memory for each request from the history store only:
//! the words of each eligible delivery's task text (the title by default) and
//! the live files and symbols that delivery changed. Nothing else is read: no
//! plan, execution summary, comment, commit message, tag or task type, and no
//! current-tree structure. A query is tokenised the same way, and every
//! context (file or symbol) is scored by the smoothed positive pointwise
//! mutual information between the query's terms and that context:
//!
//! ```text
//! E(w,c)     = df(w) · T(c) / N
//! assoc(w,c) = max(0, ln((t(w,c) + κ) / (E(w,c) + κ)))
//! score(c)   = Σ over query terms w of assoc(w,c)
//! file(F)    = score(F) + (b − 1) · max over symbols S in F of score(S)
//! ```
//!
//! The window is the newest `window_size` eligible deliveries before the
//! request's cutoff, rebuilt from the stored labels on every request; no count
//! is ever adjusted incrementally.
//!
//! In symbol-first mode (`symbol_first`, design §2.4) every changed line
//! belongs to its smallest enclosing named symbol or else to its file's
//! remainder, and each task-and-context record carries its added and deleted
//! line counts, its line count on each side and its status (added, modified
//! or deleted). Counts then become sums of a per-record weight `x(d,c)`
//! (design §5.4 and §5.5):
//!
//! ```text
//! t(w,c) = Σ over tasks d with w of x(d,c)     T(c) = Σ over tasks d of x(d,c)
//! presence         x = 1
//! symbol_fraction  x = changed / (len_before + len_after)
//! diff_share       x = changed / Σ over the task's contexts of α·changed
//! relative         x ← x / x̄(c),  x̄(c) = Σ x / n(c)
//! added_weight α   x ← α · x for a record whose task added the context
//! ```
//!
//! `min_support` and the evidence keep integer task counts under every
//! option. A remainder is scored but never returned and never boosts its
//! file; a file is scored from its own rolled-up record, not from its leaves.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use orbit_graph_extract::history::{
    FileChange, FileChangeKind, FileFallbackReason, TaskAssociation,
};

use super::{Accumulator, LocationMeta, RecommendationLevel, RecommendationReason};
use crate::GraphError;

/// Version of the word-index tokeniser. A change to splitting, folding or
/// stopwords that changes the terms of any text increments it.
pub const WORD_INDEX_TOKENIZER_VERSION: u32 = 1;

/// Default number of newest eligible deliveries in the window.
pub const WORD_INDEX_DEFAULT_WINDOW_SIZE: usize = 4_000;
/// Default symbol boost `b`; `1` means no boost.
pub const WORD_INDEX_DEFAULT_SYMBOL_BOOST: f64 = 1.5;
/// Default smallest posting (tasks with the term and the context) that scores.
pub const WORD_INDEX_DEFAULT_MIN_SUPPORT: usize = 2;
/// Default smoothing constant `κ`, counted in tasks.
pub const WORD_INDEX_DEFAULT_SMOOTHING: f64 = 1.0;
/// Default window-stopword share: a term in more than this share of the
/// window's tasks is dropped.
pub const WORD_INDEX_DEFAULT_MAX_TERM_SHARE: f64 = 0.3;

/// A delivery whose label holds more live files than this is left out of the
/// window: a sweep says nothing about any one word.
pub(crate) const WORD_INDEX_MAX_LABEL_FILES: usize = 20;

/// Which task text the word index reads, on the index and the query side
/// together.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WordIndexTextFields {
    /// The task title only.
    #[default]
    Title,
    /// The title, the description and every acceptance criterion.
    TitleDescriptionCriteria,
}

impl WordIndexTextFields {
    /// The text of `task` this setting reads.
    pub(crate) fn text(self, task: &TaskAssociation) -> String {
        match self {
            Self::Title => task.title.clone(),
            Self::TitleDescriptionCriteria => super::task_text(task),
        }
    }
}

/// How much one task's record of a context counts (design §5.4).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WordIndexPostingWeight {
    /// 1 for every task that touched the context.
    #[default]
    Presence,
    /// The share of the context's own lines the task changed:
    /// `changed / (len_before + len_after)`.
    SymbolFraction,
    /// The context's share of every line the task changed, after
    /// `added_weight`; a task's contexts at one level sum to 1.
    DiffShare,
}

impl WordIndexPostingWeight {
    /// The JSON name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Presence => "presence",
            Self::SymbolFraction => "symbol_fraction",
            Self::DiffShare => "diff_share",
        }
    }
}

/// What the smoothing constant `κ` is counted in under a weighted
/// `posting_weight` (design §5.4). Under `presence` both are identical.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WordIndexSmoothingUnit {
    /// The weights are summed as they are and `κ` stays constant.
    #[default]
    Tasks,
    /// Each context's weights are divided by their mean over the tasks that
    /// touched it before they are summed, so `T(c)` is a task count again.
    Relative,
}

impl WordIndexSmoothingUnit {
    /// The JSON name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Tasks => "tasks",
            Self::Relative => "relative",
        }
    }
}

/// Parameters of the `word_index` ranking variant. Every field has the
/// design's default, so `{}` is the default configuration.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WordIndexOptions {
    /// Task text indexed and queried. Defaults to the title.
    pub text_fields: WordIndexTextFields,
    /// Newest eligible deliveries in the window (`n`), at least 1.
    pub window_size: usize,
    /// Symbol boost `b`, finite and at least 1.
    pub symbol_boost: f64,
    /// Smallest posting that scores, at least 1.
    pub min_support: usize,
    /// Smoothing constant `κ`, finite and greater than 0.
    pub smoothing: f64,
    /// Window-stopword share, greater than 0 and at most 1.
    pub max_term_share: f64,
    /// Symbol-first labels (design §2.4): leaves are the smallest enclosing
    /// named symbols and each file's remainder, with line counts and status.
    /// Needs line lengths in the history store. Off, the labels are the live
    /// files and the live symbols they changed, counted by presence.
    ///
    /// This and the three options below are omitted from serialized options
    /// at their defaults, so options and corpora written before they existed
    /// serialize (and digest) unchanged.
    #[serde(skip_serializing_if = "is_false")]
    pub symbol_first: bool,
    /// Weight of one task's record of a context. A setting other than
    /// `presence` needs `symbol_first`.
    #[serde(skip_serializing_if = "is_presence")]
    pub posting_weight: WordIndexPostingWeight,
    /// Unit `κ` is counted in under a weighted `posting_weight`.
    #[serde(skip_serializing_if = "is_tasks")]
    pub smoothing_unit: WordIndexSmoothingUnit,
    /// `α`, from 0 to 1: the factor on the weight of a record whose task
    /// added the context. A value other than 1 needs `symbol_first`.
    #[serde(skip_serializing_if = "is_one")]
    pub added_weight: f64,
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes a reference"
)]
fn is_false(value: &bool) -> bool {
    !*value
}

fn is_presence(value: &WordIndexPostingWeight) -> bool {
    *value == WordIndexPostingWeight::Presence
}

fn is_tasks(value: &WordIndexSmoothingUnit) -> bool {
    *value == WordIndexSmoothingUnit::Tasks
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes a reference"
)]
fn is_one(value: &f64) -> bool {
    *value == 1.0
}

impl Default for WordIndexOptions {
    fn default() -> Self {
        Self {
            text_fields: WordIndexTextFields::Title,
            window_size: WORD_INDEX_DEFAULT_WINDOW_SIZE,
            symbol_boost: WORD_INDEX_DEFAULT_SYMBOL_BOOST,
            min_support: WORD_INDEX_DEFAULT_MIN_SUPPORT,
            smoothing: WORD_INDEX_DEFAULT_SMOOTHING,
            max_term_share: WORD_INDEX_DEFAULT_MAX_TERM_SHARE,
            symbol_first: false,
            posting_weight: WordIndexPostingWeight::Presence,
            smoothing_unit: WordIndexSmoothingUnit::Tasks,
            added_weight: 1.0,
        }
    }
}

impl WordIndexOptions {
    pub(crate) fn validate(&self) -> Result<(), GraphError> {
        let invalid = |field: &'static str, reason: String| {
            Err(GraphError::invalid_input(
                "validate word index options",
                field,
                reason,
            ))
        };
        if self.window_size == 0 {
            return invalid("window_size", "window_size must be at least 1".to_string());
        }
        if !self.symbol_boost.is_finite() || self.symbol_boost < 1.0 {
            return invalid(
                "symbol_boost",
                format!(
                    "symbol_boost must be finite and at least 1; got {}",
                    self.symbol_boost
                ),
            );
        }
        if self.min_support == 0 {
            return invalid("min_support", "min_support must be at least 1".to_string());
        }
        if !self.smoothing.is_finite() || self.smoothing <= 0.0 {
            return invalid(
                "smoothing",
                format!(
                    "smoothing must be finite and greater than 0; got {}",
                    self.smoothing
                ),
            );
        }
        if !self.max_term_share.is_finite()
            || self.max_term_share <= 0.0
            || self.max_term_share > 1.0
        {
            return invalid(
                "max_term_share",
                format!(
                    "max_term_share must be greater than 0 and at most 1; got {}",
                    self.max_term_share
                ),
            );
        }
        if !self.added_weight.is_finite() || !(0.0..=1.0).contains(&self.added_weight) {
            return invalid(
                "added_weight",
                format!(
                    "added_weight must be from 0 to 1; got {}",
                    self.added_weight
                ),
            );
        }
        if !self.symbol_first && self.posting_weight != WordIndexPostingWeight::Presence {
            return invalid(
                "posting_weight",
                format!(
                    "posting_weight {} needs symbol_first",
                    self.posting_weight.name()
                ),
            );
        }
        if !self.symbol_first && self.added_weight != 1.0 {
            return invalid(
                "added_weight",
                "added_weight other than 1 needs symbol_first".to_string(),
            );
        }
        Ok(())
    }

    /// The same options with `smoothing_unit` at `tasks` when the posting
    /// weight is `presence`, where the two units are identical.
    pub(crate) fn normalized(mut self) -> Self {
        if self.posting_weight == WordIndexPostingWeight::Presence {
            self.smoothing_unit = WordIndexSmoothingUnit::Tasks;
        }
        self
    }

    /// Whether every option of design §5.4 and §5.5 is at its baseline:
    /// `presence` and `added_weight` 1.
    fn baseline_weights(&self) -> bool {
        self.posting_weight == WordIndexPostingWeight::Presence && self.added_weight == 1.0
    }
}

/// How a `word_index` response was built, echoed with its results.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WordIndexSummary {
    /// Options the index was built and queried with.
    pub options: WordIndexOptions,
    /// Tokeniser version that produced every term.
    pub tokenizer_version: u32,
    /// Deliveries in the window (`N`).
    pub window_tasks: usize,
    /// Eligible deliveries left out of the window, by reason.
    pub excluded: BTreeMap<String, usize>,
    /// Terms of the query text.
    pub query_terms: Vec<String>,
    /// Query terms present in the window and not window stopwords.
    pub known_terms: Vec<String>,
    /// Query terms dropped as window stopwords.
    pub window_stopwords: Vec<String>,
    /// Why nothing scored, or `null` when something did:
    /// `no_window_tasks`, `no_query_terms`, `no_known_terms` or
    /// `below_min_support`.
    pub empty_reason: Option<String>,
}

/// Whether a task created, changed or removed a context (design §2.5).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum ContextStatus {
    /// Exists only after the delivery (a file added or copied).
    Added,
    /// Exists on both sides (a file modified, renamed or type-changed).
    #[default]
    Modified,
    /// Exists only before the delivery.
    Deleted,
}

impl ContextStatus {
    const fn index(self) -> usize {
        match self {
            Self::Added => 0,
            Self::Modified => 1,
            Self::Deleted => 2,
        }
    }
}

/// One task's record of one context: changed lines on each side, the
/// context's line count on each side (0 where it does not exist) and status.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ContextRecord {
    pub(crate) added: u32,
    pub(crate) deleted: u32,
    pub(crate) len_before: u32,
    pub(crate) len_after: u32,
    pub(crate) status: ContextStatus,
}

impl ContextRecord {
    /// Changed lines: `added + deleted`.
    pub(crate) fn changed(&self) -> u32 {
        self.added.saturating_add(self.deleted)
    }

    /// Fold `other`, a second record of the same context in one task, in.
    fn merge(&mut self, other: &Self) {
        self.added = self.added.saturating_add(other.added);
        self.deleted = self.deleted.saturating_add(other.deleted);
        self.len_before = self.len_before.max(other.len_before);
        self.len_after = self.len_after.max(other.len_after);
        if self.status != other.status {
            self.status = ContextStatus::Modified;
        }
    }
}

/// A leaf context of one task: a symbol (or, outside symbol-first mode, a
/// `file:` fallback) or a file's remainder.
#[derive(Debug, Clone, Default)]
pub(crate) struct Leaf {
    pub(crate) meta: LocationMeta,
    pub(crate) record: ContextRecord,
}

/// Prefix of a remainder's internal key: `remainder:<path>`. Never a selector.
pub(crate) const REMAINDER_PREFIX: &str = "remainder:";

/// One delivery in the window: its task's terms and its live labels.
#[derive(Debug, Clone, Default)]
pub(crate) struct WindowTask {
    pub(crate) task_id: String,
    pub(crate) delivery_id: String,
    pub(crate) terms: BTreeSet<String>,
    /// `file:` selectors of every live file the delivery changed, each with
    /// its rolled-up record.
    pub(crate) files: BTreeMap<String, ContextRecord>,
    /// Leaves: `symbol:` selectors; outside symbol-first mode also a `file:`
    /// fallback for a changed file with no live symbol, as the other variants
    /// emit them; in symbol-first mode also each file's remainder.
    pub(crate) leaves: BTreeMap<String, Leaf>,
    /// Changed lines of every leaf record of the task, live or not, by
    /// status: the `diff_share` total at the leaf level.
    pub(crate) leaf_changed: [u64; 3],
    /// Changed lines of every file record of the task, live or not, by
    /// status: the `diff_share` total at the file level.
    pub(crate) file_changed: [u64; 3],
}

impl WindowTask {
    /// Count `record` in the task's leaf-level `diff_share` total.
    pub(crate) fn count_leaf(&mut self, record: &ContextRecord) {
        self.leaf_changed[record.status.index()] += u64::from(record.changed());
    }

    /// Count `record` in the task's file-level `diff_share` total.
    pub(crate) fn count_file(&mut self, record: &ContextRecord) {
        self.file_changed[record.status.index()] += u64::from(record.changed());
    }

    /// Add a leaf, merging a second record that resolves to the same one.
    pub(crate) fn add_leaf(&mut self, key: String, leaf: Leaf) {
        match self.leaves.entry(key) {
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                entry.get_mut().record.merge(&leaf.record);
            }
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(leaf);
            }
        }
    }
}

/// The symbol-first label of one changed file (design §2.4): a record for
/// each changed symbol with significant changes, by its index in
/// `file.symbols`, the file's remainder and the file's rolled-up record.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FileLabel {
    pub(crate) symbols: Vec<(usize, ContextRecord)>,
    pub(crate) remainder: ContextRecord,
    pub(crate) file: ContextRecord,
}

/// The symbol-first label of `file`, or `None` when its payload predates
/// line lengths.
///
/// Whitespace-only changes are not changed lines. A delivery that changed no
/// line of the file (a pure rename, copy or mode change) gives it a file
/// record with no changed lines and no leaves: the extractor attributes every
/// line of such a file to its symbols by convention, which is not a change. A
/// binary side counts as one changed line of a one-line file.
pub(crate) fn file_label(file: &FileChange) -> Option<FileLabel> {
    let before_lines = file.before_lines.as_ref()?;
    let after_lines = file.after_lines.as_ref()?;
    let status = match file.kind {
        FileChangeKind::Added | FileChangeKind::Copied => ContextStatus::Added,
        FileChangeKind::Deleted => ContextStatus::Deleted,
        FileChangeKind::Modified
        | FileChangeKind::Renamed
        | FileChangeKind::TypeChanged
        | FileChangeKind::Unreadable => ContextStatus::Modified,
    };
    let before_ws = before_lines
        .whitespace_only_lines
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let after_ws = after_lines
        .whitespace_only_lines
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let binary = |fallback: Option<FileFallbackReason>, lines: u32| {
        fallback == Some(FileFallbackReason::Binary) && lines > 0
    };
    let binary_before = binary(file.before_fallback, before_lines.line_count);
    let binary_after = binary(file.after_fallback, after_lines.line_count);
    let lines_changed = file.additions + file.deletions > 0;
    let significant = |count: usize, whitespace: &BTreeSet<u32>| {
        u32::try_from(count.saturating_sub(whitespace.len())).unwrap_or(u32::MAX)
    };
    let mut label = FileLabel::default();
    let (mut attributed_deleted, mut attributed_added) = (0u32, 0u32);
    if lines_changed {
        for (index, change) in file.symbols.iter().enumerate() {
            let status = match (change.before.as_ref(), change.after.as_ref()) {
                (Some(_), Some(_)) => ContextStatus::Modified,
                (None, Some(_)) => ContextStatus::Added,
                (Some(_), None) => ContextStatus::Deleted,
                (None, None) => continue,
            };
            let mut record = ContextRecord {
                status,
                ..ContextRecord::default()
            };
            if let Some(before) = change.before.as_ref() {
                record.len_before = before.symbol.line_count()?;
                record.deleted = count_significant(&before.changed_lines, &before_ws);
            }
            if let Some(after) = change.after.as_ref() {
                record.len_after = after.symbol.line_count()?;
                record.added = count_significant(&after.changed_lines, &after_ws);
            }
            if record.changed() == 0 {
                continue;
            }
            attributed_deleted = attributed_deleted.saturating_add(record.deleted);
            attributed_added = attributed_added.saturating_add(record.added);
            label.symbols.push((index, record));
        }
    }
    let side_changed = |binary: bool, count: usize, whitespace: &BTreeSet<u32>| {
        if binary {
            1
        } else if lines_changed {
            significant(count, whitespace)
        } else {
            0
        }
    };
    let deleted = side_changed(binary_before, file.deletions, &before_ws);
    let added = side_changed(binary_after, file.additions, &after_ws);
    let remainder_len = |lines: &orbit_graph_extract::history::FileSideLines| {
        lines
            .line_count
            .saturating_sub(lines.named_symbol_covered_lines)
    };
    label.remainder = ContextRecord {
        added: added.saturating_sub(attributed_added),
        deleted: deleted.saturating_sub(attributed_deleted),
        len_before: remainder_len(before_lines),
        len_after: remainder_len(after_lines),
        status,
    };
    label.file = ContextRecord {
        added: attributed_added.saturating_add(label.remainder.added),
        deleted: attributed_deleted.saturating_add(label.remainder.deleted),
        len_before: before_lines.line_count,
        len_after: after_lines.line_count,
        status,
    };
    Some(label)
}

fn count_significant(lines: &[u32], whitespace: &BTreeSet<u32>) -> u32 {
    let count = lines
        .iter()
        .filter(|line| !whitespace.contains(line))
        .count();
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// Paths that never enter a label: lockfiles and changelogs are not chosen by
/// a task's wording.
pub(crate) fn excluded_path(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    matches!(
        name.as_str(),
        "cargo.lock"
            | "package-lock.json"
            | "yarn.lock"
            | "pnpm-lock.yaml"
            | "gemfile.lock"
            | "poetry.lock"
            | "go.sum"
            | "changelog.md"
    )
}

/// Ranked contexts plus the summary of how they were scored.
pub(crate) struct WordIndexOutcome {
    pub(crate) scored: BTreeMap<String, Accumulator>,
    pub(crate) summary: WordIndexSummary,
}

/// Score every context in `window` against `query_text`.
pub(crate) fn score(
    window: &[WindowTask],
    excluded: BTreeMap<String, usize>,
    query_text: &str,
    level: RecommendationLevel,
    options: &WordIndexOptions,
) -> WordIndexOutcome {
    let query_terms = terms(query_text);
    let total = window.len();
    let mut summary = WordIndexSummary {
        options: *options,
        tokenizer_version: WORD_INDEX_TOKENIZER_VERSION,
        window_tasks: total,
        excluded,
        query_terms: query_terms.iter().cloned().collect(),
        known_terms: Vec::new(),
        window_stopwords: Vec::new(),
        empty_reason: None,
    };
    let mut scored = BTreeMap::new();
    if total == 0 {
        summary.empty_reason = Some("no_window_tasks".to_string());
        return WordIndexOutcome { scored, summary };
    }
    if query_terms.is_empty() {
        summary.empty_reason = Some("no_query_terms".to_string());
        return WordIndexOutcome { scored, summary };
    }
    let mut df: BTreeMap<&str, usize> = BTreeMap::new();
    for task in window {
        for term in task.terms.intersection(&query_terms) {
            *df.entry(term.as_str()).or_default() += 1;
        }
    }
    let mut active = BTreeMap::new();
    for (term, count) in df {
        if count as f64 > options.max_term_share * total as f64 {
            summary.window_stopwords.push(term.to_string());
        } else {
            active.insert(term, count);
        }
    }
    summary.known_terms = active.keys().map(|term| (*term).to_string()).collect();
    if active.is_empty() {
        summary.empty_reason = Some("no_known_terms".to_string());
        return WordIndexOutcome { scored, summary };
    }

    let files = context_scores(
        window,
        &active,
        options,
        |task| {
            task.files
                .iter()
                .map(|(selector, record)| (selector, None, record))
                .collect()
        },
        |task| task.file_changed,
    );
    let leaves = if level == RecommendationLevel::Symbol || options.symbol_boost > 1.0 {
        context_scores(
            window,
            &active,
            options,
            |task| {
                task.leaves
                    .iter()
                    .map(|(selector, leaf)| (selector, Some(&leaf.meta), &leaf.record))
                    .collect()
            },
            |task| task.leaf_changed,
        )
    } else {
        BTreeMap::new()
    };
    match level {
        RecommendationLevel::Symbol => {
            scored = leaves;
            scored.retain(|selector, _| !selector.starts_with(REMAINDER_PREFIX));
        }
        RecommendationLevel::File => {
            scored = files;
            let mut best: BTreeMap<String, (f64, &str)> = BTreeMap::new();
            for (selector, entry) in &leaves {
                if let Some(path) = selector.strip_prefix(REMAINDER_PREFIX) {
                    // A remainder is scored but earns no boost: shown as
                    // evidence on its file only.
                    if entry.score > 0.0
                        && let Some(file) = scored.get_mut(&format!("file:{path}"))
                    {
                        file.reasons.push(RecommendationReason {
                            kind: "word_index_remainder".to_string(),
                            contribution: 0.0,
                            explanation: format!(
                                "the file's changed lines outside any named symbol score {:.3}; a remainder earns no symbol boost",
                                entry.score
                            ),
                        });
                    }
                    continue;
                }
                let Some((path, _, _)) = super::split_symbol_selector(selector) else {
                    continue;
                };
                let file = format!("file:{path}");
                let replace = best.get(&file).is_none_or(|(score, current)| {
                    entry.score > *score || (entry.score == *score && selector.as_str() < *current)
                });
                if replace {
                    best.insert(file, (entry.score, selector.as_str()));
                }
            }
            let boost = options.symbol_boost - 1.0;
            for (file, (symbol_score, symbol)) in best {
                let contribution = boost * symbol_score;
                if contribution <= 0.0 {
                    continue;
                }
                let symbol_entry = &leaves[symbol];
                let entry = scored.entry(file).or_default();
                entry.score += contribution;
                entry.tasks.extend(symbol_entry.tasks.iter().cloned());
                entry
                    .deliveries
                    .extend(symbol_entry.deliveries.iter().cloned());
                entry.reasons.push(RecommendationReason {
                    kind: "word_index_symbol_boost".to_string(),
                    contribution,
                    explanation: format!(
                        "(b - 1) = {boost:.3} times the best symbol in this file, {symbol} ({symbol_score:.3})"
                    ),
                });
            }
        }
    }
    if !scored.values().any(|entry| entry.score > 0.0) {
        summary.empty_reason = Some("below_min_support".to_string());
    }
    WordIndexOutcome { scored, summary }
}

type Contexts<'a> = Vec<(&'a String, Option<&'a LocationMeta>, &'a ContextRecord)>;

/// One context's records across the window.
#[derive(Default)]
struct ContextMass<'a> {
    meta: Option<&'a LocationMeta>,
    /// `(task index, weight before relative and α, whether it added the context)`.
    records: Vec<(usize, f64, bool)>,
    /// `T(c)`: the sum of the final weights.
    mass: f64,
    /// Tasks that added the context.
    tasks_added: usize,
}

/// A record's weight before `smoothing_unit` and `added_weight` act on it.
/// `task_total` is the task's changed lines at this level after `α`.
fn base_weight(record: &ContextRecord, options: &WordIndexOptions, task_total: f64) -> f64 {
    let changed = f64::from(record.changed());
    match options.posting_weight {
        WordIndexPostingWeight::Presence => 1.0,
        WordIndexPostingWeight::SymbolFraction => {
            let length = f64::from(record.len_before) + f64::from(record.len_after);
            if changed == 0.0 {
                0.0
            } else if length <= 0.0 {
                1.0
            } else {
                (changed / length).min(1.0)
            }
        }
        WordIndexPostingWeight::DiffShare => {
            if task_total > 0.0 {
                changed / task_total
            } else {
                0.0
            }
        }
    }
}

/// Score each context the `contexts` of a task name by the association of
/// every active query term with it. `totals` gives a task's changed lines at
/// this level by status (added, modified, deleted), for `diff_share`.
fn context_scores<'a>(
    window: &'a [WindowTask],
    active: &BTreeMap<&str, usize>,
    options: &WordIndexOptions,
    contexts: impl Fn(&'a WindowTask) -> Contexts<'a>,
    totals: impl Fn(&WindowTask) -> [u64; 3],
) -> BTreeMap<String, Accumulator> {
    let total = window.len() as f64;
    let alpha = options.added_weight;
    let relative = options.posting_weight != WordIndexPostingWeight::Presence
        && options.smoothing_unit == WordIndexSmoothingUnit::Relative;
    let mut by_context: HashMap<&str, ContextMass<'a>> = HashMap::new();
    for (index, task) in window.iter().enumerate() {
        let [added, modified, deleted] = totals(task);
        let task_total = alpha * added as f64 + modified as f64 + deleted as f64;
        for (selector, meta, record) in contexts(task) {
            let entry = by_context.entry(selector.as_str()).or_default();
            if entry.meta.is_none() {
                entry.meta = meta;
            }
            let was_added = record.status == ContextStatus::Added;
            entry.tasks_added += usize::from(was_added);
            entry
                .records
                .push((index, base_weight(record, options, task_total), was_added));
        }
    }
    // Final weights: relative to the context's mean, then scaled by α.
    for entry in by_context.values_mut() {
        let mean = if relative {
            entry.records.iter().map(|record| record.1).sum::<f64>() / entry.records.len() as f64
        } else {
            1.0
        };
        for record in &mut entry.records {
            let relative_weight = if mean > 0.0 { record.1 / mean } else { 0.0 };
            record.1 = if record.2 {
                alpha * relative_weight
            } else {
                relative_weight
            };
        }
        entry.mass = entry.records.iter().map(|record| record.1).sum();
    }
    // Postings: (term, context) → (mass, task indices, tasks that added it).
    let mut postings: BTreeMap<(&str, &str), (f64, Vec<usize>, usize)> = BTreeMap::new();
    for (selector, entry) in &by_context {
        for &(index, weight, was_added) in &entry.records {
            for term in &window[index].terms {
                if !active.contains_key(term.as_str()) {
                    continue;
                }
                let posting = postings.entry((term.as_str(), selector)).or_default();
                posting.0 += weight;
                posting.1.push(index);
                posting.2 += usize::from(was_added);
            }
        }
    }
    let plain = !options.symbol_first && options.baseline_weights();
    let mut scored: BTreeMap<String, Accumulator> = BTreeMap::new();
    for ((term, selector), (mass, mut tasks, tasks_added)) in postings {
        if tasks.len() < options.min_support {
            continue;
        }
        tasks.sort_unstable();
        let context = &by_context[selector];
        let term_tasks = active[term];
        let expected = term_tasks as f64 * context.mass / total;
        let contribution = association(mass, expected, options.smoothing);
        if contribution <= 0.0 {
            continue;
        }
        let entry = scored.entry(selector.to_string()).or_default();
        entry.score += contribution;
        for index in &tasks {
            entry.tasks.insert(window[*index].task_id.clone());
            entry.deliveries.insert(window[*index].delivery_id.clone());
        }
        if let Some(meta) = context.meta {
            entry.file_fallback |= meta.file_fallback;
            if entry.fallback_reason.is_none() {
                entry.fallback_reason.clone_from(&meta.fallback_reason);
            }
        }
        let context_tasks = context.records.len();
        let explanation = if plain {
            format!(
                "term \"{term}\": {} of the {term_tasks} window task(s) with it touched this; {context_tasks} of {} touched it, so {expected:.3} expected",
                tasks.len(),
                window.len()
            )
        } else {
            format!(
                "term \"{term}\": {} of the {term_tasks} window task(s) with it touched this ({tasks_added} adding it), weighing {mass:.3}; {context_tasks} of {} touched it (modified by {}, created by {}), weighing {:.3}, so {expected:.3} expected",
                tasks.len(),
                window.len(),
                context_tasks - context.tasks_added,
                context.tasks_added,
                context.mass,
            )
        };
        entry.reasons.push(RecommendationReason {
            kind: "word_index_term".to_string(),
            contribution,
            explanation,
        });
    }
    scored
}

/// Smoothed positive pointwise mutual information of one posting:
/// `max(0, ln((observed + κ) / (expected + κ)))`.
pub(crate) fn association(observed: f64, expected: f64, smoothing: f64) -> f64 {
    ((observed + smoothing) / (expected + smoothing))
        .ln()
        .max(0.0)
}

/// Fixed English stopwords. Domain boilerplate is left to the window rule.
const STOPWORDS: &[&str] = &[
    "a", "about", "after", "again", "all", "also", "an", "and", "any", "are", "as", "at", "be",
    "because", "been", "before", "being", "between", "both", "but", "by", "can", "could", "did",
    "do", "does", "doing", "done", "each", "either", "for", "from", "had", "has", "have", "having",
    "he", "her", "here", "his", "how", "i", "if", "in", "into", "is", "it", "its", "itself",
    "just", "may", "me", "might", "more", "most", "must", "my", "no", "nor", "not", "now", "of",
    "off", "on", "once", "only", "or", "other", "our", "out", "over", "own", "per", "same", "she",
    "should", "so", "some", "such", "than", "that", "the", "their", "them", "then", "there",
    "these", "they", "this", "those", "through", "to", "too", "under", "until", "up", "upon", "us",
    "very", "via", "was", "we", "were", "what", "when", "where", "whether", "which", "while",
    "who", "whom", "why", "will", "with", "within", "without", "would", "yet", "you", "your",
];

/// File extensions dropped from a token that is not a path.
const EXTENSIONS: &[&str] = &[
    "c", "cc", "cpp", "cs", "css", "go", "h", "hpp", "html", "java", "js", "json", "jsx", "kt",
    "lock", "md", "py", "rb", "rs", "scss", "sh", "sql", "swift", "toml", "ts", "tsx", "txt",
    "yaml", "yml",
];

/// Characters that join the parts of one identifier or path.
fn is_joiner(ch: char) -> bool {
    matches!(ch, '_' | '-' | '.' | '/' | ':')
}

/// The set of index terms in `text` (tokeniser version
/// [`WORD_INDEX_TOKENIZER_VERSION`]).
///
/// 1. Split on whitespace and punctuation other than identifier joiners
///    (`_ - . / :`); drop URLs, task IDs (`ORB-123`), hexadecimal runs of
///    seven or more characters with a digit, and bare numbers.
/// 2. Split identifiers at joiners and case changes, keeping the parts and
///    the lowercased whole; a path keeps its directory-free stem as the whole.
///    File extensions are dropped.
/// 3. Lowercase and fold a trailing plural `s`.
/// 4. Drop terms shorter than 2 or longer than 40 characters and fixed
///    English stopwords. Window stopwords are applied by the caller.
pub(crate) fn terms(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    each_term(text, &mut |term| {
        out.insert(term);
    });
    out
}

/// Every term of `text` in order, repeats kept: the same tokeniser as
/// [`terms`], for consumers that weigh term frequency (the evaluation's BM25
/// baseline).
pub(crate) fn term_sequence(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    each_term(text, &mut |term| out.push(term));
    out
}

fn each_term(text: &str, out: &mut impl FnMut(String)) {
    for raw in text.split(|ch: char| !(ch.is_alphanumeric() || is_joiner(ch))) {
        let token = raw.trim_matches(is_joiner);
        if token.is_empty()
            || raw.contains("://")
            || token.to_ascii_lowercase().starts_with("www.")
            || is_task_id(token)
        {
            continue;
        }
        add_token(token, out);
    }
}

fn add_token(token: &str, out: &mut impl FnMut(String)) {
    let is_path = token.contains('/');
    let token = strip_extension(token, is_path);
    let parts = split_parts(token);
    for part in &parts {
        push_term(part, out);
    }
    if parts.len() > 1 {
        if is_path {
            let stem = token.rsplit('/').next().unwrap_or(token);
            if !stem.is_empty() {
                push_term(stem, out);
            }
        } else {
            push_term(token, out);
        }
    }
}

/// `token` without a trailing file extension: always for a path, otherwise
/// only for a known extension.
fn strip_extension(token: &str, is_path: bool) -> &str {
    let name_start = token.rfind('/').map_or(0, |index| index + 1);
    let Some(dot) = token[name_start..]
        .rfind('.')
        .map(|index| name_start + index)
    else {
        return token;
    };
    let extension = &token[dot + 1..];
    let known = EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str());
    let plausible = !extension.is_empty()
        && extension.len() <= 5
        && extension.chars().all(|ch| ch.is_ascii_alphanumeric());
    if dot > name_start && plausible && (known || is_path) {
        &token[..dot]
    } else {
        token
    }
}

/// Parts of an identifier, split at joiners and case changes
/// (`widenContextFiles` → `widen`, `Context`, `Files`; `HTTPServer` →
/// `HTTP`, `Server`).
fn split_parts(token: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let chars = token.char_indices().collect::<Vec<_>>();
    let mut start = None;
    for (position, &(index, ch)) in chars.iter().enumerate() {
        if is_joiner(ch) {
            if let Some(begin) = start.take() {
                parts.push(&token[begin..index]);
            }
            continue;
        }
        if let Some(begin) = start
            && position > 0
        {
            let previous = chars[position - 1].1;
            let next = chars.get(position + 1).map(|&(_, next)| next);
            let boundary = ch.is_uppercase()
                && (previous.is_lowercase()
                    || previous.is_ascii_digit()
                    || (previous.is_uppercase() && next.is_some_and(char::is_lowercase)));
            if boundary {
                parts.push(&token[begin..index]);
                start = Some(index);
            }
        }
        if start.is_none() {
            start = Some(index);
        }
    }
    if let Some(begin) = start {
        parts.push(&token[begin..]);
    }
    parts
}

fn push_term(value: &str, out: &mut impl FnMut(String)) {
    let lower = value.to_lowercase();
    if STOPWORDS.contains(&lower.as_str()) || is_number(&lower) || is_hex_run(&lower) {
        return;
    }
    let folded = fold_plural(lower);
    let length = folded.chars().count();
    if !(2..=40).contains(&length) || STOPWORDS.contains(&folded.as_str()) {
        return;
    }
    out(folded);
}

/// Fold a trailing plural `s` (`leases` → `lease`), leaving `-ss`, `-us` and
/// `-is` endings (`class`, `status`, `analysis`) alone.
fn fold_plural(mut term: String) -> String {
    if term.chars().count() >= 4
        && term.ends_with('s')
        && !term.ends_with("ss")
        && !term.ends_with("us")
        && !term.ends_with("is")
    {
        term.pop();
    }
    term
}

fn is_number(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|ch| ch.is_ascii_digit())
}

fn is_hex_run(value: &str) -> bool {
    value.len() >= 7
        && value.chars().all(|ch| ch.is_ascii_hexdigit())
        && value.chars().any(|ch| ch.is_ascii_digit())
}

/// `ABC-123`: an uppercase prefix starting with a letter, a hyphen, digits.
fn is_task_id(token: &str) -> bool {
    let Some((prefix, number)) = token.split_once('-') else {
        return false;
    };
    prefix
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_uppercase())
        && prefix
            .chars()
            .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit())
        && is_number(number)
}
