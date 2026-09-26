//! SQLite connection setup for the graph store.

pub(crate) mod history;
pub(crate) mod schema;

use std::fs;
use std::path::Path;
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
    open_at_path(db_path, &git, IndexDirOwner::Scratch { worktree_root })
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
    open_at_path(db_path, &git, IndexDirOwner::Scratch { worktree_root })
}

pub(crate) fn open_with_db_path(
    worktree_root: &Path,
    db_path: &Path,
    owner: IndexDirOwner<'_>,
) -> Result<OpenedGraph, GraphError> {
    if db_path.is_dir() || db_path.file_name().is_none() {
        return Err(GraphError::invalid_data(
            "validate graph database path",
            format!("database path must name a file: {}", db_path.display()),
        ));
    }
    let git = GitContext::for_worktree(worktree_root);
    let db_path = GraphDbPath::new(db_path.to_path_buf(), git.branch.clone(), EXTRACTOR_VERSION);
    open_at_path(db_path, &git, owner)
}

/// Describe an existing database for a read-only open: nothing is created,
/// initialized, or migrated.
pub(crate) fn existing_db_path(
    worktree_root: &Path,
    db_path: &Path,
) -> Result<GraphDbPath, GraphError> {
    if !db_path.is_file() {
        return Err(GraphError::invalid_data(
            "open graph database read-only",
            format!("no graph database at {}", db_path.display()),
        ));
    }
    let git = GitContext::for_worktree(worktree_root);
    Ok(GraphDbPath::new(
        db_path.to_path_buf(),
        git.branch,
        EXTRACTOR_VERSION,
    ))
}

