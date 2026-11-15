//! Baselines the chronological evaluation scores beside the `recommend`
//! variants. Neither reads history:
//!
//! - **BM25 over code** ranks the files (path and content) and, separately,
//!   the symbols (name, signature, body) of one tree with SQLite FTS5's
//!   `bm25`, the task text as the query.
//! - **Explicit mentions** are the paths and symbol names a task's text
//!   writes that resolve in the tree it was written against.
//!
//! Both read text through the word index's tokeniser, so a term means the
//! same thing to every ranking the evaluation compares.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use git2::{ObjectType, Oid, Repository, TreeWalkMode, TreeWalkResult};
use orbit_graph_extract::RawSymbol;
use orbit_graph_extract::languages;
use rusqlite::{Connection, params};

use crate::recommend::{excluded_path, split_symbol_selector, term_sequence};
use crate::{GraphError, RecommendationLevel};

/// A blob larger than this is not a BM25 document: generated or vendored
/// content, not code a task names.
pub(super) const BM25_MAX_DOCUMENT_BYTES: usize = 1 << 20;

/// A written symbol name matching more symbols than this resolves to none:
/// it does not name one site.
pub(super) const MENTION_MAX_SYMBOL_MATCHES: usize = 5;

/// The blobs of one tree, and the symbols the language extractors find in
/// them. Extraction matches `recommend`'s target tree, so a selector is live
/// here exactly when `recommend` would emit it at this revision.
pub(super) struct CodeTree {
    revision: Oid,
    files: BTreeMap<String, Oid>,
    symbols: BTreeMap<String, Vec<RawSymbol>>,
    /// Short symbol name → `(path, index into symbols[path])`.
    by_name: HashMap<String, Vec<(String, usize)>>,
}

impl CodeTree {
    /// Walk the tree of `revision` and extract every supported file's symbols.
    pub(super) fn load(repo: &Repository, revision: Oid) -> Result<Self, GraphError> {
        let tree = repo
            .find_commit(revision)
            .and_then(|commit| commit.tree())
            .map_err(|error| GraphError::git("load evaluation code tree", error))?;
        let mut files = BTreeMap::new();
        tree.walk(TreeWalkMode::PreOrder, |root, entry| {
            if entry.kind() == Some(ObjectType::Blob)
                && let Ok(name) = entry.name()
            {
                files.insert(format!("{root}{name}"), entry.id());
            }
            TreeWalkResult::Ok
        })
        .map_err(|error| GraphError::git("walk evaluation code tree", error))?;
        let extractors = languages::extractors();
        let mut symbols = BTreeMap::new();
        for (path, oid) in &files {
            let path_ref = Path::new(path);
            let Some(extractor) = extractors
                .iter()
                .find(|extractor| extractor.supports(path_ref))
            else {
                continue;
            };
            let blob = repo
                .find_blob(*oid)
                .map_err(|error| GraphError::git("read evaluation code blob", error))?;
            if blob.content().contains(&0) {
                continue;
            }
            let extracted = extractor.extract(path_ref, blob.content());
            if !extracted.symbols.is_empty() {
                symbols.insert(path.clone(), extracted.symbols);
            }
        }
        let mut by_name: HashMap<String, Vec<(String, usize)>> = HashMap::new();
        for (path, list) in &symbols {
            for (index, symbol) in list.iter().enumerate() {
                by_name
                    .entry(symbol.name.clone())
                    .or_default()
                    .push((path.clone(), index));
            }
        }
        Ok(Self {
            revision,
            files,
            symbols,
            by_name,
        })
    }

    /// The commit this tree was loaded from.
    pub(super) fn revision(&self) -> Oid {
        self.revision
    }

    /// Whether `selector` names a file or symbol of this tree.
    pub(super) fn is_live(&self, selector: &str) -> bool {
        if let Some(path) = selector.strip_prefix("file:") {
            return self.files.contains_key(path);
        }
        let Some((path, qualified, kind)) = split_symbol_selector(selector) else {
            return false;
        };
        self.symbols.get(path).is_some_and(|symbols| {
            symbols
                .iter()
                .any(|symbol| symbol.qualified == qualified && symbol.kind == kind)
        })
    }
}

