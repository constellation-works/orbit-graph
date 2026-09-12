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

use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use git2::{ObjectType, Oid, Repository, RepositoryInitOptions, Status, StatusOptions, TreeEntry};
use orbit_graph::{
    Confidence, Graph, GraphError, ImpactResult, RefOpts, RefResult, Selector, SyncMode, SyncPolicy,
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

    fn build(
        repo: &Repository,
        side: SnapshotSide,
        requested_ref: &str,
        cache: Option<&SnapshotCache>,
    ) -> Result<Self, SnapshotError> {
        let started = Instant::now();
        let commit = resolve_commit(repo, requested_ref)?;
        let commit_sha = commit.to_string();

        let prepared = match cache {
            Some(cache) => Self::prepare_cached(repo, side, commit, cache)?,
            None => Self::prepare_temporary(repo, side, commit)?,
        };

        Ok(Self {
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
        })
    }

    /// Open a cached entry for `commit`, building and publishing one when no
    /// entry with a matching key exists.
    fn prepare_cached(
        repo: &Repository,
        side: SnapshotSide,
        commit: Oid,
        cache: &SnapshotCache,
    ) -> Result<PreparedSnapshot, SnapshotError> {
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
            return Ok(PreparedSnapshot {
                graph,
                storage: SnapshotStorage::Cached(entry.root().to_path_buf()),
                tree_root,
                db_path,
                materialization: materialization_from_stored(&build),
                files_indexed: build.files_indexed,
                cache: CacheOutcome::Hit,
                cache_note: None,
            });
        }

        let staged = cache.stage(commit_sha.as_str()).map_err(cache_error)?;
        let staged_tree = staged.tree();
        let staged_db = staged.db();
        let materialization = materialize_commit(repo, commit, staged_tree.as_path())?;
        anchor_git_discovery(staged_tree.as_path())?;
        let files_indexed = {
            // The handle is dropped before the entry is renamed into place, so
            // the database and its write-ahead log are closed when the
            // directory moves.
            let graph = open_graph(
                side,
                commit_sha.as_str(),
                staged_tree.as_path(),
                staged_db.as_path(),
            )?;
            let report = graph
                .sync(SyncMode::Full)
                .map_err(|source| SnapshotError::Graph {
                    operation: "index snapshot tree",
                    side,
                    commit_sha: commit_sha.clone(),
                    source,
                })?;
            report.files_indexed
        };

        let entry = cache
            .publish(staged, stored_build(&materialization, files_indexed))
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
        Ok(PreparedSnapshot {
            graph,
            storage: SnapshotStorage::Cached(entry.root().to_path_buf()),
            tree_root,
            db_path,
            materialization: materialization_from_stored(&build),
            files_indexed: build.files_indexed,
            cache: CacheOutcome::Miss,
            cache_note: None,
        })
    }

    /// Materialize and index into a task-owned temporary tree.
    ///
    /// Used when no cache directory is usable. Nothing is reused and nothing is
    /// written to the cache.
    fn prepare_temporary(
        repo: &Repository,
        side: SnapshotSide,
        commit: Oid,
    ) -> Result<PreparedSnapshot, SnapshotError> {
        let commit_sha = commit.to_string();
        let tree = TempDir::with_prefix(format!("orbit-graph-explorer-{}-", side.label()))
            .map_err(|source| SnapshotError::Io {
                operation: "create snapshot tree",
                path: std::env::temp_dir(),
                reason: source.to_string(),
            })?;
        let materialization = materialize_commit(repo, commit, tree.path())?;
        anchor_git_discovery(tree.path())?;

        let graph = Graph::open(tree.path(), SyncPolicy::Manual).map_err(|source| {
            SnapshotError::Graph {
                operation: "open snapshot graph",
                side,
                commit_sha: commit_sha.clone(),
                source,
            }
        })?;
        let report = graph
            .sync(SyncMode::Full)
            .map_err(|source| SnapshotError::Graph {
                operation: "index snapshot tree",
                side,
                commit_sha: commit_sha.clone(),
                source,
            })?;
        let tree_root = tree.path().to_path_buf();
        let db_path = graph.db_path().path().to_path_buf();
        Ok(PreparedSnapshot {
            graph,
            storage: SnapshotStorage::Temporary(tree),
            tree_root,
            db_path,
            materialization,
            files_indexed: report.files_indexed,
            cache: CacheOutcome::Disabled,
            cache_note: None,
        })
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

fn stored_build(materialization: &MaterializationReport, files_indexed: usize) -> StoredBuild {
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
        let started = Instant::now();
        let repo = open_working_tree(repository)?;
        let workdir = repo
            .workdir()
            .map(|path| fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()))
            .unwrap_or_else(|| repository.to_path_buf());
        let working_tree = working_tree_state(&repo)?;

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

        let base = Snapshot::build(&repo, SnapshotSide::Base, base_ref, cache.as_ref())?;
        let head = Snapshot::build(&repo, SnapshotSide::Head, head_ref, cache.as_ref())?;

        Ok(Self {
            repository: workdir,
            mode: ComparisonMode::DirectBaseHead,
            base,
            head,
            working_tree,
            cache_dir: cache.map(|cache| cache.dir().to_path_buf()),
            cache_note,
            prepared_in: started.elapsed(),
        })
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
            .unwrap_or_else(|| String::from_utf8_lossy(entry.path_bytes()).into_owned());
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
            .unwrap_or_else(|| String::from_utf8_lossy(entry.name_bytes()).into_owned());
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
    let Some(name) = entry.name() else {
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
