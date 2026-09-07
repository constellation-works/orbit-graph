//! Git-tree change extraction for the rebuildable delivery-history index.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use git2::{DiffFindOptions, DiffOptions, Oid, Patch, Repository, Tree};
use serde::{Deserialize, Serialize};

use super::{RawSymbol, languages};
use crate::GraphError;

/// Version of the public delivery import JSON contract.
pub const DELIVERY_IMPORT_SCHEMA_VERSION: u32 = 1;

/// Version of the historical change extractor.
pub const CHANGE_EXTRACTOR_VERSION: u32 = 1;

/// Maximum commits processed by one default incremental history sync.
pub const DEFAULT_HISTORY_SYNC_LIMIT: usize = 1_000;

/// Public, versioned description of one delivered revision range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryImport {
    /// Must equal [`DELIVERY_IMPORT_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Stable repository identity: origin URL when available, otherwise canonical worktree path.
    pub repository: String,
    /// Full landing branch name or shorthand (for example `main`).
    pub landing_branch: String,
    /// Immutable commit immediately before the delivery.
    pub before_revision: String,
    /// Immutable landed commit at the end of the delivery.
    pub after_revision: String,
    /// Source-system delivery identifier, unique within repository and branch.
    pub delivery_id: String,
    /// How the delivery-to-task association was obtained.
    pub evidence: DeliveryEvidence,
    /// Source that produced this envelope.
    pub source: Provenance,
    /// RFC 3339 capture timestamp supplied by the producer.
    pub captured_at: String,
    /// Tasks associated with the delivery. Multiple entries deliberately retain ambiguity.
    #[serde(default)]
    pub tasks: Vec<TaskAssociation>,
}

/// Strength of evidence connecting a delivery to task metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryEvidence {
    /// A delivery system supplied and verified the immutable landing boundary.
    VerifiedDelivery,
    /// Association was inferred only from Git commit metadata or trailers.
    GitOnly,
}

impl DeliveryEvidence {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::VerifiedDelivery => "verified_delivery",
            Self::GitOnly => "git_only",
        }
    }
}

/// Provenance attached to imported delivery or task text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    /// Public producer identifier.
    pub system: String,
    /// Producer-local immutable record identifier when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_id: Option<String>,
}

/// Task text associated with a delivered change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskAssociation {
    /// Source-system task identifier.
    pub task_id: String,
    /// Task title captured with the delivery.
    pub title: String,
    /// Task description captured with the delivery.
    pub description: String,
    /// Acceptance criteria captured with the delivery.
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    /// Provenance of this task snapshot.
    pub source: Provenance,
    /// RFC 3339 capture timestamp for the task snapshot.
    pub captured_at: String,
}

/// Kind of file change observed between the immutable trees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileChangeKind {
    /// File exists only in the after tree.
    Added,
    /// Content changed at the same path.
    Modified,
    /// File exists only in the before tree.
    Deleted,
    /// Git similarity detection paired different paths.
    Renamed,
    /// Git similarity detection identified a copy.
    Copied,
    /// The Git object kind changed.
    TypeChanged,
    /// Git reported a delta that could not be classified more precisely.
    Unreadable,
}

impl FileChangeKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Modified => "modified",
            Self::Deleted => "deleted",
            Self::Renamed => "renamed",
            Self::Copied => "copied",
            Self::TypeChanged => "type_changed",
            Self::Unreadable => "unreadable",
        }
    }
}

/// Revision side for a line or symbol attribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevisionSide {
    /// State in `before_revision`.
    Before,
    /// State in `after_revision`.
    After,
}

impl RevisionSide {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Before => "before",
            Self::After => "after",
        }
    }
}

/// Explicit reason that changed lines remain attributed only to a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileFallbackReason {
    /// No registered language extractor supports the path.
    UnsupportedLanguage,
    /// The blob contains binary data.
    Binary,
    /// Parsing or extraction yielded no named symbol, so identity is uncertain.
    ParseOrExtractionUncertain,
    /// A changed line is outside every named symbol.
    NoEnclosingNamedSymbol,
    /// This revision side has no file, as expected for additions or deletions.
    FileUnavailable,
}

