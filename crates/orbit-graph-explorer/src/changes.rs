//! The changed-symbol slice of a [`Comparison`].
//!
//! This module answers one question — *which symbols changed between the two
//! named revisions* — and answers it with two independent sources of evidence
//! that are never silently merged:
//!
//! - **Git**, for the changed-file set. A tree-to-tree diff between the two
//!   immutable commits, with rename and copy similarity detection enabled, says
//!   which paths moved, appeared, disappeared, or changed content.
//! - **The two snapshot indexes**, for the symbols inside those paths. Each
//!   side is queried through the public [`orbit_graph`] API only, so a symbol
//!   the extractor never saw is reported as *out of scope* rather than as an
//!   addition or a removal.
//!
//! Symbols are paired with the design document's ladder (see
//! `docs/design/change-explorer.md`, "Changed-symbol identity"), strongest
//! rung first: same selector, signature changed, moved, renamed, uncertain.
//! Rungs three and four require Git rename or copy evidence for the containing
//! file, because the index carries no cross-path symbol identity: two
//! same-named symbols at different paths are indistinguishable from an
//! unrelated removal plus an unrelated addition. Without that Git evidence the
//! pair is recorded as [`ChangeStatus::Uncertain`] with every candidate listed
//! and none chosen, which is how that index limitation — and the identically
//! collapsing `fuzzy_name` sets of same-named symbols — stays visible instead
//! of being resolved by a guess.

use std::collections::{BTreeMap, BTreeSet};

use git2::{Delta, DiffFindOptions, DiffOptions, Repository};
use orbit_graph::{DEFAULT_SHOW_MAX_BYTES, OverviewFormat, Selector};
use serde::Serialize;
use thiserror::Error;

use crate::filters::{FilterLog, FilterReason, FilterSet, FilteredOut};
use crate::snapshot::{Comparison, ExclusionReason, Snapshot, SnapshotSide};

/// Schema version of the changed-symbol payload.
///
/// Independent of the `orbit-graph` crate version and of `EXTRACTOR_VERSION`.
pub const CHANGED_SYMBOLS_SCHEMA_VERSION: u32 = 1;

/// Failure surface of the changed-symbol computation.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ChangesError {
    /// A Git operation against the inspected repository failed.
    #[error("{operation}: {reason}")]
    Git {
        /// Operation being performed.
        operation: &'static str,
        /// Failure reason.
        reason: String,
    },
    /// A query against one snapshot index failed.
    #[error("{operation} for the {side} snapshot: {reason}")]
    Query {
        /// Operation being performed.
        operation: &'static str,
        /// Snapshot side the query belonged to.
        side: SnapshotSide,
        /// Failure reason.
        reason: String,
    },
}

impl ChangesError {
    fn git(operation: &'static str, error: &git2::Error) -> Self {
        Self::Git {
            operation,
            reason: error.message().to_string(),
        }
    }
}

/// How a symbol changed between the two revisions.
///
/// The design document's data contract names `added`, `removed`, `modified`,
/// and `uncertain`. `signature_changed`, `moved`, and `renamed` promote the
/// corresponding pairing rungs to first-class statuses so a reader does not
/// have to consult [`Pairing`] to learn that a symbol's callers must be
/// recomputed on both sides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeStatus {
    /// Present in head only.
    Added,
    /// Present in base only.
    Removed,
    /// Present on both sides with the same signature and different body bytes.
    Modified,
    /// Present on both sides with a different signature line.
    SignatureChanged,
    /// Same name and kind at a different path, across a Git-detected file
    /// rename or copy.
    Moved,
    /// Different name, same kind, inside a Git-detected file rename or copy.
    Renamed,
    /// Several plausible partners, or a correspondence the available evidence
    /// cannot establish. Every candidate is listed and none is chosen.
    Uncertain,
}

impl ChangeStatus {
    /// Stable label used in reports and user-facing output.
    pub fn label(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Removed => "removed",
            Self::Modified => "modified",
            Self::SignatureChanged => "signature_changed",
            Self::Moved => "moved",
            Self::Renamed => "renamed",
            Self::Uncertain => "uncertain",
        }
    }
}

/// Which rung of the pairing ladder established this symbol's identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Pairing {
    /// Identical path, symbol name, and kind. Also used for a symbol present on
    /// one side only, whose identity is its own selector.
    SameSelector,
    /// Same path and kind with a changed signature line.
    SignatureChanged,
    /// Same name and kind across a Git-detected file rename or copy.
    Moved,
    /// Different name, same kind, inside a Git-detected file rename or copy.
    Renamed,
    /// No correspondence was established; see `uncertain_candidates`.
    Uncertain,
}

impl Pairing {
    /// Stable label used in reports and user-facing output.
    pub fn label(self) -> &'static str {
        match self {
            Self::SameSelector => "same_selector",
            Self::SignatureChanged => "signature_changed",
            Self::Moved => "moved",
            Self::Renamed => "renamed",
            Self::Uncertain => "uncertain",
        }
    }
}

/// What kind of external evidence supports a pairing.
///
/// A pairing label is evidence about identity, not about behavior. Recording
/// the evidence source lets a reader see that a `moved` symbol was paired by
/// Git rename detection and not by the index, which has no cross-path identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PairingEvidence {
    /// The canonical selector is identical on both sides.
    Selector,
    /// The symbol's signature line differs across an identical selector.
    Signature,
    /// Git reported the containing file as renamed.
    GitRename,
    /// Git reported the containing file as copied.
    GitCopy,
    /// No evidence established the correspondence.
    None,
}

