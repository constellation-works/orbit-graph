//! Read-only graph query implementations.

use std::path::{Component, Path, PathBuf};

use crate::GraphError;

pub(crate) mod callees;
pub(crate) mod deps;
pub(crate) mod impact;
pub(crate) mod implementors;
pub(crate) mod overview;
pub(crate) mod refs;
pub(crate) mod runtime;
pub(crate) mod search;
pub(crate) mod show;
mod source;
pub(crate) mod trace;
pub(crate) mod types;

pub use search::{DEFAULT_SEARCH_LIMIT, Match, SearchKind, SearchQuery, SearchResult};
pub use show::{DEFAULT_SHOW_MAX_BYTES, NodeMetadata, NodeView, SourceSpan};

/// Match inbound refs (`r`) to a symbol ID (`?1`) and qualified name (`?2`).
///
/// The qualified name is authoritative; a hint only distinguishes duplicate
/// definitions while it still points to a row with that identity. Missing or
/// reused hint rows fall back to the qualified name, as outbound queries do.
/// Keeping the qualified match outermost also lets SQLite use its target index
/// instead of scanning every ref for the unindexed hint column.
const INBOUND_TARGET_MATCH: &str = "(r.target_qualified = ?2 AND (
    r.target_symbol_hint = ?1 OR NOT EXISTS (
        SELECT 1 FROM symbols hinted
        WHERE hinted.id = r.target_symbol_hint AND hinted.qualified = r.target_qualified
    )
))";

/// Format the `symbol:` selector that addresses one stored symbol row.
///
/// The symbol part is the row's qualified name: selector resolution prefers
/// an exact qualified match, so this names the same row even when two symbols
/// in one file share a short name.
pub(crate) fn symbol_selector(file_path: &str, qualified: &str, kind: &str) -> String {
    format!("symbol:{file_path}#{qualified}:{kind}")
}

/// Join `stored_path` to `worktree_root` only when the database path cannot
/// leave the worktree.
///
/// Absolute paths and any parent-directory component are rejected before the
/// caller touches the filesystem. Query commands open the database with manual
/// sync, so a stale or attacker-supplied row must not be able to make `show`,
/// `search`, `refs`, or `callees` return bytes from outside the repository.
///
/// Containment is then decided on the physical path (`STD-05 §R6`): when the
/// joined path exists, it is canonicalized, and a path that a symlinked
/// component carries outside the canonical worktree is rejected. A path that
/// does not exist is returned as joined; reading it fails on its own.
pub(crate) fn contained_worktree_source(
    worktree_root: &Path,
    stored_path: &str,
) -> Result<PathBuf, GraphError> {
    let relative = Path::new(stored_path);
    let outside = |detail: &str| {
        GraphError::invalid_data(
            "resolve graph source path",
            format!("source path must stay inside the worktree: {stored_path}{detail}"),
        )
    };
    if relative.is_absolute() || path_leaves_worktree(relative) {
        return Err(outside(""));
    }
    let joined = worktree_root.join(relative);
    if let (Ok(physical), Ok(root)) = (joined.canonicalize(), worktree_root.canonicalize())
        && !physical.starts_with(root.as_path())
    {
        return Err(outside(&format!(
            " (a symbolic link resolves it to {})",
            physical.display()
        )));
    }
    Ok(joined)
}

fn path_leaves_worktree(path: &Path) -> bool {
    path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::Prefix(_) | Component::RootDir
        )
    })
}

#[cfg(test)]
mod tests;