impl FileFallbackReason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::UnsupportedLanguage => "unsupported_language",
            Self::Binary => "binary",
            Self::ParseOrExtractionUncertain => "parse_or_extraction_uncertain",
            Self::NoEnclosingNamedSymbol => "no_enclosing_named_symbol",
            Self::FileUnavailable => "file_unavailable",
        }
    }
}

/// Confidence in structural identity across the before and after trees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolMatchConfidence {
    /// Qualified name and kind uniquely agree.
    Exact,
    /// A unique kind and signature pair agrees.
    Similar,
    /// Multiple candidates exist and no identity was chosen.
    Uncertain,
    /// No candidate exists on the other revision.
    Unmatched,
}

impl SymbolMatchConfidence {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Similar => "similar",
            Self::Uncertain => "uncertain",
            Self::Unmatched => "unmatched",
        }
    }
}

/// A named symbol as it existed at one immutable revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolIdentity {
    /// Immutable commit containing this symbol.
    pub revision: String,
    /// Which side of the delivery contains the symbol.
    pub side: RevisionSide,
    /// Repository-relative path at this revision.
    pub file_path: String,
    /// Extracted short name.
    pub name: String,
    /// Extracted qualified name.
    pub qualified: String,
    /// Extracted language-neutral symbol kind.
    pub kind: String,
    /// Start byte in the historical blob.
    pub span_start: usize,
    /// Exclusive end byte in the historical blob.
    pub span_end: usize,
    /// Normalized declaration signature when available.
    pub signature: Option<String>,
    /// Qualified enclosing symbol when available.
    pub parent: Option<String>,
}

/// Changed-line evidence attributed to the smallest enclosing named symbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolAttribution {
    /// Historical identity selected by smallest-enclosing-span matching.
    pub symbol: SymbolIdentity,
    /// One-based changed lines assigned to this symbol.
    pub changed_lines: Vec<u32>,
}

/// Before/after identity pairing. Deleted symbols always have `after = None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolChange {
    /// Before-tree attribution, absent for an addition.
    pub before: Option<SymbolAttribution>,
    /// After-tree attribution, absent for a deletion.
    pub after: Option<SymbolAttribution>,
    /// Confidence that the optional sides describe the same identity.
    pub match_confidence: SymbolMatchConfidence,
    /// Human-readable evidence or uncertainty reason.
    pub reason: String,
    /// Whether this record identifies a destination present in the after tree.
    pub live_after: bool,
}

/// File-level change statistics and symbol/fallback attribution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    /// Before-tree path, absent for additions.
    pub old_path: Option<String>,
    /// After-tree path, absent for deletions.
    pub new_path: Option<String>,
    /// Git-level file change kind.
    pub kind: FileChangeKind,
    /// Added line count.
    pub additions: usize,
    /// Deleted line count.
    pub deletions: usize,
    /// Conservative before/after symbol pairings.
    pub symbols: Vec<SymbolChange>,
    /// Explicit reason for before-tree file-only evidence.
    pub before_fallback: Option<FileFallbackReason>,
    /// Explicit reason for after-tree file-only evidence.
    pub after_fallback: Option<FileFallbackReason>,
}

/// Fully extracted, delivered change used by later ranking stages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveredChange {
    /// Original normalized delivery envelope and provenance.
    pub delivery: DeliveryImport,
    /// Actual Git tree differences; planned context is never consulted.
    pub files: Vec<FileChange>,
}

/// Outcome of conservatively resolving a historical symbol in the current landing tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrentRevisionResolution {
    /// Historical symbol that was resolved.
    pub historical: SymbolIdentity,
    /// Current landing-branch commit when it could be read.
    pub current_revision: Option<String>,
    /// Conservative resolution state.
    pub status: CurrentSymbolStatus,
    /// Current identities. Exactly one entry is required for `live`.
    pub matches: Vec<SymbolIdentity>,
    /// Evidence or uncertainty explanation.
    pub reason: String,
}