/// How Git classified the file containing a changed symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FileChangeKind {
    /// The path exists only in head.
    Added,
    /// The path exists only in base.
    Deleted,
    /// The path exists on both sides with different content.
    Modified,
    /// Git paired the path with a different path on the other side.
    Renamed,
    /// Git paired the path with a retained path on the other side.
    Copied,
    /// The path changed file type.
    TypeChanged,
    /// Git reported a delta this module does not classify more precisely.
    Other,
}

impl FileChangeKind {
    /// Stable label used in reports and user-facing output.
    pub fn label(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Deleted => "deleted",
            Self::Modified => "modified",
            Self::Renamed => "renamed",
            Self::Copied => "copied",
            Self::TypeChanged => "type_changed",
            Self::Other => "other",
        }
    }

    /// Noun form used in prose, for example "a Git rename of the file".
    fn noun(self) -> &'static str {
        match self {
            Self::Renamed => "rename",
            Self::Copied => "copy",
            other => other.label(),
        }
    }

    fn from_delta(delta: Delta) -> Self {
        match delta {
            Delta::Added => Self::Added,
            Delta::Deleted => Self::Deleted,
            Delta::Modified => Self::Modified,
            Delta::Renamed => Self::Renamed,
            Delta::Copied => Self::Copied,
            Delta::Typechange => Self::TypeChanged,
            _ => Self::Other,
        }
    }
}

/// Why a changed path carries no symbol evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum OutOfScopeReason {
    /// The path was materialized but the extractor has no grammar for it, or
    /// the scanner's filters skipped it.
    UnsupportedLanguage,
    /// The path is a symbolic link and was not materialized.
    Symlink,
    /// The path is a submodule and was not materialized.
    Submodule,
    /// The blob exceeded the materialization size bound.
    OversizeBlob,
    /// The tree entry name was not safe to materialize.
    UnsafeName,
    /// The tree entry had a kind that is not materializable.
    UnsupportedKind,
}

impl OutOfScopeReason {
    /// Stable label used in reports and user-facing output.
    pub fn label(self) -> &'static str {
        match self {
            Self::UnsupportedLanguage => "unsupported_language",
            Self::Symlink => "symlink",
            Self::Submodule => "submodule",
            Self::OversizeBlob => "oversize_blob",
            Self::UnsafeName => "unsafe_name",
            Self::UnsupportedKind => "unsupported_kind",
        }
    }

    fn from_exclusion(reason: ExclusionReason) -> Self {
        match reason {
            ExclusionReason::Symlink => Self::Symlink,
            ExclusionReason::Submodule => Self::Submodule,
            ExclusionReason::OversizeBlob { .. } => Self::OversizeBlob,
            ExclusionReason::UnsafeName => Self::UnsafeName,
            _ => Self::UnsupportedKind,
        }
    }
}

/// One symbol occurrence, attributed to exactly one snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SymbolRef {
    /// Canonical `symbol:<path>#<name>:<kind>` selector.
    pub selector: String,
    /// Snapshot the selector resolves in.
    pub snapshot: String,
    /// Immutable commit SHA of that snapshot.
    pub commit_sha: String,
}

/// A candidate partner of an [`ChangeStatus::Uncertain`] symbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UncertainCandidate {
    /// The candidate occurrence.
    #[serde(flatten)]
    pub symbol: SymbolRef,
    /// Why this candidate is plausible and why it was not chosen.
    pub reason: String,
}

/// One changed symbol in the doc's changed-symbol payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangedSymbol {
    /// How the symbol changed.
    pub status: ChangeStatus,
    /// Which pairing rung established its identity.
    pub pairing: Pairing,
    /// What evidence supports that rung.
    pub pairing_evidence: PairingEvidence,
    /// Base-side occurrence, when the symbol exists in base.
    pub base: Option<SymbolRef>,
    /// Head-side occurrence, when the symbol exists in head.
    pub head: Option<SymbolRef>,
    /// Snapshots that support this entry, in base-then-head order.
    pub supporting_snapshots: Vec<String>,
    /// How Git classified the containing file.
    pub file_change: FileChangeKind,
    /// Base-side path of the containing file, when it exists in base.
    pub base_path: Option<String>,
    /// Head-side path of the containing file, when it exists in head.
    pub head_path: Option<String>,
    /// Every candidate partner, populated only for `uncertain` entries.
    pub uncertain_candidates: Vec<UncertainCandidate>,
    /// Human-readable qualification of this entry.
    pub note: Option<String>,
}

impl ChangedSymbol {
    /// Selector used to order and address this entry: the head-side selector
    /// when the symbol exists in head, otherwise the base-side selector.
    pub fn primary_selector(&self) -> &str {
        self.head
            .as_ref()
            .or(self.base.as_ref())
            .map(|symbol| symbol.selector.as_str())
            .unwrap_or_default()
    }
}

/// A changed path whose symbols the extractor never saw.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutOfScopeEntry {
    /// Repository-relative, slash-separated path.
    pub path: String,
    /// Why the path carries no symbol evidence.
    pub reason: OutOfScopeReason,
    /// Snapshot the path belongs to.
    pub snapshot: String,
}

/// The doc's changed-symbol payload for one comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangedSymbols {
    /// Payload schema version.
    pub schema_version: u32,
    /// Changed symbols, ordered by primary selector then status.
    pub symbols: Vec<ChangedSymbol>,
    /// Changed paths the extractor did not index, in path order.
    pub out_of_scope: Vec<OutOfScopeEntry>,
}

