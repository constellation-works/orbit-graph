//! The working tree as a comparison's head side, and the default base a
//! working-tree comparison measures against.
//!
//! A working-tree snapshot copies what is on disk now: every path in the Git
//! index that still exists in the working tree (so staged and unstaged edits
//! both count) plus every untracked file Git does not ignore. It applies the
//! same exclusions as a commit snapshot (symbolic links, submodules, oversize
//! files, unsafe names), writes into a task-owned temporary tree, and is never
//! cached: it is not an immutable revision. Nothing in the working tree or the
//! Git index is written.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::Instant;

use git2::{Repository, StatusOptions};
use orbit_graph::atomic_write;
use serde::Serialize;

use super::{
    ComparisonProgress, DEFAULT_MAX_BLOB_BYTES, ExcludedEntry, ExclusionReason,
    MaterializationReport, PreparedSnapshot, Snapshot, SnapshotError, SnapshotSide,
    is_graph_scratch, is_safe_entry_name, resolve_commit,
};

/// The `commit_sha` (and `requested_ref`) a working-tree snapshot reports in
/// place of a commit: it names uncommitted content, not a Git object.
pub const WORKING_TREE_ID: &str = "worktree";

/// Git file mode of a symbolic link in the index.
const MODE_SYMLINK: u32 = 0o120_000;
/// Git file mode of a submodule (gitlink) in the index.
const MODE_GITLINK: u32 = 0o160_000;

/// Why a working-tree snapshot is rebuilt on every launch.
const NOT_CACHED_NOTE: &str = "The working tree is not an immutable revision, so its snapshot is \
                               never cached: it is copied and indexed on every launch.";

impl Snapshot {
    /// Copy and index the working tree as the head side.
    pub(super) fn build_working_tree(
        repo: &Repository,
        scratch: Option<&Path>,
        progress: &dyn ComparisonProgress,
    ) -> Result<Option<Self>, SnapshotError> {
        let started = Instant::now();
        let prepared = Self::prepare_temporary_from(
            SnapshotSide::Head,
            WORKING_TREE_ID,
            scratch,
            &|dest| materialize_working_tree(repo, dest),
            progress,
        )?;
        let Some(PreparedSnapshot {
            graph,
            storage,
            tree_root,
            db_path,
            materialization,
            files_indexed,
            cache,
            cache_note: _,
        }) = prepared
        else {
            return Ok(None);
        };
        Ok(Some(Self {
            side: SnapshotSide::Head,
            requested_ref: WORKING_TREE_ID.to_string(),
            commit_sha: WORKING_TREE_ID.to_string(),
            graph,
            storage,
            tree_root,
            db_path,
            materialization,
            files_indexed,
            cache,
            cache_note: Some(NOT_CACHED_NOTE.to_string()),
            prepared_in: started.elapsed(),
        }))
    }
}

/// Copy the working tree's source files into `dest`.
fn materialize_working_tree(
    repo: &Repository,
    dest: &Path,
) -> Result<MaterializationReport, SnapshotError> {
    let workdir = repo.workdir().ok_or_else(|| SnapshotError::Git {
        operation: "locate the working tree",
        reason: "repository is bare and has no working tree".to_string(),
    })?;

    // Path -> index mode; `None` for an untracked file.
    let mut paths: BTreeMap<String, Option<u32>> = BTreeMap::new();
    let index = repo.index().map_err(|source| SnapshotError::Git {
        operation: "read the Git index",
        reason: source.message().to_string(),
    })?;
    for entry in index.iter() {
        let path = String::from_utf8_lossy(entry.path.as_slice()).into_owned();
        // A conflicted path has one entry per stage; the file on disk is what
        // counts, so the first mode seen is enough.
        paths.entry(path).or_insert(Some(entry.mode));
    }

    let mut options = StatusOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false)
        .include_unmodified(false)
        .update_index(false)
        .no_refresh(true);
    let statuses = repo
        .statuses(Some(&mut options))
        .map_err(|source| SnapshotError::Git {
            operation: "list untracked working-tree files",
            reason: source.message().to_string(),
        })?;
    for entry in statuses.iter() {
        if entry.status().is_wt_new() {
            let path = String::from_utf8_lossy(entry.path_bytes()).into_owned();
            paths.entry(path).or_insert(None);
        }
    }

    let mut report = MaterializationReport::default();
    for (path, mode) in paths {
        if is_graph_scratch(path.as_str()) {
            continue;
        }
        if !path.split('/').all(is_safe_entry_name) {
            report.excluded.push(ExcludedEntry {
                path,
                reason: ExclusionReason::UnsafeName,
            });
            continue;
        }
        match mode {
            Some(MODE_SYMLINK) => {
                report.excluded.push(ExcludedEntry {
                    path,
                    reason: ExclusionReason::Symlink,
                });
                continue;
            }
            Some(MODE_GITLINK) => {
                report.excluded.push(ExcludedEntry {
                    path,
                    reason: ExclusionReason::Submodule,
                });
                continue;
            }
            _ => {}
        }
        copy_working_file(workdir, dest, path, &mut report)?;
    }
    Ok(report)
}

