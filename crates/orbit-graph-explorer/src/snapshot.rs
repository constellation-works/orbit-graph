//! Immutable base/head snapshots of a Git repository.
//!
//! A [`Comparison`] resolves two user-supplied refs to immutable commit SHAs,
//! materializes each commit into its own task-owned temporary tree, and indexes
//! each tree through the public [`orbit_graph`] API. Queries then run against
//! one snapshot at a time, so every answer is attributable to exactly one
//! revision.
//!
//! Snapshot trees and their indexes are cached by [`crate::cache`], keyed by
//! `(commit SHA, EXTRACTOR_VERSION, STORE_SCHEMA_VERSION)`, so a second launch
//! against the same revisions reuses the index instead of paying the extraction
//! cost again. A key that does not match the running binary is discarded and
//! rebuilt. When the cache directory is unusable the snapshot falls back to a
//! task-owned temporary tree and says so, rather than failing the comparison.
//!
//! Safety rules this module enforces:
//!
//! - The user's working tree is never written to, checked out over, or cleaned.
//!   Snapshot trees live in the cache directory (by default
//!   `<repo>/.orbit-graph/explorer/snapshots/`) or, when that is unusable,
//!   under the system temporary directory, where they are removed when the
//!   [`Comparison`] is dropped. The repository's own `.orbit-graph/*.db` index
//!   files are never read or written.
//! - Repository content is never executed. Git is driven through `git2`; no
//!   shell string is ever interpolated, no hook, build, or package script runs,
//!   and materialized files are written without the executable bit.
//! - Symlinks, submodules, oversize blobs, and unsafe tree-entry names are
//!   skipped and reported in [`MaterializationReport::excluded`] rather than
//!   silently dropped.
//!
//! Uncommitted work is excluded from snapshots by construction: a snapshot
//! contains exactly the committed tree. The working tree is inspected
//! separately, and a dirty state is reported through
//! [`Comparison::working_tree`] so the caller can disclose it.

use std::collections::BTreeSet;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use git2::{ObjectType, Oid, Repository, RepositoryInitOptions, Status, StatusOptions, TreeEntry};
use orbit_graph::{
    Confidence, Graph, GraphError, ImpactResult, RefOpts, RefResult, Selector, SyncMode,
    SyncObserver, SyncOutcome, SyncPhase, SyncPolicy, SyncProgress,
};
use tempfile::TempDir;
use thiserror::Error;

use crate::cache::{
    CacheError, CacheOutcome, IndexIdentity, SnapshotCache, StoredBuild, StoredExclusion,
    default_cache_dir,
};

/// Largest blob materialized into a snapshot tree.
///
/// Blobs above this size are excluded with [`ExclusionReason::OversizeBlob`].
/// The graph extractors target source files; very large blobs are generated,
/// vendored, or binary payloads whose extraction cost is not justified.
pub const DEFAULT_MAX_BLOB_BYTES: u64 = 4 * 1024 * 1024;

/// Maximum number of dirty working-tree entries reported in detail.
///
/// [`WorkingTreeState::truncated`] records that the listing was cut; the dirty
/// verdict itself is never truncated.
pub const DIRTY_ENTRY_CAP: usize = 200;

/// How the base revision of a comparison was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ComparisonMode {
    /// The base ref is used exactly as supplied: a direct `base -> head`
    /// comparison with no merge-base computation.
    DirectBaseHead,
}

impl ComparisonMode {
    /// Stable label used in reports and user-facing output.
    pub fn label(self) -> &'static str {
        match self {
            Self::DirectBaseHead => "direct_base_head",
        }
    }
}

impl Display for ComparisonMode {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Which side of a comparison a snapshot represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotSide {
    /// The revision the change is measured from.
    Base,
    /// The revision the change is measured to.
    Head,
}

impl SnapshotSide {
    /// Stable label used in reports and user-facing output.
    pub fn label(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Head => "head",
        }
    }
}

impl Display for SnapshotSide {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// State of one side's cold build, observed through [`ComparisonProgress`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildState {
    /// The build has not started.
    Pending,
    /// The commit tree is being materialized into the cache or a temporary
    /// directory.
    Materializing,
    /// The materialized tree is being indexed.
    Indexing,
    /// The side is indexed and queryable.
    Ready,
    /// The build failed. [`Comparison::open_with_progress`] itself never
    /// reports this state: it surfaces a failure as an `Err`, and a caller
    /// that wants this state in its own status board, such as the explorer
    /// service, sets it after catching that `Err`.
    Failed,
    /// [`ComparisonProgress::is_cancelled`] stopped the build before it
    /// reached [`BuildState::Ready`].
    Cancelled,
}

impl BuildState {
    /// Stable label used in payloads and reports.
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Materializing => "materializing",
            Self::Indexing => "indexing",
            Self::Ready => "ready",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Finer-grained phase within [`BuildState::Materializing`] and
/// [`BuildState::Indexing`], surfaced alongside `state` so a progress
/// indicator can tell "extracting" apart from "resolving" instead of reading
/// `indexing` as one undifferentiated phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildPhase {
    /// The commit tree is being materialized. Corresponds to
    /// [`BuildState::Materializing`].
    Materializing,
    /// `orbit_graph`'s pass 1: files are read, parsed, and written.
    Extracting,
    /// `orbit_graph`'s pass 2: raw refs are resolved against the confidence
    /// ladder. This is the phase [`BuildStatus::files_indexed`] does not
    /// cover: it reaches `files_seen` well before resolving finishes.
    Resolving,
}

impl BuildPhase {
    /// Stable label used in payloads and reports.
    pub fn label(self) -> &'static str {
        match self {
            Self::Materializing => "materializing",
            Self::Extracting => "extracting",
            Self::Resolving => "resolving",
        }
    }
}

/// `(done, total)` counters for the phase named by [`BuildStatus::phase`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhaseProgress {
    /// Units of `phase` completed so far.
    pub done: usize,
    /// Total units `phase` will process.
    pub total: usize,
}