impl ChangedSymbols {
    /// Compute the changed-symbol slice for `comparison`.
    ///
    /// Reads the inspected repository's object database through `git2` and both
    /// snapshot indexes through the public [`orbit_graph`] API. Nothing is
    /// written to the user's working tree, index, or object store.
    pub fn compute(comparison: &Comparison) -> Result<Self, ChangesError> {
        let files = changed_files(comparison)?;
        let base = SideInventory::load(comparison.base())?;
        let head = SideInventory::load(comparison.head())?;
        let out_of_scope = out_of_scope(comparison, &files, &base, &head);

        let mut pairer = Pairer::new(comparison, &files, &base, &head);
        pairer.pair_same_selector()?;
        pairer.pair_across_renamed_files()?;
        pairer.pair_uncertain_cross_path();
        pairer.emit_unpaired();

        let mut symbols = pairer.finish();
        symbols.sort_by(|left, right| {
            left.primary_selector()
                .cmp(right.primary_selector())
                .then(left.status.cmp(&right.status))
        });

        Ok(Self {
            schema_version: CHANGED_SYMBOLS_SCHEMA_VERSION,
            symbols,
            out_of_scope,
        })
    }

    /// Apply presentation filters, returning the kept slice and an explanation
    /// of everything the filters removed.
    ///
    /// Filtering is presentation only: the underlying pairing is unchanged, and
    /// `out_of_scope` is never filtered, because hiding a disclosure of what the
    /// extractor could not see would turn a stated gap into a silent one.
    pub fn filtered(&self, filters: &FilterSet) -> (Self, Vec<FilteredOut>) {
        let mut log = FilterLog::default();
        if filters.is_empty() {
            return (self.clone(), log.into_filtered_out());
        }

        let mut symbols = Vec::new();
        for symbol in &self.symbols {
            let paths: Vec<&str> = [symbol.head_path.as_deref(), symbol.base_path.as_deref()]
                .into_iter()
                .flatten()
                .collect();
            let example = symbol.primary_selector().to_string();
            if !paths.is_empty() && !paths.iter().any(|path| filters.language_admits(path)) {
                log.record(FilterReason::Language, example.as_str());
                continue;
            }
            if !paths.is_empty() && !paths.iter().any(|path| filters.scope_admits(path)) {
                log.record(FilterReason::Scope, example.as_str());
                continue;
            }
            if !filters.change_kind_admits(Some(symbol.status.label())) {
                log.record(FilterReason::ChangeKind, example.as_str());
                continue;
            }
            symbols.push(symbol.clone());
        }

        (
            Self {
                schema_version: self.schema_version,
                symbols,
                out_of_scope: self.out_of_scope.clone(),
            },
            log.into_filtered_out(),
        )
    }

    /// Entries whose base or head selector equals `selector`.
    pub fn entries_for(&self, selector: &str) -> Vec<&ChangedSymbol> {
        self.symbols
            .iter()
            .filter(|symbol| {
                let matches = |side: &Option<SymbolRef>| {
                    side.as_ref()
                        .is_some_and(|entry| entry.selector == selector)
                };
                matches(&symbol.base) || matches(&symbol.head)
            })
            .collect()
    }
}

/// One symbol row from a snapshot's index.
#[derive(Debug, Clone, PartialEq, Eq)]
struct IndexedSymbol {
    path: String,
    name: String,
    kind: String,
    selector: String,
}

/// The indexed shape of one snapshot: its files and its symbols.
struct SideInventory {
    side: SnapshotSide,
    commit_sha: String,
    indexed_files: BTreeSet<String>,
    /// Symbols keyed by canonical selector.
    symbols: BTreeMap<String, IndexedSymbol>,
    /// Selectors that more than one indexed symbol row produces.
    ambiguous_selectors: BTreeSet<String>,
    /// Selectors per containing path.
    by_path: BTreeMap<String, Vec<String>>,
}

impl SideInventory {
    fn load(snapshot: &Snapshot) -> Result<Self, ChangesError> {
        let overview = snapshot
            .graph()
            .overview(None, OverviewFormat::Full)
            .map_err(|error| ChangesError::Query {
                operation: "list indexed symbols",
                side: snapshot.side(),
                reason: error.to_string(),
            })?;

        let mut indexed_files = BTreeSet::new();
        let mut symbols = BTreeMap::new();
        let mut ambiguous_selectors = BTreeSet::new();
        let mut by_path: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for file in overview.files {
            indexed_files.insert(file.path.clone());
            for symbol in file.symbols {
                let selector = format!("symbol:{}#{}:{}", file.path, symbol.name, symbol.kind);
                let entry = IndexedSymbol {
                    path: file.path.clone(),
                    name: symbol.name,
                    kind: symbol.kind,
                    selector: selector.clone(),
                };
                if symbols.insert(selector.clone(), entry).is_some() {
                    // Two indexed rows produce the same canonical selector, so
                    // no selector-addressed query can tell them apart.
                    ambiguous_selectors.insert(selector.clone());
                } else {
                    by_path.entry(file.path.clone()).or_default().push(selector);
                }
            }
        }

        Ok(Self {
            side: snapshot.side(),
            commit_sha: snapshot.commit_sha().to_string(),
            indexed_files,
            symbols,
            ambiguous_selectors,
            by_path,
        })
    }

    fn symbol_ref(&self, selector: &str) -> SymbolRef {
        SymbolRef {
            selector: selector.to_string(),
            snapshot: self.side.label().to_string(),
            commit_sha: self.commit_sha.clone(),
        }
    }

    fn selectors_in(&self, path: &str) -> &[String] {
        self.by_path
            .get(path)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
}

/// One Git delta, reduced to the two paths and the change kind.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileDelta {
    kind: FileChangeKind,
    base_path: Option<String>,
    head_path: Option<String>,
}

/// The changed-file set of a comparison, indexed by side.
#[derive(Debug, Default)]
struct ChangedFiles {
    deltas: Vec<FileDelta>,
    base_index: BTreeMap<String, usize>,
    head_index: BTreeMap<String, usize>,
}