/// Whether a historical symbol is a safe live destination in the current tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CurrentSymbolStatus {
    /// Exactly one current symbol has the same qualified name and kind.
    Live,
    /// The historical path or symbol is absent from the current tree.
    Deleted,
    /// Multiple current symbols match, so no destination is selected.
    Ambiguous,
    /// Git, parsing, or language support did not provide usable evidence.
    Unavailable,
}

pub(crate) fn repository_identity(repo: &Repository) -> Result<String, GraphError> {
    if let Ok(remote) = repo.find_remote("origin")
        && let Some(url) = remote.url()
        && !url.trim().is_empty()
    {
        return Ok(url.to_string());
    }
    let workdir = repo.workdir().ok_or_else(|| {
        GraphError::invalid_data(
            "resolve history repository identity",
            "bare repositories are unsupported",
        )
    })?;
    workdir
        .canonicalize()
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|source| GraphError::io("canonicalize history repository", workdir, source))
}

pub(crate) fn resolve_commit(repo: &Repository, revision: &str) -> Result<Oid, GraphError> {
    let oid = Oid::from_str(revision).map_err(|error| {
        GraphError::invalid_data("validate immutable Git revision", error.to_string())
    })?;
    repo.find_commit(oid).map_err(|error| {
        GraphError::invalid_data(
            "validate immutable Git revision",
            format!("commit {revision} is unavailable: {error}"),
        )
    })?;
    Ok(oid)
}

pub(crate) fn branch_tip(repo: &Repository, branch: &str) -> Result<Oid, GraphError> {
    let reference_name = if branch.starts_with("refs/") {
        branch.to_string()
    } else {
        format!("refs/heads/{branch}")
    };
    repo.find_reference(reference_name.as_str())
        .and_then(|reference| reference.peel_to_commit())
        .map(|commit| commit.id())
        .map_err(|error| {
            GraphError::invalid_data(
                "resolve history landing branch",
                format!("{reference_name}: {error}"),
            )
        })
}

pub(crate) fn validate_delivery(
    repo: &Repository,
    delivery: &DeliveryImport,
) -> Result<(Oid, Oid), GraphError> {
    if delivery.schema_version != DELIVERY_IMPORT_SCHEMA_VERSION {
        return Err(GraphError::invalid_data(
            "validate delivery import schema",
            format!(
                "unsupported schema_version {}; expected {}",
                delivery.schema_version, DELIVERY_IMPORT_SCHEMA_VERSION
            ),
        ));
    }
    if delivery.delivery_id.trim().is_empty()
        || delivery.captured_at.trim().is_empty()
        || delivery.source.system.trim().is_empty()
    {
        return Err(GraphError::invalid_data(
            "validate delivery import",
            "delivery_id, source.system, and captured_at must be non-empty",
        ));
    }
    let detected = repository_identity(repo)?;
    if delivery.repository != detected {
        return Err(GraphError::invalid_data(
            "validate delivery repository",
            format!(
                "repository mismatch: envelope={} local={detected}",
                delivery.repository
            ),
        ));
    }
    let before = resolve_commit(repo, delivery.before_revision.as_str())?;
    let after = resolve_commit(repo, delivery.after_revision.as_str())?;
    if before == after
        || !repo.graph_descendant_of(after, before).map_err(|error| {
            GraphError::invalid_data("validate delivery revision ancestry", error.to_string())
        })?
    {
        return Err(GraphError::invalid_data(
            "validate delivery revision ancestry",
            format!("{} is not a descendant of {}", after, before),
        ));
    }
    let tip = branch_tip(repo, delivery.landing_branch.as_str())?;
    if tip != after
        && !repo.graph_descendant_of(tip, after).map_err(|error| {
            GraphError::invalid_data("validate delivery landing branch", error.to_string())
        })?
    {
        return Err(GraphError::invalid_data(
            "validate delivery landing branch",
            format!("after revision {after} is not reachable from branch tip {tip}"),
        ));
    }
    Ok((before, after))
}

