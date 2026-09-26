//! The plugin-owned code-graph index and its bounded `graph_sync` build.
//!
//! Readers only ever see a complete index. Each build writes a new generation
//! database (`graph.<extractor>.<n>.db`) that no reader references, and a
//! finished build publishes it by atomically replacing the pointer file
//! `graph.current.json`. A build that runs out of time, fails, or is killed
//! leaves the published generation untouched. Every generation a build
//! creates is first recorded in `graph.owned.json`, and a later build deletes
//! only recorded generations that are no longer published (STD-03 R29); graph
//! databases it did not record are kept and reported. Builds of one index
//! directory are serialized by an advisory lock that the kernel releases if
//! the process dies.

use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use fs2::FileExt;
use git2::{Repository, StatusOptions};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};

use crate::recommend::StructureIndex;
use crate::{
    EXTRACTOR_VERSION, Graph, GraphError, STORE_SCHEMA_VERSION, SyncMode, SyncObserver,
    SyncOutcome, SyncPhase, SyncPolicy, SyncProgress,
};

/// Pointer naming the published generation.
const POINTER_FILE: &str = "graph.current.json";
/// Advisory lock serializing builds of one index directory.
const LOCK_FILE: &str = "graph.maintain.lock";
/// Provenance record: the generation databases builds created here.
const OWNED_FILE: &str = "graph.owned.json";
/// Share of the budget pass 1 may use before it stops starting new files, so
/// reference resolution can still finish inside the budget.
const EXTRACTION_SHARE_PERCENT: u32 = 60;

/// Default wall-clock budget for one `graph_sync` call, under the plugin's
/// 120 s backend timeout.
pub(crate) const DEFAULT_BUDGET_MS: u64 = 90_000;
/// Smallest accepted budget.
pub(crate) const MIN_BUDGET_MS: u64 = 1_000;
/// Largest accepted budget: the call must answer before Orbit's 120 s backend
/// timeout kills it.
pub(crate) const MAX_BUDGET_MS: u64 = 110_000;

/// Metadata of a published generation, stored in the pointer file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PublishedIndex {
    /// Generation database file name inside the index directory.
    pub(crate) database: String,
    /// Extractor version that built it.
    pub(crate) extractor_version: u32,
    /// Store schema version that built it.
    pub(crate) store_schema_version: u32,
    /// Checkout commit when the build started; `None` on an unborn branch.
    pub(crate) revision: Option<String>,
    /// Whether tracked or untracked (non-ignored) files differed from
    /// `revision` when the build started.
    pub(crate) worktree_dirty: bool,
    /// Build completion time (RFC 3339, UTC).
    pub(crate) synced_at: String,
    /// `full` when every file was re-extracted, else `incremental`.
    pub(crate) mode: String,
    /// Files in the index.
    pub(crate) files: usize,
}

/// State of the code-graph index for one repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum IndexState {
    /// No build has been published.
    Missing,
    /// The published index was built by another extractor or schema version
    /// and cannot be read; a full `graph_sync` replaces it.
    Incompatible(PublishedIndex),
    /// A complete index is published.
    Ready(PublishedIndex),
}

impl IndexState {
    /// Read the published state of `index_dir`.
    pub(crate) fn read(index_dir: &Path) -> Result<Self, GraphError> {
        let pointer = index_dir.join(POINTER_FILE);
        let bytes = match fs::read(&pointer) {
            Ok(bytes) => bytes,
            Err(source) if source.kind() == ErrorKind::NotFound => return Ok(Self::Missing),
            Err(source) => {
                return Err(GraphError::io(
                    "read code-graph index pointer",
                    pointer,
                    source,
                ));
            }
        };
        let published: PublishedIndex = serde_json::from_slice(&bytes).map_err(|error| {
            GraphError::invalid_data("decode code-graph index pointer", error.to_string())
        })?;
        if published.extractor_version != EXTRACTOR_VERSION
            || published.store_schema_version != STORE_SCHEMA_VERSION
        {
            return Ok(Self::Incompatible(published));
        }
        if !is_generation_name(published.database.as_str())
            || !index_dir.join(&published.database).is_file()
        {
            return Ok(Self::Missing);
        }
        Ok(Self::Ready(published))
    }

