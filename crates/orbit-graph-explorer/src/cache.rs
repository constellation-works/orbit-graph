//! On-disk cache of materialized snapshot trees and their indexes.
//!
//! A cache entry holds exactly one commit: the materialized tree and the graph
//! database built from it. An entry is addressed by commit SHA and keyed by
//! `(commit SHA, EXTRACTOR_VERSION, STORE_SCHEMA_VERSION)`. A key that does not
//! match the running binary is discarded and rebuilt — never reused with a
//! warning — because an index produced by a different extractor or store schema
//! is not the same evidence.
//!
//! Layout, under `<repo>/.orbit-graph/explorer/snapshots/` by default:
//!
//! ```text
//! <cache-dir>/<commit-sha>/entry.json   the key plus what the build observed
//! <cache-dir>/<commit-sha>/tree/        the materialized commit tree
//! <cache-dir>/<commit-sha>/index/*.db   the graph database for that tree
//! ```
//!
//! Two rules this module never breaks:
//!
//! - **Nothing outside the cache directory is ever removed.** Every deletion is
//!   a path this module built inside its own directory, and the entry directory
//!   name must be a full commit SHA (or this module's own staging prefix)
//!   before it is considered removable at all.
//! - **The user repository's own `.orbit-graph/*.db` files are never read or
//!   written.** The cache lives in its own `explorer/snapshots` subdirectory and
//!   every graph handle is opened with an explicit database path inside it.
//!
//! An entry is published by building into a staging directory and renaming it
//! into place, so a crashed or concurrent build can never leave a half-indexed
//! tree behind under a SHA that a later launch would trust.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use orbit_graph::{EXTRACTOR_VERSION, STORE_SCHEMA_VERSION};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Schema version of the `entry.json` metadata file.
pub const CACHE_SCHEMA_VERSION: u32 = 1;

/// Prefix of a staging directory: a build that has not been published yet.
const STAGING_PREFIX: &str = ".staging-";
const LAST_USED_FILE: &str = "last_used";

/// Default cache directory for `repository`.
pub fn default_cache_dir(repository: &Path) -> PathBuf {
    repository
        .join(".orbit-graph")
        .join("explorer")
        .join("snapshots")
}

/// Failure surface of cache operations.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CacheError {
    /// A filesystem operation inside the cache directory failed.
    #[error("{operation} at {path}: {reason}")]
    Io {
        /// Operation being performed.
        operation: &'static str,
        /// Path involved in the failed operation.
        path: PathBuf,
        /// Failure reason.
        reason: String,
    },
    /// An entry's metadata could not be written or parsed.
    #[error("{operation} at {path}: {reason}")]
    Metadata {
        /// Operation being performed.
        operation: &'static str,
        /// Metadata file involved.
        path: PathBuf,
        /// Failure reason.
        reason: String,
    },
}

impl CacheError {
    fn io(operation: &'static str, path: &Path, source: &std::io::Error) -> Self {
        Self::Io {
            operation,
            path: path.to_path_buf(),
            reason: source.to_string(),
        }
    }
}

/// Whether a snapshot was served from the cache or built for this launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheOutcome {
    /// A valid entry for this key already existed and was opened as it stood.
    Hit,
    /// No valid entry existed, so the tree was materialized and indexed now.
    Miss,
    /// The cache directory was unusable, so a task-owned temporary tree was
    /// used instead. Nothing was reused and nothing was written to the cache.
    Disabled,
}

impl CacheOutcome {
    /// Stable label used in payloads and reports.
    pub fn label(self) -> &'static str {
        match self {
            Self::Hit => "hit",
            Self::Miss => "miss",
            Self::Disabled => "disabled",
        }
    }
}

/// Identity of the index format a snapshot was built with.
///
/// Together these two numbers decide whether a cached index is the same
/// evidence the running binary would produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexIdentity {
    /// `orbit_graph::EXTRACTOR_VERSION` the entry was built with.
    pub extractor_version: u32,
    /// `orbit_graph::STORE_SCHEMA_VERSION` the entry was built with.
    pub store_schema_version: u32,
}

impl IndexIdentity {
    /// Identity of the running binary.
    pub fn current() -> Self {
        Self {
            extractor_version: EXTRACTOR_VERSION,
            store_schema_version: STORE_SCHEMA_VERSION,
        }
    }
}

