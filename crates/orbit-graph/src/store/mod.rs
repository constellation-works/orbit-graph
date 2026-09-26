//! SQLite connection setup for the graph store.

pub(crate) mod history;
pub(crate) mod schema;

use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::{io::ErrorKind, os::unix::fs::OpenOptionsExt};

use git2::Repository;
use rusqlite::Connection;

use crate::{EXTRACTOR_VERSION, GraphDbPath, GraphError, SyncPolicy, resolve_db_path_for_commit};

pub(crate) struct OpenedGraph {
    pub(crate) db_path: GraphDbPath,
}

pub(crate) fn open(worktree_root: &Path, _policy: SyncPolicy) -> Result<OpenedGraph, GraphError> {
    let git = GitContext::for_worktree(worktree_root);
    let db_path = resolve_db_path_for_commit(
        worktree_root,
        git.branch.as_str(),
        git.commit_sha.as_str(),
        EXTRACTOR_VERSION,
    );
    open_at_path(db_path, &git)
}

pub(crate) fn open_for_revision(
    worktree_root: &Path,
    revision: &str,
) -> Result<OpenedGraph, GraphError> {
    let git = GitContext {
        branch: "HEAD".to_string(),
        commit_sha: revision.to_string(),
    };
    let db_path = resolve_db_path_for_commit(
        worktree_root,
        git.branch.as_str(),
        git.commit_sha.as_str(),
        EXTRACTOR_VERSION,
    );
    open_at_path(db_path, &git)
}

pub(crate) fn open_with_db_path(
    worktree_root: &Path,
    db_path: &Path,
) -> Result<OpenedGraph, GraphError> {
    if db_path.is_dir() || db_path.file_name().is_none() {
        return Err(GraphError::invalid_data(
            "validate graph database path",
            format!("database path must name a file: {}", db_path.display()),
        ));
    }
    let git = GitContext::for_worktree(worktree_root);
    let db_path = GraphDbPath::new(db_path.to_path_buf(), git.branch.clone(), EXTRACTOR_VERSION);
    open_at_path(db_path, &git)
}

fn open_at_path(db_path: GraphDbPath, git: &GitContext) -> Result<OpenedGraph, GraphError> {
    if let Some(parent) = db_path.path().parent() {
        create_owned_dir(parent, "create graph database directory")?;
    }

    // SQLite's default creation mode can expose indexed source text. Create
    // the database atomically with private permissions before SQLite opens it.
    #[cfg(unix)]
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(db_path.path())
    {
        Ok(_) => {}
        Err(source) if source.kind() == ErrorKind::AlreadyExists => {}
        Err(source) => {
            return Err(GraphError::io(
                "create private graph database",
                db_path.path(),
                source,
            ));
        }
    }

    let mut conn = Connection::open(db_path.path())
        .map_err(|source| GraphError::sqlite("open graph database", source))?;
    configure_connection(&conn)?;

    if schema::database_is_empty(&conn)? {
        schema::initialize(
            &mut conn,
            &schema::InitialMeta {
                extractor_version: EXTRACTOR_VERSION,
                branch: git.branch.as_str(),
                commit_sha: git.commit_sha.as_str(),
            },
        )?;
    }

    Ok(OpenedGraph { db_path })
}

// Recovered from the graph crate's local SQLite setup. WAL is a hard
// requirement because the graph store is local scratch state on a writable
// filesystem; the other pragmas provide bounded waits and consistent keys.
fn configure_connection(conn: &Connection) -> Result<(), GraphError> {
    let journal_mode = conn
        .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get::<_, String>(0))
        .map_err(|source| GraphError::sqlite("set journal_mode=WAL", source))?;
    if !journal_mode.eq_ignore_ascii_case("wal") {
        return Err(GraphError::sqlite_message(
            "set journal_mode=WAL",
            format!("SQLite kept journal_mode={journal_mode}"),
        ));
    }

    conn.pragma_update(None, "busy_timeout", 5_000)
        .map_err(|source| GraphError::sqlite("set busy_timeout", source))?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|source| GraphError::sqlite("set foreign_keys=ON", source))?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(|source| GraphError::sqlite("set synchronous=NORMAL", source))?;
    Ok(())
}

