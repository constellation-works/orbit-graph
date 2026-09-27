//! SQLite connection setup for the graph store.

pub(crate) mod history;
pub(crate) mod schema;

use std::fs;
use std::io::ErrorKind;
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use git2::Repository;
use rusqlite::{Connection, OpenFlags};

use crate::state_dir::{SCRATCH_DIR_NAME, StateAccess, open_state_file};
use crate::sync::scanner::DbLockGuard;
use crate::{EXTRACTOR_VERSION, GraphDbPath, GraphError, SyncPolicy, resolve_db_path_for_commit};

pub(crate) struct OpenedGraph {
    pub(crate) db_path: GraphDbPath,
    /// The database's physical path: its directory checked by
    /// [`create_index_dir`], so SQLite opens it with `SQLITE_OPEN_NOFOLLOW`.
    pub(crate) physical_db: PathBuf,
}

pub(crate) fn open(worktree_root: &Path, _policy: SyncPolicy) -> Result<OpenedGraph, GraphError> {
    let git = GitContext::for_worktree(worktree_root)?;
    let db_path = git.db_path(worktree_root);
    open_at_path(db_path, &git, IndexDirOwner::Scratch { worktree_root })
}

/// The database path the worktree's current branch or detached commit selects,
/// resolved without touching the filesystem beyond reading Git's `HEAD`.
pub(crate) fn resolve_worktree_db_path(worktree_root: &Path) -> Result<GraphDbPath, GraphError> {
    Ok(GitContext::for_worktree(worktree_root)?.db_path(worktree_root))
}

/// The [`GraphError::IndexMissing`] a read reports when `db_path` has not
/// been built by `orbit-graph sync`.
pub(crate) fn missing_graph_index(worktree_root: &Path, db_path: &Path) -> GraphError {
    GraphError::IndexMissing {
        path: db_path.to_path_buf(),
        reason: format!(
            "no graph index for {} at {}; run `orbit-graph sync`",
            worktree_root.display(),
            db_path.display()
        ),
    }
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
    let git = GitContext::for_worktree(worktree_root)?;
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
    let git = GitContext::for_worktree(worktree_root)?;
    Ok(GraphDbPath::new(
        db_path.to_path_buf(),
        git.branch,
        EXTRACTOR_VERSION,
    ))
}

/// The physical path of the existing database `db_path` for a read-only
/// open, or `None` when it does not exist. Its directory is checked as
/// [`existing_index_dir`] describes and the file itself must be a regular
/// file the current user owns, not a symlink (`STD-05 §R6`, `§R9`). Nothing
/// is created or changed.
pub(crate) fn existing_physical_db(
    worktree_root: &Path,
    db_path: &Path,
) -> Result<Option<PathBuf>, GraphError> {
    let Some(file_name) = db_path.file_name() else {
        return Ok(None);
    };
    let Some(dir) = existing_index_dir(worktree_root, db_path)? else {
        return Ok(None);
    };
    let physical = dir.join(file_name);
    if !crate::state_dir::check_state_file(physical.as_path())? {
        return Ok(None);
    }
    check_sidecars(physical.as_path())?;
    Ok(Some(physical))
}

/// Refuse a database whose `-wal`, `-shm` or `.lock` sidecar is a symlink,
/// not a regular file, or another user's, without changing anything.
pub(crate) fn check_sidecars(physical_db: &Path) -> Result<(), GraphError> {
    for suffix in ["-wal", "-shm", ".lock"] {
        let mut name = physical_db.as_os_str().to_os_string();
        name.push(suffix);
        crate::state_dir::check_state_file(Path::new(&name))?;
    }
    Ok(())
}