    /// Where recommendation ranking reads structure from.
    pub(crate) fn structure_index(&self, index_dir: &Path) -> StructureIndex {
        match self {
            Self::Ready(published) => StructureIndex::Published {
                db_path: index_dir.join(&published.database),
                revision: published.revision.clone(),
            },
            Self::Missing => StructureIndex::Unavailable {
                kind: "structure_index_missing",
                reason: "no code-graph index has been built for this repository; run the \
                         graph_sync maintenance operation"
                    .to_string(),
            },
            Self::Incompatible(published) => StructureIndex::Unavailable {
                kind: "structure_index_incompatible",
                reason: format!(
                    "the code-graph index was built by extractor {} (schema {}), and this \
                     plugin reads extractor {EXTRACTOR_VERSION} (schema \
                     {STORE_SCHEMA_VERSION}); run the graph_sync maintenance operation with \
                     full: true",
                    published.extractor_version, published.store_schema_version
                ),
            },
        }
    }

    /// JSON description for tool responses.
    pub(crate) fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Missing => serde_json::json!({"state": "missing"}),
            Self::Incompatible(published) => {
                serde_json::json!({"state": "incompatible", "published": published})
            }
            Self::Ready(published) => serde_json::json!({"state": "ready", "published": published}),
        }
    }
}

/// A validated `graph_sync` request.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SyncRequest {
    /// Re-extract every file instead of starting from the published index.
    pub(crate) full: bool,
    /// Wall-clock budget for the whole call.
    pub(crate) budget: Duration,
}

/// Why a `graph_sync` call did not publish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Incomplete {
    /// Pass 1 stopped starting files at its share of the budget.
    ExtractionBudget,
    /// The budget ended while references were still being resolved.
    ResolutionBudget,
}

/// Outcome of one `graph_sync` call.
#[derive(Debug, Clone)]
pub(crate) struct SyncResult {
    /// Why nothing was published; `None` when the build was published.
    pub(crate) incomplete: Option<Incomplete>,
    /// Whether the build started from the previously published index.
    pub(crate) seeded: bool,
    /// Files re-extracted by this build.
    pub(crate) files_changed: usize,
    /// Files removed by this build.
    pub(crate) files_removed: usize,
    /// Files in the build when it stopped or completed.
    pub(crate) files_indexed: usize,
    /// Milliseconds spent per phase.
    pub(crate) timings: Timings,
    /// Graph databases in the index directory that no build recorded
    /// creating; they are kept, never deleted.
    pub(crate) unowned: Vec<String>,
}

/// Wall-clock milliseconds per build phase.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub(crate) struct Timings {
    /// Locking, cleanup, reading the checkout state, and copying the
    /// published index into the new generation.
    pub(crate) prepare_ms: u64,
    /// Scanning and pass 1 extraction.
    pub(crate) extract_ms: u64,
    /// Pass 2 reference resolution.
    pub(crate) resolve_ms: u64,
    /// Whole call.
    pub(crate) total_ms: u64,
}