/// One tree entry a build deliberately did not materialize.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredExclusion {
    /// Slash-separated path inside the commit tree.
    pub path: String,
    /// Stable exclusion-reason label.
    pub reason: String,
    /// Blob size, for an oversize blob.
    #[serde(default)]
    pub bytes: Option<u64>,
}

/// What a cached build wrote, excluded, and indexed.
///
/// Persisted with the entry: a warm launch never re-materializes the tree, so
/// without this the report it serves would lose its exclusions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredBuild {
    /// Blobs written into the tree.
    pub files_written: usize,
    /// Bytes written into the tree.
    pub bytes_written: u64,
    /// Entries deliberately not materialized.
    pub excluded: Vec<StoredExclusion>,
    /// Files the extractor indexed.
    pub files_indexed: usize,
    /// Best-effort, extension-derived languages seen while indexing.
    ///
    /// Absent from entries published before this field existed; those
    /// deserialize to an empty list rather than failing to load.
    #[serde(default)]
    pub languages: Vec<String>,
}

/// The on-disk `entry.json` document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryMetadata {
    /// Schema version of this document.
    pub schema_version: u32,
    /// Commit SHA this entry indexes.
    pub commit_sha: String,
    /// Index identity the entry was built with.
    #[serde(flatten)]
    pub identity: IndexIdentity,
    /// What the build observed.
    pub build: StoredBuild,
    /// Unix seconds the entry was published at, for diagnostics only.
    #[serde(default)]
    pub published_at: u64,
}

impl EntryMetadata {
    /// Whether this entry's key matches `commit_sha` and the running binary.
    pub fn matches(&self, commit_sha: &str) -> bool {
        self.schema_version == CACHE_SCHEMA_VERSION
            && self.commit_sha == commit_sha
            && self.identity == IndexIdentity::current()
    }
}

/// A published cache entry: a materialized tree and its index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedEntry {
    root: PathBuf,
    metadata: EntryMetadata,
}

impl CachedEntry {
    /// Directory holding this entry.
    pub fn root(&self) -> &Path {
        self.root.as_path()
    }

    /// Materialized tree of the commit.
    pub fn tree(&self) -> PathBuf {
        self.root.join("tree")
    }

    /// Graph database built from that tree.
    pub fn db(&self) -> PathBuf {
        db_path_in(self.root.as_path(), self.metadata.commit_sha.as_str())
    }

    /// The entry's key and recorded build.
    pub fn metadata(&self) -> &EntryMetadata {
        &self.metadata
    }
}

/// A build in progress, not yet visible to any other launch.
#[derive(Debug)]
pub struct StagedEntry {
    root: PathBuf,
    commit_sha: String,
}

impl StagedEntry {
    /// Tree directory to materialize into.
    pub fn tree(&self) -> PathBuf {
        self.root.join("tree")
    }

    /// Database path to index into.
    pub fn db(&self) -> PathBuf {
        db_path_in(self.root.as_path(), self.commit_sha.as_str())
    }
}

/// Database file name inside an entry directory.
///
/// The name carries the detached-commit spelling `orbit_graph` uses for
/// synthetic trees plus the index identity, so a file left behind by another
/// version is visibly not the file this binary would write.
fn db_path_in(root: &Path, commit_sha: &str) -> PathBuf {
    let short: String = commit_sha.chars().take(12).collect();
    let identity = IndexIdentity::current();
    root.join("index").join(format!(
        "detached-{short}.{}.{}.db",
        identity.extractor_version, identity.store_schema_version
    ))
}

/// A snapshot cache rooted at one directory.
#[derive(Debug, Clone)]
pub struct SnapshotCache {
    dir: PathBuf,
}

impl SnapshotCache {
    /// Open, creating the directory when it does not exist.
    pub fn open(dir: &Path) -> Result<Self, CacheError> {
        fs::create_dir_all(dir)
            .map_err(|source| CacheError::io("create snapshot cache directory", dir, &source))?;
        let dir = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        Ok(Self { dir })
    }

    /// Directory holding this cache.
    pub fn dir(&self) -> &Path {
        self.dir.as_path()
    }

