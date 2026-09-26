//! Worktree scanner and file-table diffing.

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::extract::Extractor;
use crate::extract::languages;
use crate::lock::{self, FileLockGuard};
use git2::Repository;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use rusqlite::{Connection, params};

use super::{graph_failure, io_failure};
use crate::{GraphError, SyncFailure, SyncMode};

/// Largest file sync reads, hashes or extracts: the change explorer's 4 MiB
/// blob cap. A larger file is skipped with a warning and gets no rows
/// (`STD-03 §R22`).
pub(crate) const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

const ORBITIGNORE_FILE_NAME: &str = ".orbitignore";

const DEFAULT_ORBITIGNORE_PATTERNS: &[&str] = &[
    ".orbit-graph/",
    ".orbit/",
    "node_modules/",
    "target/",
    "dist/",
    "build/",
    ".venv/",
    "venv/",
    "__pycache__/",
    "*.egg-info/",
];

const SKIP_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "ico", "woff", "woff2", "ttf", "eot", "exe", "dll", "so", "dylib",
    "pdf", "zip", "tar", "gz", "lock",
];

/// Per-file classification produced by the scanner.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Diff {
    /// Existing indexed files whose content remains current.
    pub(crate) unchanged: Vec<PathBuf>,
    /// Existing indexed files whose content changed or must be fully refreshed.
    pub(crate) modified: Vec<PathBuf>,
    /// Indexable files present on disk without a `files` row.
    pub(crate) new: Vec<PathBuf>,
    /// Files present in `files` but absent from the filtered worktree scan.
    pub(crate) deleted: Vec<PathBuf>,
    /// Supported files skipped because they exceed [`MAX_FILE_BYTES`]. A
    /// previously indexed one is also listed in `deleted`, so its rows go.
    pub(crate) oversize: Vec<PathBuf>,
    /// Directories and files the scan could not read, one entry each
    /// (`STD-02 §R32`). What is already indexed at or under such a path is
    /// neither modified nor deleted, since its state on disk is unknown.
    pub(crate) failed: Vec<SyncFailure>,
}

impl Diff {
    pub(crate) fn has_changes(&self) -> bool {
        !(self.modified.is_empty() && self.new.is_empty() && self.deleted.is_empty())
    }
}

#[cfg(test)]
pub(crate) fn scan_diff(
    db_path: &Path,
    worktree_root: &Path,
    mode: SyncMode,
) -> Result<Diff, GraphError> {
    Scanner::new(db_path, worktree_root)?.scan(mode, &Blake3Hasher)
}

pub(crate) fn scan_diff_with_lock_held(
    db_path: &Path,
    worktree_root: &Path,
    mode: SyncMode,
) -> Result<Diff, GraphError> {
    Scanner::new_with_lock(db_path, worktree_root, None).scan(mode, &Blake3Hasher)
}

struct Scanner {
    db_path: PathBuf,
    worktree_root: PathBuf,
    _lock: Option<DbLockGuard>,
    registry: ExtractorRegistry,
}

impl Scanner {
    #[cfg(test)]
    fn new(db_path: &Path, worktree_root: &Path) -> Result<Self, GraphError> {
        let lock = DbLockGuard::acquire(db_path)?;
        Ok(Self::new_with_lock(db_path, worktree_root, Some(lock)))
    }

    fn new_with_lock(db_path: &Path, worktree_root: &Path, lock: Option<DbLockGuard>) -> Self {
        Self {
            db_path: db_path.to_path_buf(),
            worktree_root: worktree_root.to_path_buf(),
            _lock: lock,
            registry: ExtractorRegistry::default(),
        }
    }