/// Build and publish the code-graph index of `repository` in `index_dir`
/// within `request.budget`.
pub(crate) fn sync(
    repository: &Path,
    index_dir: &Path,
    request: SyncRequest,
) -> Result<SyncResult, GraphError> {
    let started = Instant::now();
    fs::create_dir_all(index_dir)
        .map_err(|source| GraphError::io("create code-graph index directory", index_dir, source))?;
    let lock = acquire_lock(index_dir)?;
    let previous = IndexState::read(index_dir)?;
    let published_database = match &previous {
        IndexState::Ready(published) => Some(published.database.clone()),
        IndexState::Missing | IndexState::Incompatible(_) => None,
    };
    let owned = remove_superseded_generations(index_dir, published_database.as_deref())?;

    let checkout = CheckoutState::read(repository)?;
    let generation_name = next_generation_name(index_dir, &previous);
    let mut recorded = owned;
    recorded.insert(generation_name.clone());
    write_owned(index_dir, &recorded)?;
    let unowned = unowned_databases(index_dir, &recorded)?;
    let generation = index_dir.join(generation_name);
    let seed = match (&previous, request.full) {
        (IndexState::Ready(published), false) => Some(index_dir.join(&published.database)),
        _ => None,
    };
    if let Some(seed) = seed.as_deref() {
        copy_database(seed, generation.as_path())?;
    }
    let prepare_ms = elapsed_ms(started);

    let shared = Arc::new(Shared::default());
    let deadline = started + request.budget;
    let extraction_deadline = started
        + request
            .budget
            .mul_f64(f64::from(EXTRACTION_SHARE_PERCENT) / 100.0);
    let job = BuildJob {
        repository: repository.to_path_buf(),
        index_dir: index_dir.to_path_buf(),
        generation,
        mode: if request.full || seed.is_none() {
            SyncMode::Full
        } else {
            SyncMode::Auto
        },
        checkout,
        extraction_deadline,
        shared: Arc::clone(&shared),
        lock,
        started,
    };
    std::thread::Builder::new()
        .name("orbit-graph-code-sync".to_string())
        .spawn(move || job.run())
        .map_err(|source| GraphError::io("start code-graph build thread", index_dir, source))?;

    let finished = shared.wait_until(deadline);
    let mut timings = Timings {
        prepare_ms,
        total_ms: elapsed_ms(started),
        ..Timings::default()
    };
    match finished {
        Some(Ok(build)) => {
            timings.extract_ms = build.extract_ms;
            timings.resolve_ms = build.resolve_ms;
            Ok(SyncResult {
                incomplete: build
                    .published
                    .is_none()
                    .then_some(Incomplete::ExtractionBudget),
                seeded: seed.is_some(),
                files_changed: build.files_changed,
                files_removed: build.files_removed,
                files_indexed: build.files_indexed,
                timings,
                unowned,
            })
        }
        Some(Err(error)) => Err(error),
        None => {
            let progress = shared.progress();
            timings.extract_ms = progress.extract_ms;
            Ok(SyncResult {
                incomplete: Some(match progress.phase {
                    Some(SyncPhase::Resolving) => Incomplete::ResolutionBudget,
                    _ => Incomplete::ExtractionBudget,
                }),
                seeded: seed.is_some(),
                files_changed: 0,
                files_removed: 0,
                files_indexed: progress.files_indexed,
                timings,
                unowned,
            })
        }
    }
}

