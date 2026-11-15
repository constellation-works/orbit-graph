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

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use orbit_graph_extract::history::TaskAssociation;

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
        Ok(())
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

/// One delivery in the window: its task's terms and its live labels.
#[derive(Debug, Clone, Default)]
pub(crate) struct WindowTask {
    pub(crate) task_id: String,
    pub(crate) delivery_id: String,
    pub(crate) terms: BTreeSet<String>,
    /// `file:` selectors of every live file the delivery changed.
    pub(crate) files: BTreeSet<String>,
    /// Symbol-level leaves: `symbol:` selectors, plus a `file:` fallback for a
    /// changed file with no live symbol, as the other variants emit them.
    pub(crate) leaves: BTreeMap<String, LocationMeta>,
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

    let files = context_scores(window, &active, options, |task| {
        Box::new(task.files.iter().map(|selector| (selector, None)))
    });
    let leaves = if level == RecommendationLevel::Symbol || options.symbol_boost > 1.0 {
        context_scores(window, &active, options, |task| {
            Box::new(
                task.leaves
                    .iter()
                    .map(|(selector, meta)| (selector, Some(meta))),
            )
        })
    } else {
        BTreeMap::new()
    };
    match level {
        RecommendationLevel::Symbol => scored = leaves,
        RecommendationLevel::File => {
            scored = files;
            let mut best: BTreeMap<String, (f64, &str)> = BTreeMap::new();
            for (selector, entry) in &leaves {
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

type Contexts<'a> = Box<dyn Iterator<Item = (&'a String, Option<&'a LocationMeta>)> + 'a>;

/// Score each context the `contexts` of a task name by the association of
/// every active query term with it.
fn context_scores<'a>(
    window: &'a [WindowTask],
    active: &BTreeMap<&str, usize>,
    options: &WordIndexOptions,
    contexts: impl Fn(&'a WindowTask) -> Contexts<'a>,
) -> BTreeMap<String, Accumulator> {
    let total = window.len() as f64;
    let mut touching: HashMap<&str, usize> = HashMap::new();
    let mut postings: BTreeMap<(&str, &str), Vec<usize>> = BTreeMap::new();
    let mut metas: HashMap<&str, &LocationMeta> = HashMap::new();
    for (index, task) in window.iter().enumerate() {
        let task_terms = task
            .terms
            .iter()
            .filter(|term| active.contains_key(term.as_str()))
            .collect::<Vec<_>>();
        for (selector, meta) in contexts(task) {
            *touching.entry(selector.as_str()).or_default() += 1;
            if let Some(meta) = meta {
                metas.entry(selector.as_str()).or_insert(meta);
            }
            for term in &task_terms {
                postings
                    .entry((term.as_str(), selector.as_str()))
                    .or_default()
                    .push(index);
            }
        }
    }
    let mut scored: BTreeMap<String, Accumulator> = BTreeMap::new();
    for ((term, selector), tasks) in postings {
        if tasks.len() < options.min_support {
            continue;
        }
        let term_tasks = active[term];
        let context_tasks = touching[selector];
        let expected = term_tasks as f64 * context_tasks as f64 / total;
        let contribution = association(tasks.len() as f64, expected, options.smoothing);
        if contribution <= 0.0 {
            continue;
        }
        let entry = scored.entry(selector.to_string()).or_default();
        entry.score += contribution;
        for index in &tasks {
            entry.tasks.insert(window[*index].task_id.clone());
            entry.deliveries.insert(window[*index].delivery_id.clone());
        }
        if let Some(meta) = metas.get(selector) {
            entry.file_fallback |= meta.file_fallback;
            if entry.fallback_reason.is_none() {
                entry.fallback_reason.clone_from(&meta.fallback_reason);
            }
        }
        entry.reasons.push(RecommendationReason {
            kind: "word_index_term".to_string(),
            contribution,
            explanation: format!(
                "term \"{term}\": {} of the {term_tasks} window task(s) with it touched this; {context_tasks} of {} touched it, so {expected:.3} expected",
                tasks.len(),
                window.len()
            ),
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