impl ChangedFiles {
    fn push(&mut self, delta: FileDelta) {
        let index = self.deltas.len();
        if let Some(path) = delta.base_path.as_ref() {
            self.base_index.insert(path.clone(), index);
        }
        if let Some(path) = delta.head_path.as_ref() {
            self.head_index.insert(path.clone(), index);
        }
        self.deltas.push(delta);
    }

    fn for_base(&self, path: &str) -> Option<&FileDelta> {
        self.base_index
            .get(path)
            .and_then(|at| self.deltas.get(*at))
    }

    fn for_head(&self, path: &str) -> Option<&FileDelta> {
        self.head_index
            .get(path)
            .and_then(|at| self.deltas.get(*at))
    }

    fn base_paths(&self) -> impl Iterator<Item = &String> {
        self.base_index.keys()
    }

    fn head_paths(&self) -> impl Iterator<Item = &String> {
        self.head_index.keys()
    }

    /// File pairs Git resolved as a rename or a copy, base path first.
    fn renamed_pairs(&self) -> impl Iterator<Item = (&String, &String, FileChangeKind)> {
        self.deltas.iter().filter_map(|delta| {
            if !matches!(delta.kind, FileChangeKind::Renamed | FileChangeKind::Copied) {
                return None;
            }
            let base = delta.base_path.as_ref()?;
            let head = delta.head_path.as_ref()?;
            Some((base, head, delta.kind))
        })
    }
}

/// Diff the two commits with rename and copy similarity detection enabled.
fn changed_files(comparison: &Comparison) -> Result<ChangedFiles, ChangesError> {
    let repo = Repository::discover(comparison.repository())
        .map_err(|error| ChangesError::git("open repository for change detection", &error))?;
    let tree_for = |sha: &str| -> Result<git2::Tree<'_>, ChangesError> {
        let oid = git2::Oid::from_str(sha)
            .map_err(|error| ChangesError::git("parse comparison commit SHA", &error))?;
        repo.find_commit(oid)
            .and_then(|commit| commit.tree())
            .map_err(|error| ChangesError::git("load comparison commit tree", &error))
    };
    let base_tree = tree_for(comparison.base().commit_sha())?;
    let head_tree = tree_for(comparison.head().commit_sha())?;

    let mut diff_options = DiffOptions::new();
    diff_options
        .include_typechange(true)
        .ignore_submodules(true)
        .skip_binary_check(false);
    let mut diff = repo
        .diff_tree_to_tree(Some(&base_tree), Some(&head_tree), Some(&mut diff_options))
        .map_err(|error| ChangesError::git("diff comparison commit trees", &error))?;

    // Rename and copy detection with libgit2's default similarity thresholds.
    // Rewrites are deliberately not broken apart: splitting a heavily edited
    // file into a delete plus an add would report every symbol in it as removed
    // and re-added, which is exactly the false remove/add pair the pairing
    // ladder exists to avoid.
    let mut find_options = DiffFindOptions::new();
    find_options.renames(true).copies(true);
    diff.find_similar(Some(&mut find_options))
        .map_err(|error| ChangesError::git("detect renames between commit trees", &error))?;

    let mut files = ChangedFiles::default();
    for delta in diff.deltas() {
        let kind = FileChangeKind::from_delta(delta.status());
        let base_path = match kind {
            FileChangeKind::Added => None,
            _ => path_of(delta.old_file().path()),
        };
        let head_path = match kind {
            FileChangeKind::Deleted => None,
            _ => path_of(delta.new_file().path()),
        };
        if base_path.is_none() && head_path.is_none() {
            continue;
        }
        files.push(FileDelta {
            kind,
            base_path,
            head_path,
        });
    }

    Ok(files)
}

fn path_of(path: Option<&std::path::Path>) -> Option<String> {
    let path = path?;
    let mut parts = Vec::new();
    for component in path.components() {
        parts.push(component.as_os_str().to_string_lossy().into_owned());
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

/// Changed paths that carry no symbol evidence, because the snapshot never
/// indexed them. Reported as out of scope, never as removed or added.
fn out_of_scope(
    comparison: &Comparison,
    files: &ChangedFiles,
    base: &SideInventory,
    head: &SideInventory,
) -> Vec<OutOfScopeEntry> {
    let mut entries = Vec::new();
    let mut sides: [(&SideInventory, Vec<&String>, &Snapshot); 2] = [
        (base, files.base_paths().collect(), comparison.base()),
        (head, files.head_paths().collect(), comparison.head()),
    ];
    for (inventory, paths, snapshot) in sides.iter_mut() {
        for path in paths.iter() {
            if inventory.indexed_files.contains(path.as_str()) {
                continue;
            }
            let reason = snapshot
                .materialization()
                .excluded
                .iter()
                .find(|entry| entry.path == **path)
                .map(|entry| OutOfScopeReason::from_exclusion(entry.reason))
                .unwrap_or(OutOfScopeReason::UnsupportedLanguage);
            entries.push(OutOfScopeEntry {
                path: (*path).clone(),
                reason,
                snapshot: inventory.side.label().to_string(),
            });
        }
    }
    entries.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(left.snapshot.cmp(&right.snapshot))
    });
    entries.dedup();
    entries
}

/// Walks the pairing ladder, consuming candidates rung by rung.
struct Pairer<'a> {
    comparison: &'a Comparison,
    files: &'a ChangedFiles,
    base: &'a SideInventory,
    head: &'a SideInventory,
    unpaired_base: BTreeSet<String>,
    unpaired_head: BTreeSet<String>,
    symbols: Vec<ChangedSymbol>,
}

impl<'a> Pairer<'a> {
    fn new(
        comparison: &'a Comparison,
        files: &'a ChangedFiles,
        base: &'a SideInventory,
        head: &'a SideInventory,
    ) -> Self {
        let unpaired_base = candidate_selectors(base, files.base_paths());
        let unpaired_head = candidate_selectors(head, files.head_paths());
        Self {
            comparison,
            files,
            base,
            head,
            unpaired_base,
            unpaired_head,
            symbols: Vec::new(),
        }
    }