    fn scan(&self, mode: SyncMode, hasher: &dyn ContentHasher) -> Result<Diff, GraphError> {
        note_scan_started(self.worktree_root.as_path());

        let conn = Connection::open(self.db_path.as_path())
            .map_err(|source| GraphError::sqlite("open graph database for scan", source))?;
        let mut rows = load_file_rows(&conn)?;
        let orbitignore = OrbitIgnoreMatcher::load(self.worktree_root.as_path())?;
        let mut walk = Walk::default();
        walk_dir(
            self.worktree_root.as_path(),
            self.worktree_root.as_path(),
            &orbitignore,
            &self.registry,
            &mut walk,
        );
        let Walk {
            files: mut disk_files,
            unreadable,
        } = walk;
        disk_files.sort_by(|left, right| left.path.cmp(&right.path));

        let ignored = git_ignored_paths(
            self.worktree_root.as_path(),
            disk_files
                .iter()
                .map(|file| file.path.as_path())
                .chain(unreadable.iter().map(|entry| entry.path.as_path())),
        )?;
        let mut diff = Diff::default();
        let mut seen = HashSet::new();
        // An unreadable path Git ignores would not be indexed anyway.
        let unreadable = unreadable
            .into_iter()
            .filter(|entry| !ignored.contains(&entry.path))
            .collect::<Vec<_>>();

        for disk_file in disk_files {
            if ignored.contains(&disk_file.path) {
                continue;
            }
            if disk_file.byte_len > MAX_FILE_BYTES {
                skip_oversize(&mut diff, disk_file.path, disk_file.byte_len);
                continue;
            }

            let existing = rows.remove(&disk_file.path);
            if let Some(existing) = existing.as_ref()
                && mode == SyncMode::Auto
                && existing.mtime_ns == disk_file.mtime_ns
            {
                seen.insert(disk_file.path.clone());
                diff.unchanged.push(disk_file.path);
                continue;
            }

            let content_hash =
                match hash_file(self.worktree_root.as_path(), &disk_file.path, hasher) {
                    Ok(Some(content_hash)) => content_hash,
                    Ok(None) => {
                        // The file grew past the cap after the walk measured it.
                        skip_oversize(&mut diff, disk_file.path, MAX_FILE_BYTES + 1);
                        continue;
                    }
                    Err(error) => {
                        // Keep what is indexed for it: its content is unknown.
                        diff.failed.push(io_failure(
                            &disk_file.path,
                            "read file for content hash",
                            &error,
                        ));
                        seen.insert(disk_file.path);
                        continue;
                    }
                };
            seen.insert(disk_file.path.clone());
            let Some(existing) = existing else {
                diff.new.push(disk_file.path);
                continue;
            };
            if mode == SyncMode::Auto && content_hash == existing.content_hash {
                touch_mtime(&conn, &disk_file.path, disk_file.mtime_ns)?;
                diff.unchanged.push(disk_file.path);
            } else {
                diff.modified.push(disk_file.path);
            }
        }

        diff.deleted.extend(rows.into_keys().filter(|path| {
            !seen.contains(path)
                && !unreadable
                    .iter()
                    .any(|entry| path.starts_with(entry.path.as_path()))
        }));
        diff.failed
            .extend(unreadable.into_iter().map(|entry| entry.failure));
        sort_diff(&mut diff);
        Ok(diff)
    }
}

pub(crate) struct DbLockGuard {
    _lock: FileLockGuard,
}

impl DbLockGuard {
    /// Takes the graph database's sync lock, waiting at most the configured
    /// [`lock::lock_timeout`].
    pub(crate) fn acquire(db_path: &Path) -> Result<Self, GraphError> {
        Self::acquire_within(db_path, lock::lock_timeout()?)
    }

    pub(crate) fn acquire_within(
        db_path: &Path,
        timeout: std::time::Duration,
    ) -> Result<Self, GraphError> {
        // L-0048: lock a sidecar so SQLite can still read the DB while the RAII guard is held.
        let lock_path = lock_path_for(db_path);
        let lock = FileLockGuard::acquire(
            lock_path.as_path(),
            &lock::holder_label("graph sync"),
            timeout,
            "lock graph database",
        )?;
        Ok(Self { _lock: lock })
    }
}

fn lock_path_for(db_path: &Path) -> PathBuf {
    let mut lock_path = db_path.to_path_buf();
    let file_name = db_path
        .file_name()
        .and_then(|name| name.to_str())
        .map_or_else(|| "graph.db".to_string(), ToString::to_string);
    lock_path.set_file_name(format!("{file_name}.lock"));
    lock_path
}

struct ExtractorRegistry {
    extractors: Vec<Box<dyn Extractor>>,
}

impl Default for ExtractorRegistry {
    fn default() -> Self {
        Self {
            extractors: languages::extractors(),
        }
    }
}

impl ExtractorRegistry {
    fn language_for(&self, path: &Path) -> Option<&'static str> {
        self.extractors
            .iter()
            .find(|extractor| extractor.supports(path))
            .map(|extractor| extractor.lang())
    }
}

trait ContentHasher {
    fn hash(&self, path: &Path, bytes: &[u8]) -> Vec<u8>;
}

struct Blake3Hasher;

