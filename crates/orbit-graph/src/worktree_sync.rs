//! Runs the worktree sync operation using graph handles, obsolete database cleanup, sync
//! contracts and graph errors.

use std::path::Path;

use crate::{Graph, GraphError, SyncMode, SyncPolicy, SyncReport, clean_old_databases};

/// Sync the graph index of the worktree at `worktree_root`: the one operation
/// behind `orbit-graph sync`.
///
/// It runs these steps in order, and a step that fails ends the operation:
///
/// 1. open the worktree's graph database for writing, creating it when needed,
///    so a database that cannot be opened removes nothing;
/// 2. remove obsolete databases with [`clean_old_databases`], which keeps any
///    database whose lock another process holds;
/// 3. index the worktree in `mode` with [`Graph::sync`], under the database's
///    sync lock.
///
/// A path that cannot be read or extracted is listed in
/// [`SyncReport::failed`] and is not fatal while any file is indexed
/// (STD-02 §R32).
///
/// # Errors
///
/// The error of the step that failed, and [`GraphError::InvalidData`] when
/// paths failed and no file is indexed, because the sync did not happen
/// (STD-01 §R30). Step 2 is not undone by a later failure.
///
/// # Examples
///
/// ```
/// use orbit_graph::{SyncMode, sync_worktree};
///
/// let dir = tempfile::tempdir()?;
/// let report = sync_worktree(dir.path(), SyncMode::Auto)?;
/// assert!(report.failed.is_empty());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn sync_worktree(worktree_root: &Path, mode: SyncMode) -> Result<SyncReport, GraphError> {
    let graph = Graph::open(worktree_root, SyncPolicy::Manual)?;
    clean_old_databases(worktree_root)?;
    require_indexed_files(graph.sync(mode)?)
}

/// Refuses a report in which paths failed and no file is indexed; every other
/// report is a successful sync.
pub(crate) fn require_indexed_files(report: SyncReport) -> Result<SyncReport, GraphError> {
    if report.files_indexed != 0 {
        return Ok(report);
    }
    let Some(first) = report.failed.first() else {
        return Ok(report);
    };
    Err(GraphError::invalid_data(
        "sync graph",
        format!(
            "indexed no file: {} path(s) failed; first: {} ({}: {})",
            report.failed.len(),
            first.path,
            first.operation,
            first.message
        ),
    ))
}