/// Page cache for a sync writer connection, in KiB (SQLite reads a negative
/// `cache_size` as KiB). SQLite's default of about 2 MiB is small beside a
/// full index of a large repository (about 76 MB for Orbit), and each sync
/// pass reads symbols and imports back while it writes.
const SYNC_WRITER_CACHE_KIB: i64 = 64 * 1024;

/// Configures a connection that a sync pass writes through.
///
/// `foreign_keys` and `synchronous` are per-connection settings, so a writer
/// opened separately from [`open`] must set them again: without
/// `synchronous=NORMAL` every per-file commit waits for a full WAL fsync.
/// Under WAL, `NORMAL` still keeps the database consistent after a crash; at
/// worst the last commits roll back, and the index is rebuildable.
pub(crate) fn configure_sync_writer(
    conn: &Connection,
    operation: &'static str,
) -> Result<(), GraphError> {
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|source| GraphError::sqlite(operation, source))?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(|source| GraphError::sqlite(operation, source))?;
    conn.pragma_update(None, "cache_size", -SYNC_WRITER_CACHE_KIB)
        .map_err(|source| GraphError::sqlite(operation, source))?;
    Ok(())
}

/// Name of the scratch directory orbit-graph keeps in a worktree root.
const SCRATCH_DIR_NAME: &str = ".orbit-graph";

/// The `.gitignore` orbit-graph writes into a directory it owns. `*` also
/// matches the file itself, so the directory never shows up as untracked.
const OWNED_DIR_GITIGNORE: &str =
    "# Written by orbit-graph: this directory holds local, rebuildable indexes.\n*\n";

/// Creates `dir`, with any missing parents, to hold orbit-graph's own files,
/// and keeps the directories orbit-graph owns out of `git status`.
///
/// orbit-graph owns the directories this call creates, and any
/// `.orbit-graph` scratch directory on the path, including one that already
/// exists. The topmost of each gets a `.gitignore` containing `*`, so
/// databases, WAL files and locks are never offered for commit. An existing
/// `.gitignore` is never touched, and a directory the caller supplied that
/// already exists (a custom `--db` parent, say) gets nothing. Failing to write
/// the file is logged, not fatal: the index works without it.
pub(crate) fn create_owned_dir(dir: &Path, operation: &'static str) -> Result<(), GraphError> {
    let created = topmost_missing_ancestor(dir);
    fs::create_dir_all(dir).map_err(|source| GraphError::io(operation, dir, source))?;
    let scratch = dir
        .ancestors()
        .find(|ancestor| ancestor.file_name() == Some(OsStr::new(SCRATCH_DIR_NAME)));
    for owned in created.as_deref().into_iter().chain(scratch) {
        write_owned_dir_gitignore(owned);
    }
    Ok(())
}

/// The outermost ancestor of `dir` (or `dir` itself) that does not exist yet.
fn topmost_missing_ancestor(dir: &Path) -> Option<PathBuf> {
    let mut missing = None;
    for ancestor in dir.ancestors() {
        if ancestor.as_os_str().is_empty() {
            break;
        }
        match fs::symlink_metadata(ancestor) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing = Some(ancestor.to_path_buf());
            }
            _ => break,
        }
    }
    missing
}

fn write_owned_dir_gitignore(dir: &Path) {
    let path = dir.join(".gitignore");
    let written = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path.as_path())
        .and_then(|mut file| file.write_all(OWNED_DIR_GITIGNORE.as_bytes()));
    match written {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => tracing::warn!(
            path = %path.display(),
            error = %error,
            "could not write .gitignore for orbit-graph index directory"
        ),
    }
}

struct GitContext {
    branch: String,
    commit_sha: String,
}

impl GitContext {
    fn for_worktree(worktree_root: &Path) -> Self {
        let Ok(repo) = Repository::discover(worktree_root) else {
            return Self::without_git();
        };
        let Ok(head) = repo.head() else {
            return Self::without_git();
        };

        let branch = if head.is_branch() {
            head.shorthand()
                .ok()
                .filter(|name| !name.is_empty())
                .unwrap_or("HEAD")
                .to_string()
        } else {
            "HEAD".to_string()
        };
        let commit_sha = head.target().map(|oid| oid.to_string()).unwrap_or_default();

        Self { branch, commit_sha }
    }

    fn without_git() -> Self {
        Self {
            branch: "HEAD".to_string(),
            commit_sha: String::new(),
        }
    }
}

#[cfg(test)]
mod tests;