fn symbol_selector(path: &str, symbol: &RawSymbol) -> String {
    format!("symbol:{path}#{}:{}", symbol.qualified, symbol.kind)
}

/// Space-joined terms, in order with repeats: what FTS5 indexes and counts.
fn indexed_text(text: &str) -> String {
    term_sequence(text).join(" ")
}

/// FTS5 tokeniser for pre-tokenised text: whitespace separates terms, and the
/// identifier joiners the word-index tokeniser keeps inside a term stay in it.
const FTS_TOKENIZER: &str = "unicode61 remove_diacritics 0 tokenchars '_-.:'";

/// An in-memory FTS5 index over one tree: one table of file documents, one
/// of symbol documents.
pub(super) struct Bm25Index {
    connection: Connection,
    /// File documents indexed.
    pub(super) file_documents: usize,
    /// Symbol documents indexed.
    pub(super) symbol_documents: usize,
    /// Blobs left out of the file table, by reason.
    pub(super) skipped: BTreeMap<String, usize>,
}

impl Bm25Index {
    /// Index every text blob of `tree` and every symbol extracted from it.
    pub(super) fn build(repo: &Repository, tree: &CodeTree) -> Result<Self, GraphError> {
        let connection = Connection::open_in_memory()
            .map_err(|error| GraphError::sqlite("open evaluation BM25 index", error))?;
        connection
            .execute_batch(
                format!(
                    "CREATE VIRTUAL TABLE file_docs USING fts5(selector UNINDEXED, path, content, tokenize = \"{FTS_TOKENIZER}\");
                     CREATE VIRTUAL TABLE symbol_docs USING fts5(selector UNINDEXED, name, signature, body, tokenize = \"{FTS_TOKENIZER}\");"
                )
                .as_str(),
            )
            .map_err(|error| GraphError::sqlite("create evaluation BM25 tables", error))?;
        let mut index = Self {
            connection,
            file_documents: 0,
            symbol_documents: 0,
            skipped: BTreeMap::new(),
        };
        let transaction = index
            .connection
            .unchecked_transaction()
            .map_err(|error| GraphError::sqlite("begin evaluation BM25 load", error))?;
        {
            let mut insert_file = transaction
                .prepare("INSERT INTO file_docs (selector, path, content) VALUES (?1, ?2, ?3)")
                .map_err(|error| GraphError::sqlite("prepare BM25 file insert", error))?;
            let mut insert_symbol = transaction
                .prepare(
                    "INSERT INTO symbol_docs (selector, name, signature, body) VALUES (?1, ?2, ?3, ?4)",
                )
                .map_err(|error| GraphError::sqlite("prepare BM25 symbol insert", error))?;
            for (path, oid) in &tree.files {
                if excluded_path(path) {
                    *index
                        .skipped
                        .entry("excluded_path".to_string())
                        .or_default() += 1;
                    continue;
                }
                let blob = repo
                    .find_blob(*oid)
                    .map_err(|error| GraphError::git("read BM25 document blob", error))?;
                let content = blob.content();
                let reason = if content.contains(&0) {
                    Some("binary")
                } else if content.len() > BM25_MAX_DOCUMENT_BYTES {
                    Some("larger_than_1_mib")
                } else {
                    None
                };
                if let Some(reason) = reason {
                    *index.skipped.entry(reason.to_string()).or_default() += 1;
                    continue;
                }
                let text = String::from_utf8_lossy(content);
                insert_file
                    .execute(params![
                        format!("file:{path}"),
                        indexed_text(path),
                        indexed_text(text.as_ref())
                    ])
                    .map_err(|error| GraphError::sqlite("insert BM25 file document", error))?;
                index.file_documents += 1;
                for symbol in tree.symbols.get(path).into_iter().flatten() {
                    let body = content
                        .get(symbol.span_start..symbol.span_end.min(content.len()))
                        .map(String::from_utf8_lossy)
                        .unwrap_or_default();
                    insert_symbol
                        .execute(params![
                            symbol_selector(path, symbol),
                            indexed_text(format!("{} {}", symbol.name, symbol.qualified).as_str()),
                            indexed_text(symbol.signature.as_deref().unwrap_or_default()),
                            indexed_text(body.as_ref())
                        ])
                        .map_err(|error| {
                            GraphError::sqlite("insert BM25 symbol document", error)
                        })?;
                    index.symbol_documents += 1;
                }
            }
        }
        transaction
            .commit()
            .map_err(|error| GraphError::sqlite("commit evaluation BM25 load", error))?;
        Ok(index)
    }

    /// The best `limit` documents of `level` for `query_text`, by FTS5
    /// `bm25` (best first, ties by selector). Any query term matches; a
    /// query with no terms ranks nothing.
    pub(super) fn rank(
        &self,
        level: RecommendationLevel,
        query_text: &str,
        limit: usize,
    ) -> Result<Vec<String>, GraphError> {
        let terms = term_sequence(query_text)
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|term| format!("\"{term}\""))
            .collect::<Vec<_>>();
        if terms.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let table = match level {
            RecommendationLevel::File => "file_docs",
            RecommendationLevel::Symbol => "symbol_docs",
        };
        let mut statement = self
            .connection
            .prepare(
                format!(
                    "SELECT selector FROM {table} WHERE {table} MATCH ?1 ORDER BY bm25({table}), selector LIMIT ?2"
                )
                .as_str(),
            )
            .map_err(|error| GraphError::sqlite("prepare BM25 query", error))?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = statement
            .query_map(params![terms.join(" OR "), limit], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|error| GraphError::sqlite("run BM25 query", error))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| GraphError::sqlite("read BM25 results", error))
    }
}