fn open_at_path(
    db_path: GraphDbPath,
    git: &GitContext,
    owner: IndexDirOwner<'_>,
) -> Result<OpenedGraph, GraphError> {
    if let Some(parent) = db_path.path().parent() {
        create_index_dir(parent, owner, "create graph database directory")?;
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

/// Who owns an index directory, which decides whether orbit-graph may mark it
/// with a `.gitignore`.
#[derive(Clone, Copy, Debug)]
pub(crate) enum IndexDirOwner<'a> {
    /// The default layout: `<worktree_root>/.orbit-graph`.
    Scratch {
        /// The worktree whose scratch directory this is.
        worktree_root: &'a Path,
    },
    /// orbit-graph's own per-repository directory under `$ORBIT_PLUGIN_STATE`.
    PluginState,
    /// A directory the caller chose, such as the parent of a library caller's
    /// database path. orbit-graph creates it when missing but never writes
    /// into it.
    Caller,
}

/// Creates `dir`, with any missing parents, to hold an index, and marks it
/// with a `.gitignore` containing `*` when orbit-graph owns it.
///
/// orbit-graph owns exactly two kinds of directory: the canonical
/// `<worktree>/.orbit-graph` scratch directory and its own per-repository
/// directory under `$ORBIT_PLUGIN_STATE`. A caller-chosen directory
/// ([`IndexDirOwner::Caller`]) and the parents created on the way to any
/// directory are never marked, so a `.gitignore` cannot land in a directory
/// that holds the user's files.
///
/// The scratch rule is decided on the physical path (STD-05 §R6): the
/// directory must be a real directory that canonicalizes to
/// `<canonical worktree>/.orbit-graph`. The write itself never follows a
/// symlink in the directory's own name and never replaces an existing
/// `.gitignore` (STD-05 §R7; see [`write_new_gitignore`]). Failing to mark
/// the directory is logged, not fatal: the index works without it.
pub(crate) fn create_index_dir(
    dir: &Path,
    owner: IndexDirOwner<'_>,
    operation: &'static str,
) -> Result<(), GraphError> {
    fs::create_dir_all(dir).map_err(|source| GraphError::io(operation, dir, source))?;
    let owned = match owner {
        IndexDirOwner::Scratch { worktree_root } => is_canonical_scratch_dir(dir, worktree_root),
        IndexDirOwner::PluginState => true,
        IndexDirOwner::Caller => false,
    };
    if owned && let Err(error) = write_new_gitignore(dir, OWNED_DIR_GITIGNORE.as_bytes()) {
        tracing::warn!(
            path = %dir.join(".gitignore").display(),
            error = %error,
            "could not write .gitignore for orbit-graph index directory"
        );
    }
    Ok(())
}

/// Whether `dir` is, physically, the `.orbit-graph` directory directly under
/// `worktree_root`. A `.orbit-graph` that is a symlink, or that resolves
/// anywhere else, is not.
fn is_canonical_scratch_dir(dir: &Path, worktree_root: &Path) -> bool {
    let is_real_dir = fs::symlink_metadata(dir).is_ok_and(|metadata| metadata.is_dir());
    let physical = dir.canonicalize();
    let expected = worktree_root
        .canonicalize()
        .map(|root| root.join(SCRATCH_DIR_NAME));
    let owned = is_real_dir
        && matches!((&physical, &expected), (Ok(physical), Ok(expected)) if physical == expected);
    if !owned {
        tracing::warn!(
            path = %dir.display(),
            "not marking the orbit-graph scratch directory: it is not a real directory directly under the worktree"
        );
    }
    owned
}

/// Atomically creates `<dir>/.gitignore` holding `contents`, unless an entry
/// named `.gitignore` already exists there, in which case it is kept and
/// `Ok(false)` is returned.
///
/// `dir` is opened once with `O_NOFOLLOW | O_DIRECTORY`, so a symlink in its
/// final component is refused, and every later step is relative to that open
/// directory. The contents go to a temp file that is flushed and then
/// hard-linked to `.gitignore`; `linkat` never replaces an existing entry, so
/// this is STD-03 §R5's temp-and-rename with no-replace semantics. A crash or
/// a full disk leaves at most a stray temp file, never an empty `.gitignore`
/// that would later count as the user's.
#[cfg(unix)]
pub(crate) fn write_new_gitignore(dir: &Path, contents: &[u8]) -> std::io::Result<bool> {
    use std::ffi::{CStr, CString};
    use std::io::Write;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
    const TARGET: &CStr = c".gitignore";

    let directory = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(dir)?;
    let dir_fd = directory.as_raw_fd();
    let temp_name = CString::new(format!(
        ".gitignore.orbit-graph-{}-{}.tmp",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
    .map_err(std::io::Error::other)?;

    // SAFETY: `dir_fd` is an open directory descriptor owned by `directory`,
    // which outlives this call, and `temp_name` is NUL-terminated.
    let temp_fd = unsafe {
        libc::openat(
            dir_fd,
            temp_name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o644 as libc::c_uint,
        )
    };
    if temp_fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `openat` returned a fresh descriptor that nothing else owns.
    let mut temp = unsafe { fs::File::from_raw_fd(temp_fd) };
    let linked = temp
        .write_all(contents)
        .and_then(|()| temp.sync_all())
        .and_then(|()| {
            // SAFETY: both descriptors are the open directory above and both
            // names are NUL-terminated; flags 0 means the source is not
            // followed if it is a symlink, and an existing target fails.
            let status =
                unsafe { libc::linkat(dir_fd, temp_name.as_ptr(), dir_fd, TARGET.as_ptr(), 0) };
            if status == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    drop(temp);
    // SAFETY: as above; this removes only the temp entry this call created.
    unsafe {
        libc::unlinkat(dir_fd, temp_name.as_ptr(), 0);
    }
    match linked {
        Ok(()) => {
            directory.sync_all()?;
            Ok(true)
        }
        Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error),
    }
}

/// Non-Unix platforms lack the descriptor-relative calls the Unix version
/// relies on, so orbit-graph leaves the directory unmarked there.
#[cfg(not(unix))]
pub(crate) fn write_new_gitignore(_dir: &Path, _contents: &[u8]) -> std::io::Result<bool> {
    Ok(false)
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