fn elapsed_ms(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Checkout identity recorded with a build.
#[derive(Debug, Clone)]
struct CheckoutState {
    revision: Option<String>,
    dirty: bool,
}

impl CheckoutState {
    fn read(repository: &Path) -> Result<Self, GraphError> {
        let repo = Repository::open(repository).map_err(|error| {
            GraphError::invalid_data("open repository for code-graph sync", error.to_string())
        })?;
        let revision = head_revision(&repo);
        let mut options = StatusOptions::new();
        options
            .include_untracked(true)
            .include_ignored(false)
            .recurse_untracked_dirs(false);
        let dirty = repo
            .statuses(Some(&mut options))
            .map_err(|error| {
                GraphError::invalid_data(
                    "read repository status for code-graph sync",
                    error.to_string(),
                )
            })?
            .iter()
            .any(|entry| entry.status() != git2::Status::CURRENT);
        Ok(Self { revision, dirty })
    }
}

/// The commit `HEAD` names; `None` on an unborn branch.
fn head_revision(repo: &Repository) -> Option<String> {
    let head = repo.head().ok()?;
    let commit = head.peel_to_commit().ok()?;
    Some(commit.id().to_string())
}

/// The checkout's `HEAD` commit, when `repository` opens and `HEAD` is born.
pub(crate) fn checkout_revision(repository: &Path) -> Option<String> {
    head_revision(&Repository::open(repository).ok()?)
}

/// Exclusive, non-blocking build lock for `index_dir`.
fn acquire_lock(index_dir: &Path) -> Result<File, GraphError> {
    let path = index_dir.join(LOCK_FILE);
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options
        .open(&path)
        .map_err(|source| GraphError::io("open code-graph build lock", &path, source))?;
    match file.try_lock_exclusive() {
        Ok(()) => {
            // Diagnostic holder record (STD-03 R7); ownership is the flock.
            let holder = serde_json::json!({
                "pid": std::process::id(),
                "acquired_at": crate::recommend::current_observation_cutoff()?,
                "label": "graph_sync",
            });
            let mut writer = &file;
            file.set_len(0)
                .and_then(|()| writer.write_all(holder.to_string().as_bytes()))
                .map_err(|source| {
                    GraphError::io("record code-graph build lock holder", &path, source)
                })?;
            Ok(file)
        }
        Err(source) if source.kind() == fs2::lock_contended_error().kind() => {
            let holder = fs::read_to_string(&path).unwrap_or_default();
            let holder = if holder.trim().is_empty() {
                "an unrecorded holder".to_string()
            } else {
                holder.trim().to_string()
            };
            // Acquisition never waits, so its deadline is immediate.
            Err(GraphError::invalid_data(
                "lock code-graph index",
                format!(
                    "another graph_sync is building this repository's index (holder {holder}); \
                     retry after it finishes (the published index stays readable meanwhile)"
                ),
            ))
        }
        Err(source) => Err(GraphError::io("lock code-graph index", path, source)),
    }
}

fn generation_number(name: &str) -> Option<u64> {
    let rest = name.strip_prefix(&format!("graph.{EXTRACTOR_VERSION}."))?;
    rest.strip_suffix(".db")?.parse().ok()
}

fn is_generation_name(name: &str) -> bool {
    generation_number(name).is_some()
}

/// The next generation after the published one whose database and sidecars
/// are all absent, so a build never writes into a file it did not create.
fn next_generation_name(index_dir: &Path, previous: &IndexState) -> String {
    let mut number = match previous {
        IndexState::Ready(published) => generation_number(published.database.as_str()),
        IndexState::Missing | IndexState::Incompatible(_) => None,
    }
    .map_or(1, |number| number + 1);
    loop {
        let name = format!("graph.{EXTRACTOR_VERSION}.{number}.db");
        let taken = ["", "-wal", "-shm", "-journal", ".lock"]
            .iter()
            .any(|suffix| index_dir.join(format!("{name}{suffix}")).exists());
        if !taken {
            return name;
        }
        number += 1;
    }
}

/// Delete the generations earlier builds recorded creating, except the
/// published one, and return what stays recorded. Only recorded generations
/// are deleted: ownership comes from `graph.owned.json`, never from a file
/// name (STD-03 R29). An unreadable record deletes nothing.
fn remove_superseded_generations(
    index_dir: &Path,
    published: Option<&str>,
) -> Result<std::collections::BTreeSet<String>, GraphError> {
    let owned = read_owned(index_dir);
    let mut kept = std::collections::BTreeSet::new();
    for database in owned {
        if Some(database.as_str()) == published {
            kept.insert(database);
            continue;
        }
        if generation_of_any_extractor(&database).is_none() {
            // Not a name a build creates; never delete it on the record's word.
            continue;
        }
        for suffix in ["", "-wal", "-shm", "-journal", ".lock"] {
            let path = index_dir.join(format!("{database}{suffix}"));
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(source) if source.kind() == ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(GraphError::io(
                        "remove superseded code-graph generation",
                        path,
                        source,
                    ));
                }
            }
        }
    }
    Ok(kept)
}