/// Live progress for one side of a comparison build.
///
/// [`Comparison::open_with_progress`] itself never reports
/// [`BuildState::Failed`]: a build failure surfaces as an `Err` from that
/// call, and a caller that wants the failure folded into its own status board
/// sets that state itself after catching the `Err`, alongside the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildStatus {
    /// Current phase of the build.
    pub state: BuildState,
    /// Total files this phase will touch, once known; `0` before that.
    pub files_seen: usize,
    /// Files processed so far; increases monotonically up to `files_seen`.
    pub files_indexed: usize,
    /// Tree entries materialization deliberately did not write (symlinks,
    /// submodules, oversize blobs, unsafe names, and unsupported object
    /// kinds).
    pub files_ignored: usize,
    /// The subset of `files_ignored` excluded because their Git object kind
    /// is not one this crate materializes (see
    /// [`ExclusionReason::UnsupportedKind`]).
    pub unsupported_constructs: usize,
    /// Languages observed among the files indexed so far, sorted and
    /// deduplicated. Best-effort: derived from file extensions as indexing
    /// progresses, not from the extractor that actually ran.
    pub languages: Vec<String>,
    /// Finer-grained phase within `state`, when one is known. `None` while
    /// `state` is `pending`, `ready`, `failed`, or `cancelled`.
    pub phase: Option<BuildPhase>,
    /// `(done, total)` counters for `phase`. `None` when `phase` has not yet
    /// produced a counter (materialization, and the moment indexing starts
    /// before pass 1's first report).
    pub phase_progress: Option<PhaseProgress>,
}

impl BuildStatus {
    fn pending() -> Self {
        Self {
            state: BuildState::Pending,
            files_seen: 0,
            files_indexed: 0,
            files_ignored: 0,
            unsupported_constructs: 0,
            languages: Vec::new(),
            phase: None,
            phase_progress: None,
        }
    }
}

/// Observes live per-side status while [`Comparison::open_with_progress`]
/// materializes and indexes both sides, and can request cancellation.
pub trait ComparisonProgress: Send + Sync {
    /// Called whenever `side`'s status changes.
    fn on_status(&self, side: SnapshotSide, status: &BuildStatus);
    /// Checked between materialization entries and between indexed files;
    /// once this returns `true`, the build stops as soon as it safely can.
    fn is_cancelled(&self) -> bool;
}

/// Outcome of [`Comparison::open_with_progress`].
pub enum ComparisonOutcome {
    /// Both sides were materialized and indexed.
    Ready(Box<Comparison>),
    /// [`ComparisonProgress::is_cancelled`] stopped the build. Neither side's
    /// cache entry, if any was being built, was published.
    Cancelled,
}

/// Best-effort language guess from a file extension, for live status display
/// only. Mirrors the extensions `orbit_graph`'s registered extractors accept;
/// a mismatch here never affects what gets indexed.
fn language_hint(path: &str) -> Option<&'static str> {
    let extension = Path::new(path).extension()?.to_str()?;
    Some(match extension {
        "rs" => "rust",
        "c" | "h" => "c",
        "md" | "markdown" => "markdown",
        "yaml" | "yml" | "toml" | "json" | "env" => "config",
        "java" => "java",
        "js" | "jsx" => "javascript",
        "kt" | "kts" => "kotlin",
        "cs" => "csharp",
        "go" => "go",
        "py" => "python",
        "rb" => "ruby",
        "ts" | "tsx" => "typescript",
        _ => return None,
    })
}

/// Why a tree entry was not materialized into a snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExclusionReason {
    /// The entry is a symbolic link. Links are not recreated, so no snapshot
    /// path can resolve outside the snapshot tree.
    Symlink,
    /// The entry is a submodule (gitlink). Submodule content is not fetched.
    Submodule,
    /// The blob exceeds [`DEFAULT_MAX_BLOB_BYTES`].
    OversizeBlob {
        /// Size of the excluded blob in bytes.
        bytes: u64,
    },
    /// The entry name is empty, a path traversal component, contains a path
    /// separator or NUL byte, or is a Git metadata directory name.
    UnsafeName,
    /// The entry has a type this module does not materialize.
    UnsupportedKind,
}

impl ExclusionReason {
    /// Stable label used in reports and user-facing output.
    pub fn label(self) -> &'static str {
        match self {
            Self::Symlink => "symlink",
            Self::Submodule => "submodule",
            Self::OversizeBlob { .. } => "oversize_blob",
            Self::UnsafeName => "unsafe_name",
            Self::UnsupportedKind => "unsupported_kind",
        }
    }

    /// Size recorded with the reason, for an oversize blob.
    pub fn bytes(self) -> Option<u64> {
        match self {
            Self::OversizeBlob { bytes } => Some(bytes),
            _ => None,
        }
    }

    /// Rebuild a reason from a cached [`ExclusionReason::label`].
    ///
    /// An unrecognized label becomes [`ExclusionReason::UnsupportedKind`]
    /// rather than silently disappearing from the report.
    pub fn from_label(label: &str, bytes: Option<u64>) -> Self {
        match label {
            "symlink" => Self::Symlink,
            "submodule" => Self::Submodule,
            "oversize_blob" => Self::OversizeBlob {
                bytes: bytes.unwrap_or_default(),
            },
            "unsafe_name" => Self::UnsafeName,
            _ => Self::UnsupportedKind,
        }
    }
}

/// A tree entry that was deliberately not materialized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExcludedEntry {
    /// Slash-separated path of the entry inside the commit tree.
    pub path: String,
    /// Why the entry was excluded.
    pub reason: ExclusionReason,
}

/// What a snapshot materialization wrote and what it left out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MaterializationReport {
    /// Number of blobs written into the snapshot tree.
    pub files_written: usize,
    /// Total bytes written into the snapshot tree.
    pub bytes_written: u64,
    /// Entries that were not materialized, in tree order.
    pub excluded: Vec<ExcludedEntry>,
}

/// Kind of uncommitted change observed in the user's working tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum WorkingTreeChange {
    /// The path has unresolved merge conflicts.
    Conflicted,
    /// The path exists on disk but is not tracked.
    Untracked,
    /// The path is newly added to the index.
    Added,
    /// The path's content differs from the committed content.
    Modified,
    /// The path is deleted in the index or on disk.
    Deleted,
    /// The path was renamed in the index or working tree.
    Renamed,
    /// The path changed file type (for example file to symlink).
    TypeChanged,
}

impl WorkingTreeChange {
    /// Stable label used in reports and user-facing output.
    pub fn label(self) -> &'static str {
        match self {
            Self::Conflicted => "conflicted",
            Self::Untracked => "untracked",
            Self::Added => "added",
            Self::Modified => "modified",
            Self::Deleted => "deleted",
            Self::Renamed => "renamed",
            Self::TypeChanged => "type_changed",
        }
    }
}