pub(crate) fn extract_delivery(
    repo: &Repository,
    delivery: DeliveryImport,
) -> Result<DeliveredChange, GraphError> {
    let (before, after) = validate_delivery(repo, &delivery)?;
    let before_commit = repo
        .find_commit(before)
        .map_err(git_error("load before commit"))?;
    let after_commit = repo
        .find_commit(after)
        .map_err(git_error("load after commit"))?;
    let before_tree = before_commit
        .tree()
        .map_err(git_error("load before tree"))?;
    let after_tree = after_commit.tree().map_err(git_error("load after tree"))?;
    let files = extract_tree_diff(repo, &before_tree, &after_tree, before, after)?;
    Ok(DeliveredChange { delivery, files })
}

fn extract_tree_diff(
    repo: &Repository,
    before_tree: &Tree<'_>,
    after_tree: &Tree<'_>,
    before: Oid,
    after: Oid,
) -> Result<Vec<FileChange>, GraphError> {
    let mut options = DiffOptions::new();
    options.include_untracked(false).ignore_submodules(true);
    let mut diff = repo
        .diff_tree_to_tree(Some(before_tree), Some(after_tree), Some(&mut options))
        .map_err(git_error("diff delivery trees"))?;
    diff.find_similar(Some(DiffFindOptions::new().renames(true).copies(true)))
        .map_err(git_error("detect delivery renames"))?;

    let mut changes = Vec::with_capacity(diff.deltas().len());
    for index in 0..diff.deltas().len() {
        let delta = diff.get_delta(index).ok_or_else(|| {
            GraphError::invalid_data("read delivery diff", format!("missing delta {index}"))
        })?;
        let kind = match delta.status() {
            git2::Delta::Added => FileChangeKind::Added,
            git2::Delta::Deleted => FileChangeKind::Deleted,
            git2::Delta::Renamed => FileChangeKind::Renamed,
            git2::Delta::Copied => FileChangeKind::Copied,
            git2::Delta::Typechange => FileChangeKind::TypeChanged,
            git2::Delta::Modified | git2::Delta::Unmodified => FileChangeKind::Modified,
            _ => FileChangeKind::Unreadable,
        };
        let old_path = (!matches!(kind, FileChangeKind::Added))
            .then(|| delta.old_file().path().map(normalize_path))
            .flatten();
        let new_path = (!matches!(kind, FileChangeKind::Deleted))
            .then(|| delta.new_file().path().map(normalize_path))
            .flatten();
        let patch = Patch::from_diff(&diff, index).map_err(git_error("read delivery patch"))?;
        let mut old_lines = BTreeSet::new();
        let mut new_lines = BTreeSet::new();
        let mut additions = 0;
        let mut deletions = 0;
        if let Some(patch) = patch.as_ref() {
            for hunk_index in 0..patch.num_hunks() {
                let (_, line_count) = patch
                    .hunk(hunk_index)
                    .map_err(git_error("read diff hunk"))?;
                for line_index in 0..line_count {
                    let line = patch
                        .line_in_hunk(hunk_index, line_index)
                        .map_err(git_error("read diff line"))?;
                    match line.origin() {
                        '-' => {
                            if let Some(line) = line.old_lineno() {
                                old_lines.insert(line);
                            }
                            deletions += 1;
                        }
                        '+' => {
                            if let Some(line) = line.new_lineno() {
                                new_lines.insert(line);
                            }
                            additions += 1;
                        }
                        _ => {}
                    }
                }
            }
        }

        let before_content = read_tree_file(repo, before_tree, delta.old_file().path())?;
        let after_content = read_tree_file(repo, after_tree, delta.new_file().path())?;
        if matches!(kind, FileChangeKind::Renamed | FileChangeKind::Copied)
            && old_lines.is_empty()
            && new_lines.is_empty()
        {
            if let Some(content) = before_content.as_deref() {
                old_lines.extend(all_line_numbers(content));
            }
            if let Some(content) = after_content.as_deref() {
                new_lines.extend(all_line_numbers(content));
            }
        }
        let before_attr = attribute_side(
            before.to_string().as_str(),
            RevisionSide::Before,
            delta.old_file().path(),
            before_content.as_deref(),
            &old_lines,
        );
        let after_attr = attribute_side(
            after.to_string().as_str(),
            RevisionSide::After,
            delta.new_file().path(),
            after_content.as_deref(),
            &new_lines,
        );
        let symbols = pair_symbols(before_attr.symbols, after_attr.symbols);
        changes.push(FileChange {
            old_path,
            new_path,
            kind,
            additions,
            deletions,
            symbols,
            before_fallback: before_attr.fallback,
            after_fallback: after_attr.fallback,
        });
    }
    changes.sort_by(|left, right| {
        left.new_path
            .cmp(&right.new_path)
            .then(left.old_path.cmp(&right.old_path))
    });
    Ok(changes)
}

