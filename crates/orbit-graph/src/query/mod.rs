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
pub(crate) mod trace;

pub use search::{DEFAULT_SEARCH_LIMIT, Match, SearchKind, SearchQuery, SearchResult};
pub use show::{DEFAULT_SHOW_MAX_BYTES, NodeMetadata, NodeView, SourceSpan};

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
pub(crate) fn contained_worktree_source(
    worktree_root: &Path,
    stored_path: &str,
) -> Result<PathBuf, GraphError> {
    let relative = Path::new(stored_path);
    if relative.is_absolute() || path_leaves_worktree(relative) {
        return Err(GraphError::invalid_data(
            "resolve graph source path",
            format!("source path must stay inside the worktree: {stored_path}"),
        ));
    }
    Ok(worktree_root.join(relative))
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

#[cfg(test)]
mod contained_worktree_source_tests {
    use std::path::Path;

    use super::contained_worktree_source;

    #[test]
    fn relative_database_paths_stay_under_the_worktree() {
        let resolved =
            contained_worktree_source(Path::new("/repo"), "src/lib.rs").expect("relative path");
        assert_eq!(resolved, Path::new("/repo/src/lib.rs"));

        let dotted =
            contained_worktree_source(Path::new("/repo"), "src/./lib.rs").expect("dot path");
        assert_eq!(dotted, Path::new("/repo/src/./lib.rs"));
    }

    #[test]
    fn absolute_and_parent_database_paths_are_rejected() {
        let root = Path::new("/repo");
        for stored in [
            "/etc/passwd",
            "../outside.rs",
            "src/../../outside.rs",
            "src/../src/lib.rs",
            "./../../outside.rs",
        ] {
            let error = contained_worktree_source(root, stored).expect_err(stored);
            let rendered = error.to_string();
            assert!(rendered.contains("worktree"), "{rendered}");
            assert!(rendered.contains(stored), "{rendered}");
        }
    }
}
