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
pub(crate) mod types;

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

    #[cfg(unix)]
    #[test]
    fn a_symlinked_component_leaving_the_worktree_is_rejected() {
        let root = tempfile::tempdir().expect("tempdir");
        let worktree = root.path().join("repo");
        let outside = root.path().join("outside");
        std::fs::create_dir_all(worktree.join("src")).expect("worktree");
        std::fs::create_dir(&outside).expect("outside");
        std::fs::write(outside.join("secret.rs"), b"fn secret() {}\n").expect("secret");
        std::fs::write(worktree.join("src/lib.rs"), b"fn lib() {}\n").expect("lib");
        std::os::unix::fs::symlink(&outside, worktree.join("src/dir")).expect("link out");
        std::os::unix::fs::symlink(worktree.join("src"), worktree.join("alias")).expect("link in");

        let error = contained_worktree_source(&worktree, "src/dir/secret.rs")
            .expect_err("a link out of the worktree is rejected");
        assert!(error.to_string().contains("symbolic link"), "{error}");
        assert_eq!(
            contained_worktree_source(&worktree, "alias/lib.rs").expect("a link inside"),
            worktree.join("alias/lib.rs")
        );
        assert_eq!(
            contained_worktree_source(&worktree, "src/missing.rs").expect("missing"),
            worktree.join("src/missing.rs")
        );
    }
}