fn read_tree_file(
    repo: &Repository,
    tree: &Tree<'_>,
    path: Option<&Path>,
) -> Result<Option<Vec<u8>>, GraphError> {
    let Some(path) = path else { return Ok(None) };
    match tree.get_path(path) {
        Ok(entry) => entry
            .to_object(repo)
            .and_then(|object| object.peel_to_blob())
            .map(|blob| Some(blob.content().to_vec()))
            .map_err(git_error("read historical file blob")),
        Err(error) if error.code() == git2::ErrorCode::NotFound => Ok(None),
        Err(error) => Err(git_error("read historical tree entry")(error)),
    }
}

struct SideAttribution {
    symbols: Vec<SymbolAttribution>,
    fallback: Option<FileFallbackReason>,
}

type SymbolRangeKey = (usize, usize, String, String);
type AttributedLines = (RawSymbol, Vec<u32>);

fn attribute_side(
    revision: &str,
    side: RevisionSide,
    path: Option<&Path>,
    content: Option<&[u8]>,
    changed_lines: &BTreeSet<u32>,
) -> SideAttribution {
    let (Some(path), Some(content)) = (path, content) else {
        return SideAttribution {
            symbols: Vec::new(),
            fallback: Some(FileFallbackReason::FileUnavailable),
        };
    };
    if content.contains(&0) {
        return SideAttribution {
            symbols: Vec::new(),
            fallback: Some(FileFallbackReason::Binary),
        };
    }
    let extractors = languages::extractors();
    let Some(extractor) = extractors.iter().find(|extractor| extractor.supports(path)) else {
        return SideAttribution {
            symbols: Vec::new(),
            fallback: Some(FileFallbackReason::UnsupportedLanguage),
        };
    };
    let extracted = extractor.extract(path, content);
    if extracted.symbols.is_empty() {
        let has_non_symbol_evidence = !extracted.refs.is_empty()
            || !extracted.relations.is_empty()
            || !extracted.imports.is_empty()
            || !extracted.strings.is_empty()
            || !extracted.configs.is_empty()
            || !extracted.commands.is_empty();
        return SideAttribution {
            symbols: Vec::new(),
            fallback: Some(if has_non_symbol_evidence {
                FileFallbackReason::NoEnclosingNamedSymbol
            } else {
                FileFallbackReason::ParseOrExtractionUncertain
            }),
        };
    }
    let line_ranges = line_byte_ranges(content);
    let mut by_symbol: BTreeMap<SymbolRangeKey, AttributedLines> = BTreeMap::new();
    let mut unmatched = false;
    for line in changed_lines {
        let Some((start, end)) = line_ranges.get((*line as usize).saturating_sub(1)).copied()
        else {
            unmatched = true;
            continue;
        };
        let selected = extracted
            .symbols
            .iter()
            .filter(|symbol| symbol.span_start < end && symbol.span_end > start)
            .min_by_key(|symbol| symbol.span_end.saturating_sub(symbol.span_start));
        if let Some(symbol) = selected {
            by_symbol
                .entry((
                    symbol.span_start,
                    symbol.span_end,
                    symbol.qualified.clone(),
                    symbol.kind.clone(),
                ))
                .or_insert_with(|| (symbol.clone(), Vec::new()))
                .1
                .push(*line);
        } else {
            unmatched = true;
        }
    }
    let symbols = by_symbol
        .into_values()
        .map(|(symbol, changed_lines)| SymbolAttribution {
            symbol: SymbolIdentity {
                revision: revision.to_string(),
                side,
                file_path: normalize_path(path),
                name: symbol.name,
                qualified: symbol.qualified,
                kind: symbol.kind,
                span_start: symbol.span_start,
                span_end: symbol.span_end,
                signature: symbol.signature,
                parent: symbol.parent_symbol,
            },
            changed_lines,
        })
        .collect();
    SideAttribution {
        symbols,
        fallback: unmatched.then_some(FileFallbackReason::NoEnclosingNamedSymbol),
    }
}

