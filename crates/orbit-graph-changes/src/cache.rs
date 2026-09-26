//! On-disk cache of materialized snapshot trees and their indexes.
//!
//! A cache entry holds exactly one commit: the materialized tree and the graph
//! database built from it. An entry is addressed by commit SHA and keyed by
//! `(commit SHA, EXTRACTOR_VERSION, STORE_SCHEMA_VERSION)`. A key from an older
//! binary is discarded and rebuilt — never reused with a warning — because an
//! index produced by a different extractor or store schema is not the same
//! evidence. A key from a newer binary belongs to that binary: it is never
//! removed or rewritten (STD-03 §R10), and a launch that finds one indexes that
//! side into a temporary tree instead.
//!
//! Layout, under `<repo>/.orbit-graph/explorer/snapshots/` by default:
//!
//! ```text
//! <cache-dir>/.orbit-graph-changes-cache   marker: this directory is a cache
//! <cache-dir>/<commit-sha>/entry.json       the key plus what the build observed
//! <cache-dir>/<commit-sha>/in_use.lock      shared-locked while a comparison has it open
//! <cache-dir>/<commit-sha>/tree/            the materialized commit tree
//! <cache-dir>/<commit-sha>/index/*.db       the graph database for that tree
//! <cache-dir>/.staging-<sha>-…/building.lock  exclusively locked by its builder
//! ```
//!
//! Rules this module never breaks (STD-03 §R29):
//!
//! - **Nothing outside the cache directory is ever removed.** Every deletion is
//!   a path this module built inside its own directory, and the entry directory
//!   name must be a full commit SHA (or this module's own staging prefix)
//!   before it is considered removable at all.
//! - **Only a marked directory is cleaned.** [`SnapshotCache::open`] writes the
//!   cache-root marker; `clean` refuses a directory without it, so a mistyped
//!   `--cache-dir` never has its 40-hex subdirectories treated as ours.
//! - **Nothing in use is removed.** A builder holds an exclusive lock on its
//!   staging directory's `building.lock`, and every open [`CachedEntry`] holds a
//!   shared lock on its `in_use.lock`. Removal takes the exclusive lock first
//!   and keeps anything it cannot lock.
//! - **Nothing unverified is removed.** An entry whose `entry.json` is missing or
//!   unreadable, whose key is newer than this binary, whose commit Git cannot
//!   confirm gone, or whose age is unknown is kept and reported.
//! - **The user repository's own `.orbit-graph/*.db` files are never read or
//!   written.** The cache lives in its own `explorer/snapshots` subdirectory and
//!   every graph handle is opened with an explicit database path inside it.
//!
//! An entry is published by building into a staging directory and renaming it
//! into place, so a crashed or concurrent build can never leave a half-indexed
//! tree behind under a SHA that a later launch would trust.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
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
/// File whose presence records that [`SnapshotCache::open`] created or adopted
/// the directory as a snapshot cache.
pub const CACHE_MARKER_FILE: &str = ".orbit-graph-changes-cache";
const CACHE_MARKER_CONTENTS: &str = "orbit-graph-changes snapshot cache\n";
/// Exclusively locked by the process building a staging directory.
const BUILDING_LOCK_FILE: &str = "building.lock";
/// Shared-locked by every process that has the entry open.
const IN_USE_LOCK_FILE: &str = "in_use.lock";
/// How long `lookup` waits for a shared lock that a `clean` briefly holds
/// exclusively (STD-03 §R7: every wait is bounded).
const LOOKUP_LOCK_WAIT: Duration = Duration::from_millis(250);

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
    /// An entry occupies the commit's directory, but this binary may neither
    /// use nor remove it: it is newer, unreadable, or in use.
    #[error(
        "snapshot cache entry {path} is kept ({}) and cannot be used by this binary",
        reason.label()
    )]
    Kept {
        /// Entry directory.
        path: PathBuf,
        /// Why it is kept.
        reason: CleanReason,
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
    /// Unix seconds the entry was published at, when recorded. A missing value
    /// stays absent (STD-02 §R16): an entry of unknown age never expires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<u64>,
}