fn open_at_path(
    db_path: GraphDbPath,
    git: &GitContext,
    owner: IndexDirOwner<'_>,
) -> Result<OpenedGraph, GraphError> {
    let (Some(parent), Some(file_name)) = (db_path.path().parent(), db_path.path().file_name())
    else {
        return Err(GraphError::invalid_data(
            "validate graph database path",
            format!(
                "database path must name a file: {}",
                db_path.path().display()
            ),
        ));
    };
    let physical_db =
        create_index_dir(parent, owner, "create graph database directory")?.join(file_name);

    // SQLite's default creation mode can expose indexed source text. Create
    // the database with private permissions before SQLite opens it, repair an
    // existing one to 0600, and refuse a symlink in its name (STD-05 §R7-§R9);
    // SQLite gives the -wal and -shm sidecars the database's mode.
    drop(open_state_file(physical_db.as_path(), StateAccess::Write)?);

    // Switching a new file to WAL needs the database to itself, and SQLite
    // answers a concurrent switch with SQLITE_BUSY instead of waiting. First
    // opens therefore serialize on the sync lock until the file is a WAL
    // database; an initialized one needs no lock (STD-03 §R6).
    let setup_lock = if database_uses_wal(physical_db.as_path(), "read graph database header")? {
        None
    } else {
        Some(DbLockGuard::acquire(physical_db.as_path())?)
    };
    let mut conn = open_writer(physical_db.as_path(), "open graph database")?;
    configure_connection(&conn)?;

    // The emptiness check is repeated inside the schema transaction, so
    // exactly one opener creates the tables even without the setup lock.
    if schema::database_is_empty(&conn)? {
        schema::initialize_if_empty(
            &mut conn,
            &schema::InitialMeta {
                extractor_version: EXTRACTOR_VERSION,
                branch: git.branch.as_str(),
                commit_sha: git.commit_sha.as_str(),
            },
        )?;
    }
    drop(setup_lock);
    // A database whose stored schema identity differs is never written
    // (STD-03 §R10).
    schema::validate_identity(&conn, db_path.path())?;

    Ok(OpenedGraph {
        db_path,
        physical_db,
    })
}

/// Open the physical database `path` read-write with `SQLITE_OPEN_NOFOLLOW`,
/// so SQLite refuses a symlink anywhere in the path it is given.
pub(crate) fn open_writer(path: &Path, operation: &'static str) -> Result<Connection, GraphError> {
    Connection::open_with_flags(path, OpenFlags::default() | OpenFlags::SQLITE_OPEN_NOFOLLOW)
        .map_err(|source| GraphError::sqlite(operation, source))
}

// Recovered from the graph crate's local SQLite setup. WAL is a hard
// requirement because the graph store is local scratch state on a writable
// filesystem; the other pragmas provide bounded waits and consistent keys.
// The busy wait is set first, so every later statement waits out a
// concurrent writer.
fn configure_connection(conn: &Connection) -> Result<(), GraphError> {
    conn.pragma_update(None, "busy_timeout", 5_000)
        .map_err(|source| GraphError::sqlite("set busy_timeout", source))?;
    let journal_mode = conn
        .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get::<_, String>(0))
        .map_err(|source| GraphError::sqlite("set journal_mode=WAL", source))?;
    if !journal_mode.eq_ignore_ascii_case("wal") {
        return Err(GraphError::sqlite_message(
            "set journal_mode=WAL",
            format!("SQLite kept journal_mode={journal_mode}"),
        ));
    }

    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|source| GraphError::sqlite("set foreign_keys=ON", source))?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(|source| GraphError::sqlite("set synchronous=NORMAL", source))?;
    Ok(())
}

/// Size of a WAL file header; a `-wal` no longer than this holds no frames.
const WAL_HEADER_BYTES: u64 = 32;
/// The 16-byte magic string every SQLite database file starts with.
const DATABASE_MAGIC: &[u8] = b"SQLite format 3\0";

/// How a read-only connection sees a database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadCurrency {
    /// An ordinary read-only connection: a rollback-journal database, or a
    /// WAL database whose `-wal` and `-shm` sidecars already exist.
    Live,
    /// `immutable=1`: a WAL database with no wal-index. An ordinary read-only
    /// connection would create the `-wal` and `-shm` sidecars and leave them
    /// behind, and cannot open at all on a read-only filesystem.
    MainFileOnly,
}