impl ContentHasher for Blake3Hasher {
    fn hash(&self, _path: &Path, bytes: &[u8]) -> Vec<u8> {
        blake3::hash(bytes).as_bytes().to_vec()
    }
}

#[derive(Debug)]
struct FileRow {
    content_hash: Vec<u8>,
    mtime_ns: i64,
}

#[derive(Debug)]
struct DiskFile {
    path: PathBuf,
    mtime_ns: i64,
    byte_len: u64,
}

/// What a worktree walk found.
#[derive(Debug, Default)]
struct Walk {
    files: Vec<DiskFile>,
    unreadable: Vec<Unreadable>,
}

/// A directory or file the walk could not read. Everything at or under
/// `path` is unknown.
#[derive(Debug)]
struct Unreadable {
    /// Worktree-relative path; empty for the worktree root itself.
    path: PathBuf,
    failure: SyncFailure,
}

impl Walk {
    fn unreadable(&mut self, root: &Path, path: &Path, operation: &str, error: &std::io::Error) {
        let rel = path.strip_prefix(root).unwrap_or(path).to_path_buf();
        let failure = io_failure(&rel, operation, error);
        self.unreadable.push(Unreadable { path: rel, failure });
    }
}

struct OrbitIgnoreMatcher {
    gitignore: Gitignore,
}

impl OrbitIgnoreMatcher {
    fn load(repo_path: &Path) -> Result<Self, GraphError> {
        let mut builder = GitignoreBuilder::new(repo_path);
        add_default_orbitignore_patterns(&mut builder)?;
        let default_orbitignore = Self {
            gitignore: builder.build().map_err(|error| {
                GraphError::invalid_data("build default .orbitignore matcher", error.to_string())
            })?,
        };

        let mut orbitignore_files = Vec::new();
        collect_orbitignore_files(
            repo_path,
            repo_path,
            &default_orbitignore,
            &mut orbitignore_files,
        )?;
        orbitignore_files.sort_by(|left, right| {
            let left_rel = left.strip_prefix(repo_path).unwrap_or(left.as_path());
            let right_rel = right.strip_prefix(repo_path).unwrap_or(right.as_path());
            left_rel
                .components()
                .count()
                .cmp(&right_rel.components().count())
                .then_with(|| left_rel.cmp(right_rel))
        });

        for orbitignore in orbitignore_files {
            if let Some(error) = builder.add(&orbitignore) {
                return Err(GraphError::invalid_data(
                    "load .orbitignore",
                    format!("load {}: {error}", orbitignore.display()),
                ));
            }
        }

        let gitignore = builder.build().map_err(|error| {
            GraphError::invalid_data("build .orbitignore matcher", error.to_string())
        })?;
        Ok(Self { gitignore })
    }

    fn is_ignored(&self, rel_path: &Path, is_dir: bool) -> bool {
        self.gitignore
            .matched_path_or_any_parents(rel_path, is_dir)
            .is_ignore()
    }
}

fn add_default_orbitignore_patterns(builder: &mut GitignoreBuilder) -> Result<(), GraphError> {
    for pattern in DEFAULT_ORBITIGNORE_PATTERNS {
        builder.add_line(None, pattern).map_err(|error| {
            GraphError::invalid_data(
                "load default .orbitignore patterns",
                format!("invalid default .orbitignore pattern `{pattern}`: {error}"),
            )
        })?;
    }
    Ok(())
}

fn load_file_rows(conn: &Connection) -> Result<BTreeMap<PathBuf, FileRow>, GraphError> {
    let mut stmt = conn
        .prepare("SELECT path, content_hash, mtime_ns FROM files")
        .map_err(|source| GraphError::sqlite("prepare files scan query", source))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                PathBuf::from(row.get::<_, String>(0)?),
                FileRow {
                    content_hash: row.get(1)?,
                    mtime_ns: row.get(2)?,
                },
            ))
        })
        .map_err(|source| GraphError::sqlite("query files for scan", source))?;

    rows.collect::<Result<BTreeMap<_, _>, _>>()
        .map_err(|source| GraphError::sqlite("collect files scan rows", source))
}

fn touch_mtime(conn: &Connection, path: &Path, mtime_ns: i64) -> Result<(), GraphError> {
    conn.execute(
        "UPDATE files SET mtime_ns = ?1 WHERE path = ?2",
        params![mtime_ns, normalize_path(path)],
    )
    .map_err(|source| GraphError::sqlite("update unchanged file mtime", source))?;
    Ok(())
}