    /// Return the entry for `commit_sha` when its key matches this binary.
    ///
    /// A mismatched, unreadable, or incomplete entry is removed and reported as
    /// absent, so the caller rebuilds it rather than serving another version's
    /// evidence.
    pub fn lookup(&self, commit_sha: &str) -> Result<Option<CachedEntry>, CacheError> {
        let root = self.entry_root(commit_sha)?;
        if !root.exists() {
            return Ok(None);
        }
        let metadata = match read_metadata(root.as_path()) {
            Ok(Some(metadata)) if metadata.matches(commit_sha) => metadata,
            Ok(_) | Err(_) => {
                self.remove_entry(root.as_path())?;
                return Ok(None);
            }
        };
        let entry = CachedEntry {
            root: root.clone(),
            metadata,
        };
        if !entry.tree().is_dir() || !entry.db().is_file() {
            self.remove_entry(root.as_path())?;
            return Ok(None);
        }
        write_last_used(root.as_path(), now_seconds())?;
        Ok(Some(entry))
    }

    /// Create a staging directory for a fresh build of `commit_sha`.
    pub fn stage(&self, commit_sha: &str) -> Result<StagedEntry, CacheError> {
        // The clock alone is not unique: two workers in one process (a
        // comparison whose base and head coincide) can stage the same commit
        // within one timer tick, and the second would wipe the first's
        // half-built tree. A process-wide counter keeps every name distinct.
        static STAGE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let name = format!(
            "{STAGING_PREFIX}{commit_sha}-{}-{}-{}",
            std::process::id(),
            now_nanos(),
            STAGE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let root = self.dir.join(name);
        if root.exists() {
            remove_dir_all_within(self.dir.as_path(), root.as_path())?;
        }
        fs::create_dir_all(root.join("tree")).map_err(|source| {
            CacheError::io("create staged snapshot tree", root.as_path(), &source)
        })?;
        fs::create_dir_all(root.join("index")).map_err(|source| {
            CacheError::io("create staged snapshot index", root.as_path(), &source)
        })?;
        Ok(StagedEntry {
            root,
            commit_sha: commit_sha.to_string(),
        })
    }

    /// Publish a staged build, or adopt an equivalent entry another process
    /// published first.
    ///
    /// The staged directory is renamed into place, so no partially indexed tree
    /// is ever visible under a commit SHA.
    pub fn publish(
        &self,
        staged: StagedEntry,
        build: StoredBuild,
    ) -> Result<CachedEntry, CacheError> {
        let metadata = EntryMetadata {
            schema_version: CACHE_SCHEMA_VERSION,
            commit_sha: staged.commit_sha.clone(),
            identity: IndexIdentity::current(),
            build,
            published_at: now_seconds(),
        };
        write_metadata(staged.root.as_path(), &metadata)?;
        write_last_used(staged.root.as_path(), now_seconds())?;

        let destination = self.entry_root(staged.commit_sha.as_str())?;
        if destination.exists() {
            return self.adopt_published(staged, destination);
        }
        match fs::rename(staged.root.as_path(), destination.as_path()) {
            Ok(()) => Ok(CachedEntry {
                root: destination,
                metadata,
            }),
            // The existence check above and the rename are not atomic: a
            // concurrent worker for the same commit (a comparison whose base
            // and head coincide builds both sides at once) can publish in
            // between, and the rename then fails because the destination is a
            // non-empty directory. Treat that exactly like finding the entry
            // already published.
            Err(_) if destination.exists() => self.adopt_published(staged, destination),
            Err(source) => Err(CacheError::io(
                "publish snapshot cache entry",
                destination.as_path(),
                &source,
            )),
        }
    }

    /// Another launch published this commit while this one was building. Its
    /// entry carries the same key, so the staged build is discarded rather
    /// than racing a rename over a directory another process may already have
    /// open.
    fn adopt_published(
        &self,
        staged: StagedEntry,
        destination: PathBuf,
    ) -> Result<CachedEntry, CacheError> {
        self.remove_entry(staged.root.as_path())?;
        if let Some(existing) = self.lookup(staged.commit_sha.as_str())? {
            return Ok(existing);
        }
        Err(CacheError::Io {
            operation: "publish snapshot cache entry",
            path: destination,
            reason: "an entry appeared and then failed its own key check".to_string(),
        })
    }

    /// Discard a staged build without publishing it.
    pub fn discard(&self, staged: StagedEntry) -> Result<(), CacheError> {
        self.remove_entry(staged.root.as_path())
    }

    /// Remove entries with a stale key and entries whose commit `is_live`
    /// rejects, leaving everything else in place.
    pub fn clean(&self, is_live: &dyn Fn(&str) -> bool) -> Result<CleanReport, CacheError> {
        self.clean_with_options(is_live, &CleanOptions::default())
    }

    /// Remove entries according to `options`, after applying the base key and
    /// reachability checks. Existing cleanup reasons always take precedence
    /// over retention-policy reasons.
    pub fn clean_with_options(
        &self,
        is_live: &dyn Fn(&str) -> bool,
        options: &CleanOptions,
    ) -> Result<CleanReport, CacheError> {
        let mut report = CleanReport {
            cache_dir: self.dir.clone(),
            removed: Vec::new(),
            kept: Vec::new(),
        };
        let mut current = Vec::new();
        let entries = fs::read_dir(self.dir.as_path()).map_err(|source| {
            CacheError::io("read snapshot cache directory", self.dir.as_path(), &source)
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| {
                CacheError::io("read snapshot cache entry", self.dir.as_path(), &source)
            })?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();

            if name.starts_with(STAGING_PREFIX) {
                let size_bytes = entry_size(path.as_path())?;
                self.remove_entry(path.as_path())?;
                report.removed.push(CleanedEntry {
                    path,
                    reason: CleanReason::AbandonedStaging,
                    size_bytes,
                });
                continue;
            }
            if !is_commit_sha(name.as_str()) {
                // Not something this module created, so it is never removed.
                let size_bytes = entry_size(path.as_path())?;
                report.kept.push(CleanedEntry {
                    path,
                    reason: CleanReason::Unrecognized,
                    size_bytes,
                });
                continue;
            }
            let stale_key = match read_metadata(path.as_path()) {
                Ok(Some(metadata)) => !metadata.matches(name.as_str()),
                Ok(None) | Err(_) => true,
            };
            if stale_key {
                let size_bytes = entry_size(path.as_path())?;
                self.remove_entry(path.as_path())?;
                report.removed.push(CleanedEntry {
                    path,
                    reason: CleanReason::StaleKey,
                    size_bytes,
                });
                continue;
            }
            if !is_live(name.as_str()) {
                let size_bytes = entry_size(path.as_path())?;
                self.remove_entry(path.as_path())?;
                report.removed.push(CleanedEntry {
                    path,
                    reason: CleanReason::UnreferencedCommit,
                    size_bytes,
                });
                continue;
            }
            let size_bytes = entry_size(path.as_path())?;
            current.push(CleanedEntry {
                path,
                reason: CleanReason::Current,
                size_bytes,
            });
        }

        if options.older_than.is_none() && options.keep.is_none() {
            report.kept.extend(current);
            report
                .removed
                .sort_by(|left, right| left.path.cmp(&right.path));
            report
                .kept
                .sort_by(|left, right| left.path.cmp(&right.path));
            return Ok(report);
        }

        let now = options.now_seconds.unwrap_or_else(now_seconds);
        let older_than = options.older_than.as_ref().map(Duration::as_secs);
        let mut retained = Vec::new();
        for entry in current {
            let last_used = read_last_used(entry.path.as_path())?.unwrap_or_else(|| {
                read_metadata(entry.path.as_path())
                    .ok()
                    .flatten()
                    .map_or(0, |metadata| metadata.published_at)
            });
            if older_than.is_some_and(|age| now.saturating_sub(last_used) > age) {
                self.remove_entry(entry.path.as_path())?;
                report.removed.push(CleanedEntry {
                    reason: CleanReason::Expired,
                    ..entry
                });
            } else {
                retained.push((last_used, entry));
            }
        }
        if let Some(keep) = options.keep {
            retained.sort_by(|left, right| {
                right
                    .0
                    .cmp(&left.0)
                    .then_with(|| left.1.path.cmp(&right.1.path))
            });
            for (_, entry) in retained.drain(keep.min(retained.len())..) {
                self.remove_entry(entry.path.as_path())?;
                report.removed.push(CleanedEntry {
                    reason: CleanReason::EvictedLru,
                    ..entry
                });
            }
        }
        report
            .kept
            .extend(retained.into_iter().map(|(_, entry)| entry));
        report
            .removed
            .sort_by(|left, right| left.path.cmp(&right.path));
        report
            .kept
            .sort_by(|left, right| left.path.cmp(&right.path));
        Ok(report)
    }

    fn entry_root(&self, commit_sha: &str) -> Result<PathBuf, CacheError> {
        if !is_commit_sha(commit_sha) {
            return Err(CacheError::Io {
                operation: "address snapshot cache entry",
                path: self.dir.clone(),
                reason: format!("`{commit_sha}` is not a full commit SHA"),
            });
        }
        Ok(self.dir.join(commit_sha))
    }

    fn remove_entry(&self, path: &Path) -> Result<(), CacheError> {
        remove_dir_all_within(self.dir.as_path(), path)
    }
}

/// Why `clean` removed or kept a directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CleanReason {
    /// The entry's key does not match this binary, or could not be read.
    StaleKey,
    /// The commit is no longer present in the repository.
    UnreferencedCommit,
    /// A staging directory left behind by an interrupted build.
    AbandonedStaging,
    /// The entry has not been used within the requested retention age.
    Expired,
    /// The entry falls outside the requested most-recently-used count.
    EvictedLru,
    /// The entry is current and was kept.
    Current,
    /// The path was not created by this module and was left alone.
    Unrecognized,
}