/// Opens an existing SQLite database for reads that write nothing: no file,
/// sidecar, lock, schema or row is created or changed (STD-01 §R31), so it
/// works against a read-only directory. Writes are refused by SQLite itself
/// (`SQLITE_OPEN_READ_ONLY` and `query_only`).
///
/// A rollback-journal database, or a WAL database whose sidecars a writer has
/// published, opens as an ordinary read-only connection. A WAL database with
/// no wal-index, which is how `orbit-graph sync` leaves one when it closes,
/// opens with `immutable=1` and reads the main file alone: nothing on disk
/// proves no writer starts during the read, so a sync that checkpoints into
/// the file mid-query can make that one query fail or see torn results. A
/// `-wal` holding committed frames without its `-shm` would read as stale,
/// so it fails closed with a message naming the writable step that fixes it.
pub(crate) fn open_observational(
    path: &Path,
    operation: &'static str,
) -> Result<Connection, GraphError> {
    let currency = read_currency(path, operation)?;
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = match currency {
        ReadCurrency::Live => Connection::open_with_flags(path, flags),
        ReadCurrency::MainFileOnly => {
            Connection::open_with_flags(immutable_uri(path), flags | OpenFlags::SQLITE_OPEN_URI)
        }
    }
    .map_err(|source| GraphError::sqlite(operation, source))?;
    conn.pragma_update(None, "query_only", "ON")
        .map_err(|source| GraphError::sqlite("set query_only for a read", source))?;
    conn.pragma_update(None, "busy_timeout", 5_000)
        .map_err(|source| GraphError::sqlite("set busy_timeout for a read", source))?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|source| GraphError::sqlite("enable foreign keys for a read", source))?;
    // Opening is lazy; read once so an unreadable file fails here, naming it.
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |row| {
        row.get::<_, i64>(0)
    })
    .map_err(|source| {
        GraphError::sqlite_message(operation, format!("{}: {source}", path.display()))
    })?;
    Ok(conn)
}

fn read_currency(path: &Path, operation: &'static str) -> Result<ReadCurrency, GraphError> {
    if !database_uses_wal(path, operation)? {
        return Ok(ReadCurrency::Live);
    }
    let wal = sidecar_len(path, "-wal", operation)?;
    let shm = sidecar_len(path, "-shm", operation)?;
    match (wal, shm) {
        (Some(_), Some(_)) => Ok(ReadCurrency::Live),
        (None, Some(_)) => Err(unreadable_without_writing(
            path,
            "its -shm wal-index exists without the -wal it indexes",
        )),
        (Some(wal), None) if wal > WAL_HEADER_BYTES => Err(unreadable_without_writing(
            path,
            "its -wal holds commits but its -shm wal-index is missing",
        )),
        (Some(_) | None, None) => Ok(ReadCurrency::MainFileOnly),
    }
}

fn unreadable_without_writing(path: &Path, reason: &str) -> GraphError {
    GraphError::invalid_data(
        "open index read-only",
        format!(
            "cannot read {} without writing: {reason}; run the command that maintains it \
             (`orbit-graph sync` for a graph index) from writable storage to checkpoint it, \
             then retry",
            path.display()
        ),
    )
}

/// Whether the database header records WAL mode. A missing, short or
/// non-SQLite file is reported as not WAL and left to SQLite to diagnose.
fn database_uses_wal(path: &Path, operation: &'static str) -> Result<bool, GraphError> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(source) => return Err(GraphError::io(operation, path, source)),
    };
    let mut header = Vec::with_capacity(20);
    file.take(20)
        .read_to_end(&mut header)
        .map_err(|source| GraphError::io(operation, path, source))?;
    Ok(header.len() == 20
        && header.starts_with(DATABASE_MAGIC)
        && (header[18] == 2 || header[19] == 2))
}

fn sidecar_len(
    path: &Path,
    suffix: &str,
    operation: &'static str,
) -> Result<Option<u64>, GraphError> {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    let sidecar = PathBuf::from(name);
    match fs::metadata(&sidecar) {
        Ok(metadata) => Ok(Some(metadata.len())),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(GraphError::io(operation, sidecar, source)),
    }
}