    fn finish(self) -> Vec<ChangedSymbol> {
        self.symbols
    }

    /// Rungs 1 and 2: identical canonical selector on both sides.
    fn pair_same_selector(&mut self) -> Result<(), ChangesError> {
        let shared: Vec<String> = self
            .unpaired_base
            .intersection(&self.unpaired_head)
            .cloned()
            .collect();
        for selector in shared {
            self.unpaired_base.remove(&selector);
            self.unpaired_head.remove(&selector);

            let base_ref = self.base.symbol_ref(&selector);
            let head_ref = self.head.symbol_ref(&selector);
            let file_change = self.file_change_for_base(&selector);

            if self.base.ambiguous_selectors.contains(&selector)
                || self.head.ambiguous_selectors.contains(&selector)
            {
                // More than one indexed row shares this selector, so no
                // selector-addressed query can attribute evidence to one of
                // them. Ambiguity is preserved rather than resolved.
                self.symbols.push(uncertain_pair(
                    base_ref,
                    head_ref,
                    PairingEvidence::None,
                    file_change,
                    "the selector addresses more than one indexed symbol on this side",
                    "Several indexed symbols share this canonical selector; the index collapses \
                     them, so no candidate is chosen.",
                ));
                continue;
            }

            let base_source = self.source_of(SnapshotSide::Base, &selector)?;
            let head_source = self.source_of(SnapshotSide::Head, &selector)?;
            let (Some(base_source), Some(head_source)) = (base_source, head_source) else {
                // The overview listed the symbol but `show` could not resolve
                // it; state the correspondence without claiming a body change.
                self.symbols.push(uncertain_pair(
                    base_ref,
                    head_ref,
                    PairingEvidence::Selector,
                    file_change,
                    "source for this selector could not be read on this side",
                    "The index lists this selector on both sides but its source span could not be \
                     read, so no body or signature comparison was made.",
                ));
                continue;
            };

            if base_source.bytes == head_source.bytes {
                if !base_source.truncated && !head_source.truncated {
                    // Byte-identical on both sides: the containing file changed
                    // but this symbol did not.
                    continue;
                }
                // Only the first `DEFAULT_SHOW_MAX_BYTES` of each side were
                // read, and those prefixes match. A truncated result is never
                // presented as complete, so this is recorded as uncertain
                // rather than silently dropped as unchanged.
                self.symbols.push(uncertain_pair(
                    base_ref,
                    head_ref,
                    PairingEvidence::Selector,
                    file_change,
                    format!("source comparison was bounded to {DEFAULT_SHOW_MAX_BYTES} bytes")
                        .as_str(),
                    format!(
                        "The first {DEFAULT_SHOW_MAX_BYTES} bytes are identical on both sides, \
                         but at least one side was truncated by that bound, so whether the symbol \
                         changed is undetermined."
                    )
                    .as_str(),
                ));
                continue;
            }

            let signature_changed = base_source.signature != head_source.signature;
            let status = if signature_changed {
                ChangeStatus::SignatureChanged
            } else {
                ChangeStatus::Modified
            };
            let note = if signature_changed {
                Some(format!(
                    "signature changed: `{}` -> `{}`",
                    base_source.signature, head_source.signature
                ))
            } else {
                Some("body changed; signature line is unchanged".to_string())
            };
            self.symbols.push(ChangedSymbol {
                status,
                pairing: if signature_changed {
                    Pairing::SignatureChanged
                } else {
                    Pairing::SameSelector
                },
                pairing_evidence: if signature_changed {
                    PairingEvidence::Signature
                } else {
                    PairingEvidence::Selector
                },
                base: Some(base_ref),
                head: Some(head_ref),
                supporting_snapshots: vec!["base".to_string(), "head".to_string()],
                file_change: file_change.0,
                base_path: file_change.1,
                head_path: file_change.2,
                uncertain_candidates: Vec::new(),
                note,
            });
        }
        Ok(())
    }

    /// Rungs 3 and 4: symbols inside a file Git resolved as renamed or copied.
    fn pair_across_renamed_files(&mut self) -> Result<(), ChangesError> {
        let pairs: Vec<(String, String, FileChangeKind)> = self
            .files
            .renamed_pairs()
            .map(|(base, head, kind)| (base.clone(), head.clone(), kind))
            .collect();

        for (base_path, head_path, kind) in pairs {
            let evidence = match kind {
                FileChangeKind::Copied => PairingEvidence::GitCopy,
                _ => PairingEvidence::GitRename,
            };
            self.pair_moved_in_file(&base_path, &head_path, kind, evidence)?;
            self.pair_renamed_in_file(&base_path, &head_path, kind, evidence)?;
        }
        Ok(())
    }