impl CleanReason {
    /// Stable label used in reports and user-facing output.
    pub fn label(self) -> &'static str {
        match self {
            Self::StaleKey => "stale_key",
            Self::UnreferencedCommit => "unreferenced_commit",
            Self::AbandonedStaging => "abandoned_staging",
            Self::Expired => "expired",
            Self::EvictedLru => "evicted_lru",
            Self::Current => "current",
            Self::Unrecognized => "unrecognized",
        }
    }
}

/// One directory `clean` considered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanedEntry {
    /// Directory this decision applies to.
    pub path: PathBuf,
    /// Why it was removed or kept.
    pub reason: CleanReason,
    /// Total bytes occupied by this file or directory before clean ran.
    pub size_bytes: u64,
}

/// Optional manual retention policy for [`SnapshotCache::clean_with_options`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CleanOptions {
    /// Remove live entries unused longer than this duration.
    pub older_than: Option<Duration>,
    /// Retain only this many most-recently-used live entries.
    pub keep: Option<usize>,
    /// Test seam for deterministic age calculations; production uses now.
    pub now_seconds: Option<u64>,
}

/// What one `clean` run removed and kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanReport {
    /// Cache directory that was scanned.
    pub cache_dir: PathBuf,
    /// Entries removed, in path order.
    pub removed: Vec<CleanedEntry>,
    /// Entries left in place, in path order.
    pub kept: Vec<CleanedEntry>,
}