/// Explicit mentions of one task text, best first: the order they are
/// written in.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct Mentions {
    /// `file:` selectors of mentioned paths, and of the files of mentioned
    /// symbols.
    pub(super) files: Vec<String>,
    /// `symbol:` selectors of mentioned symbols.
    pub(super) symbols: Vec<String>,
}

impl Mentions {
    pub(super) fn level(&self, level: RecommendationLevel) -> &[String] {
        match level {
            RecommendationLevel::File => self.files.as_slice(),
            RecommendationLevel::Symbol => self.symbols.as_slice(),
        }
    }

    fn push_file(&mut self, path: &str) {
        let selector = format!("file:{path}");
        if !self.files.contains(&selector) {
            self.files.push(selector);
        }
    }

    fn push_symbol(&mut self, path: &str, symbol: &RawSymbol) {
        self.push_file(path);
        let selector = symbol_selector(path, symbol);
        if !self.symbols.contains(&selector) {
            self.symbols.push(selector);
        }
    }
}

/// The paths and symbol names `text` writes that resolve in `tree`.
///
/// A token is read as a path when it has a `/` or a file extension: it
/// resolves to the file of that exact path, or else to the one file whose
/// path ends with it at a `/`. `file:` and `symbol:` selectors, `./`
/// prefixes and `:line` suffixes are accepted. Any other token is read as a
/// symbol name when it is shaped like an identifier in code (it has a `_`, a
/// `::` or `.` qualifier, or an uppercase letter after its first character),
/// so ordinary words never match: it resolves to every symbol with that
/// short name (and, when qualified, a qualified name ending with it), unless
/// more than [`MENTION_MAX_SYMBOL_MATCHES`] symbols match.
pub(super) fn explicit_mentions(text: &str, tree: &CodeTree) -> Mentions {
    let mut mentions = Mentions::default();
    for raw in text.split(|ch: char| !(ch.is_alphanumeric() || "_-./:#<>".contains(ch))) {
        let token = raw.trim_matches(|ch: char| ".:-#<>".contains(ch));
        if token.is_empty() {
            continue;
        }
        if let Some(selector) = token.strip_prefix("symbol:") {
            let full = format!("symbol:{selector}");
            if let Some((path, qualified, kind)) = split_symbol_selector(full.as_str())
                && let Some(symbol) = tree.symbols.get(path).and_then(|symbols| {
                    symbols
                        .iter()
                        .find(|symbol| symbol.qualified == qualified && symbol.kind == kind)
                })
            {
                mentions.push_symbol(path, symbol);
                continue;
            }
            if let Some(path) = selector.split('#').next()
                && let Some(path) = resolve_path(path, tree)
            {
                mentions.push_file(path.as_str());
            }
            continue;
        }
        let token = token.strip_prefix("file:").unwrap_or(token);
        if looks_like_path(token) {
            if let Some(path) = resolve_path(token, tree) {
                mentions.push_file(path.as_str());
            }
            continue;
        }
        if looks_like_identifier(token) {
            for (path, symbol) in resolve_symbol(token, tree) {
                mentions.push_symbol(path, symbol);
            }
        }
    }
    mentions
}