/// How an entry's key compares with the running binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyMatch {
    /// Same commit, document schema and index identity.
    Current,
    /// Same commit, and every version is at or below this binary's.
    Older,
    /// Some version is above this binary's: a newer binary's entry.
    Newer,
    /// The document names another commit than its directory.
    OtherCommit,
}

impl EntryMetadata {
    /// Whether this entry's key matches `commit_sha` and the running binary.
    pub fn matches(&self, commit_sha: &str) -> bool {
        self.key_match(commit_sha) == KeyMatch::Current
    }

    fn key_match(&self, commit_sha: &str) -> KeyMatch {
        if self.commit_sha != commit_sha {
            return KeyMatch::OtherCommit;
        }
        let current = IndexIdentity::current();
        let versions = [
            (self.schema_version, CACHE_SCHEMA_VERSION),
            (self.identity.extractor_version, current.extractor_version),
            (
                self.identity.store_schema_version,
                current.store_schema_version,
            ),
        ];
        if versions.iter().any(|(entry, ours)| entry > ours) {
            KeyMatch::Newer
        } else if versions.iter().all(|(entry, ours)| entry == ours) {
            KeyMatch::Current
        } else {
            KeyMatch::Older
        }
    }
}

/// A published cache entry: a materialized tree and its index.
///
/// Holding one holds a shared lock on the entry's `in_use.lock`, so no `clean`
/// or `lookup` in any process removes it while it is open. Clones share the
/// lock; it is released when the last clone drops.
#[derive(Debug, Clone)]
pub struct CachedEntry {
    root: PathBuf,
    metadata: EntryMetadata,
    _in_use: Arc<fs::File>,
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
///
/// Holds the staging directory's `building.lock` exclusively, so `clean` keeps
/// it, and its `in_use.lock` shared, so the lock carries over into the
/// published entry without a gap.
#[derive(Debug)]
pub struct StagedEntry {
    root: PathBuf,
    commit_sha: String,
    _building: fs::File,
    in_use: fs::File,
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

/// Whether a commit a cache entry indexes is still in the repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Liveness {
    /// The repository has the commit.
    Live,
    /// Git reported the commit not found.
    Gone,
    /// Git could not answer; the entry is kept and this reason reported.
    Unknown(String),
}

/// Whether `clean` reports its plan or carries it out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Plan,
    Apply,
}

/// A snapshot cache rooted at one directory.
#[derive(Debug, Clone)]
pub struct SnapshotCache {
    dir: PathBuf,
}

impl SnapshotCache {
    /// Open, creating the directory when it does not exist, and mark it as a
    /// snapshot cache.
    pub fn open(dir: &Path) -> Result<Self, CacheError> {
        fs::create_dir_all(dir)
            .map_err(|source| CacheError::io("create snapshot cache directory", dir, &source))?;
        let dir = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        write_marker(dir.as_path())?;
        Ok(Self { dir })
    }

    /// Address an existing cache directory without creating or marking
    /// anything, for [`SnapshotCache::clean_with_options`].
    pub fn existing(dir: &Path) -> Self {
        let dir = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        Self { dir }
    }

    /// Directory holding this cache.
    pub fn dir(&self) -> &Path {
        self.dir.as_path()
    }