/// One uncommitted working-tree change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkingTreeEntry {
    /// Repository-relative, slash-separated path.
    pub path: String,
    /// Observed change kind.
    pub change: WorkingTreeChange,
}

/// Uncommitted state of the user's working tree at comparison time.
///
/// Snapshots never include this state. A caller that renders evidence must
/// disclose `dirty` so the reader knows the indexed revisions are not what is
/// currently on disk.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkingTreeState {
    /// Whether any uncommitted change (tracked or untracked) was observed.
    pub dirty: bool,
    /// Observed changes, capped at [`DIRTY_ENTRY_CAP`] entries.
    pub entries: Vec<WorkingTreeEntry>,
    /// Whether `entries` was truncated by [`DIRTY_ENTRY_CAP`].
    pub truncated: bool,
}

impl WorkingTreeState {
    /// Human-readable notice to show alongside snapshot evidence, or `None`
    /// when the working tree is clean.
    pub fn notice(&self) -> Option<String> {
        if !self.dirty {
            return None;
        }
        let more = if self.truncated { "+" } else { "" };
        Some(format!(
            "Working tree has {}{more} uncommitted change(s). Snapshots index committed revisions \
             only; uncommitted files are excluded from all evidence.",
            self.entries.len()
        ))
    }
}

/// Failure surface of snapshot resolution, materialization, and querying.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SnapshotError {
    /// The supplied path is not usable as a Git working tree.
    #[error("{path} is not a usable Git working tree: {reason}")]
    Repository {
        /// Path that was opened.
        path: PathBuf,
        /// Failure reason.
        reason: String,
    },
    /// A ref did not resolve to a commit.
    #[error("reference `{reference}` did not resolve to a commit: {reason}")]
    Revision {
        /// Ref as supplied by the caller.
        reference: String,
        /// Failure reason.
        reason: String,
    },
    /// A Git object operation failed.
    #[error("{operation}: {reason}")]
    Git {
        /// Operation being performed.
        operation: &'static str,
        /// Failure reason.
        reason: String,
    },
    /// A filesystem operation failed inside a snapshot tree.
    #[error("{operation} at {path}: {reason}")]
    Io {
        /// Operation being performed.
        operation: &'static str,
        /// Path involved in the failed operation.
        path: PathBuf,
        /// Failure reason.
        reason: String,
    },
    /// A snapshot cache operation failed.
    #[error("snapshot cache: {source}")]
    Cache {
        /// Underlying cache error.
        #[source]
        source: CacheError,
    },
    /// A graph operation failed for one snapshot.
    #[error("{operation} for the {side} snapshot at {commit_sha}: {source}")]
    Graph {
        /// Operation being performed.
        operation: &'static str,
        /// Snapshot side the operation belonged to.
        side: SnapshotSide,
        /// Immutable commit SHA of that snapshot.
        commit_sha: String,
        /// Underlying graph error.
        #[source]
        source: GraphError,
    },
}

/// One immutable revision, materialized and indexed in isolation.
///
/// The materialized tree lives as long as the snapshot: [`orbit_graph`] reads
/// source text from disk when resolving reference lines and source views, so
/// the tree must outlive every query against it. A cached tree therefore stays
/// on disk when the snapshot is dropped; only a fallback temporary tree is
/// removed with it.
pub struct Snapshot {
    side: SnapshotSide,
    requested_ref: String,
    commit_sha: String,
    // `graph` is declared before `storage` so the index connection closes
    // before a temporary directory holding it is removed.
    graph: Graph,
    storage: SnapshotStorage,
    tree_root: PathBuf,
    db_path: PathBuf,
    materialization: MaterializationReport,
    files_indexed: usize,
    cache: CacheOutcome,
    cache_note: Option<String>,
    prepared_in: Duration,
}

/// Where a snapshot's materialized tree lives, and who removes it.
enum SnapshotStorage {
    /// A cache entry. It outlives the snapshot: a later launch reuses it, and
    /// only `clean` removes it.
    Cached(PathBuf),
    /// A task-owned temporary tree, removed when the snapshot is dropped.
    Temporary(TempDir),
}

impl Snapshot {
    /// Which side of the comparison this snapshot represents.
    pub fn side(&self) -> SnapshotSide {
        self.side
    }

    /// Ref exactly as supplied by the caller.
    pub fn requested_ref(&self) -> &str {
        self.requested_ref.as_str()
    }

    /// Immutable commit SHA this snapshot indexes.
    pub fn commit_sha(&self) -> &str {
        self.commit_sha.as_str()
    }

    /// Root of the materialized tree backing this snapshot.
    pub fn root(&self) -> &Path {
        self.tree_root.as_path()
    }

    /// Database file backing this snapshot's index.
    pub fn db_path(&self) -> &Path {
        self.db_path.as_path()
    }

    /// Directory that owns the materialized tree: the cache entry when the
    /// snapshot is cached, and the temporary directory otherwise.
    pub fn storage_root(&self) -> &Path {
        match &self.storage {
            SnapshotStorage::Cached(root) => root.as_path(),
            SnapshotStorage::Temporary(tree) => tree.path(),
        }
    }

    /// Whether the tree backing this snapshot outlives the comparison.
    ///
    /// A cached tree is left on disk for the next launch; a temporary tree is
    /// removed when the snapshot is dropped.
    pub fn tree_is_cached(&self) -> bool {
        matches!(self.storage, SnapshotStorage::Cached(_))
    }

    /// Whether this snapshot was served from the cache or built for this
    /// launch.
    pub fn cache_outcome(&self) -> CacheOutcome {
        self.cache
    }

    /// Why the cache was unusable, when it was.
    pub fn cache_note(&self) -> Option<&str> {
        self.cache_note.as_deref()
    }

    /// Index identity of this snapshot: the extractor and store schema
    /// versions its database was built with.
    pub fn index_identity(&self) -> IndexIdentity {
        IndexIdentity::current()
    }

    /// Wall-clock time spent preparing this snapshot, cache lookup included.
    pub fn prepared_in(&self) -> Duration {
        self.prepared_in
    }

    /// Graph handle for this snapshot, for queries beyond the convenience
    /// wrappers below.
    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    /// What the materialization wrote and excluded.
    pub fn materialization(&self) -> &MaterializationReport {
        &self.materialization
    }

    /// Number of files indexed in this snapshot.
    pub fn files_indexed(&self) -> usize {
        self.files_indexed
    }