    /// Rung 3, within one renamed or copied file pair: same name and kind.
    fn pair_moved_in_file(
        &mut self,
        base_path: &str,
        head_path: &str,
        kind: FileChangeKind,
        evidence: PairingEvidence,
    ) -> Result<(), ChangesError> {
        let base_side = self.unpaired_in(self.base, base_path);
        let head_side = self.unpaired_in(self.head, head_path);

        let mut head_by_identity: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
        for selector in &head_side {
            if let Some(symbol) = self.head.symbols.get(selector) {
                head_by_identity
                    .entry((symbol.name.clone(), symbol.kind.clone()))
                    .or_default()
                    .push(selector.clone());
            }
        }

        let mut base_by_identity: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
        for selector in &base_side {
            if let Some(symbol) = self.base.symbols.get(selector) {
                base_by_identity
                    .entry((symbol.name.clone(), symbol.kind.clone()))
                    .or_default()
                    .push(selector.clone());
            }
        }

        for (identity, base_selectors) in base_by_identity {
            let Some(head_selectors) = head_by_identity.get(&identity) else {
                continue;
            };
            if base_selectors.len() == 1 && head_selectors.len() == 1 {
                let (Some(base_selector), Some(head_selector)) =
                    (base_selectors.first(), head_selectors.first())
                else {
                    continue;
                };
                self.emit_moved(base_selector, head_selector, kind, evidence)?;
            } else {
                self.emit_uncertain_group(
                    base_selectors.as_slice(),
                    head_selectors.as_slice(),
                    kind,
                    format!(
                        "`{}` occurs more than once on one side of this file rename",
                        identity.0
                    ),
                );
            }
        }
        Ok(())
    }

    /// Rung 4, within one renamed or copied file pair: same kind, different
    /// name, once every same-name pairing has been made.
    fn pair_renamed_in_file(
        &mut self,
        base_path: &str,
        head_path: &str,
        kind: FileChangeKind,
        evidence: PairingEvidence,
    ) -> Result<(), ChangesError> {
        let base_side = self.unpaired_in(self.base, base_path);
        let head_side = self.unpaired_in(self.head, head_path);

        let mut base_by_kind: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for selector in &base_side {
            if let Some(symbol) = self.base.symbols.get(selector) {
                base_by_kind
                    .entry(symbol.kind.clone())
                    .or_default()
                    .push(selector.clone());
            }
        }
        let mut head_by_kind: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for selector in &head_side {
            if let Some(symbol) = self.head.symbols.get(selector) {
                head_by_kind
                    .entry(symbol.kind.clone())
                    .or_default()
                    .push(selector.clone());
            }
        }

        for (symbol_kind, base_selectors) in base_by_kind {
            let Some(head_selectors) = head_by_kind.get(&symbol_kind) else {
                continue;
            };
            if base_selectors.len() == 1 && head_selectors.len() == 1 {
                let (Some(base_selector), Some(head_selector)) =
                    (base_selectors.first(), head_selectors.first())
                else {
                    continue;
                };
                self.emit_renamed(base_selector, head_selector, kind, evidence)?;
            } else {
                self.emit_uncertain_group(
                    base_selectors.as_slice(),
                    head_selectors.as_slice(),
                    kind,
                    format!(
                        "several `{symbol_kind}` symbols are unpaired on both sides of this file \
                         rename"
                    ),
                );
            }
        }
        Ok(())
    }

    /// Rung 5: same name and kind at different paths with no Git rename or copy
    /// evidence linking the two files. The index has no cross-path symbol
    /// identity, so every candidate is listed and none is chosen.
    fn pair_uncertain_cross_path(&mut self) {
        let mut base_by_identity: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
        for selector in self.unpaired_base.clone() {
            if let Some(symbol) = self.base.symbols.get(&selector) {
                base_by_identity
                    .entry((symbol.name.clone(), symbol.kind.clone()))
                    .or_default()
                    .push(selector);
            }
        }
        let mut head_by_identity: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
        for selector in self.unpaired_head.clone() {
            if let Some(symbol) = self.head.symbols.get(&selector) {
                head_by_identity
                    .entry((symbol.name.clone(), symbol.kind.clone()))
                    .or_default()
                    .push(selector);
            }
        }

        for (identity, base_selectors) in base_by_identity {
            let Some(head_selectors) = head_by_identity.get(&identity) else {
                continue;
            };
            self.emit_uncertain_group(
                base_selectors.as_slice(),
                head_selectors.as_slice(),
                FileChangeKind::Other,
                format!(
                    "`{}` disappears at one path and appears at another, with no Git rename or \
                     copy evidence linking the two files",
                    identity.0
                ),
            );
        }
    }

    /// Everything still unpaired is stated relative to the two named SHAs only.
    fn emit_unpaired(&mut self) {
        for selector in std::mem::take(&mut self.unpaired_base) {
            let (file_change, base_path, head_path) = self.file_change_for_base(&selector);
            self.symbols.push(ChangedSymbol {
                status: ChangeStatus::Removed,
                pairing: Pairing::SameSelector,
                pairing_evidence: PairingEvidence::Selector,
                base: Some(self.base.symbol_ref(&selector)),
                head: None,
                supporting_snapshots: vec!["base".to_string()],
                file_change,
                base_path,
                head_path,
                uncertain_candidates: Vec::new(),
                note: Some(
                    "Present in base and absent from head. This is not a claim that nothing \
                     replaced it."
                        .to_string(),
                ),
            });
        }
        for selector in std::mem::take(&mut self.unpaired_head) {
            let (file_change, base_path, head_path) = self.file_change_for_head(&selector);
            self.symbols.push(ChangedSymbol {
                status: ChangeStatus::Added,
                pairing: Pairing::SameSelector,
                pairing_evidence: PairingEvidence::Selector,
                base: None,
                head: Some(self.head.symbol_ref(&selector)),
                supporting_snapshots: vec!["head".to_string()],
                file_change,
                base_path,
                head_path,
                uncertain_candidates: Vec::new(),
                note: Some(
                    "Present in head and absent from base. This is not a claim that it did not \
                     exist under another identity."
                        .to_string(),
                ),
            });
        }
    }