fn line_byte_ranges(content: &[u8]) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut start = 0;
    for (index, byte) in content.iter().enumerate() {
        if *byte == b'\n' {
            ranges.push((start, index + 1));
            start = index + 1;
        }
    }
    if start < content.len() || content.is_empty() {
        ranges.push((start, content.len()));
    }
    ranges
}

fn all_line_numbers(content: &[u8]) -> impl Iterator<Item = u32> {
    let count = line_byte_ranges(content).len();
    1..=u32::try_from(count).unwrap_or(u32::MAX)
}

fn pair_symbols(
    before: Vec<SymbolAttribution>,
    after: Vec<SymbolAttribution>,
) -> Vec<SymbolChange> {
    let mut after = after.into_iter().map(Some).collect::<Vec<_>>();
    let mut changes = Vec::new();
    for old in before {
        let exact = unique_candidate(&after, |new| {
            new.symbol.qualified == old.symbol.qualified
                && new.symbol.kind == old.symbol.kind
                && (!old.symbol.qualified.contains('#')
                    || new.symbol.signature == old.symbol.signature)
        });
        let similar = exact.or_else(|| {
            unique_candidate(&after, |new| {
                new.symbol.kind == old.symbol.kind
                    && new.symbol.signature.is_some()
                    && new.symbol.signature == old.symbol.signature
            })
        });
        if let Some(index) = similar {
            let Some(new) = after[index].take() else {
                continue;
            };
            let confidence = if new.symbol.qualified == old.symbol.qualified {
                SymbolMatchConfidence::Exact
            } else {
                SymbolMatchConfidence::Similar
            };
            changes.push(SymbolChange {
                before: Some(old),
                after: Some(new),
                match_confidence: confidence,
                reason: if confidence == SymbolMatchConfidence::Exact {
                    "same qualified name and kind".to_string()
                } else {
                    "unique equal signature and kind across revisions".to_string()
                },
                live_after: true,
            });
        } else {
            let ambiguous = after
                .iter()
                .flatten()
                .filter(|new| {
                    new.symbol.kind == old.symbol.kind
                        && (new.symbol.name == old.symbol.name
                            || (new.symbol.signature.is_some()
                                && new.symbol.signature == old.symbol.signature))
                })
                .count()
                > 1;
            changes.push(SymbolChange {
                before: Some(old),
                after: None,
                match_confidence: if ambiguous {
                    SymbolMatchConfidence::Uncertain
                } else {
                    SymbolMatchConfidence::Unmatched
                },
                reason: if ambiguous {
                    "multiple structural candidates; identity not invented".to_string()
                } else {
                    "no unique after-tree identity".to_string()
                },
                live_after: false,
            });
        }
    }
    changes.extend(after.into_iter().flatten().map(|new| SymbolChange {
        before: None,
        after: Some(new),
        match_confidence: SymbolMatchConfidence::Unmatched,
        reason: "no unique before-tree identity".to_string(),
        live_after: true,
    }));
    changes
}

fn unique_candidate(
    candidates: &[Option<SymbolAttribution>],
    predicate: impl Fn(&SymbolAttribution) -> bool,
) -> Option<usize> {
    let matches = candidates
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| {
            candidate
                .as_ref()
                .filter(|item| predicate(item))
                .map(|_| index)
        })
        .collect::<Vec<_>>();
    if matches.len() == 1 {
        Some(matches[0])
    } else {
        None
    }
}

fn normalize_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn git_error(operation: &'static str) -> impl FnOnce(git2::Error) -> GraphError {
    move |error| GraphError::invalid_data(operation, error.to_string())
}

#[cfg(test)]
mod tests;
