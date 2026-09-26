use std::path::{Path, PathBuf};

use crate::{GraphError, STORE_SCHEMA_VERSION, store};

/// Resolved, worktree-scoped graph database path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphDbPath {
    path: PathBuf,
    branch: String,
    extractor_version: u32,
}

impl GraphDbPath {
    pub(crate) fn new(path: PathBuf, branch: String, extractor_version: u32) -> Self {
        Self {
            path,
            branch,
            extractor_version,
        }
    }

    /// Return the canonical SQLite database path.
    pub fn path(&self) -> &Path {
        self.path.as_path()
    }

    /// Return the unsanitized branch name represented by this database path.
    pub fn branch(&self) -> &str {
        self.branch.as_str()
    }

    /// Return the extractor version embedded in the database filename.
    pub fn extractor_version(&self) -> u32 {
        self.extractor_version
    }

    /// Return the graph store schema version.
    pub const fn schema_version(&self) -> u32 {
        STORE_SCHEMA_VERSION
    }
}

/// Resolve the graph database path the worktree's current branch or detached
/// commit selects, as [`crate::Graph::open`] would, without creating anything.
///
/// Only an unborn branch, or a directory outside any Git repository, selects
/// the `HEAD` family; any other failure to read `HEAD` is an error.
///
/// # Examples
///
/// ```
/// use orbit_graph::{EXTRACTOR_VERSION, resolve_worktree_db_path};
///
/// let dir = tempfile::tempdir()?;
/// let db_path = resolve_worktree_db_path(dir.path())?;
/// assert_eq!(db_path.branch(), "HEAD");
/// assert_eq!(db_path.extractor_version(), EXTRACTOR_VERSION);
/// assert!(!db_path.path().exists());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn resolve_worktree_db_path(worktree_root: &Path) -> Result<GraphDbPath, GraphError> {
    store::resolve_worktree_db_path(worktree_root)
}

/// Resolve the canonical graph database path for a worktree and branch.
///
/// The filename sanitizes the branch with a conservative filesystem-safe
/// allowlist, while the returned [`GraphDbPath`] keeps the raw branch name for
/// future `meta.branch` storage.
pub fn resolve_db_path(worktree_root: &Path, branch: &str, extractor_version: u32) -> GraphDbPath {
    resolve_db_path_for_commit(worktree_root, branch, "", extractor_version)
}

/// Resolve the canonical graph database path, using per-commit filenames for detached HEAD.
///
/// Branch-attached graphs keep the branch-scoped filename. Detached HEAD graphs
/// use `detached-<short-sha>.<version>.db` when a commit SHA is available so
/// concurrent detached checkouts on different commits do not churn the same DB.
pub fn resolve_db_path_for_commit(
    worktree_root: &Path,
    branch: &str,
    commit_sha: &str,
    extractor_version: u32,
) -> GraphDbPath {
    let filename_stem = graph_db_filename_stem(branch, commit_sha);
    let filename = format!("{filename_stem}.{extractor_version}.db");
    GraphDbPath::new(
        worktree_root.join(".orbit-graph").join(filename),
        branch.to_string(),
        extractor_version,
    )
}

fn graph_db_filename_stem(branch: &str, commit_sha: &str) -> String {
    if branch == "HEAD" {
        detached_commit_prefix(commit_sha)
            .map(|prefix| format!("detached-{prefix}"))
            .unwrap_or_else(|| sanitize_branch_for_filename(branch))
    } else {
        sanitize_branch_for_filename(branch)
    }
}

pub(crate) fn detached_commit_prefix(commit_sha: &str) -> Option<&str> {
    let commit_sha = commit_sha.trim();
    let prefix = commit_sha.get(..12)?;
    if prefix.chars().all(|ch| ch.is_ascii_hexdigit()) {
        Some(prefix)
    } else {
        None
    }
}

fn sanitize_branch_for_filename(branch: &str) -> String {
    if branch.is_empty() {
        return "_".to_string();
    }

    let chars = branch.chars().collect::<Vec<_>>();
    let mut sanitized = String::with_capacity(branch.len());
    for (index, ch) in chars.iter().copied().enumerate() {
        let is_dot = ch == '.';
        let is_double_dot = is_dot
            && ((index > 0 && chars[index - 1] == '.')
                || (index + 1 < chars.len() && chars[index + 1] == '.'));
        let allowed = ch.is_ascii_alphanumeric()
            || ch == '_'
            || ch == '-'
            || (index > 0 && is_dot && !is_double_dot);

        sanitized.push(if allowed { ch } else { '_' });
    }
    sanitized
}

#[cfg(test)]
mod tests;
