//! Local state that repository content can neither redirect nor pre-populate,
//! created owner-only, and the one durable write path.
//!
//! Every directory and file orbit-graph keeps between runs is reached through
//! this module (`STD-05 §R6`–`§R9`):
//!
//! - a state directory is resolved against a trusted, canonicalized root (the
//!   worktree, the plugin state root, or a directory the caller chose), and
//!   every component below that root is `lstat`ed: a symlink, something that
//!   is not a directory, or a directory another user owns is refused with
//!   [`GraphError::UnsafeStatePath`] naming the path and the reason;
//! - inside a Git worktree, a state directory with tracked index entries is
//!   refused, so a committed `.orbit-graph/` (or a symlink standing in for it)
//!   is never read, written, locked or cleaned;
//! - missing directories are created one at a time with mode `0700`, and a
//!   writer repairs an existing state directory to `0700`;
//! - files are opened with `O_NOFOLLOW` and checked through the open
//!   descriptor: a symlink, a non-regular file or another user's file is
//!   refused, and a writer repairs group or other permission bits to `0600`
//!   with `fchmod` on that descriptor.
//!
//! These checks decide on the path as it is when they run. A directory another
//! local process can change concurrently is not a boundary this module claims
//! to hold (`STD-05 §R5`); what it stops is state redirected or pre-populated
//! by repository content.
//!
//! [`atomic_write`] is the one durable replacement of a file (`STD-03 §R5`):
//! an owner-only temp file in the same directory, flushed, renamed over the
//! target, then the directory flushed.

use std::fs::{self, File};
use std::io::{self, ErrorKind, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use git2::Repository;

use crate::GraphError;

/// Name of the scratch directory orbit-graph keeps in a worktree root.
pub(crate) const SCRATCH_DIR_NAME: &str = ".orbit-graph";

/// Mode of every directory orbit-graph creates for its state.
const PRIVATE_DIR_MODE: u32 = 0o700;
/// Mode of every file orbit-graph creates for its state.
const PRIVATE_FILE_MODE: u32 = 0o600;

/// Whether a state path is only read or may be created and repaired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateAccess {
    /// Nothing is created, chmodded or written (`STD-01 §R31`). A missing
    /// directory or file is reported as absent.
    Read,
    /// Missing directories are created `0700` and files `0600`; existing
    /// state with group or other permission bits is repaired.
    Write,
}

/// The physical `<worktree_root>/.orbit-graph/<sub>` directory, checked as
/// the module documentation describes.
///
/// The whole scratch directory is refused when the worktree's Git index
/// tracks `.orbit-graph` or anything under it, or when it directly holds a
/// symlink: orbit-graph never creates one there, so a symlinked database,
/// lock or history file is repository content standing in for its state. With
/// [`StateAccess::Read`], `Ok(None)` means a component does not exist.
///
/// Public for the change-analysis library's snapshot cache and the CLI's
/// plugin protocol; not part of the supported API.
#[doc(hidden)]
pub fn scratch_state_dir(
    worktree_root: &Path,
    sub: &Path,
    access: StateAccess,
) -> Result<Option<PathBuf>, GraphError> {
    let root = canonical_root(worktree_root)?;
    let scratch = Path::new(SCRATCH_DIR_NAME);
    refuse_tracked(root.as_path(), scratch)?;
    let Some(scratch) = walk(root.as_path(), scratch, access, 0)? else {
        return Ok(None);
    };
    refuse_links_in(scratch.as_path())?;
    walk(scratch.as_path(), sub, access, 0)
}

/// Refuse `dir` when it directly holds a symlink, dangling or not, naming the
/// first one found.
fn refuse_links_in(dir: &Path) -> Result<(), GraphError> {
    let entries =
        fs::read_dir(dir).map_err(|source| GraphError::io("list state directory", dir, source))?;
    for entry in entries {
        let entry = entry.map_err(|source| GraphError::io("list state directory", dir, source))?;
        let is_link = entry
            .file_type()
            .map_err(|source| GraphError::io("inspect state file", entry.path(), source))?
            .is_symlink();
        if is_link {
            return Err(GraphError::unsafe_state_path(
                entry.path(),
                "it is a symbolic link inside the orbit-graph scratch directory, where \
                 orbit-graph never creates one; remove it and retry",
            ));
        }
    }
    Ok(())
}