/// The recorded generations; empty when the record is absent or unreadable.
fn read_owned(index_dir: &Path) -> std::collections::BTreeSet<String> {
    #[derive(Deserialize)]
    struct Owned {
        generations: std::collections::BTreeSet<String>,
    }
    fs::read(index_dir.join(OWNED_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Owned>(&bytes).ok())
        .map(|owned| owned.generations)
        .unwrap_or_default()
}

fn write_owned(
    index_dir: &Path,
    generations: &std::collections::BTreeSet<String>,
) -> Result<(), GraphError> {
    let bytes = serde_json::to_vec_pretty(&serde_json::json!({"generations": generations}))
        .map_err(|error| {
            GraphError::invalid_data("encode code-graph generation record", error.to_string())
        })?;
    write_durably(index_dir, OWNED_FILE, &bytes)
}

/// Graph databases in `index_dir` that no build recorded creating.
fn unowned_databases(
    index_dir: &Path,
    recorded: &std::collections::BTreeSet<String>,
) -> Result<Vec<String>, GraphError> {
    let mut unowned = std::collections::BTreeSet::new();
    let entries = fs::read_dir(index_dir)
        .map_err(|source| GraphError::io("list code-graph index directory", index_dir, source))?;
    for entry in entries {
        let entry = entry.map_err(|source| {
            GraphError::io("list code-graph index directory", index_dir, source)
        })?;
        let name = entry.file_name();
        if let Some(database) = name.to_str().and_then(graph_database_of)
            && !recorded.contains(database)
        {
            unowned.insert(database.to_string());
        }
    }
    Ok(unowned.into_iter().collect())
}

/// The generation number of `graph.<any extractor>.<n>.db`.
fn generation_of_any_extractor(name: &str) -> Option<u64> {
    let rest = name.strip_prefix("graph.")?.strip_suffix(".db")?;
    let (extractor, generation) = rest.split_once('.')?;
    extractor.parse::<u32>().ok()?;
    generation.parse().ok()
}

/// The database a graph store file belongs to: `graph.<n...>.db` itself or
/// one of its SQLite/lock sidecars.
fn graph_database_of(name: &str) -> Option<&str> {
    if !name.starts_with("graph.") || [POINTER_FILE, LOCK_FILE, OWNED_FILE].contains(&name) {
        return None;
    }
    for suffix in ["-wal", "-shm", "-journal", ".lock"] {
        if let Some(database) = name.strip_suffix(suffix) {
            return database.ends_with(".db").then_some(database);
        }
    }
    name.ends_with(".db").then_some(name)
}

/// Copy a consistent snapshot of `source` into the new, private `target`.
fn copy_database(source: &Path, target: &Path) -> Result<(), GraphError> {
    create_private_file(target)?;
    let conn = Connection::open_with_flags(
        source,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|source| GraphError::sqlite("open published code-graph index", source))?;
    let target_text = target.to_str().ok_or_else(|| {
        GraphError::invalid_data(
            "copy published code-graph index",
            format!("index path is not UTF-8: {}", target.display()),
        )
    })?;
    conn.execute("VACUUM INTO ?1", [target_text])
        .map_err(|source| GraphError::sqlite("copy published code-graph index", source))?;
    Ok(())
}

fn create_private_file(path: &Path) -> Result<(), GraphError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    options
        .open(path)
        .map(drop)
        .map_err(|source| GraphError::io("create code-graph generation", path, source))
}

/// Build state shared between the calling thread and the build thread.
#[derive(Default)]
struct Shared {
    slot: Mutex<Slot>,
    changed: Condvar,
    phase: AtomicU8,
    files_indexed: std::sync::atomic::AtomicUsize,
    extract_ms: std::sync::atomic::AtomicU64,
}

#[derive(Default)]
enum Slot {
    #[default]
    Running,
    /// The caller stopped waiting; the build must discard its generation.
    Abandoned,
    Finished(Result<Build, GraphError>),
}

struct Progress {
    phase: Option<SyncPhase>,
    files_indexed: usize,
    extract_ms: u64,
}