    fn emit_moved(
        &mut self,
        base_selector: &str,
        head_selector: &str,
        kind: FileChangeKind,
        evidence: PairingEvidence,
    ) -> Result<(), ChangesError> {
        self.unpaired_base.remove(base_selector);
        self.unpaired_head.remove(head_selector);
        let base_source = self.source_of(SnapshotSide::Base, base_selector)?;
        let head_source = self.source_of(SnapshotSide::Head, head_selector)?;
        let content_identical = match (base_source.as_ref(), head_source.as_ref()) {
            (Some(base), Some(head)) => Some(base.bytes == head.bytes),
            _ => None,
        };
        let body = match content_identical {
            Some(true) => "content is byte-identical across the move",
            Some(false) => "content also changed across the move",
            None => "content could not be compared across the move",
        };
        let (_, base_path, head_path) = self.file_change_for_base(base_selector);
        self.symbols.push(ChangedSymbol {
            status: ChangeStatus::Moved,
            pairing: Pairing::Moved,
            pairing_evidence: evidence,
            base: Some(self.base.symbol_ref(base_selector)),
            head: Some(self.head.symbol_ref(head_selector)),
            supporting_snapshots: vec!["base".to_string(), "head".to_string()],
            file_change: kind,
            base_path,
            head_path,
            uncertain_candidates: Vec::new(),
            note: Some(format!(
                "Paired by Git {} detection for the containing file; the index carries no \
                 cross-path symbol identity of its own. {body}.",
                kind.noun()
            )),
        });
        Ok(())
    }

    fn emit_renamed(
        &mut self,
        base_selector: &str,
        head_selector: &str,
        kind: FileChangeKind,
        evidence: PairingEvidence,
    ) -> Result<(), ChangesError> {
        self.unpaired_base.remove(base_selector);
        self.unpaired_head.remove(head_selector);
        let (_, base_path, head_path) = self.file_change_for_base(base_selector);
        self.symbols.push(ChangedSymbol {
            status: ChangeStatus::Renamed,
            pairing: Pairing::Renamed,
            pairing_evidence: evidence,
            base: Some(self.base.symbol_ref(base_selector)),
            head: Some(self.head.symbol_ref(head_selector)),
            supporting_snapshots: vec!["base".to_string(), "head".to_string()],
            file_change: kind,
            base_path,
            head_path,
            uncertain_candidates: Vec::new(),
            note: Some(format!(
                "Sole remaining symbol of this kind on each side of a Git {} of the containing \
                 file. A pairing label is evidence about identity, not about behavior.",
                kind.noun()
            )),
        });
        Ok(())
    }

    fn emit_uncertain_group(
        &mut self,
        base_selectors: &[String],
        head_selectors: &[String],
        kind: FileChangeKind,
        reason: String,
    ) {
        let mut candidates = Vec::new();
        for selector in base_selectors {
            if !self.unpaired_base.remove(selector) {
                continue;
            }
            candidates.push(UncertainCandidate {
                symbol: self.base.symbol_ref(selector),
                reason: reason.clone(),
            });
        }
        for selector in head_selectors {
            if !self.unpaired_head.remove(selector) {
                continue;
            }
            candidates.push(UncertainCandidate {
                symbol: self.head.symbol_ref(selector),
                reason: reason.clone(),
            });
        }
        if candidates.is_empty() {
            return;
        }

        let base_path = base_selectors
            .first()
            .and_then(|selector| self.base.symbols.get(selector))
            .map(|symbol| symbol.path.clone());
        let head_path = head_selectors
            .first()
            .and_then(|selector| self.head.symbols.get(selector))
            .map(|symbol| symbol.path.clone());
        self.symbols.push(ChangedSymbol {
            status: ChangeStatus::Uncertain,
            pairing: Pairing::Uncertain,
            pairing_evidence: PairingEvidence::None,
            base: None,
            head: None,
            supporting_snapshots: vec!["base".to_string(), "head".to_string()],
            file_change: kind,
            base_path,
            head_path,
            uncertain_candidates: candidates,
            note: Some(format!(
                "{reason}. Every candidate is listed and none is chosen."
            )),
        });
    }

    fn unpaired_in(&self, inventory: &SideInventory, path: &str) -> Vec<String> {
        let pending = if inventory.side == SnapshotSide::Base {
            &self.unpaired_base
        } else {
            &self.unpaired_head
        };
        inventory
            .selectors_in(path)
            .iter()
            .filter(|selector| pending.contains(*selector))
            .cloned()
            .collect()
    }

    fn file_change_for_base(
        &self,
        selector: &str,
    ) -> (FileChangeKind, Option<String>, Option<String>) {
        let Some(symbol) = self.base.symbols.get(selector) else {
            return (FileChangeKind::Other, None, None);
        };
        match self.files.for_base(symbol.path.as_str()) {
            Some(delta) => (delta.kind, delta.base_path.clone(), delta.head_path.clone()),
            None => (
                FileChangeKind::Other,
                Some(symbol.path.clone()),
                Some(symbol.path.clone()),
            ),
        }
    }

    fn file_change_for_head(
        &self,
        selector: &str,
    ) -> (FileChangeKind, Option<String>, Option<String>) {
        let Some(symbol) = self.head.symbols.get(selector) else {
            return (FileChangeKind::Other, None, None);
        };
        match self.files.for_head(symbol.path.as_str()) {
            Some(delta) => (delta.kind, delta.base_path.clone(), delta.head_path.clone()),
            None => (
                FileChangeKind::Other,
                Some(symbol.path.clone()),
                Some(symbol.path.clone()),
            ),
        }
    }

    fn source_of(
        &self,
        side: SnapshotSide,
        selector: &str,
    ) -> Result<Option<SymbolSource>, ChangesError> {
        let Ok(parsed) = selector.parse::<Selector>() else {
            return Ok(None);
        };
        let view = self
            .comparison
            .snapshot(side)
            .graph()
            .show(&parsed, DEFAULT_SHOW_MAX_BYTES)
            .map_err(|error| ChangesError::Query {
                operation: "read changed symbol source",
                side,
                reason: error.to_string(),
            })?;
        Ok(view.map(|view| SymbolSource {
            signature: signature_line(view.bytes.as_slice()),
            truncated: view.metadata.truncated,
            bytes: view.bytes,
        }))
    }
}