/// The physical `<root>/<relative>` directory, where `root` is trusted and
/// every component of `relative` is state orbit-graph owns (the plugin's
/// per-repository directory under `$ORBIT_PLUGIN_STATE`, or a component the
/// caller asked it to create).
///
/// Public for the CLI's plugin protocol; not part of the supported API.
#[doc(hidden)]
pub fn private_state_dir(
    root: &Path,
    relative: &Path,
    access: StateAccess,
) -> Result<Option<PathBuf>, GraphError> {
    let root = canonical_root(root)?;
    walk(root.as_path(), relative, access, 0)
}

/// A caller-chosen state directory such as a `--cache-dir`: the caller's
/// spelling of `dir` is trusted up to its nearest existing ancestor, and
/// the physical path is decided from there (`STD-05 §R6`).
///
/// When the physical path lies inside `worktree` (a Git worktree), the part
/// below the worktree is checked like the scratch directory: no component may
/// be a symlink, and nothing at or below `dir` may be tracked. Missing
/// components are created `0700`; the final directory must be owned by the
/// current user and, for a writer, is repaired to `0700`.
///
/// Public for the change-analysis library's snapshot cache; not part of the
/// supported API.
#[doc(hidden)]
pub fn chosen_state_dir(
    dir: &Path,
    worktree: Option<&Path>,
    access: StateAccess,
) -> Result<Option<PathBuf>, GraphError> {
    let physical = physical_with_missing_tail(dir)?;
    if let Some(worktree) = worktree {
        let worktree = canonical_root(worktree)?;
        if let Ok(relative) = physical.strip_prefix(worktree.as_path())
            && !relative.as_os_str().is_empty()
        {
            let relative = relative.to_path_buf();
            refuse_tracked(worktree.as_path(), relative.as_path())?;
            let last = relative.components().count().saturating_sub(1);
            return walk(worktree.as_path(), relative.as_path(), access, last);
        }
    }
    // Outside any worktree the existing prefix is the caller's own choice;
    // only the components this call creates, and the final one, are state.
    let (existing, missing) = split_existing(physical.as_path());
    if missing.as_os_str().is_empty() {
        let metadata = fs::symlink_metadata(existing.as_path())
            .map_err(|source| GraphError::io("inspect state directory", &existing, source))?;
        verify_dir(existing.as_path(), &metadata, access, true)?;
        return Ok(Some(existing));
    }
    let last = missing.components().count().saturating_sub(1);
    walk(existing.as_path(), missing.as_path(), access, last)
}

/// Open the state file `path` without following a symlink in its final
/// component, and check it through the open descriptor.
///
/// [`StateAccess::Write`] opens read-write, creating the file `0600` when it
/// is missing and repairing group or other permission bits of an existing
/// one to `0600`. [`StateAccess::Read`] opens read-only and returns
/// `Ok(None)` for a missing file. Either way a symlink, a non-regular file
/// or a file another user owns is refused.
///
/// Public for the change-analysis library's snapshot cache and the CLI's
/// plugin protocol; not part of the supported API.
#[doc(hidden)]
pub fn open_state_file(path: &Path, access: StateAccess) -> Result<Option<File>, GraphError> {
    let mut options = fs::OpenOptions::new();
    match access {
        StateAccess::Read => options.read(true),
        StateAccess::Write => options.read(true).write(true).create(true),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(PRIVATE_FILE_MODE)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    #[cfg(not(unix))]
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(symlink_refused(path));
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(source) if access == StateAccess::Read && source.kind() == ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(source) if is_symlink_refusal(&source) => return Err(symlink_refused(path)),
        Err(source) => return Err(GraphError::io("open state file", path, source)),
    };
    let metadata = file
        .metadata()
        .map_err(|source| GraphError::io("inspect state file", path, source))?;
    if !metadata.is_file() {
        return Err(GraphError::unsafe_state_path(
            path,
            "it is not a regular file",
        ));
    }
    check_owner(path, &metadata)?;
    if access == StateAccess::Write {
        repair_mode(&file, path, &metadata, PRIVATE_FILE_MODE)?;
    }
    Ok(Some(file))
}

/// Check the state file `path` without opening it: `Ok(false)` when it does
/// not exist, and a refusal when it is a symlink (dangling or not), not a
/// regular file, or owned by another user. Nothing is changed.
#[doc(hidden)]
pub fn check_state_file(path: &Path) -> Result<bool, GraphError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == ErrorKind::NotFound => return Ok(false),
        Err(source) => return Err(GraphError::io("inspect state file", path, source)),
    };
    if metadata.file_type().is_symlink() {
        return Err(symlink_refused(path));
    }
    if !metadata.is_file() {
        return Err(GraphError::unsafe_state_path(
            path,
            "it is not a regular file",
        ));
    }
    check_owner(path, &metadata)?;
    Ok(true)
}