/// Walks `dir`, recording each directory or file it cannot read in
/// `out.unreadable` and carrying on with the rest (`STD-02 §R32`).
fn walk_dir(
    root: &Path,
    dir: &Path,
    orbitignore: &OrbitIgnoreMatcher,
    registry: &ExtractorRegistry,
    out: &mut Walk,
) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            out.unreadable(root, dir, "scan directory", &error);
            return;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                // The rest of the directory cannot be listed.
                out.unreadable(root, dir, "read directory entry", &error);
                return;
            }
        };
        let path = entry.path();
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                out.unreadable(root, path.as_path(), "read file type", &error);
                continue;
            }
        };

        if file_type.is_dir() {
            if name.starts_with('.') {
                continue;
            }
            if let Ok(rel) = path.strip_prefix(root)
                && orbitignore.is_ignored(rel, true)
            {
                continue;
            }
            walk_dir(root, path.as_path(), orbitignore, registry, out);
        } else if file_type.is_file() {
            if name.as_ref() == ORBITIGNORE_FILE_NAME {
                continue;
            }
            if name.starts_with('.') {
                continue;
            }
            if let Some(ext) = path.extension().and_then(|ext| ext.to_str())
                && SKIP_EXTENSIONS.contains(&ext)
            {
                continue;
            }
            if let Ok(rel) = path.strip_prefix(root) {
                if orbitignore.is_ignored(rel, false) || registry.language_for(rel).is_none() {
                    continue;
                }
                let metadata = match fs::metadata(path.as_path()) {
                    Ok(metadata) => metadata,
                    Err(error) => {
                        out.unreadable(root, path.as_path(), "read file metadata", &error);
                        continue;
                    }
                };
                let mtime_ns = match metadata_mtime_ns(path.as_path(), &metadata) {
                    Ok(mtime_ns) => mtime_ns,
                    Err(error) => {
                        out.unreadable.push(Unreadable {
                            path: rel.to_path_buf(),
                            failure: graph_failure(rel, &error),
                        });
                        continue;
                    }
                };
                out.files.push(DiskFile {
                    path: rel.to_path_buf(),
                    mtime_ns,
                    byte_len: metadata.len(),
                });
            }
        }
    }
}

fn collect_orbitignore_files(
    root: &Path,
    dir: &Path,
    default_orbitignore: &OrbitIgnoreMatcher,
    out: &mut Vec<PathBuf>,
) -> Result<(), GraphError> {
    // A directory it cannot read is skipped here; the worktree walk reports
    // it once as a sync failure.
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(());
    };

    for entry in entries {
        let Ok(entry) = entry else {
            break;
        };
        let path = entry.path();
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };

        if file_type.is_dir() {
            if let Ok(rel) = path.strip_prefix(root)
                && default_orbitignore.is_ignored(rel, true)
            {
                continue;
            }
            if name.starts_with('.') {
                continue;
            }
            collect_orbitignore_files(root, path.as_path(), default_orbitignore, out)?;
        } else if file_type.is_file() && name.as_ref() == ORBITIGNORE_FILE_NAME {
            let relative = path.strip_prefix(root).map_err(|error| {
                GraphError::invalid_data("strip .orbitignore prefix", error.to_string())
            })?;
            out.push(root.join(relative));
        }
    }

    Ok(())
}

/// Paths Git would ignore, matched in process with libgit2.
///
/// Matching follows `git check-ignore` without `--no-index`: nested
/// `.gitignore` files, `info/exclude` and the configured excludes file apply,
/// a file inside an ignored directory is ignored, and a tracked file is never
/// ignored. No Git process is started, so repository-configured programs such
/// as `core.fsmonitor` or hooks never run (`STD-05 §R10`).
fn git_ignored_paths<'a>(
    worktree_root: &Path,
    paths: impl IntoIterator<Item = &'a Path>,
) -> Result<HashSet<PathBuf>, GraphError> {
    let mut ignored = HashSet::new();
    let mut paths = paths.into_iter().peekable();
    if paths.peek().is_none() {
        return Ok(ignored);
    }
    // A directory outside a Git repository has no Git ignore rules to apply.
    let Ok(repo) = Repository::discover(worktree_root) else {
        return Ok(ignored);
    };
    let Some(workdir) = repo.workdir() else {
        return Ok(ignored);
    };
    let prefix = worktree_prefix(workdir, worktree_root)?;
    let index = repo
        .index()
        .map_err(|error| git_ignore_error("read the Git index", &error))?;

    for path in paths {
        let repo_path = prefix.join(path);
        if index.get_path(repo_path.as_path(), 0).is_some() {
            continue;
        }
        if repo
            .is_path_ignored(repo_path.as_path())
            .map_err(|error| git_ignore_error("match Git ignore rules", &error))?
        {
            ignored.insert(path.to_path_buf());
        }
    }

    Ok(ignored)
}