/// `file:<path>?immutable=1`, with the characters SQLite's URI parser
/// treats specially percent-encoded.
fn immutable_uri(path: &Path) -> PathBuf {
    #[cfg(unix)]
    let bytes = {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    };
    #[cfg(not(unix))]
    let bytes = path.to_string_lossy().replace('\\', "/").into_bytes();
    let mut uri = b"file:".to_vec();
    for byte in bytes {
        match byte {
            b'?' | b'#' | b'%' => uri.extend_from_slice(format!("%{byte:02X}").as_bytes()),
            _ => uri.push(byte),
        }
    }
    uri.extend_from_slice(b"?immutable=1");
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_vec(uri))
    }
    #[cfg(not(unix))]
    PathBuf::from(String::from_utf8_lossy(&uri).into_owned())
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
    /// orbit-graph's own per-repository state directory, which the CLI's
    /// plugin protocol places under the plugin state root it is given.
    PluginState,
    /// A directory the caller chose, such as the parent of a library caller's
    /// database path. orbit-graph creates it when missing but never writes
    /// into it.
    Caller,
}

/// Creates `dir`, with any missing parents, to hold an index, marks it with a
/// `.gitignore` containing `*` when orbit-graph owns it, and returns its
/// physical path, which every later open of a file in it uses.
///
/// orbit-graph owns exactly two kinds of directory: the canonical
/// `<worktree>/.orbit-graph` scratch directory and its own per-repository
/// state directory ([`IndexDirOwner::PluginState`]). Both are resolved through
/// [`crate::state_dir`]: decided on the physical path, never through a
/// symlink, refused when another user owns them or (for the scratch
/// directory) when Git tracks them, created owner-only (`0700`) and repaired
/// to `0700` whatever the umask (`STD-05 §R6`–`§R9`). A caller-chosen
/// directory ([`IndexDirOwner::Caller`]) and the parents created on the way to
/// it keep the default mode and are never marked, so a `.gitignore` cannot
/// land in a directory that holds the user's files. A `Scratch` directory
/// that is not directly under its worktree is treated like a caller's.
///
/// The marker write never replaces an existing `.gitignore` (`STD-05 §R7`;
/// see [`write_new_gitignore`]). Failing to mark the directory is logged, not
/// fatal: the index works without it.
pub(crate) fn create_index_dir(
    dir: &Path,
    owner: IndexDirOwner<'_>,
    operation: &'static str,
) -> Result<PathBuf, GraphError> {
    let physical = match owner {
        IndexDirOwner::Scratch { worktree_root }
            if dir.parent() == Some(worktree_root)
                && dir.file_name() == Some(std::ffi::OsStr::new(SCRATCH_DIR_NAME)) =>
        {
            Some(scratch_dir(worktree_root, StateAccess::Write)?)
        }
        IndexDirOwner::PluginState => Some(plugin_state_dir(dir, StateAccess::Write)?),
        IndexDirOwner::Scratch { .. } | IndexDirOwner::Caller => None,
    };
    let Some(physical) = physical.flatten() else {
        fs::create_dir_all(dir).map_err(|source| GraphError::io(operation, dir, source))?;
        return dir
            .canonicalize()
            .map_err(|source| GraphError::io(operation, dir, source));
    };
    if let Err(error) = write_new_gitignore(physical.as_path(), OWNED_DIR_GITIGNORE.as_bytes()) {
        tracing::warn!(
            path = %physical.join(".gitignore").display(),
            error = %error,
            "could not write .gitignore for orbit-graph index directory"
        );
    }
    Ok(physical)
}

/// The physical `<worktree_root>/.orbit-graph`, checked by
/// [`crate::state_dir::scratch_state_dir`]; `None` for a reader when it does
/// not exist.
pub(crate) fn scratch_dir(
    worktree_root: &Path,
    access: StateAccess,
) -> Result<Option<PathBuf>, GraphError> {
    crate::state_dir::scratch_state_dir(worktree_root, Path::new(""), access)
}