    /// Return the entry for `commit_sha` when its key matches this binary.
    ///
    /// An older binary's entry, or an incomplete one of ours, is removed and
    /// reported as absent, so the caller rebuilds it; it is removed only while
    /// no other process has it open. An entry this binary may not remove — a
    /// newer binary's, one with a missing or unreadable `entry.json`, or one
    /// another process holds — fails with [`CacheError::Kept`] and is left in
    /// place.
    pub fn lookup(&self, commit_sha: &str) -> Result<Option<CachedEntry>, CacheError> {
        let root = self.entry_root(commit_sha)?;
        if !root.exists() {
            return Ok(None);
        }
        let kept = |reason| CacheError::Kept {
            path: root.clone(),
            reason,
        };
        // Integrity check, fail closed (STD-02 §R31): only a readable key is
        // evidence of what the directory holds.
        let metadata = match read_metadata(root.as_path()) {
            Ok(Some(metadata)) => metadata,
            Ok(None) | Err(_) => return Err(kept(CleanReason::Unreadable)),
        };
        match metadata.key_match(commit_sha) {
            KeyMatch::Current => {}
            KeyMatch::Newer => return Err(kept(CleanReason::NewerIdentity)),
            KeyMatch::OtherCommit => return Err(kept(CleanReason::Unrecognized)),
            KeyMatch::Older => {
                return if self.remove_entry_if_unused(root.as_path())? {
                    Ok(None)
                } else {
                    Err(kept(CleanReason::InUse))
                };
            }
        }
        let Some(in_use) = open_lock(lock_path(root.as_path(), IN_USE_LOCK_FILE), true)? else {
            // The directory was removed since its key was read.
            return Ok(None);
        };
        if !lock_shared_within(&in_use, LOOKUP_LOCK_WAIT).map_err(|source| {
            CacheError::io("lock snapshot cache entry", root.as_path(), &source)
        })? {
            // A `clean` holds it exclusively while removing it.
            return Err(kept(CleanReason::InUse));
        }
        // The entry may have been removed between reading its key and taking
        // the lock; the lock is then on an unlinked file.
        if !metadata_path(root.as_path()).is_file() {
            return Ok(None);
        }
        let entry = CachedEntry {
            root: root.clone(),
            metadata,
            _in_use: Arc::new(in_use),
        };
        if !entry.tree().is_dir() || !entry.db().is_file() {
            drop(entry);
            return if self.remove_entry_if_unused(root.as_path())? {
                Ok(None)
            } else {
                Err(kept(CleanReason::InUse))
            };
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
        fs::create_dir(root.as_path()).map_err(|source| {
            CacheError::io("create staged snapshot entry", root.as_path(), &source)
        })?;
        // Lock before writing anything else, so `clean` sees a held builder
        // lock for as long as there is anything to build.
        let building = create_locked(
            lock_path(root.as_path(), BUILDING_LOCK_FILE),
            LockKind::Exclusive,
        )?;
        let in_use = create_locked(
            lock_path(root.as_path(), IN_USE_LOCK_FILE),
            LockKind::Shared,
        )?;
        fs::create_dir_all(root.join("tree")).map_err(|source| {
            CacheError::io("create staged snapshot tree", root.as_path(), &source)
        })?;
        fs::create_dir_all(root.join("index")).map_err(|source| {
            CacheError::io("create staged snapshot index", root.as_path(), &source)
        })?;
        Ok(StagedEntry {
            root,
            commit_sha: commit_sha.to_string(),
            _building: building,
            in_use,
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
            published_at: Some(now_seconds()),
        };
        write_metadata(staged.root.as_path(), &metadata)?;
        write_last_used(staged.root.as_path(), now_seconds())?;

        let destination = self.entry_root(staged.commit_sha.as_str())?;
        if destination.exists() {
            return self.adopt_published(staged, destination);
        }
        match fs::rename(staged.root.as_path(), destination.as_path()) {
            // The shared in-use lock moved with the directory; the builder
            // lock is released as `staged` drops.
            Ok(()) => Ok(CachedEntry {
                root: destination,
                metadata,
                _in_use: Arc::new(staged.in_use),
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
        let commit_sha = staged.commit_sha.clone();
        self.discard(staged)?;
        if let Some(existing) = self.lookup(commit_sha.as_str())? {
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
        // The builder's own locks are held while its directory is removed.
        remove_dir_all_within(self.dir.as_path(), staged.root.as_path())
    }

    /// Decide which entries `options` removes, and remove them when
    /// `options.apply` is set.
    ///
    /// Without `apply` this is a report: nothing is created, written or
    /// removed, not even a lock file (STD-01 §R5). With it, exactly the
    /// entries the report lists under `would_delete` are removed, each while
    /// this call holds its lock exclusively; an entry that became busy since
    /// is kept instead.
    ///
    /// An entry is removable only on a verified fact: a staging directory
    /// whose builder lock is free, an older binary's key, a commit Git reports
    /// not found, or — with `older_than` or `keep` — a known age outside the
    /// retention policy. Anything else is kept with its reason (STD-03 §R29).
    /// Existing cleanup reasons always take precedence over retention-policy
    /// reasons.
    pub fn clean_with_options(
        &self,
        is_live: &dyn Fn(&str) -> Liveness,
        options: &CleanOptions,
    ) -> Result<CleanReport, CacheError> {
        let mode = if options.apply {
            Mode::Apply
        } else {
            Mode::Plan
        };
        let mut report = CleanReport {
            cache_dir: self.dir.clone(),
            would_delete: Vec::new(),
            kept: Vec::new(),
            applied: options.apply,
        };
        if !self.dir.exists() {
            return Ok(report);
        }
        // Ownership check, fail closed (STD-02 §R31): only a directory this
        // module marked is a cache whose entries it may judge.
        if !self.dir.join(CACHE_MARKER_FILE).is_file() {
            report.kept.push(CleanedEntry::new(
                self.dir.clone(),
                CleanReason::NoCacheMarker,
                0,
            ));
            return Ok(report);
        }

        let mut candidates = Vec::new();
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
            if name == CACHE_MARKER_FILE {
                continue;
            }
            let size_bytes = entry_size(path.as_path())?;
            let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());

            if name.starts_with(STAGING_PREFIX) && is_dir {
                candidates.push((
                    CleanedEntry::new(path, CleanReason::AbandonedStaging, size_bytes),
                    BUILDING_LOCK_FILE,
                ));
                continue;
            }
            if !is_commit_sha(name.as_str()) || !is_dir {
                // Not something this module created, so it is never removed.
                report.kept.push(CleanedEntry::new(
                    path,
                    CleanReason::Unrecognized,
                    size_bytes,
                ));
                continue;
            }
            let metadata = match read_metadata(path.as_path()) {
                Ok(Some(metadata)) => metadata,
                Ok(None) | Err(_) => {
                    report
                        .kept
                        .push(CleanedEntry::new(path, CleanReason::Unreadable, size_bytes));
                    continue;
                }
            };
            let reason = match metadata.key_match(name.as_str()) {
                KeyMatch::Older => {
                    candidates.push((
                        CleanedEntry::new(path, CleanReason::StaleKey, size_bytes),
                        IN_USE_LOCK_FILE,
                    ));
                    continue;
                }
                KeyMatch::Newer => CleanReason::NewerIdentity,
                KeyMatch::OtherCommit => CleanReason::Unrecognized,
                KeyMatch::Current => match is_live(name.as_str()) {
                    Liveness::Gone => {
                        candidates.push((
                            CleanedEntry::new(path, CleanReason::UnreferencedCommit, size_bytes),
                            IN_USE_LOCK_FILE,
                        ));
                        continue;
                    }
                    Liveness::Unknown(detail) => {
                        report.kept.push(CleanedEntry {
                            detail: Some(detail),
                            ..CleanedEntry::new(path, CleanReason::Unverifiable, size_bytes)
                        });
                        continue;
                    }
                    Liveness::Live => {
                        // Age is a side channel of the retention policy: an
                        // unreadable `last_used` falls back to `published_at`,
                        // and an unknown age never expires.
                        let age = read_last_used(path.as_path())
                            .ok()
                            .flatten()
                            .or(metadata.published_at);
                        current.push((
                            age,
                            CleanedEntry::new(path, CleanReason::Current, size_bytes),
                        ));
                        continue;
                    }
                },
            };
            report
                .kept
                .push(CleanedEntry::new(path, reason, size_bytes));
        }

        if options.older_than.is_some() || options.keep.is_some() {
            let now = options.now_seconds.unwrap_or_else(now_seconds);
            let older_than = options.older_than.as_ref().map(Duration::as_secs);
            let mut retained = Vec::new();
            for (age, entry) in current {
                let Some(last_used) = age else {
                    report.kept.push(CleanedEntry {
                        reason: CleanReason::UnknownAge,
                        ..entry
                    });
                    continue;
                };
                if older_than.is_some_and(|limit| now.saturating_sub(last_used) > limit) {
                    candidates.push((
                        CleanedEntry {
                            reason: CleanReason::Expired,
                            ..entry
                        },
                        IN_USE_LOCK_FILE,
                    ));
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
                    candidates.push((
                        CleanedEntry {
                            reason: CleanReason::EvictedLru,
                            ..entry
                        },
                        IN_USE_LOCK_FILE,
                    ));
                }
            }
            report
                .kept
                .extend(retained.into_iter().map(|(_, entry)| entry));
        } else {
            report
                .kept
                .extend(current.into_iter().map(|(_, entry)| entry));
        }

        for (entry, lock_name) in candidates {
            let busy = match mode {
                Mode::Plan => lock_is_held(lock_path(entry.path.as_path(), lock_name))?,
                Mode::Apply => !self.remove_if_unlocked(entry.path.as_path(), lock_name)?,
            };
            if busy {
                let reason = if lock_name == BUILDING_LOCK_FILE {
                    CleanReason::Building
                } else {
                    CleanReason::InUse
                };
                report.kept.push(CleanedEntry { reason, ..entry });
            } else {
                report.would_delete.push(entry);
            }
        }

        report
            .would_delete
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

    /// Remove a published entry unless another process has it open.
    fn remove_entry_if_unused(&self, path: &Path) -> Result<bool, CacheError> {
        self.remove_if_unlocked(path, IN_USE_LOCK_FILE)
    }

    /// Remove `path` while holding its `lock_name` exclusively. Returns
    /// `false`, removing nothing, when another process holds that lock.
    fn remove_if_unlocked(&self, path: &Path, lock_name: &str) -> Result<bool, CacheError> {
        let Some(lock) = open_lock(lock_path(path, lock_name), true)? else {
            // The directory itself is gone: nothing left to remove.
            return Ok(true);
        };
        match lock.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => return Ok(false),
            Err(fs::TryLockError::Error(source)) => {
                return Err(CacheError::io("lock snapshot cache entry", path, &source));
            }
        }
        remove_dir_all_within(self.dir.as_path(), path)?;
        drop(lock);
        Ok(true)
    }
}

/// Why `clean` removed or kept a directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CleanReason {
    /// The entry was built by an older binary: its key is stale.
    StaleKey,
    /// Git reported the entry's commit not found in the repository.
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
    /// A staging directory whose builder still holds its lock.
    Building,
    /// An entry a comparison in some process has open.
    InUse,
    /// An entry whose `entry.json` is missing or cannot be parsed.
    Unreadable,
    /// An entry a newer binary built; it belongs to that binary.
    NewerIdentity,
    /// A retention policy was requested, but the entry records no age.
    UnknownAge,
    /// Git could not say whether the entry's commit still exists.
    Unverifiable,
    /// The directory lacks the cache-root marker, so nothing in it is judged.
    NoCacheMarker,
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
            Self::Building => "building",
            Self::InUse => "in_use",
            Self::Unreadable => "unreadable",
            Self::NewerIdentity => "newer_identity",
            Self::UnknownAge => "unknown_age",
            Self::Unverifiable => "unverifiable",
            Self::NoCacheMarker => "no_cache_marker",
        }
    }
}

/// One directory `clean` considered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanedEntry {
    /// Directory this decision applies to.
    pub path: PathBuf,
    /// Why it would be removed, was removed, or was kept.
    pub reason: CleanReason,
    /// Total bytes occupied by this file or directory before clean ran.
    pub size_bytes: u64,
    /// What could not be verified, for an `unverifiable` entry.
    pub detail: Option<String>,
}