    /// Inbound references and relations for `selector` within this snapshot.
    pub fn refs(&self, selector: &Selector, opts: &RefOpts) -> Result<RefResult, SnapshotError> {
        self.graph
            .refs(selector, opts)
            .map_err(|source| self.graph_error("query snapshot refs", source))
    }

    /// Bounded impact set around `selector` within this snapshot.
    pub fn impact(
        &self,
        selector: &Selector,
        depth: u8,
        min_confidence: Confidence,
    ) -> Result<ImpactResult, SnapshotError> {
        self.graph
            .impact(selector, depth, min_confidence)
            .map_err(|source| self.graph_error("query snapshot impact", source))
    }

    fn graph_error(&self, operation: &'static str, source: GraphError) -> SnapshotError {
        SnapshotError::Graph {
            operation,
            side: self.side,
            commit_sha: self.commit_sha.clone(),
            source,
        }
    }

    /// Build this side, reporting live status through `progress`.
    ///
    /// Returns `Ok(None)` if `progress.is_cancelled()` stopped the build
    /// before it reached [`BuildState::Ready`]; a cache entry that was being
    /// built for this side is discarded, never published, so no later launch
    /// can see a half-indexed tree under this commit's SHA.
    fn build(
        repo: &Repository,
        side: SnapshotSide,
        requested_ref: &str,
        cache: Option<&SnapshotCache>,
        progress: &dyn ComparisonProgress,
    ) -> Result<Option<Self>, SnapshotError> {
        let started = Instant::now();
        let commit = resolve_commit(repo, requested_ref)?;
        let commit_sha = commit.to_string();

        let prepared = match cache {
            Some(cache) => Self::prepare_cached(repo, side, commit, cache, progress)?,
            None => Self::prepare_temporary(repo, side, commit, progress)?,
        };
        let Some(prepared) = prepared else {
            return Ok(None);
        };

        Ok(Some(Self {
            side,
            requested_ref: requested_ref.to_string(),
            commit_sha,
            graph: prepared.graph,
            storage: prepared.storage,
            tree_root: prepared.tree_root,
            db_path: prepared.db_path,
            materialization: prepared.materialization,
            files_indexed: prepared.files_indexed,
            cache: prepared.cache,
            cache_note: prepared.cache_note,
            prepared_in: started.elapsed(),
        }))
    }

    /// Open a cached entry for `commit`, building and publishing one when no
    /// entry with a matching key exists.
    fn prepare_cached(
        repo: &Repository,
        side: SnapshotSide,
        commit: Oid,
        cache: &SnapshotCache,
        progress: &dyn ComparisonProgress,
    ) -> Result<Option<PreparedSnapshot>, SnapshotError> {
        let commit_sha = commit.to_string();
        if let Some(entry) = cache.lookup(commit_sha.as_str()).map_err(cache_error)? {
            // The tree and the index are immutable, and the key already proves
            // they were produced by this extractor and store schema, so the
            // entry is opened as it stands: no re-materialization, no re-sync.
            let tree_root = entry.tree();
            let db_path = entry.db();
            let graph = open_graph(
                side,
                commit_sha.as_str(),
                tree_root.as_path(),
                db_path.as_path(),
            )?;
            let build = entry.metadata().build.clone();
            progress.on_status(side, &ready_status(&build));
            return Ok(Some(PreparedSnapshot {
                graph,
                storage: SnapshotStorage::Cached(entry.root().to_path_buf()),
                tree_root,
                db_path,
                materialization: materialization_from_stored(&build),
                files_indexed: build.files_indexed,
                cache: CacheOutcome::Hit,
                cache_note: None,
            }));
        }

        if progress.is_cancelled() {
            return Ok(None);
        }
        progress.on_status(
            side,
            &BuildStatus {
                state: BuildState::Materializing,
                phase: Some(BuildPhase::Materializing),
                ..BuildStatus::pending()
            },
        );

        let staged = cache.stage(commit_sha.as_str()).map_err(cache_error)?;
        let staged_tree = staged.tree();
        let staged_db = staged.db();
        let materialization = materialize_commit(repo, commit, staged_tree.as_path())?;
        anchor_git_discovery(staged_tree.as_path())?;

        if progress.is_cancelled() {
            cache.discard(staged).map_err(cache_error)?;
            return Ok(None);
        }

        let files_ignored = materialization.excluded.len();
        let unsupported_constructs = unsupported_construct_count(&materialization);
        let (files_indexed, languages) = {
            // The handle is dropped before the entry is renamed into place, so
            // the database and its write-ahead log are closed when the
            // directory moves.
            let graph = open_graph(
                side,
                commit_sha.as_str(),
                staged_tree.as_path(),
                staged_db.as_path(),
            )?;
            let indexed = index_with_progress(
                &graph,
                side,
                commit_sha.as_str(),
                files_ignored,
                unsupported_constructs,
                progress,
            )?;
            drop(graph);
            match indexed {
                Some(outcome) => (outcome.files_indexed, outcome.languages),
                None => {
                    cache.discard(staged).map_err(cache_error)?;
                    return Ok(None);
                }
            }
        };

        let entry = cache
            .publish(
                staged,
                stored_build(&materialization, files_indexed, languages),
            )
            .map_err(cache_error)?;
        let tree_root = entry.tree();
        let db_path = entry.db();
        let graph = open_graph(
            side,
            commit_sha.as_str(),
            tree_root.as_path(),
            db_path.as_path(),
        )?;
        let build = entry.metadata().build.clone();
        progress.on_status(side, &ready_status(&build));
        Ok(Some(PreparedSnapshot {
            graph,
            storage: SnapshotStorage::Cached(entry.root().to_path_buf()),
            tree_root,
            db_path,
            materialization: materialization_from_stored(&build),
            files_indexed: build.files_indexed,
            cache: CacheOutcome::Miss,
            cache_note: None,
        }))
    }