/// The physical plugin state directory `dir`: its parent, the plugin state
/// root, is trusted (and created owner-only by a writer when missing), and
/// `dir` itself is checked as orbit-graph's own state.
pub(crate) fn plugin_state_dir(
    dir: &Path,
    access: StateAccess,
) -> Result<Option<PathBuf>, GraphError> {
    let (Some(root), Some(name)) = (dir.parent(), dir.file_name()) else {
        return Err(GraphError::unsafe_state_path(
            dir,
            "a plugin state directory must be a named directory under the state root",
        ));
    };
    if access == StateAccess::Write && !root.exists() {
        crate::state_dir::create_private_dir_all(root)
            .map_err(|source| GraphError::io("create plugin state root", root, source))?;
    }
    if access == StateAccess::Read && !root.exists() {
        return Ok(None);
    }
    crate::state_dir::private_state_dir(root, Path::new(name), access)
}

/// The physical directory holding the database `db_path`, checked for a
/// reader as [`create_index_dir`] checks it for a writer. `None` when it does
/// not exist. A database outside `<worktree_root>/.orbit-graph` is a
/// caller-chosen or plugin path and its directory is taken as given.
pub(crate) fn existing_index_dir(
    worktree_root: &Path,
    db_path: &Path,
) -> Result<Option<PathBuf>, GraphError> {
    let Some(parent) = db_path.parent() else {
        return Ok(None);
    };
    if parent == worktree_root.join(SCRATCH_DIR_NAME) {
        return scratch_dir(worktree_root, StateAccess::Read);
    }
    match parent.canonicalize() {
        Ok(physical) => Ok(Some(physical)),
        Err(source) if source.kind() == ErrorKind::NotFound => Ok(None),
        Err(source) => Err(GraphError::io("resolve index directory", parent, source)),
    }
}

/// Create `dir`, orbit-graph's own per-repository state directory, owner-only
/// (`0700`, with any missing parents) and mark it with a self-ignoring
/// `.gitignore`, as [`create_index_dir`] does for
/// [`IndexDirOwner::PluginState`]. `operation` names the step in any error.
///
/// Public only for the `orbit-graph` CLI's plugin protocol, which creates its
/// code-graph index directory with it; the store module itself stays private.
#[doc(hidden)]
pub fn create_plugin_state_dir(dir: &Path, operation: &'static str) -> Result<(), GraphError> {
    create_index_dir(dir, IndexDirOwner::PluginState, operation).map(drop)
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
            0o600 as libc::c_uint,
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
    /// The branch and commit that select the worktree's database family.
    ///
    /// A directory outside any repository, and a repository on an unborn
    /// branch, have no commit and use the `HEAD` family. Any other failure to
    /// read the repository or its `HEAD` is an error: silently choosing the
    /// `HEAD` family would read or write another branch's index
    /// (STD-02 §R29).
    fn for_worktree(worktree_root: &Path) -> Result<Self, GraphError> {
        let repo = match Repository::discover(worktree_root) {
            Ok(repo) => repo,
            Err(error) if error.code() == git2::ErrorCode::NotFound => {
                return Ok(Self::without_git());
            }
            Err(error) => {
                return Err(GraphError::invalid_data(
                    "open the worktree's Git repository",
                    format!(
                        "cannot open the Git repository at {}: {}; repair it, then retry",
                        worktree_root.display(),
                        error.message()
                    ),
                ));
            }
        };
        let head = match repo.head() {
            Ok(head) => head,
            Err(error) if error.code() == git2::ErrorCode::UnbornBranch => {
                return Ok(Self::without_git());
            }
            Err(error) => {
                return Err(GraphError::invalid_data(
                    "read the worktree's HEAD",
                    format!(
                        "cannot read HEAD in {}: {}; repair HEAD (for example with \
                         `git checkout <branch>`), then retry",
                        worktree_root.display(),
                        error.message()
                    ),
                ));
            }
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

        Ok(Self { branch, commit_sha })
    }

    fn db_path(&self, worktree_root: &Path) -> GraphDbPath {
        resolve_db_path_for_commit(
            worktree_root,
            self.branch.as_str(),
            self.commit_sha.as_str(),
            EXTRACTOR_VERSION,
        )
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