/// A two-sided entry whose correspondence the available evidence cannot settle.
///
/// Ambiguity is a first-class state: both occurrences are listed as candidates,
/// each carrying the same reason, and neither is elevated over the other.
fn uncertain_pair(
    base: SymbolRef,
    head: SymbolRef,
    pairing_evidence: PairingEvidence,
    file_change: (FileChangeKind, Option<String>, Option<String>),
    candidate_reason: &str,
    note: &str,
) -> ChangedSymbol {
    ChangedSymbol {
        status: ChangeStatus::Uncertain,
        pairing: Pairing::Uncertain,
        pairing_evidence,
        base: Some(base.clone()),
        head: Some(head.clone()),
        supporting_snapshots: vec!["base".to_string(), "head".to_string()],
        file_change: file_change.0,
        base_path: file_change.1,
        head_path: file_change.2,
        uncertain_candidates: vec![
            UncertainCandidate {
                symbol: base,
                reason: candidate_reason.to_string(),
            },
            UncertainCandidate {
                symbol: head,
                reason: candidate_reason.to_string(),
            },
        ],
        note: Some(note.to_string()),
    }
}

/// A symbol's source bytes plus its signature line.
///
/// `bytes` is bounded by [`DEFAULT_SHOW_MAX_BYTES`], so `truncated` says
/// whether the comparison saw the whole symbol. Two truncated symbols whose
/// visible prefixes are identical are not evidence that the symbols are.
struct SymbolSource {
    bytes: Vec<u8>,
    signature: String,
    truncated: bool,
}

/// Selectors eligible to be reported as changed: indexed symbols in files the
/// Git diff touched.
fn candidate_selectors<'a>(
    inventory: &SideInventory,
    paths: impl Iterator<Item = &'a String>,
) -> BTreeSet<String> {
    let mut selectors = BTreeSet::new();
    for path in paths {
        for selector in inventory.selectors_in(path) {
            selectors.insert(selector.clone());
        }
    }
    selectors
}

/// The first non-empty line of a symbol's source, used as its signature.
///
/// The index stores a normalized signature, but the public API does not return
/// it, so the explorer derives one from the source span it can read. This is a
/// deliberate approximation: a change confined to a symbol's body leaves this
/// line untouched, while a parameter, return type, or visibility change alters
/// it. A multi-line declaration whose first line is unchanged is reported as
/// `modified` rather than `signature_changed`.
fn signature_line(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_stable() {
        assert_eq!(ChangeStatus::Added.label(), "added");
        assert_eq!(ChangeStatus::Removed.label(), "removed");
        assert_eq!(ChangeStatus::Modified.label(), "modified");
        assert_eq!(ChangeStatus::SignatureChanged.label(), "signature_changed");
        assert_eq!(ChangeStatus::Moved.label(), "moved");
        assert_eq!(ChangeStatus::Renamed.label(), "renamed");
        assert_eq!(ChangeStatus::Uncertain.label(), "uncertain");
        assert_eq!(Pairing::SameSelector.label(), "same_selector");
        assert_eq!(Pairing::Uncertain.label(), "uncertain");
        assert_eq!(FileChangeKind::TypeChanged.label(), "type_changed");
        assert_eq!(
            OutOfScopeReason::UnsupportedLanguage.label(),
            "unsupported_language"
        );
    }

    #[test]
    fn signature_line_skips_leading_blank_lines() {
        assert_eq!(signature_line(b"\n\n  fn a() {\n  x\n}"), "fn a() {");
        assert_eq!(
            signature_line(b"def process(x, y=0):\n"),
            "def process(x, y=0):"
        );
        assert_eq!(signature_line(b""), "");
    }

    #[test]
    fn exclusion_reasons_map_to_out_of_scope_reasons() {
        assert_eq!(
            OutOfScopeReason::from_exclusion(ExclusionReason::Symlink),
            OutOfScopeReason::Symlink
        );
        assert_eq!(
            OutOfScopeReason::from_exclusion(ExclusionReason::OversizeBlob { bytes: 9 }),
            OutOfScopeReason::OversizeBlob
        );
        assert_eq!(
            OutOfScopeReason::from_exclusion(ExclusionReason::UnsafeName),
            OutOfScopeReason::UnsafeName
        );
    }

    #[test]
    fn file_change_kinds_map_from_git_deltas() {
        assert_eq!(
            FileChangeKind::from_delta(Delta::Renamed),
            FileChangeKind::Renamed
        );
        assert_eq!(
            FileChangeKind::from_delta(Delta::Typechange),
            FileChangeKind::TypeChanged
        );
        assert_eq!(
            FileChangeKind::from_delta(Delta::Unreadable),
            FileChangeKind::Other
        );
    }

    #[test]
    fn changed_files_index_both_sides_of_a_rename() {
        let mut files = ChangedFiles::default();
        files.push(FileDelta {
            kind: FileChangeKind::Renamed,
            base_path: Some("helper.py".to_string()),
            head_path: Some("helpers.py".to_string()),
        });
        assert_eq!(
            files.for_base("helper.py").map(|delta| delta.kind),
            Some(FileChangeKind::Renamed)
        );
        assert_eq!(
            files.for_head("helpers.py").map(|delta| delta.kind),
            Some(FileChangeKind::Renamed)
        );
        let pairs: Vec<_> = files
            .renamed_pairs()
            .map(|(base, head, _)| (base.clone(), head.clone()))
            .collect();
        assert_eq!(
            pairs,
            vec![("helper.py".to_string(), "helpers.py".to_string())]
        );
    }
}