/// Create `dir` and any missing parents owner-only (`0700`), leaving existing
/// directories as they are. For state below a directory the caller chose,
/// such as the trees inside a snapshot cache entry.
#[doc(hidden)]
pub fn create_private_dir_all(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, PRIVATE_DIR_MODE);
    builder.create(dir)
}

/// Create the new file `path` owner-only (`0600`) and write `bytes` into it,
/// failing if anything, a symlink included, already has that name.
#[doc(hidden)]
pub fn write_new_private_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = new_private_file(path)?;
    file.write_all(bytes)
}

/// Replace `path` with `bytes` atomically and durably (`STD-03 §R5`).
///
/// The bytes go to a new owner-only (`0600`) temp file beside `path`, named
/// `<file name>.tmp-<pid>-<sequence>`, which is flushed and renamed over
/// `path`; the directory is then flushed so the rename survives a crash.
/// Readers see the old file or the new one, never a partial one, and a
/// rename replaces a symlink at `path` rather than writing through it. On
/// failure the temp file is removed; a crash may leave it behind, and
/// [`atomic_write_temp_pid`] recognizes it for the owner to reclaim.
///
/// # Examples
///
/// ```
/// let dir = tempfile::tempdir()?;
/// let path = dir.path().join("state.json");
/// orbit_graph::atomic_write(&path, b"{}\n")?;
/// orbit_graph::atomic_write(&path, b"{\"v\":2}\n")?;
/// assert_eq!(std::fs::read(&path)?, b"{\"v\":2}\n");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            format!("{} does not name a file", path.display()),
        ));
    };
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    let mut temp_name = name.to_os_string();
    temp_name.push(format!(
        ".tmp-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let temp = dir.join(temp_name);
    let written = (|| {
        let mut file = new_private_file(temp.as_path())?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(temp.as_path(), path)?;
        sync_dir(dir)
    })();
    if written.is_err() {
        let _ = fs::remove_file(temp.as_path());
    }
    written
}

/// The process ID in `file_name` when it is a temp file [`atomic_write`]
/// creates for a target named `target_name`, so the owner of a directory can
/// reclaim temp files a crashed writer left behind.
///
/// # Examples
///
/// ```
/// use orbit_graph::atomic_write_temp_pid;
///
/// assert_eq!(atomic_write_temp_pid("entry.json.tmp-42-7", "entry.json"), Some(42));
/// assert_eq!(atomic_write_temp_pid("entry.json", "entry.json"), None);
/// assert_eq!(atomic_write_temp_pid("other.tmp-42-7", "entry.json"), None);
/// ```
pub fn atomic_write_temp_pid(file_name: &str, target_name: &str) -> Option<u32> {
    let rest = file_name.strip_prefix(target_name)?.strip_prefix(".tmp-")?;
    let (pid, sequence) = rest.split_once('-')?;
    let decimal =
        |value: &str| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit());
    if !decimal(pid) || !decimal(sequence) {
        return None;
    }
    pid.parse().ok()
}

fn new_private_file(path: &Path) -> io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(PRIVATE_FILE_MODE)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    options.open(path)
}

#[cfg(unix)]
fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

/// Directories cannot be opened for flushing on every platform; the rename
/// is still atomic there.
#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}

fn canonical_root(root: &Path) -> Result<PathBuf, GraphError> {
    root.canonicalize()
        .map_err(|source| GraphError::io("resolve state root", root, source))
}

/// `path` with its nearest existing ancestor canonicalized and the missing
/// names appended (`STD-05 §R6`).
fn physical_with_missing_tail(path: &Path) -> Result<PathBuf, GraphError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|source| GraphError::io("resolve state directory", path, source))?
            .join(path)
    };
    let mut missing = Vec::new();
    let mut existing = absolute.as_path();
    loop {
        match existing.canonicalize() {
            Ok(physical) => {
                let mut physical = physical;
                for name in missing.iter().rev() {
                    physical.push(name);
                }
                return Ok(physical);
            }
            Err(source) if source.kind() == ErrorKind::NotFound => {
                let (Some(parent), Some(name)) = (existing.parent(), existing.file_name()) else {
                    return Err(GraphError::io("resolve state directory", path, source));
                };
                if fs::symlink_metadata(existing).is_ok() {
                    // A dangling symlink: canonicalizing it fails, but it
                    // exists, so it must not be treated as a missing name.
                    return Err(symlink_refused(existing));
                }
                missing.push(name.to_os_string());
                existing = parent;
            }
            Err(source) => return Err(GraphError::io("resolve state directory", path, source)),
        }
    }
}