fn looks_like_path(token: &str) -> bool {
    if token.contains('/') {
        return true;
    }
    let token = strip_line_suffix(token);
    token.rsplit_once('.').is_some_and(|(stem, extension)| {
        !stem.is_empty()
            && (1..=5).contains(&extension.len())
            && extension.chars().all(|ch| ch.is_ascii_alphanumeric())
            && extension.chars().any(|ch| ch.is_ascii_alphabetic())
            && !extension.chars().any(|ch| ch.is_ascii_uppercase())
            && !stem.contains("::")
    })
}

/// `path:12` or `path:12:4` → `path`.
fn strip_line_suffix(token: &str) -> &str {
    let mut token = token;
    for _ in 0..2 {
        if let Some((head, tail)) = token.rsplit_once(':')
            && !tail.is_empty()
            && tail.chars().all(|ch| ch.is_ascii_digit())
        {
            token = head;
        }
    }
    token
}

fn resolve_path(token: &str, tree: &CodeTree) -> Option<String> {
    let token = strip_line_suffix(token);
    let token = token.split('#').next().unwrap_or(token);
    let token = token.trim_start_matches("./").trim_end_matches('/');
    if token.is_empty() {
        return None;
    }
    if tree.files.contains_key(token) {
        return Some(token.to_string());
    }
    let suffix = format!("/{token}");
    let mut matches = tree.files.keys().filter(|path| path.ends_with(&suffix));
    let first = matches.next()?;
    matches.next().is_none().then(|| first.clone())
}

fn looks_like_identifier(token: &str) -> bool {
    let segments = token.split("::").flat_map(|part| part.split('.'));
    let mut count = 0;
    for segment in segments {
        count += 1;
        let mut chars = segment.chars();
        let valid = chars
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
            && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_');
        if !valid {
            return false;
        }
    }
    count > 1 || token.contains('_') || token.chars().skip(1).any(|ch| ch.is_ascii_uppercase())
}

fn resolve_symbol<'a>(token: &str, tree: &'a CodeTree) -> Vec<(&'a str, &'a RawSymbol)> {
    let normalized = token.replace('.', "::");
    let name = normalized
        .rsplit("::")
        .next()
        .unwrap_or(normalized.as_str());
    let qualified_suffix = normalized.contains("::").then_some(normalized.as_str());
    let mut matches = tree
        .by_name
        .get(name)
        .into_iter()
        .flatten()
        .filter_map(|(path, index)| {
            let (path, symbols) = tree.symbols.get_key_value(path)?;
            let symbol = symbols.get(*index)?;
            let qualified = symbol.qualified.replace(['<', '>'], "");
            let accepted = qualified_suffix.is_none_or(|suffix| {
                qualified == suffix || qualified.ends_with(format!("::{suffix}").as_str())
            });
            accepted.then_some((path.as_str(), symbol))
        })
        .collect::<Vec<_>>();
    if matches.len() > MENTION_MAX_SYMBOL_MATCHES {
        return Vec::new();
    }
    matches.sort_by_key(|(path, symbol)| symbol_selector(path, symbol));
    matches
}
