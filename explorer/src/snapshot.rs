//! Immutable base/head snapshots of a Git repository.
//!
//! A [`Comparison`] resolves two user-supplied refs to immutable commit SHAs,
//! materializes each commit into its own task-owned temporary tree, and indexes
//! each tree through the public [`orbit_graph`] API. Queries then run against
//! one snapshot at a time, so every answer is attributable to exactly one
//! revision.
//!
//! Safety rules this module enforces:
//!
//! - The user's working tree is never written to, checked out over, or cleaned.
//!   Snapshots live under the system temporary directory and are removed when
//!   the [`Comparison`] is dropped.
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

use git2::{ObjectType, Oid, Repository, RepositoryInitOptions, Status, StatusOptions, TreeEntry};
use orbit_graph::{
    Confidence, Graph, GraphError, ImpactResult, RefOpts, RefResult, Selector, SyncMode,
    SyncPolicy, SyncReport,
};
use tempfile::TempDir;
use thiserror::Error;

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
/// the tree must outlive every query against it.
pub struct Snapshot {
    side: SnapshotSide,
    requested_ref: String,
    commit_sha: String,
    // `graph` is declared before `tree` so the index connection closes before
    // the directory holding it is removed.
    graph: Graph,
    tree: TempDir,
    materialization: MaterializationReport,
    sync_report: SyncReport,
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
        self.tree.path()
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

    /// Index sync summary produced when the snapshot was built.
    pub fn sync_report(&self) -> &SyncReport {
        &self.sync_report
    }

    /// Number of files indexed in this snapshot.
    pub fn files_indexed(&self) -> usize {
        self.sync_report.files_indexed
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
    ) -> Result<Self, SnapshotError> {
        let commit = resolve_commit(repo, requested_ref)?;
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
                commit_sha: commit.to_string(),
                source,
            }
        })?;
        let sync_report = graph
            .sync(SyncMode::Full)
            .map_err(|source| SnapshotError::Graph {
                operation: "index snapshot tree",
                side,
                commit_sha: commit.to_string(),
                source,
            })?;

        Ok(Self {
            side,
            requested_ref: requested_ref.to_string(),
            commit_sha: commit.to_string(),
            graph,
            tree,
            materialization,
            sync_report,
        })
    }
}

/// A resolved base/head comparison with both snapshots indexed.
pub struct Comparison {
    repository: PathBuf,
    mode: ComparisonMode,
    base: Snapshot,
    head: Snapshot,
    working_tree: WorkingTreeState,
}

impl Comparison {
    /// Resolve `base_ref` and `head_ref` in the repository at `repository`,
    /// then materialize and index both revisions.
    ///
    /// The repository is opened read-only: nothing in the user's working tree,
    /// index, or object store is written.
    pub fn open(repository: &Path, base_ref: &str, head_ref: &str) -> Result<Self, SnapshotError> {
        let repo = open_working_tree(repository)?;
        let workdir = repo
            .workdir()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| repository.to_path_buf());
        let working_tree = working_tree_state(&repo)?;
        let base = Snapshot::build(&repo, SnapshotSide::Base, base_ref)?;
        let head = Snapshot::build(&repo, SnapshotSide::Head, head_ref)?;

        Ok(Self {
            repository: workdir,
            mode: ComparisonMode::DirectBaseHead,
            base,
            head,
            working_tree,
        })
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
        state.dirty = true;
        if state.entries.len() >= DIRTY_ENTRY_CAP {
            state.truncated = true;
            continue;
        }
        let path = entry
            .path()
            .map(str::to_string)
            .unwrap_or_else(|| String::from_utf8_lossy(entry.path_bytes()).into_owned());
        state.entries.push(WorkingTreeEntry { path, change });
    }

    Ok(state)
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