/// The longest existing prefix of the physical `path`, and the rest.
fn split_existing(path: &Path) -> (PathBuf, PathBuf) {
    let mut existing = path.to_path_buf();
    let mut missing = Vec::new();
    while fs::symlink_metadata(existing.as_path()).is_err() {
        let Some(name) = existing.file_name().map(std::ffi::OsStr::to_os_string) else {
            break;
        };
        missing.push(name);
        existing.pop();
    }
    let tail = missing.iter().rev().collect::<PathBuf>();
    (existing, tail)
}

/// Resolve `relative` under the canonical `root` one component at a time.
/// Components from index `owned_from` on are state orbit-graph owns: a writer
/// repairs their modes. Every component must be a real directory owned by
/// the current user.
fn walk(
    root: &Path,
    relative: &Path,
    access: StateAccess,
    owned_from: usize,
) -> Result<Option<PathBuf>, GraphError> {
    let mut current = root.to_path_buf();
    for (index, component) in relative.components().enumerate() {
        let Component::Normal(name) = component else {
            return Err(GraphError::unsafe_state_path(
                root.join(relative),
                "a state path must be a plain relative path below its root",
            ));
        };
        current.push(name);
        let owned = index >= owned_from;
        let metadata = match fs::symlink_metadata(current.as_path()) {
            Ok(metadata) => metadata,
            Err(source) if source.kind() == ErrorKind::NotFound => {
                if access == StateAccess::Read {
                    return Ok(None);
                }
                let mut builder = fs::DirBuilder::new();
                #[cfg(unix)]
                std::os::unix::fs::DirBuilderExt::mode(&mut builder, PRIVATE_DIR_MODE);
                match builder.create(current.as_path()) {
                    Ok(()) => {}
                    Err(source) if source.kind() == ErrorKind::AlreadyExists => {}
                    Err(source) => {
                        return Err(GraphError::io("create state directory", &current, source));
                    }
                }
                // What was created must be the directory that was meant
                // (`STD-05 §R7`): re-check it rather than trust the create.
                fs::symlink_metadata(current.as_path())
                    .map_err(|source| GraphError::io("inspect state directory", &current, source))?
            }
            Err(source) => return Err(GraphError::io("inspect state directory", &current, source)),
        };
        verify_dir(current.as_path(), &metadata, access, owned)?;
    }
    Ok(Some(current))
}

fn verify_dir(
    path: &Path,
    metadata: &fs::Metadata,
    access: StateAccess,
    owned: bool,
) -> Result<(), GraphError> {
    if metadata.file_type().is_symlink() {
        return Err(symlink_refused(path));
    }
    if !metadata.is_dir() {
        return Err(GraphError::unsafe_state_path(path, "it is not a directory"));
    }
    check_owner(path, metadata)?;
    if owned && access == StateAccess::Write {
        #[cfg(unix)]
        if metadata_mode(metadata) & 0o077 != 0 {
            use std::os::unix::fs::OpenOptionsExt;
            let dir = fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(path)
                .map_err(|source| {
                    // `O_DIRECTORY | O_NOFOLLOW` on a symlink may be ENOTDIR.
                    if is_symlink_refusal(&source) || source.raw_os_error() == Some(libc::ENOTDIR) {
                        symlink_refused(path)
                    } else {
                        GraphError::io("open state directory", path, source)
                    }
                })?;
            let opened = dir
                .metadata()
                .map_err(|source| GraphError::io("inspect state directory", path, source))?;
            repair_mode(&dir, path, &opened, PRIVATE_DIR_MODE)?;
        }
    }
    Ok(())
}