/// Copy one working-tree file, or record why it was left out. A tracked path
/// that no longer exists on disk is a deletion and is simply absent.
fn copy_working_file(
    workdir: &Path,
    dest: &Path,
    path: String,
    report: &mut MaterializationReport,
) -> Result<(), SnapshotError> {
    let source = workdir.join(path.as_str());
    let metadata = match fs::symlink_metadata(source.as_path()) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(SnapshotError::Io {
                operation: "inspect working-tree file",
                path: source,
                reason: error.to_string(),
            });
        }
    };
    let file_type = metadata.file_type();
    let reason = if file_type.is_symlink() {
        Some(ExclusionReason::Symlink)
    } else if !file_type.is_file() {
        Some(ExclusionReason::UnsupportedKind)
    } else if metadata.len() > DEFAULT_MAX_BLOB_BYTES {
        Some(ExclusionReason::OversizeBlob {
            bytes: metadata.len(),
        })
    } else {
        None
    };
    if let Some(reason) = reason {
        report.excluded.push(ExcludedEntry { path, reason });
        return Ok(());
    }

    let bytes = fs::read(source.as_path()).map_err(|error| SnapshotError::Io {
        operation: "read working-tree file",
        path: source.clone(),
        reason: error.to_string(),
    })?;
    let target = dest.join(path.as_str());
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|error| SnapshotError::Io {
            operation: "create snapshot directory",
            path: parent.to_path_buf(),
            reason: error.to_string(),
        })?;
    }
    // Written owner-only without the executable bit, like a commit snapshot.
    atomic_write(target.as_path(), bytes.as_slice()).map_err(|error| SnapshotError::Io {
        operation: "write snapshot file",
        path: target,
        reason: error.to_string(),
    })?;
    report.files_written += 1;
    report.bytes_written = report
        .bytes_written
        .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
    Ok(())
}

/// The base a working-tree comparison uses when the caller names none: the
/// merge base of `HEAD` and the branch the work will land on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DefaultBase {
    /// The ref the merge base was taken with, as resolved (`origin/main`).
    pub reference: String,
    /// How that ref was chosen: `upstream`, `origin_head`, `main`, or
    /// `master`.
    pub source: String,
    /// Merge base of `HEAD` and `reference`: the comparison's base commit.
    pub merge_base: String,
}

/// Choose the default base for a working-tree comparison in `repository`.
///
/// Candidates, in order: the current branch's upstream, the remote default
/// branch `origin/HEAD` points at, then local `main` and `master`. The first
/// that resolves is merged with `HEAD`, so the comparison covers exactly the
/// work on this branch, committed or not. Nothing is fetched.
pub fn default_base(repository: &Path) -> Result<DefaultBase, SnapshotError> {
    let repo = super::open_working_tree(repository)?;
    let head = resolve_commit(&repo, "HEAD")?;

    let mut candidates: Vec<(String, &'static str)> = Vec::new();
    if let Some(upstream) = upstream_of_head(&repo) {
        candidates.push((upstream, "upstream"));
    }
    if let Some(origin_head) = origin_head(&repo) {
        candidates.push((origin_head, "origin_head"));
    }
    candidates.push(("main".to_string(), "main"));
    candidates.push(("master".to_string(), "master"));

    let mut tried = Vec::new();
    for (reference, source) in candidates {
        let Ok(commit) = resolve_commit(&repo, reference.as_str()) else {
            tried.push(reference);
            continue;
        };
        let merge_base =
            repo.merge_base(head, commit)
                .map_err(|error| SnapshotError::Revision {
                    reference: format!("merge-base HEAD {reference}"),
                    reason: error.message().to_string(),
                })?;
        return Ok(DefaultBase {
            reference,
            source: source.to_string(),
            merge_base: merge_base.to_string(),
        });
    }
    Err(SnapshotError::NoDefaultBase { tried })
}

/// Short name of the current branch's upstream, such as `origin/main`.
fn upstream_of_head(repo: &Repository) -> Option<String> {
    let head = repo.head().ok()?;
    if !head.is_branch() {
        return None;
    }
    let name = head.name().ok()?;
    let upstream = repo.branch_upstream_name(name).ok()?;
    let upstream = upstream.as_str().ok()?;
    Some(
        upstream
            .strip_prefix("refs/remotes/")
            .or_else(|| upstream.strip_prefix("refs/heads/"))
            .unwrap_or(upstream)
            .to_string(),
    )
}

/// The branch `origin/HEAD` points at, such as `origin/main`.
fn origin_head(repo: &Repository) -> Option<String> {
    let reference = repo.find_reference("refs/remotes/origin/HEAD").ok()?;
    let target = reference.symbolic_target().ok()??;
    Some(
        target
            .strip_prefix("refs/remotes/")
            .unwrap_or(target)
            .to_string(),
    )
}