const PHASE_NONE: u8 = 0;
const PHASE_EXTRACTING: u8 = 1;
const PHASE_RESOLVING: u8 = 2;

impl Shared {
    /// Wait for the build until `deadline`. `None` means the caller gave up
    /// and the build will discard its work.
    fn wait_until(&self, deadline: Instant) -> Option<Result<Build, GraphError>> {
        let mut slot = self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if matches!(*slot, Slot::Finished(_)) {
                let Slot::Finished(result) = std::mem::take(&mut *slot) else {
                    unreachable!("slot was just checked");
                };
                *slot = Slot::Abandoned;
                return Some(result);
            }
            let now = Instant::now();
            if now >= deadline {
                *slot = Slot::Abandoned;
                return None;
            }
            slot = self
                .changed
                .wait_timeout(slot, deadline - now)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }

    fn progress(&self) -> Progress {
        Progress {
            phase: match self.phase.load(Ordering::Acquire) {
                PHASE_EXTRACTING => Some(SyncPhase::Extracting),
                PHASE_RESOLVING => Some(SyncPhase::Resolving),
                _ => None,
            },
            files_indexed: self.files_indexed.load(Ordering::Acquire),
            extract_ms: self.extract_ms.load(Ordering::Acquire),
        }
    }
}

struct BuildJob {
    repository: PathBuf,
    index_dir: PathBuf,
    generation: PathBuf,
    mode: SyncMode,
    checkout: CheckoutState,
    extraction_deadline: Instant,
    shared: Arc<Shared>,
    /// Held until the build has published or discarded its generation.
    lock: File,
    started: Instant,
}

struct Build {
    published: Option<PublishedIndex>,
    files_changed: usize,
    files_removed: usize,
    files_indexed: usize,
    extract_ms: u64,
    resolve_ms: u64,
}

struct DeadlineObserver<'a> {
    deadline: Instant,
    shared: &'a Shared,
    started: Instant,
}

impl SyncObserver for DeadlineObserver<'_> {
    fn on_progress(&self, progress: &SyncProgress) {
        match progress.phase {
            SyncPhase::Extracting => {
                self.shared.phase.store(PHASE_EXTRACTING, Ordering::Release);
                self.shared
                    .files_indexed
                    .store(progress.files_indexed, Ordering::Release);
            }
            SyncPhase::Resolving => {
                if self.shared.phase.swap(PHASE_RESOLVING, Ordering::AcqRel) != PHASE_RESOLVING {
                    self.shared
                        .extract_ms
                        .store(elapsed_ms(self.started), Ordering::Release);
                }
            }
        }
    }

    fn is_cancelled(&self) -> bool {
        Instant::now() >= self.deadline
            || matches!(
                *self
                    .shared
                    .slot
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
                Slot::Abandoned
            )
    }
}

impl BuildJob {
    fn run(self) {
        self.shared.phase.store(PHASE_NONE, Ordering::Release);
        let outcome = self.build();
        let mut slot = self
            .shared
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let result = match (&*slot, outcome) {
            (Slot::Abandoned, _) => {
                discard(self.generation.as_path());
                return;
            }
            (_, Ok((SyncOutcome::Completed(report), extract_ms, resolve_ms))) => {
                self.publish(report.files_indexed).map(|published| Build {
                    published: Some(published),
                    files_changed: report.files_changed,
                    files_removed: report.files_removed,
                    files_indexed: report.files_indexed,
                    extract_ms,
                    resolve_ms,
                })
            }
            (_, Ok((SyncOutcome::Cancelled(report), extract_ms, resolve_ms))) => {
                discard(self.generation.as_path());
                Ok(Build {
                    published: None,
                    files_changed: report.files_changed,
                    files_removed: report.files_removed,
                    files_indexed: report.files_indexed,
                    extract_ms,
                    resolve_ms,
                })
            }
            (_, Err(error)) => {
                discard(self.generation.as_path());
                Err(error)
            }
        };
        if result.is_err() {
            discard(self.generation.as_path());
        }
        *slot = Slot::Finished(result);
        self.shared.changed.notify_all();
        drop(slot);
        drop(self.lock);
    }