    /// Materialize and index into a task-owned temporary tree.
    ///
    /// Used when no cache directory is usable. Nothing is reused and nothing is
    /// written to the cache.
    fn prepare_temporary(
        repo: &Repository,
        side: SnapshotSide,
        commit: Oid,
        progress: &dyn ComparisonProgress,
    ) -> Result<Option<PreparedSnapshot>, SnapshotError> {
        if progress.is_cancelled() {
            return Ok(None);
        }
        progress.on_status(
            side,
            &BuildStatus {
                state: BuildState::Materializing,
                phase: Some(BuildPhase::Materializing),
                ..BuildStatus::pending()
            },
        );

        let commit_sha = commit.to_string();
        let tree = TempDir::with_prefix(format!("orbit-graph-explorer-{}-", side.label()))
            .map_err(|source| SnapshotError::Io {
                operation: "create snapshot tree",
                path: std::env::temp_dir(),
                reason: source.to_string(),
            })?;
        let materialization = materialize_commit(repo, commit, tree.path())?;
        anchor_git_discovery(tree.path())?;

        if progress.is_cancelled() {
            // `tree` drops here, removing the temporary directory. Nothing was
            // ever written to the cache, so there is nothing else to discard.
            return Ok(None);
        }

        let files_ignored = materialization.excluded.len();
        let unsupported_constructs = unsupported_construct_count(&materialization);
        let graph = Graph::open(tree.path(), SyncPolicy::Manual).map_err(|source| {
            SnapshotError::Graph {
                operation: "open snapshot graph",
                side,
                commit_sha: commit_sha.clone(),
                source,
            }
        })?;
        let Some(IndexOutcome {
            files_indexed,
            languages,
        }) = index_with_progress(
            &graph,
            side,
            commit_sha.as_str(),
            files_ignored,
            unsupported_constructs,
            progress,
        )?
        else {
            return Ok(None);
        };
        let tree_root = tree.path().to_path_buf();
        let db_path = graph.db_path().path().to_path_buf();
        progress.on_status(
            side,
            &BuildStatus {
                state: BuildState::Ready,
                files_seen: files_indexed,
                files_indexed,
                files_ignored,
                unsupported_constructs,
                languages,
                phase: None,
                phase_progress: None,
            },
        );
        Ok(Some(PreparedSnapshot {
            graph,
            storage: SnapshotStorage::Temporary(tree),
            tree_root,
            db_path,
            materialization,
            files_indexed,
            cache: CacheOutcome::Disabled,
            cache_note: None,
        }))
    }
}

/// Reports [`ComparisonProgress`] no status has been requested for, and never
/// cancels. Used by [`Comparison::open_with_options`], so it shares the exact
/// build path [`Comparison::open_with_progress`] uses without asking any
/// caller to observe progress.
struct NoopComparisonProgress;

impl ComparisonProgress for NoopComparisonProgress {
    fn on_status(&self, _side: SnapshotSide, _status: &BuildStatus) {}

    fn is_cancelled(&self) -> bool {
        false
    }
}

/// Bridges [`ComparisonProgress`] to `orbit_graph`'s [`SyncObserver`], adding
/// the materialization counts (fixed before indexing starts) and a running
/// language guess.
struct ProgressSyncObserver<'a> {
    side: SnapshotSide,
    progress: &'a dyn ComparisonProgress,
    files_ignored: usize,
    unsupported_constructs: usize,
    languages: Mutex<BTreeSet<&'static str>>,
}

impl SyncObserver for ProgressSyncObserver<'_> {
    fn on_progress(&self, sync_progress: &SyncProgress) {
        let languages = {
            let mut languages = self
                .languages
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(path) = sync_progress.current_path.as_deref()
                && let Some(lang) = language_hint(path)
            {
                languages.insert(lang);
            }
            languages.iter().map(|lang| lang.to_string()).collect()
        };
        let phase = match sync_progress.phase {
            SyncPhase::Extracting => BuildPhase::Extracting,
            SyncPhase::Resolving => BuildPhase::Resolving,
        };
        self.progress.on_status(
            self.side,
            &BuildStatus {
                state: BuildState::Indexing,
                files_seen: sync_progress.files_seen,
                files_indexed: sync_progress.files_indexed,
                files_ignored: self.files_ignored,
                unsupported_constructs: self.unsupported_constructs,
                languages,
                phase: Some(phase),
                phase_progress: Some(PhaseProgress {
                    done: sync_progress.units_done,
                    total: sync_progress.units_total,
                }),
            },
        );
    }

    fn is_cancelled(&self) -> bool {
        self.progress.is_cancelled()
    }
}

/// Files indexed and languages observed by a completed [`index_with_progress`]
/// call, carried forward into the side's terminal `ready` status and, for a
/// freshly published cache entry, into its [`StoredBuild`] so a later cache
/// hit can still report them.
struct IndexOutcome {
    files_indexed: usize,
    languages: Vec<String>,
}

/// Index `graph`, reporting progress through `progress`.
///
/// Returns `None` if `progress` requested cancellation before indexing
/// finished.
fn index_with_progress(
    graph: &Graph,
    side: SnapshotSide,
    commit_sha: &str,
    files_ignored: usize,
    unsupported_constructs: usize,
    progress: &dyn ComparisonProgress,
) -> Result<Option<IndexOutcome>, SnapshotError> {
    if progress.is_cancelled() {
        return Ok(None);
    }
    progress.on_status(
        side,
        &BuildStatus {
            state: BuildState::Indexing,
            files_ignored,
            unsupported_constructs,
            phase: Some(BuildPhase::Extracting),
            ..BuildStatus::pending()
        },
    );
    let observer = ProgressSyncObserver {
        side,
        progress,
        files_ignored,
        unsupported_constructs,
        languages: Mutex::new(BTreeSet::new()),
    };
    let outcome = graph
        .sync_with_observer(SyncMode::Full, &observer)
        .map_err(|source| SnapshotError::Graph {
            operation: "index snapshot tree",
            side,
            commit_sha: commit_sha.to_string(),
            source,
        })?;
    let languages = observer
        .languages
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .into_iter()
        .map(str::to_string)
        .collect();
    Ok(match outcome {
        SyncOutcome::Completed(report) => Some(IndexOutcome {
            files_indexed: report.files_indexed,
            languages,
        }),
        SyncOutcome::Cancelled(_) => None,
    })
}

/// Count of `materialization.excluded` entries excluded because their Git
/// object kind is not one this crate materializes.
fn unsupported_construct_count(materialization: &MaterializationReport) -> usize {
    materialization
        .excluded
        .iter()
        .filter(|entry| matches!(entry.reason, ExclusionReason::UnsupportedKind))
        .count()
}