impl CleanReport {
    /// Total bytes represented by all entries considered by this clean run.
    pub fn total_size_bytes(&self) -> u64 {
        self.removed
            .iter()
            .chain(self.kept.iter())
            .map(|entry| entry.size_bytes)
            .sum()
    }
}

/// Remove `path`, refusing anything that is not inside `root`.
///
/// The guard is the reason this module can delete at all: a caller-supplied
/// cache directory is the only tree it may touch, so every removal is checked
/// against it rather than trusted.
fn remove_dir_all_within(root: &Path, path: &Path) -> Result<(), CacheError> {
    if !path.starts_with(root) || path == root {
        return Err(CacheError::Io {
            operation: "remove snapshot cache entry",
            path: path.to_path_buf(),
            reason: format!("path is not inside the cache directory {}", root.display()),
        });
    }
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(CacheError::io("remove snapshot cache entry", path, &source)),
    }
}

fn metadata_path(root: &Path) -> PathBuf {
    root.join("entry.json")
}

fn last_used_path(root: &Path) -> PathBuf {
    root.join(LAST_USED_FILE)
}

fn read_last_used(root: &Path) -> Result<Option<u64>, CacheError> {
    let path = last_used_path(root);
    let contents = match fs::read_to_string(path.as_path()) {
        Ok(contents) => contents,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(CacheError::io(
                "read cache last-used time",
                path.as_path(),
                &source,
            ));
        }
    };
    contents
        .trim()
        .parse::<u64>()
        .map(Some)
        .map_err(|source| CacheError::Metadata {
            operation: "parse cache last-used time",
            path,
            reason: source.to_string(),
        })
}

fn write_last_used(root: &Path, seconds: u64) -> Result<(), CacheError> {
    let path = last_used_path(root);
    fs::write(path.as_path(), seconds.to_string())
        .map_err(|source| CacheError::io("write cache last-used time", path.as_path(), &source))
}

fn entry_size(path: &Path) -> Result<u64, CacheError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| CacheError::io("read snapshot cache entry size", path, &source))?;
    if !metadata.is_dir() {
        return Ok(metadata.len());
    }
    let mut total = 0;
    let entries = fs::read_dir(path)
        .map_err(|source| CacheError::io("read snapshot cache entry size", path, &source))?;
    for entry in entries {
        let entry = entry
            .map_err(|source| CacheError::io("read snapshot cache entry size", path, &source))?;
        total += entry_size(entry.path().as_path())?;
    }
    Ok(total)
}