    fn build(&self) -> Result<(SyncOutcome, u64, u64), GraphError> {
        let outcome = {
            let graph = Graph::open_with_db_path(
                self.repository.as_path(),
                self.generation.as_path(),
                SyncPolicy::Manual,
            )?;
            let observer = DeadlineObserver {
                deadline: self.extraction_deadline,
                shared: &self.shared,
                started: self.started,
            };
            graph.sync_with_observer(self.mode, &observer)?
        };
        let total = elapsed_ms(self.started);
        let extract_ms = match self.shared.extract_ms.load(Ordering::Acquire) {
            0 => total,
            extract => extract,
        };
        Ok((outcome, extract_ms, total.saturating_sub(extract_ms)))
    }

    /// Fold the generation's WAL into its database, leave WAL journaling so
    /// readers can open it read-only without creating files, and publish it.
    fn publish(&self, files: usize) -> Result<PublishedIndex, GraphError> {
        {
            let conn = Connection::open(self.generation.as_path())
                .map_err(|source| GraphError::sqlite("open built code-graph index", source))?;
            conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
                .map_err(|source| {
                    GraphError::sqlite("checkpoint built code-graph index", source)
                })?;
            let mode: String = conn
                .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
                .map_err(|source| GraphError::sqlite("finalize built code-graph index", source))?;
            if !mode.eq_ignore_ascii_case("delete") {
                return Err(GraphError::invalid_data(
                    "finalize built code-graph index",
                    format!("SQLite kept journal_mode={mode}"),
                ));
            }
        }
        let database = self
            .generation
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                GraphError::invalid_data(
                    "publish code-graph index",
                    "generation file name is not UTF-8",
                )
            })?
            .to_string();
        let published = PublishedIndex {
            database,
            extractor_version: EXTRACTOR_VERSION,
            store_schema_version: STORE_SCHEMA_VERSION,
            revision: self.checkout.revision.clone(),
            worktree_dirty: self.checkout.dirty,
            synced_at: crate::recommend::current_observation_cutoff()?,
            mode: match self.mode {
                SyncMode::Full => "full",
                SyncMode::Auto => "incremental",
            }
            .to_string(),
            files,
        };
        write_pointer(self.index_dir.as_path(), &published)?;
        Ok(published)
    }
}

/// Replace the pointer file atomically and durably.
fn write_pointer(index_dir: &Path, published: &PublishedIndex) -> Result<(), GraphError> {
    let bytes = serde_json::to_vec_pretty(published).map_err(|error| {
        GraphError::invalid_data("encode code-graph index pointer", error.to_string())
    })?;
    write_durably(index_dir, POINTER_FILE, &bytes)
}

/// The one durable-write path of this module (STD-03 R5): write a private
/// temp file beside `name`, flush it, rename it over `name`, and flush the
/// directory, so readers see the old or the new file, never a partial one.
fn write_durably(index_dir: &Path, name: &str, bytes: &[u8]) -> Result<(), GraphError> {
    let target = index_dir.join(name);
    let staging = index_dir.join(format!("{name}.{}.tmp", std::process::id()));
    let written = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&staging)?;
        file.write_all(bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&staging, &target)?;
        File::open(index_dir)?.sync_all()
    })();
    written.map_err(|source| {
        let _ = fs::remove_file(&staging);
        GraphError::io("write code-graph index file", target, source)
    })
}

/// Best-effort removal of the generation this attempt created; the next build
/// removes anything left behind, because the generation is recorded as owned.
fn discard(generation: &Path) {
    let Some(name) = generation.file_name().and_then(|name| name.to_str()) else {
        return;
    };
    for suffix in ["", "-wal", "-shm", "-journal", ".lock"] {
        let _ = fs::remove_file(generation.with_file_name(format!("{name}{suffix}")));
    }
}

#[cfg(test)]
mod tests;