/// [`BuildStatus::Ready`] status for a cache entry opened as it stood, with no
/// live materialization or indexing to report progress for.
fn ready_status(build: &StoredBuild) -> BuildStatus {
    BuildStatus {
        state: BuildState::Ready,
        files_seen: build.files_indexed,
        files_indexed: build.files_indexed,
        files_ignored: build.excluded.len(),
        unsupported_constructs: build
            .excluded
            .iter()
            .filter(|entry| entry.reason == "unsupported_kind")
            .count(),
        languages: build.languages.clone(),
        phase: None,
        phase_progress: None,
    }
}

/// A prepared tree and index, before it becomes a [`Snapshot`].
struct PreparedSnapshot {
    graph: Graph,
    storage: SnapshotStorage,
    tree_root: PathBuf,
    db_path: PathBuf,
    materialization: MaterializationReport,
    files_indexed: usize,
    cache: CacheOutcome,
    cache_note: Option<String>,
}

fn cache_error(source: CacheError) -> SnapshotError {
    SnapshotError::Cache { source }
}

/// Open a graph that indexes `tree_root` at a caller-owned `db_path`.
///
/// The explicit database path is what keeps a snapshot index out of the user
/// repository's own `.orbit-graph/` database files.
fn open_graph(
    side: SnapshotSide,
    commit_sha: &str,
    tree_root: &Path,
    db_path: &Path,
) -> Result<Graph, SnapshotError> {
    Graph::open_with_db_path(tree_root, db_path, SyncPolicy::Manual).map_err(|source| {
        SnapshotError::Graph {
            operation: "open snapshot graph",
            side,
            commit_sha: commit_sha.to_string(),
            source,
        }
    })
}

fn stored_build(
    materialization: &MaterializationReport,
    files_indexed: usize,
    languages: Vec<String>,
) -> StoredBuild {
    StoredBuild {
        files_written: materialization.files_written,
        bytes_written: materialization.bytes_written,
        excluded: materialization
            .excluded
            .iter()
            .map(|entry| StoredExclusion {
                path: entry.path.clone(),
                reason: entry.reason.label().to_string(),
                bytes: entry.reason.bytes(),
            })
            .collect(),
        files_indexed,
        languages,
    }
}

fn materialization_from_stored(build: &StoredBuild) -> MaterializationReport {
    MaterializationReport {
        files_written: build.files_written,
        bytes_written: build.bytes_written,
        excluded: build
            .excluded
            .iter()
            .map(|entry| ExcludedEntry {
                path: entry.path.clone(),
                reason: ExclusionReason::from_label(entry.reason.as_str(), entry.bytes),
            })
            .collect(),
    }
}

/// Where a comparison keeps its snapshot trees and indexes.
#[derive(Debug, Clone, Default)]
pub struct ComparisonOptions {
    /// Cache directory. `None` selects
    /// `<repository>/.orbit-graph/explorer/snapshots`.
    pub cache_dir: Option<PathBuf>,
    /// Skip the cache entirely and use task-owned temporary trees.
    pub no_cache: bool,
}

/// A resolved base/head comparison with both snapshots indexed.
pub struct Comparison {
    repository: PathBuf,
    mode: ComparisonMode,
    base: Snapshot,
    head: Snapshot,
    working_tree: WorkingTreeState,
    cache_dir: Option<PathBuf>,
    cache_note: Option<String>,
    prepared_in: Duration,
}

impl Comparison {
    /// Resolve `base_ref` and `head_ref` in the repository at `repository`,
    /// then materialize and index both revisions, reusing the default snapshot
    /// cache.
    ///
    /// The repository is opened read-only: nothing in the user's working tree,
    /// Git index, or object store is written, and the repository's own
    /// `.orbit-graph/*.db` index files are neither read nor written.
    pub fn open(repository: &Path, base_ref: &str, head_ref: &str) -> Result<Self, SnapshotError> {
        Self::open_with_options(
            repository,
            base_ref,
            head_ref,
            &ComparisonOptions::default(),
        )
    }

    /// Open a comparison with an explicit cache policy.
    ///
    /// An unusable cache directory is reported through
    /// [`Comparison::cache_note`] and both snapshots fall back to task-owned
    /// temporary trees: an inspectable repository must not become
    /// un-inspectable because its cache directory is read-only.
    pub fn open_with_options(
        repository: &Path,
        base_ref: &str,
        head_ref: &str,
        options: &ComparisonOptions,
    ) -> Result<Self, SnapshotError> {
        match Self::open_with_progress(
            repository,
            base_ref,
            head_ref,
            options,
            &NoopComparisonProgress,
        )? {
            ComparisonOutcome::Ready(comparison) => Ok(*comparison),
            ComparisonOutcome::Cancelled => {
                unreachable!("NoopComparisonProgress::is_cancelled never returns true")
            }
        }
    }

    /// Open a comparison like [`Comparison::open_with_options`], but report
    /// live per-side status through `progress` and stop early once it
    /// requests cancellation.
    ///
    /// On cancellation, neither side's cache entry, if one was being built,
    /// is published: a subsequent open of the same scope starts that side's
    /// build over from nothing cached.
    pub fn open_with_progress(
        repository: &Path,
        base_ref: &str,
        head_ref: &str,
        options: &ComparisonOptions,
        progress: &dyn ComparisonProgress,
    ) -> Result<ComparisonOutcome, SnapshotError> {
        let started = Instant::now();
        let repo = open_working_tree(repository)?;
        let workdir = repo
            .workdir()
            .map(|path| fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()))
            .unwrap_or_else(|| repository.to_path_buf());
        let working_tree = working_tree_state(&repo)?;

        for side in [SnapshotSide::Base, SnapshotSide::Head] {
            progress.on_status(side, &BuildStatus::pending());
        }

        let mut cache_note = None;
        let cache = if options.no_cache {
            cache_note = Some("Snapshot caching was disabled for this launch.".to_string());
            None
        } else {
            let dir = options
                .cache_dir
                .clone()
                .unwrap_or_else(|| default_cache_dir(workdir.as_path()));
            match SnapshotCache::open(dir.as_path()) {
                Ok(cache) => Some(cache),
                Err(error) => {
                    cache_note = Some(format!(
                        "Snapshot cache at {} is unusable ({error}); this launch indexed into \
                         task-owned temporary trees and reused nothing.",
                        dir.display()
                    ));
                    None
                }
            }
        };