fn read_metadata(root: &Path) -> Result<Option<EntryMetadata>, CacheError> {
    let path = metadata_path(root);
    let bytes = match fs::read(path.as_path()) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(CacheError::io(
                "read snapshot cache entry",
                path.as_path(),
                &source,
            ));
        }
    };
    serde_json::from_slice(bytes.as_slice())
        .map(Some)
        .map_err(|source| CacheError::Metadata {
            operation: "parse snapshot cache entry",
            path,
            reason: source.to_string(),
        })
}

fn write_metadata(root: &Path, metadata: &EntryMetadata) -> Result<(), CacheError> {
    let path = metadata_path(root);
    let bytes = serde_json::to_vec_pretty(metadata).map_err(|source| CacheError::Metadata {
        operation: "serialize snapshot cache entry",
        path: path.clone(),
        reason: source.to_string(),
    })?;
    fs::write(path.as_path(), bytes)
        .map_err(|source| CacheError::io("write snapshot cache entry", path.as_path(), &source))
}

/// Whether `value` is a full 40-character hexadecimal commit SHA.
fn is_commit_sha(value: &str) -> bool {
    value.len() == 40 && value.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    fn sha(byte: char) -> String {
        std::iter::repeat_n(byte, 40).collect()
    }

    fn write_current_entry(cache: &SnapshotCache, commit: &str, last_used: u64) {
        let root = cache.dir().join(commit);
        fs::create_dir_all(root.join("tree")).expect("tree");
        fs::create_dir_all(root.join("index")).expect("index");
        fs::write(root.join("tree").join("source.rs"), b"fn source() {}\n").expect("source");
        write_metadata(
            root.as_path(),
            &EntryMetadata {
                schema_version: CACHE_SCHEMA_VERSION,
                commit_sha: commit.to_string(),
                identity: IndexIdentity::current(),
                build: StoredBuild::default(),
                published_at: last_used,
            },
        )
        .expect("metadata");
        write_last_used(root.as_path(), last_used).expect("last used");
    }

    #[test]
    fn commit_shas_are_recognized_strictly() {
        assert!(is_commit_sha(sha('a').as_str()));
        assert!(!is_commit_sha("abc"));
        assert!(!is_commit_sha(sha('z').as_str()));
        assert!(!is_commit_sha(format!("{}x", sha('a')).as_str()));
    }

    #[test]
    fn the_key_covers_commit_and_index_identity() {
        let metadata = EntryMetadata {
            schema_version: CACHE_SCHEMA_VERSION,
            commit_sha: sha('1'),
            identity: IndexIdentity::current(),
            build: StoredBuild::default(),
            published_at: 0,
        };
        assert!(metadata.matches(sha('1').as_str()));
        assert!(!metadata.matches(sha('2').as_str()));

        let mut stale = metadata.clone();
        stale.identity.extractor_version += 1;
        assert!(!stale.matches(sha('1').as_str()));

        let mut stale_schema = metadata.clone();
        stale_schema.identity.store_schema_version += 1;
        assert!(!stale_schema.matches(sha('1').as_str()));

        let mut stale_document = metadata;
        stale_document.schema_version += 1;
        assert!(!stale_document.matches(sha('1').as_str()));
    }

    #[test]
    fn removal_refuses_paths_outside_the_cache_directory() {
        let dir = TempDir::new().expect("cache directory");
        let outside = dir.path().parent().unwrap_or(dir.path()).join("elsewhere");
        let error = remove_dir_all_within(dir.path(), outside.as_path())
            .expect_err("outside path is refused");
        assert!(
            error.to_string().contains("not inside the cache directory"),
            "{error}"
        );
        let error = remove_dir_all_within(dir.path(), dir.path())
            .expect_err("the cache directory itself is refused");
        assert!(
            error.to_string().contains("not inside the cache directory"),
            "{error}"
        );
    }

    #[test]
    fn a_missing_entry_is_absent_rather_than_an_error() {
        let dir = TempDir::new().expect("cache directory");
        let cache = SnapshotCache::open(dir.path()).expect("open cache");
        assert_eq!(cache.lookup(sha('1').as_str()).expect("lookup"), None);
        assert!(cache.lookup("not-a-sha").is_err());
    }

    #[test]
    fn a_cache_hit_refreshes_the_last_used_time() {
        let dir = TempDir::new().expect("cache directory");
        let cache = SnapshotCache::open(dir.path()).expect("open cache");
        let commit = sha('2');
        let staged = cache.stage(commit.as_str()).expect("stage");
        fs::write(staged.db(), b"index").expect("database");
        let entry = cache
            .publish(staged, StoredBuild::default())
            .expect("publish");
        write_last_used(entry.root(), 0).expect("old last used");

        assert!(cache.lookup(commit.as_str()).expect("lookup").is_some());
        assert!(
            read_last_used(entry.root())
                .expect("read last used")
                .unwrap_or_default()
                > 0,
            "a hit refreshes the sidecar timestamp"
        );
    }

    #[test]
    fn a_stale_key_is_discarded_rather_than_reused() {
        let dir = TempDir::new().expect("cache directory");
        let cache = SnapshotCache::open(dir.path()).expect("open cache");
        let commit = sha('3');
        let root = dir.path().join(commit.as_str());
        fs::create_dir_all(root.join("tree")).expect("tree");
        fs::create_dir_all(root.join("index")).expect("index");
        let mut metadata = EntryMetadata {
            schema_version: CACHE_SCHEMA_VERSION,
            commit_sha: commit.clone(),
            identity: IndexIdentity::current(),
            build: StoredBuild::default(),
            published_at: 0,
        };
        metadata.identity.extractor_version += 1;
        write_metadata(root.as_path(), &metadata).expect("write metadata");

        assert_eq!(cache.lookup(commit.as_str()).expect("lookup"), None);
        assert!(!root.exists(), "a stale entry is removed, never reused");
    }

    #[test]
    fn clean_removes_stale_and_unreferenced_entries_only() {
        let dir = TempDir::new().expect("cache directory");
        let cache = SnapshotCache::open(dir.path()).expect("open cache");

        let live = sha('a');
        let unreferenced = sha('b');
        let stale = sha('c');
        for (commit, identity) in [
            (live.as_str(), IndexIdentity::current()),
            (unreferenced.as_str(), IndexIdentity::current()),
            (
                stale.as_str(),
                IndexIdentity {
                    extractor_version: 0,
                    store_schema_version: 0,
                },
            ),
        ] {
            let root = dir.path().join(commit);
            fs::create_dir_all(root.join("tree")).expect("tree");
            write_metadata(
                root.as_path(),
                &EntryMetadata {
                    schema_version: CACHE_SCHEMA_VERSION,
                    commit_sha: commit.to_string(),
                    identity,
                    build: StoredBuild::default(),
                    published_at: 0,
                },
            )
            .expect("write metadata");
        }
        let foreign = dir.path().join("not-an-entry");
        fs::create_dir_all(foreign.as_path()).expect("foreign directory");
        let staging = dir.path().join(format!("{STAGING_PREFIX}{live}-1-2"));
        fs::create_dir_all(staging.as_path()).expect("staging directory");

        let report = cache
            .clean(&|commit: &str| commit == live)
            .expect("clean the cache");

        let removed: Vec<(String, &'static str)> = report
            .removed
            .iter()
            .map(|entry| {
                (
                    entry
                        .path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    entry.reason.label(),
                )
            })
            .collect();
        assert!(
            removed.contains(&(stale.clone(), "stale_key")),
            "{removed:?}"
        );
        assert!(
            removed.contains(&(unreferenced.clone(), "unreferenced_commit")),
            "{removed:?}"
        );
        assert!(
            removed
                .iter()
                .any(|(name, reason)| name.starts_with(STAGING_PREFIX)
                    && *reason == "abandoned_staging"),
            "{removed:?}"
        );
        assert!(
            dir.path().join(live.as_str()).exists(),
            "a current entry is kept"
        );
        assert!(foreign.exists(), "unrecognized paths are never removed");
    }

    #[test]
    fn clean_applies_age_and_lru_after_existing_reasons() {
        let dir = TempDir::new().expect("cache directory");
        let cache = SnapshotCache::open(dir.path()).expect("open cache");
        let expired = sha('a');
        let newest = sha('b');
        let evicted = sha('c');
        let stale = sha('d');
        let live = [
            expired.as_str(),
            newest.as_str(),
            evicted.as_str(),
            stale.as_str(),
        ];

        write_current_entry(&cache, expired.as_str(), 100);
        write_current_entry(&cache, newest.as_str(), 900);
        write_current_entry(&cache, evicted.as_str(), 800);
        write_current_entry(&cache, stale.as_str(), 1_000);
        let stale_root = cache.dir().join(stale.as_str());
        let mut metadata = read_metadata(stale_root.as_path())
            .expect("metadata")
            .expect("present metadata");
        metadata.identity.extractor_version += 1;
        write_metadata(stale_root.as_path(), &metadata).expect("stale metadata");
        let staging = cache.dir().join(format!("{STAGING_PREFIX}{}-1-2", newest));
        fs::create_dir_all(staging.as_path()).expect("staging");

        let report = cache
            .clean_with_options(
                &|commit| live.contains(&commit),
                &CleanOptions {
                    older_than: Some(Duration::from_secs(200)),
                    keep: Some(1),
                    now_seconds: Some(1_000),
                },
            )
            .expect("clean");
        let reasons: Vec<(String, &'static str)> = report
            .removed
            .iter()
            .map(|entry| {
                (
                    entry
                        .path
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    entry.reason.label(),
                )
            })
            .collect();

        assert!(
            reasons.contains(&(expired.clone(), "expired")),
            "{reasons:?}"
        );
        assert!(
            reasons.contains(&(evicted.clone(), "evicted_lru")),
            "{reasons:?}"
        );
        assert!(
            reasons.contains(&(stale.clone(), "stale_key")),
            "{reasons:?}"
        );
        assert!(
            reasons
                .iter()
                .any(|(name, reason)| name.starts_with(STAGING_PREFIX)
                    && *reason == "abandoned_staging"),
            "{reasons:?}"
        );
        assert!(
            cache.dir().join(newest).exists(),
            "the newest live entry is kept"
        );
        assert!(report.total_size_bytes() > 0, "entries report their sizes");
    }

    #[test]
    fn keep_larger_than_the_live_entry_count_retains_everything() {
        let dir = TempDir::new().expect("cache directory");
        let cache = SnapshotCache::open(dir.path()).expect("open cache");
        let first = sha('e');
        let second = sha('f');
        write_current_entry(&cache, first.as_str(), 100);
        write_current_entry(&cache, second.as_str(), 200);

        let report = cache
            .clean_with_options(
                &|commit| commit == first || commit == second,
                &CleanOptions {
                    older_than: None,
                    keep: Some(10),
                    now_seconds: Some(1_000),
                },
            )
            .expect("clean must not panic when keep exceeds the entry count");

        assert!(report.removed.is_empty(), "{:?}", report.removed);
        assert_eq!(report.kept.len(), 2);
    }

    #[test]
    fn concurrent_publishes_of_the_same_commit_both_succeed() {
        // Two workers (a comparison whose base and head are the same commit)
        // stage independently and race to publish; the loser must adopt the
        // winner's entry instead of failing on the non-empty destination.
        let dir = TempDir::new().expect("cache directory");
        let cache = SnapshotCache::open(dir.path()).expect("open cache");
        let commit = sha('9');
        for _ in 0..20 {
            let staged: Vec<StagedEntry> = (0..2)
                .map(|_| {
                    let staged = cache.stage(commit.as_str()).expect("stage");
                    fs::write(staged.db(), b"index").expect("database");
                    staged
                })
                .collect();
            let barrier = std::sync::Barrier::new(2);
            let roots: Vec<PathBuf> = std::thread::scope(|scope| {
                let workers: Vec<_> = staged
                    .into_iter()
                    .map(|staged| {
                        let (cache, barrier) = (&cache, &barrier);
                        scope.spawn(move || {
                            barrier.wait();
                            cache
                                .publish(staged, StoredBuild::default())
                                .expect("publish must tolerate a concurrent winner")
                                .root()
                                .to_path_buf()
                        })
                    })
                    .collect();
                workers
                    .into_iter()
                    .map(|worker| worker.join().expect("worker"))
                    .collect()
            });
            assert_eq!(roots[0], roots[1]);
            assert_eq!(roots[0], cache.dir().join(commit.as_str()));
            let leftovers: Vec<_> = fs::read_dir(cache.dir())
                .expect("read cache dir")
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(STAGING_PREFIX)
                })
                .collect();
            assert!(leftovers.is_empty(), "{leftovers:?}");
            fs::remove_dir_all(cache.dir().join(commit.as_str())).expect("reset");
        }
    }
}