/// Refuse `root/relative` when the Git index of the worktree containing
/// `root` tracks it, or anything below it. Outside a Git worktree nothing is
/// tracked.
fn refuse_tracked(root: &Path, relative: &Path) -> Result<(), GraphError> {
    let repo = match Repository::discover(root) {
        Ok(repo) => repo,
        Err(error) if error.code() == git2::ErrorCode::NotFound => return Ok(()),
        Err(error) => {
            return Err(GraphError::invalid_data(
                "check state directory against the Git index",
                format!(
                    "cannot open the Git repository at {}: {}",
                    root.display(),
                    error.message()
                ),
            ));
        }
    };
    let Some(workdir) = repo.workdir() else {
        return Ok(());
    };
    let workdir = canonical_root(workdir)?;
    let target = root.join(relative);
    let Ok(in_repo) = target.strip_prefix(workdir.as_path()) else {
        return Ok(());
    };
    if in_repo.as_os_str().is_empty() {
        return Ok(());
    }
    let index = repo.index().map_err(|error| {
        GraphError::invalid_data(
            "check state directory against the Git index",
            format!(
                "cannot read the Git index of {}: {}",
                workdir.display(),
                error.message()
            ),
        )
    })?;
    let spelled = in_repo
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    let tracked = index.get_path(Path::new(spelled.as_str()), 0).is_some() || {
        let prefix = format!("{spelled}/");
        match index.find_prefix(prefix.as_str()) {
            Ok(position) => index
                .get(position)
                .is_some_and(|entry| entry.path.starts_with(prefix.as_bytes())),
            Err(error) if error.code() == git2::ErrorCode::NotFound => false,
            Err(error) => {
                return Err(GraphError::invalid_data(
                    "check state directory against the Git index",
                    format!("cannot search the Git index: {}", error.message()),
                ));
            }
        }
    };
    if tracked {
        return Err(GraphError::unsafe_state_path(
            target,
            format!(
                "the Git index tracks `{spelled}` or files under it, so it is repository \
                 content, not orbit-graph state; remove it from the index (`git rm -r --cached \
                 {spelled}`) and retry"
            ),
        ));
    }
    Ok(())
}

fn symlink_refused(path: &Path) -> GraphError {
    GraphError::unsafe_state_path(
        path,
        "it is a symbolic link, and orbit-graph never follows a link in its state path",
    )
}

#[cfg(unix)]
fn is_symlink_refusal(error: &io::Error) -> bool {
    // Linux and macOS report `O_NOFOLLOW` on a symlink as ELOOP; FreeBSD
    // uses EMLINK.
    matches!(
        error.raw_os_error(),
        Some(code) if code == libc::ELOOP || code == libc::EMLINK
    )
}

#[cfg(not(unix))]
fn is_symlink_refusal(_error: &io::Error) -> bool {
    false
}

#[cfg(unix)]
fn metadata_mode(metadata: &fs::Metadata) -> u32 {
    std::os::unix::fs::PermissionsExt::mode(&metadata.permissions()) & 0o7777
}

#[cfg(unix)]
fn check_owner(path: &Path, metadata: &fs::Metadata) -> Result<(), GraphError> {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    if metadata.uid() == euid {
        return Ok(());
    }
    Err(GraphError::unsafe_state_path(
        path,
        format!(
            "it is owned by uid {}, not by the current user (uid {euid})",
            metadata.uid()
        ),
    ))
}

#[cfg(not(unix))]
fn check_owner(_path: &Path, _metadata: &fs::Metadata) -> Result<(), GraphError> {
    Ok(())
}

/// Clear group and other permission bits through the open descriptor, so the
/// repair lands on the file that was checked (`STD-05 §R7`, `§R9`).
#[cfg(unix)]
fn repair_mode(
    file: &File,
    path: &Path,
    metadata: &fs::Metadata,
    mode: u32,
) -> Result<(), GraphError> {
    use std::os::unix::fs::PermissionsExt;
    let current = metadata_mode(metadata);
    if current & 0o077 == 0 {
        return Ok(());
    }
    file.set_permissions(fs::Permissions::from_mode(mode))
        .map_err(|source| GraphError::io("restrict state permissions", path, source))?;
    tracing::warn!(
        path = %path.display(),
        from = format_args!("{current:o}"),
        to = format_args!("{mode:o}"),
        "repaired state permissions to owner-only"
    );
    Ok(())
}

#[cfg(not(unix))]
fn repair_mode(
    _file: &File,
    _path: &Path,
    _metadata: &fs::Metadata,
    _mode: u32,
) -> Result<(), GraphError> {
    Ok(())
}

#[cfg(test)]
#[path = "state_dir/tests/mod.rs"]
mod tests;