        if progress.is_cancelled() {
            return Ok(ComparisonOutcome::Cancelled);
        }
        let Some(base) = Snapshot::build(
            &repo,
            SnapshotSide::Base,
            base_ref,
            cache.as_ref(),
            progress,
        )?
        else {
            return Ok(ComparisonOutcome::Cancelled);
        };
        let Some(head) = Snapshot::build(
            &repo,
            SnapshotSide::Head,
            head_ref,
            cache.as_ref(),
            progress,
        )?
        else {
            return Ok(ComparisonOutcome::Cancelled);
        };

        Ok(ComparisonOutcome::Ready(Box::new(Self {
            repository: workdir,
            mode: ComparisonMode::DirectBaseHead,
            base,
            head,
            working_tree,
            cache_dir: cache.map(|cache| cache.dir().to_path_buf()),
            cache_note,
            prepared_in: started.elapsed(),
        })))
    }

    /// Cache directory both snapshots used, when caching was available.
    pub fn cache_dir(&self) -> Option<&Path> {
        self.cache_dir.as_deref()
    }

    /// Why caching was unavailable, when it was.
    pub fn cache_note(&self) -> Option<&str> {
        self.cache_note.as_deref()
    }

    /// Wall-clock time spent resolving and preparing both snapshots.
    pub fn prepared_in(&self) -> Duration {
        self.prepared_in
    }

    /// Working directory of the inspected repository.
    pub fn repository(&self) -> &Path {
        self.repository.as_path()
    }

    /// How the base revision was chosen.
    pub fn mode(&self) -> ComparisonMode {
        self.mode
    }

    /// Base snapshot.
    pub fn base(&self) -> &Snapshot {
        &self.base
    }

    /// Head snapshot.
    pub fn head(&self) -> &Snapshot {
        &self.head
    }

    /// Snapshot for `side`.
    pub fn snapshot(&self, side: SnapshotSide) -> &Snapshot {
        match side {
            SnapshotSide::Base => &self.base,
            SnapshotSide::Head => &self.head,
        }
    }

    /// Uncommitted state observed in the user's working tree when this
    /// comparison was opened.
    pub fn working_tree(&self) -> &WorkingTreeState {
        &self.working_tree
    }
}

/// Open `path` as a Git repository with a working tree.
fn open_working_tree(path: &Path) -> Result<Repository, SnapshotError> {
    let repo = Repository::discover(path).map_err(|source| SnapshotError::Repository {
        path: path.to_path_buf(),
        reason: source.message().to_string(),
    })?;
    if repo.is_bare() || repo.workdir().is_none() {
        return Err(SnapshotError::Repository {
            path: path.to_path_buf(),
            reason: "repository is bare and has no working tree".to_string(),
        });
    }
    Ok(repo)
}

/// Report uncommitted changes without refreshing or rewriting the Git index.
pub fn working_tree_state(repo: &Repository) -> Result<WorkingTreeState, SnapshotError> {
    let mut options = StatusOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false)
        .include_unmodified(false)
        .renames_head_to_index(true)
        .renames_index_to_workdir(true)
        .update_index(false)
        .no_refresh(true);
    let statuses = repo
        .statuses(Some(&mut options))
        .map_err(|source| SnapshotError::Git {
            operation: "read working tree status",
            reason: source.message().to_string(),
        })?;

    let mut state = WorkingTreeState::default();
    for entry in statuses.iter() {
        let Some(change) = classify_status(entry.status()) else {
            continue;
        };
        let path = entry
            .path()
            .map(str::to_string)
            .unwrap_or_else(|_| String::from_utf8_lossy(entry.path_bytes()).into_owned());
        if is_graph_scratch(path.as_str()) {
            // `.orbit-graph/` is graph scratch state, including this crate's
            // own snapshot cache. It is never source, so it is not reported as
            // uncommitted work the snapshots exclude.
            continue;
        }
        state.dirty = true;
        if state.entries.len() >= DIRTY_ENTRY_CAP {
            state.truncated = true;
            continue;
        }
        state.entries.push(WorkingTreeEntry { path, change });
    }

    Ok(state)
}

/// Whether a repository-relative path is graph scratch state.
fn is_graph_scratch(path: &str) -> bool {
    path == ".orbit-graph" || path.starts_with(".orbit-graph/")
}

fn classify_status(status: Status) -> Option<WorkingTreeChange> {
    if status.is_conflicted() {
        return Some(WorkingTreeChange::Conflicted);
    }
    if status.is_wt_new() {
        return Some(WorkingTreeChange::Untracked);
    }
    if status.is_index_new() {
        return Some(WorkingTreeChange::Added);
    }
    if status.is_wt_deleted() || status.is_index_deleted() {
        return Some(WorkingTreeChange::Deleted);
    }
    if status.is_wt_renamed() || status.is_index_renamed() {
        return Some(WorkingTreeChange::Renamed);
    }
    if status.is_wt_typechange() || status.is_index_typechange() {
        return Some(WorkingTreeChange::TypeChanged);
    }
    if status.is_wt_modified() || status.is_index_modified() {
        return Some(WorkingTreeChange::Modified);
    }
    None
}

/// Resolve a ref, tag, or revision string to an immutable commit SHA.
fn resolve_commit(repo: &Repository, reference: &str) -> Result<Oid, SnapshotError> {
    let trimmed = reference.trim();
    if trimmed.is_empty() {
        return Err(SnapshotError::Revision {
            reference: reference.to_string(),
            reason: "reference is empty".to_string(),
        });
    }
    repo.revparse_single(trimmed)
        .and_then(|object| object.peel_to_commit())
        .map(|commit| commit.id())
        .map_err(|source| SnapshotError::Revision {
            reference: reference.to_string(),
            reason: source.message().to_string(),
        })
}

/// Write the committed tree of `commit` into `dest`.
fn materialize_commit(
    repo: &Repository,
    commit: Oid,
    dest: &Path,
) -> Result<MaterializationReport, SnapshotError> {
    let tree = repo
        .find_commit(commit)
        .and_then(|commit| commit.tree())
        .map_err(|source| SnapshotError::Git {
            operation: "load commit tree for snapshot",
            reason: source.message().to_string(),
        })?;
    let mut report = MaterializationReport::default();
    materialize_tree(repo, &tree, dest, "", &mut report)?;
    Ok(report)
}