impl CleanedEntry {
    fn new(path: PathBuf, reason: CleanReason, size_bytes: u64) -> Self {
        Self {
            path,
            reason,
            size_bytes,
            detail: None,
        }
    }
}

/// Retention policy and mode for [`SnapshotCache::clean_with_options`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CleanOptions {
    /// Remove live entries unused longer than this duration.
    pub older_than: Option<Duration>,
    /// Retain only this many most-recently-used live entries.
    pub keep: Option<usize>,
    /// Test seam for deterministic age calculations; production uses now.
    pub now_seconds: Option<u64>,
    /// Remove what the report lists. The default only reports.
    pub apply: bool,
}

/// What one `clean` run would remove, or removed, and kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanReport {
    /// Cache directory that was scanned.
    pub cache_dir: PathBuf,
    /// Entries the plan removes, in path order. After an apply, exactly the
    /// entries removed.
    pub would_delete: Vec<CleanedEntry>,
    /// Entries left in place, in path order.
    pub kept: Vec<CleanedEntry>,
    /// Whether `would_delete` was actually removed.
    pub applied: bool,
}

impl CleanReport {
    /// Total bytes represented by all entries considered by this clean run.
    pub fn total_size_bytes(&self) -> u64 {
        self.would_delete
            .iter()
            .chain(self.kept.iter())
            .map(|entry| entry.size_bytes)
            .sum()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LockKind {
    Exclusive,
    Shared,
}

fn lock_path(root: &Path, name: &str) -> PathBuf {
    root.join(name)
}

/// Open a lock file, creating it when `create` is set. `None` means the file
/// does not exist (`create` unset) or its directory is gone.
fn open_lock(path: PathBuf, create: bool) -> Result<Option<fs::File>, CacheError> {
    match fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .truncate(false)
        .open(path.as_path())
    {
        Ok(file) => Ok(Some(file)),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(CacheError::io(
            "open snapshot cache lock",
            path.as_path(),
            &source,
        )),
    }
}

/// Create a new lock file and take it. Another process having created it
/// first means the directory is not this builder's alone, which is an error.
fn create_locked(path: PathBuf, kind: LockKind) -> Result<fs::File, CacheError> {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path.as_path())
        .map_err(|source| CacheError::io("create snapshot cache lock", path.as_path(), &source))?;
    let locked = match kind {
        LockKind::Exclusive => file.try_lock(),
        LockKind::Shared => file.try_lock_shared(),
    };
    match locked {
        Ok(()) => Ok(file),
        Err(fs::TryLockError::WouldBlock) => Err(CacheError::Io {
            operation: "lock staged snapshot entry",
            path,
            reason: "another process holds the lock of a directory this build created".to_string(),
        }),
        Err(fs::TryLockError::Error(source)) => Err(CacheError::io(
            "lock staged snapshot entry",
            path.as_path(),
            &source,
        )),
    }
}

/// Take `file`'s shared lock, polling until `wait` elapses. `false` means an
/// exclusive holder kept it the whole time.
fn lock_shared_within(file: &fs::File, wait: Duration) -> std::io::Result<bool> {
    let deadline = std::time::Instant::now() + wait;
    loop {
        match file.try_lock_shared() {
            Ok(()) => return Ok(true),
            Err(fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(fs::TryLockError::WouldBlock) => return Ok(false),
            Err(fs::TryLockError::Error(source)) => return Err(source),
        }
    }
}

/// Whether another process holds `path`, checked without creating it: a lock
/// file that does not exist is held by no one.
fn lock_is_held(path: PathBuf) -> Result<bool, CacheError> {
    let Some(lock) = open_lock(path.clone(), false)? else {
        return Ok(false);
    };
    match lock.try_lock() {
        Ok(()) => Ok(false),
        Err(fs::TryLockError::WouldBlock) => Ok(true),
        Err(fs::TryLockError::Error(source)) => Err(CacheError::io(
            "check snapshot cache lock",
            path.as_path(),
            &source,
        )),
    }
}

/// Write the cache-root marker unless it is already there.
fn write_marker(dir: &Path) -> Result<(), CacheError> {
    let path = dir.join(CACHE_MARKER_FILE);
    if path.is_file() {
        return Ok(());
    }
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path.as_path())
    {
        Ok(mut file) => std::io::Write::write_all(&mut file, CACHE_MARKER_CONTENTS.as_bytes())
            .map_err(|source| {
                CacheError::io("write snapshot cache marker", path.as_path(), &source)
            }),
        Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(source) => Err(CacheError::io(
            "write snapshot cache marker",
            path.as_path(),
            &source,
        )),
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

    fn live_if(predicate: impl Fn(&str) -> bool) -> impl Fn(&str) -> Liveness {
        move |commit| {
            if predicate(commit) {
                Liveness::Live
            } else {
                Liveness::Gone
            }
        }
    }

    fn apply() -> CleanOptions {
        CleanOptions {
            apply: true,
            ..CleanOptions::default()
        }
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
                published_at: Some(last_used),
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
            published_at: None,
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
        assert!(cache.lookup(sha('1').as_str()).expect("lookup").is_none());
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
            published_at: None,
        };
        metadata.identity.extractor_version -= 1;
        write_metadata(root.as_path(), &metadata).expect("write metadata");

        assert!(cache.lookup(commit.as_str()).expect("lookup").is_none());
        assert!(!root.exists(), "a stale entry is removed, never reused");
    }

    #[test]
    fn lookup_keeps_a_newer_unreadable_or_in_use_entry_in_place() {
        let dir = TempDir::new().expect("cache directory");
        let cache = SnapshotCache::open(dir.path()).expect("open cache");

        let newer = sha('4');
        write_current_entry(&cache, newer.as_str(), 100);
        let newer_root = cache.dir().join(newer.as_str());
        let mut metadata = read_metadata(newer_root.as_path())
            .expect("metadata")
            .expect("present metadata");
        metadata.identity.store_schema_version += 1;
        write_metadata(newer_root.as_path(), &metadata).expect("newer metadata");
        let before = fs::read(metadata_path(newer_root.as_path())).expect("read metadata");
        assert!(matches!(
            cache.lookup(newer.as_str()),
            Err(CacheError::Kept {
                reason: CleanReason::NewerIdentity,
                ..
            })
        ));
        assert_eq!(
            fs::read(metadata_path(newer_root.as_path())).expect("read metadata"),
            before,
            "a newer binary's entry is neither removed nor rewritten"
        );
        assert!(
            !newer_root.join(IN_USE_LOCK_FILE).exists(),
            "nothing is written into a newer binary's entry"
        );

        let unreadable = sha('5');
        write_current_entry(&cache, unreadable.as_str(), 100);
        let unreadable_root = cache.dir().join(unreadable.as_str());
        fs::write(metadata_path(unreadable_root.as_path()), b"{not json").expect("corrupt");
        assert!(matches!(
            cache.lookup(unreadable.as_str()),
            Err(CacheError::Kept {
                reason: CleanReason::Unreadable,
                ..
            })
        ));
        assert!(unreadable_root.exists(), "an unreadable entry is kept");

        // An older binary's entry that another comparison holds open is kept.
        let stale = sha('6');
        write_current_entry(&cache, stale.as_str(), 100);
        let stale_root = cache.dir().join(stale.as_str());
        let holder = fs::File::create(stale_root.join(IN_USE_LOCK_FILE)).expect("lock file");
        holder.lock_shared().expect("hold shared");
        let mut metadata = read_metadata(stale_root.as_path())
            .expect("metadata")
            .expect("present metadata");
        metadata.identity.extractor_version -= 1;
        write_metadata(stale_root.as_path(), &metadata).expect("stale metadata");
        assert!(matches!(
            cache.lookup(stale.as_str()),
            Err(CacheError::Kept {
                reason: CleanReason::InUse,
                ..
            })
        ));
        assert!(stale_root.exists(), "an entry in use is kept");
        drop(holder);
        assert!(cache.lookup(stale.as_str()).expect("lookup").is_none());
        assert!(!stale_root.exists(), "a free stale entry is removed");
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
                    published_at: None,
                },
            )
            .expect("write metadata");
        }
        let foreign = dir.path().join("not-an-entry");
        fs::create_dir_all(foreign.as_path()).expect("foreign directory");
        let staging = dir.path().join(format!("{STAGING_PREFIX}{live}-1-2"));
        fs::create_dir_all(staging.as_path()).expect("staging directory");

        let report = cache
            .clean_with_options(&live_if(|commit| commit == live), &apply())
            .expect("clean the cache");

        let removed: Vec<(String, &'static str)> = report
            .would_delete
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
        metadata.identity.extractor_version -= 1;
        write_metadata(stale_root.as_path(), &metadata).expect("stale metadata");
        let staging = cache.dir().join(format!("{STAGING_PREFIX}{}-1-2", newest));
        fs::create_dir_all(staging.as_path()).expect("staging");

        let report = cache
            .clean_with_options(
                &live_if(|commit| live.contains(&commit)),
                &CleanOptions {
                    older_than: Some(Duration::from_secs(200)),
                    keep: Some(1),
                    now_seconds: Some(1_000),
                    apply: true,
                },
            )
            .expect("clean");
        let reasons: Vec<(String, &'static str)> = report
            .would_delete
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
                &live_if(|commit| commit == first || commit == second),
                &CleanOptions {
                    older_than: None,
                    keep: Some(10),
                    now_seconds: Some(1_000),
                    apply: true,
                },
            )
            .expect("clean must not panic when keep exceeds the entry count");

        assert!(report.would_delete.is_empty(), "{:?}", report.would_delete);
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
    #[test]
    fn a_plan_changes_nothing_and_an_apply_removes_exactly_the_plan() {
        let dir = TempDir::new().expect("cache directory");
        let cache = SnapshotCache::open(dir.path()).expect("open cache");
        let gone = sha('7');
        let unknown = sha('8');
        write_current_entry(&cache, gone.as_str(), 100);
        write_current_entry(&cache, unknown.as_str(), 100);
        let staging = cache.dir().join(format!("{STAGING_PREFIX}{gone}-1-2"));
        fs::create_dir_all(staging.as_path()).expect("staging");
        let is_live = |commit: &str| {
            if commit == gone {
                Liveness::Gone
            } else {
                Liveness::Unknown("odb error".to_string())
            }
        };
        let listing = |dir: &Path| -> Vec<PathBuf> {
            let mut paths = Vec::new();
            let mut stack = vec![dir.to_path_buf()];
            while let Some(next) = stack.pop() {
                for entry in fs::read_dir(next).expect("read dir") {
                    let path = entry.expect("entry").path();
                    if path.is_dir() {
                        stack.push(path.clone());
                    }
                    paths.push(path);
                }
            }
            paths.sort();
            paths
        };

        let before = listing(cache.dir());
        let plan = cache
            .clean_with_options(&is_live, &CleanOptions::default())
            .expect("plan");
        assert!(!plan.applied);
        assert_eq!(listing(cache.dir()), before, "a plan writes nothing");
        let planned: Vec<PathBuf> = plan.would_delete.iter().map(|e| e.path.clone()).collect();
        assert_eq!(
            planned,
            vec![staging.clone(), cache.dir().join(gone.as_str())]
        );
        assert!(
            plan.kept
                .iter()
                .any(|entry| entry.reason == CleanReason::Unverifiable
                    && entry.detail.as_deref() == Some("odb error")),
            "{:?}",
            plan.kept
        );

        let applied = cache.clean_with_options(&is_live, &apply()).expect("apply");
        assert!(applied.applied);
        assert_eq!(applied.would_delete, plan.would_delete);
        for path in &planned {
            assert!(!path.exists(), "{} was planned and removed", path.display());
        }
        assert!(cache.dir().join(unknown.as_str()).exists());
    }
}