/// Where `worktree_root` sits inside the repository's working directory.
fn worktree_prefix(workdir: &Path, worktree_root: &Path) -> Result<PathBuf, GraphError> {
    let canonical = |path: &Path| {
        fs::canonicalize(path)
            .map_err(|source| GraphError::io("resolve Git ignore root", path, source))
    };
    let workdir = canonical(workdir)?;
    let root = canonical(worktree_root)?;
    root.strip_prefix(workdir.as_path())
        .map(Path::to_path_buf)
        .map_err(|_| {
            GraphError::invalid_data(
                "match Git ignore rules",
                format!(
                    "{} is outside the Git working directory {}",
                    root.display(),
                    workdir.display()
                ),
            )
        })
}

fn git_ignore_error(operation: &str, error: &git2::Error) -> GraphError {
    GraphError::invalid_data("match Git ignore rules", format!("{operation}: {error}"))
}

fn skip_oversize(diff: &mut Diff, path: PathBuf, byte_len: u64) {
    tracing::warn!(
        path = %path.display(),
        byte_len,
        max_bytes = MAX_FILE_BYTES,
        "skipping file larger than the graph sync byte cap"
    );
    diff.oversize.push(path);
}

/// Reads `path` whole, or returns `None` when it exceeds [`MAX_FILE_BYTES`].
/// Never reads more than one byte past the cap.
pub(crate) fn read_capped(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_FILE_BYTES {
        return Ok(None);
    }
    Ok(Some(bytes))
}

/// Hashes a file, or returns `None` when it exceeds [`MAX_FILE_BYTES`].
fn hash_file(
    root: &Path,
    rel_path: &Path,
    hasher: &dyn ContentHasher,
) -> std::io::Result<Option<Vec<u8>>> {
    let bytes = read_capped(root.join(rel_path).as_path())?;
    Ok(bytes.map(|bytes| hasher.hash(rel_path, &bytes)))
}

pub(crate) fn mtime_ns(path: &Path) -> Result<i64, GraphError> {
    let metadata =
        fs::metadata(path).map_err(|source| GraphError::io("read file mtime", path, source))?;
    metadata_mtime_ns(path, &metadata)
}

fn metadata_mtime_ns(path: &Path, metadata: &fs::Metadata) -> Result<i64, GraphError> {
    let modified = metadata
        .modified()
        .map_err(|source| GraphError::io("read file mtime", path, source))?;
    let duration = modified.duration_since(UNIX_EPOCH).map_err(|error| {
        GraphError::invalid_data(
            "read file mtime",
            format!("{} is before UNIX_EPOCH: {error}", path.display()),
        )
    })?;
    i64::try_from(duration.as_nanos()).map_err(|error| {
        GraphError::invalid_data(
            "read file mtime",
            format!("{} mtime is out of range: {error}", path.display()),
        )
    })
}

pub(crate) fn normalize_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn sort_diff(diff: &mut Diff) {
    diff.unchanged.sort();
    diff.modified.sort();
    diff.new.sort();
    diff.deleted.sort();
    diff.oversize.sort();
    diff.failed
        .sort_by(|left, right| left.path.cmp(&right.path));
}

#[cfg(test)]
fn note_scan_started(worktree_root: &Path) {
    let mut counts = scan_counts()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *counts.entry(worktree_root.to_path_buf()).or_insert(0) += 1;
}

#[cfg(not(test))]
fn note_scan_started(_worktree_root: &Path) {}

#[cfg(test)]
pub(crate) fn scan_count(worktree_root: &Path) -> usize {
    scan_counts()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(worktree_root)
        .copied()
        .unwrap_or(0)
}

#[cfg(test)]
fn scan_counts() -> &'static std::sync::Mutex<BTreeMap<PathBuf, usize>> {
    static SCAN_COUNTS: std::sync::OnceLock<std::sync::Mutex<BTreeMap<PathBuf, usize>>> =
        std::sync::OnceLock::new();
    SCAN_COUNTS.get_or_init(|| std::sync::Mutex::new(BTreeMap::new()))
}

#[cfg(test)]
#[path = "tests/scanner.rs"]
mod tests;