fn materialize_tree(
    repo: &Repository,
    tree: &git2::Tree<'_>,
    dest: &Path,
    prefix: &str,
    report: &mut MaterializationReport,
) -> Result<(), SnapshotError> {
    for entry in tree.iter() {
        let name = entry
            .name()
            .map(str::to_string)
            .unwrap_or_else(|_| String::from_utf8_lossy(entry.name_bytes()).into_owned());
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };

        if !is_safe_entry_name(name.as_str()) {
            report.excluded.push(ExcludedEntry {
                path,
                reason: ExclusionReason::UnsafeName,
            });
            continue;
        }
        if entry.filemode() == i32::from(git2::FileMode::Link) {
            report.excluded.push(ExcludedEntry {
                path,
                reason: ExclusionReason::Symlink,
            });
            continue;
        }

        match entry.kind() {
            Some(ObjectType::Tree) => {
                let child = repo
                    .find_tree(entry.id())
                    .map_err(|source| SnapshotError::Git {
                        operation: "load subtree for snapshot",
                        reason: source.message().to_string(),
                    })?;
                let child_dir = dest.join(name.as_str());
                fs::create_dir_all(child_dir.as_path()).map_err(|source| SnapshotError::Io {
                    operation: "create snapshot directory",
                    path: child_dir.clone(),
                    reason: source.to_string(),
                })?;
                materialize_tree(repo, &child, child_dir.as_path(), path.as_str(), report)?;
            }
            Some(ObjectType::Blob) => {
                materialize_blob(repo, &entry, dest, path, report)?;
            }
            Some(ObjectType::Commit) => report.excluded.push(ExcludedEntry {
                path,
                reason: ExclusionReason::Submodule,
            }),
            _ => report.excluded.push(ExcludedEntry {
                path,
                reason: ExclusionReason::UnsupportedKind,
            }),
        }
    }

    Ok(())
}

fn materialize_blob(
    repo: &Repository,
    entry: &TreeEntry<'_>,
    dest: &Path,
    path: String,
    report: &mut MaterializationReport,
) -> Result<(), SnapshotError> {
    let blob = repo
        .find_blob(entry.id())
        .map_err(|source| SnapshotError::Git {
            operation: "load blob for snapshot",
            reason: source.message().to_string(),
        })?;
    let bytes = u64::try_from(blob.size()).unwrap_or(u64::MAX);
    if bytes > DEFAULT_MAX_BLOB_BYTES {
        report.excluded.push(ExcludedEntry {
            path,
            reason: ExclusionReason::OversizeBlob { bytes },
        });
        return Ok(());
    }

    // Written without the executable bit: the explorer never runs repository
    // content, and a non-executable tree cannot be invoked by accident.
    let Ok(name) = entry.name() else {
        report.excluded.push(ExcludedEntry {
            path,
            reason: ExclusionReason::UnsafeName,
        });
        return Ok(());
    };
    let file_path = dest.join(name);
    fs::write(file_path.as_path(), blob.content()).map_err(|source| SnapshotError::Io {
        operation: "write snapshot file",
        path: file_path,
        reason: source.to_string(),
    })?;
    report.files_written += 1;
    report.bytes_written = report.bytes_written.saturating_add(bytes);
    Ok(())
}

fn is_safe_entry_name(name: &str) -> bool {
    !(name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
        || name.eq_ignore_ascii_case(".git"))
}

/// Initialize an empty repository at the snapshot root.
///
/// This anchors Git discovery inside the snapshot so neither `orbit_graph` nor
/// the `git check-ignore` call in its scanner can reach a repository that
/// happens to contain the system temporary directory. The repository has no
/// remote, no commit, and no templates, so no hook or template script is ever
/// installed.
fn anchor_git_discovery(root: &Path) -> Result<(), SnapshotError> {
    let mut options = RepositoryInitOptions::new();
    options.external_template(false).no_reinit(true).bare(false);
    Repository::init_opts(root, &options).map_err(|source| SnapshotError::Git {
        operation: "anchor git discovery in snapshot tree",
        reason: source.message().to_string(),
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsafe_entry_names_are_rejected() {
        assert!(is_safe_entry_name("src"));
        assert!(is_safe_entry_name("lib.rs"));
        assert!(!is_safe_entry_name(""));
        assert!(!is_safe_entry_name("."));
        assert!(!is_safe_entry_name(".."));
        assert!(!is_safe_entry_name("a/b"));
        assert!(!is_safe_entry_name("a\\b"));
        assert!(!is_safe_entry_name(".git"));
        assert!(!is_safe_entry_name(".GIT"));
    }

    #[test]
    fn clean_working_tree_has_no_notice() {
        let state = WorkingTreeState::default();
        assert!(!state.dirty);
        assert_eq!(state.notice(), None);
    }

    #[test]
    fn dirty_working_tree_notice_counts_entries() {
        let state = WorkingTreeState {
            dirty: true,
            entries: vec![WorkingTreeEntry {
                path: "src/lib.rs".to_string(),
                change: WorkingTreeChange::Modified,
            }],
            truncated: false,
        };
        let notice = state.notice().expect("dirty state has a notice");
        assert!(notice.contains("1 uncommitted change(s)"), "{notice}");
    }

    #[test]
    fn graph_scratch_paths_are_not_uncommitted_work() {
        assert!(is_graph_scratch(".orbit-graph"));
        assert!(is_graph_scratch(
            ".orbit-graph/explorer/snapshots/abc/tree/src/lib.rs"
        ));
        assert!(!is_graph_scratch("src/lib.rs"));
        assert!(!is_graph_scratch(".orbit-graphics/lib.rs"));
    }

    #[test]
    fn exclusion_reasons_round_trip_through_their_labels() {
        for reason in [
            ExclusionReason::Symlink,
            ExclusionReason::Submodule,
            ExclusionReason::UnsafeName,
            ExclusionReason::UnsupportedKind,
            ExclusionReason::OversizeBlob { bytes: 9 },
        ] {
            assert_eq!(
                ExclusionReason::from_label(reason.label(), reason.bytes()),
                reason
            );
        }
        assert_eq!(
            ExclusionReason::from_label("something-new", None),
            ExclusionReason::UnsupportedKind
        );
    }

    #[test]
    fn labels_are_stable() {
        assert_eq!(SnapshotSide::Base.label(), "base");
        assert_eq!(SnapshotSide::Head.label(), "head");
        assert_eq!(ComparisonMode::DirectBaseHead.label(), "direct_base_head");
        assert_eq!(ExclusionReason::Symlink.label(), "symlink");
        assert_eq!(
            ExclusionReason::OversizeBlob { bytes: 1 }.label(),
            "oversize_blob"
        );
        assert_eq!(WorkingTreeChange::Untracked.label(), "untracked");
    }
}
