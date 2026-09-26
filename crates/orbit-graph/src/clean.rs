use std::fs;
use std::path::{Path, PathBuf};

use git2::Repository;
use serde::Serialize;

use crate::{EXTRACTOR_VERSION, GraphError, store};

/// Report which graph database files [`clean_old_databases`] would delete.
///
/// This is the plan half of `orbit-graph clean`: it applies exactly the
/// predicates the apply half uses and reports every non-active database
/// family as either `would_delete` or `kept`, but it writes, creates and
/// removes nothing (STD-01 §R5). A lock file that does not exist yet is not
/// created to test it; a lock another process holds is reported as `locked`.
pub fn plan_clean_old_databases(worktree_root: &Path) -> Result<CleanReport, GraphError> {
    let active = store::resolve_worktree_db_path(worktree_root)?;
    clean_old_databases_excluding(worktree_root, active.path(), CleanMode::Plan)
}

/// Delete obsolete graph database files.
///
/// Removes databases from strictly older extractor versions, plus detached-HEAD
/// databases whose commit is no longer reachable from any local ref, each with
/// its `-wal`, `-shm` and `.db.lock` sidecars. A database is removed only while
/// this call holds its `.db.lock`, so one another process is syncing is kept;
/// a newer extractor version's database is never removed (STD-03 §R10). A
/// detached database is removed only when Git proves its commit is gone or
/// unreachable; any other Git error keeps it and reports why (STD-03 §R29).
/// The active database is never touched and is not created.
///
/// Only the writing commands, `orbit-graph sync` and `orbit-graph clean
/// --confirm`, call this; opening a graph removes nothing, and
/// [`plan_clean_old_databases`] reports the same decisions without acting.
pub fn clean_old_databases(worktree_root: &Path) -> Result<CleanReport, GraphError> {
    let active = store::resolve_worktree_db_path(worktree_root)?;
    clean_old_databases_excluding(worktree_root, active.path(), CleanMode::Apply)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CleanMode {
    Plan,
    Apply,
}

fn clean_old_databases_excluding(
    worktree_root: &Path,
    active_db_path: &Path,
    mode: CleanMode,
) -> Result<CleanReport, GraphError> {
    let graph_dir = active_db_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| worktree_root.join(".orbit-graph"));
    let mut report = CleanReport {
        graph_dir,
        would_delete: Vec::new(),
        kept: Vec::new(),
        applied: mode == CleanMode::Apply,
        deleted: Vec::new(),
    };

    if !report.graph_dir.exists() {
        return Ok(report);
    }

    let entries = fs::read_dir(report.graph_dir.as_path()).map_err(|source| {
        GraphError::io("read graph database directory", &report.graph_dir, source)
    })?;
    // Every file of one database (the `.db` and its sidecars) is decided and
    // removed together, keyed by the `.db` path.
    let mut families = std::collections::BTreeSet::new();
    for entry in entries {
        let entry = entry.map_err(|source| {
            GraphError::io(
                "read graph database directory entry",
                &report.graph_dir,
                source,
            )
        })?;
        let path = entry.path();
        let is_graph_file = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(graph_db_file_metadata)
            .is_some();
        if is_graph_file && path.is_file() && !is_active_graph_db_family(&path, active_db_path) {
            families.insert(graph_db_base_path(path.as_path()));
        }
    }

    let repo = Repository::discover(worktree_root).map_err(|source| source.message().to_string());
    for db_path in families {
        let reason = match judge_graph_db_family(db_path.as_path(), repo.as_ref()) {
            CleanDecision::Keep(reason, detail) => {
                report.kept.push(CleanItem::new(db_path, reason, detail));
                continue;
            }
            CleanDecision::Delete(reason) => reason,
        };
        let removed = match mode {
            CleanMode::Apply => delete_graph_db_family_if_unlocked(db_path.as_path())?,
            CleanMode::Plan => existing_graph_db_family_files_if_unlocked(db_path.as_path())?,
        };
        match removed {
            Some(paths) => {
                if mode == CleanMode::Apply {
                    report.deleted.extend(paths.iter().cloned());
                }
                report.would_delete.extend(
                    paths
                        .into_iter()
                        .map(|path| CleanItem::new(path, reason, None)),
                );
            }
            None => report
                .kept
                .push(CleanItem::new(db_path, CleanReason::Locked, None)),
        }
    }
    report.deleted.sort();
    report
        .would_delete
        .sort_by(|left, right| left.path.cmp(&right.path));
    report
        .kept
        .sort_by(|left, right| left.path.cmp(&right.path));

    Ok(report)
}

/// The files of `db_path`'s family that exist, or `None` when another process
/// holds its `.db.lock`. Nothing is created or removed: a lock file that does
/// not exist is not held by anyone.
fn existing_graph_db_family_files_if_unlocked(
    db_path: &Path,
) -> Result<Option<Vec<PathBuf>>, GraphError> {
    let lock_path = graph_db_sidecar(db_path, ".lock");
    match fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
    {
        Ok(lock) => match lock.try_lock() {
            Ok(()) => drop(lock),
            Err(fs::TryLockError::WouldBlock) => return Ok(None),
            Err(fs::TryLockError::Error(source)) => {
                return Err(GraphError::io(
                    "lock old graph database",
                    &lock_path,
                    source,
                ));
            }
        },
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(GraphError::io(
                "open old graph database lock",
                &lock_path,
                source,
            ));
        }
    }
    Ok(Some(
        graph_db_family_files(db_path)
            .into_iter()
            .filter(|path| path.exists())
            .collect(),
    ))
}

fn graph_db_family_files(db_path: &Path) -> [PathBuf; 4] {
    [
        db_path.to_path_buf(),
        graph_db_sidecar(db_path, "-wal"),
        graph_db_sidecar(db_path, "-shm"),
        graph_db_sidecar(db_path, ".lock"),
    ]
}

/// Removes `db_path` and its sidecars while holding its `.db.lock`, and
/// returns the paths removed. When another process holds the lock, as a sync
/// of that database does, nothing is removed and `None` is returned.
fn delete_graph_db_family_if_unlocked(db_path: &Path) -> Result<Option<Vec<PathBuf>>, GraphError> {
    let lock_path = graph_db_sidecar(db_path, ".lock");
    // A lock file this call creates only to take the lock is not reported.
    let lock_existed = lock_path.exists();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|source| GraphError::io("open old graph database lock", &lock_path, source))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(fs::TryLockError::WouldBlock) => {
            tracing::info!(
                path = %db_path.display(),
                "kept an old graph database whose lock another process holds"
            );
            return Ok(None);
        }
        Err(fs::TryLockError::Error(source)) => {
            return Err(GraphError::io(
                "lock old graph database",
                &lock_path,
                source,
            ));
        }
    }
    let mut deleted = Vec::new();
    for path in graph_db_family_files(db_path) {
        let report = path != lock_path || lock_existed;
        match fs::remove_file(&path) {
            Ok(()) if report => deleted.push(path),
            Ok(()) => {}
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(GraphError::io(
                    "delete old graph database file",
                    &path,
                    source,
                ));
            }
        }
    }
    drop(lock);
    Ok(Some(deleted))
}

fn graph_db_sidecar(db_path: &Path, suffix: &str) -> PathBuf {
    let mut name = db_path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// What `clean` decided for one database family before its lock is checked.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CleanDecision {
    Delete(CleanReason),
    Keep(CleanReason, Option<String>),
}

/// Decide one family. Integrity check, fail closed (STD-02 §R31): deletion
/// needs a positive staleness fact (an older extractor version, or Git proving
/// the detached commit gone or unreachable); anything that cannot be verified
/// is kept with the reason (STD-03 §R29).
fn judge_graph_db_family(path: &Path, repo: Result<&Repository, &String>) -> CleanDecision {
    let Some(metadata) = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(graph_db_file_metadata)
    else {
        return CleanDecision::Keep(
            CleanReason::Unverifiable,
            Some("not a graph database file name".to_string()),
        );
    };

    // A newer extractor's database belongs to a newer binary (STD-03 §R10).
    match metadata.extractor_version.cmp(&EXTRACTOR_VERSION) {
        std::cmp::Ordering::Less => {
            return CleanDecision::Delete(CleanReason::OlderExtractorVersion);
        }
        std::cmp::Ordering::Greater => {
            return CleanDecision::Keep(CleanReason::NewerExtractorVersion, None);
        }
        std::cmp::Ordering::Equal => {}
    }

    // Per-commit detached databases are pruned by Git reachability.
    let Some(commit_prefix) = metadata.detached_commit_prefix else {
        return CleanDecision::Keep(CleanReason::Current, None);
    };
    if let Err(detail) = detached_db_meta_matches(path, commit_prefix.as_str()) {
        return CleanDecision::Keep(CleanReason::Unverifiable, Some(detail));
    }
    let repo = match repo {
        Ok(repo) => repo,
        Err(message) => {
            return CleanDecision::Keep(
                CleanReason::Unverifiable,
                Some(format!(
                    "no Git repository to check reachability: {message}"
                )),
            );
        }
    };
    match detached_commit_reachability(repo, commit_prefix.as_str()) {
        Reachability::Unreachable => CleanDecision::Delete(CleanReason::UnreachableDetachedCommit),
        Reachability::Reachable => CleanDecision::Keep(CleanReason::Current, None),
        Reachability::Unknown(detail) => {
            CleanDecision::Keep(CleanReason::Unverifiable, Some(detail))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GraphDbFileMetadata {
    extractor_version: u32,
    detached_commit_prefix: Option<String>,
}

fn graph_db_file_metadata(file_name: &str) -> Option<GraphDbFileMetadata> {
    let db_name = file_name
        .strip_suffix(".db")
        .or_else(|| file_name.strip_suffix(".db-wal"))
        .or_else(|| file_name.strip_suffix(".db-shm"))
        .or_else(|| file_name.strip_suffix(".db.lock"))?;
    let (stem, version) = db_name.rsplit_once('.')?;
    let extractor_version = version.parse().ok()?;
    let detached_commit_prefix = stem
        .strip_prefix("detached-")
        .filter(|prefix| prefix.len() == 12)
        .filter(|prefix| prefix.chars().all(|ch| ch.is_ascii_hexdigit()))
        .map(str::to_string);
    Some(GraphDbFileMetadata {
        extractor_version,
        detached_commit_prefix,
    })
}

fn is_active_graph_db_family(path: &Path, active_db_path: &Path) -> bool {
    graph_db_base_path(path) == graph_db_base_path(active_db_path)
}

fn graph_db_base_path(path: &Path) -> PathBuf {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return path.to_path_buf();
    };
    for suffix in [".db-wal", ".db-shm", ".db.lock"] {
        if let Some(stem) = file_name.strip_suffix(suffix) {
            return path.with_file_name(format!("{stem}.db"));
        }
    }
    path.to_path_buf()
}

/// Confirm the database really is the detached index its file name claims,
/// or say why that could not be confirmed.
fn detached_db_meta_matches(path: &Path, commit_prefix: &str) -> Result<(), String> {
    let db_path = graph_db_base_path(path);
    if !db_path.exists() {
        return Err("the database file is missing, so its metadata cannot be read".to_string());
    }
    let conn = store::open_observational(db_path.as_path(), "read detached graph metadata")
        .map_err(|error| format!("read detached graph metadata: {error}"))?;
    let metadata = || -> Result<(Option<String>, Option<String>), rusqlite::Error> {
        let mut stmt =
            conn.prepare("SELECT key, value FROM meta WHERE key IN ('branch', 'commit_sha')")?;
        let mut rows = stmt.query([])?;
        let mut branch = None;
        let mut commit_sha = None;
        while let Some(row) = rows.next()? {
            let key: String = row.get(0)?;
            let value: String = row.get(1)?;
            match key.as_str() {
                "branch" => branch = Some(value),
                "commit_sha" => commit_sha = Some(value),
                _ => {}
            }
        }
        Ok((branch, commit_sha))
    };
    let (branch, commit_sha) =
        metadata().map_err(|error| format!("read detached graph metadata: {error}"))?;
    if branch.as_deref() == Some("HEAD")
        && commit_sha
            .as_deref()
            .is_some_and(|sha| sha.starts_with(commit_prefix))
    {
        Ok(())
    } else {
        Err(format!(
            "stored metadata (branch {}, commit {}) does not match the detached file name",
            branch.as_deref().unwrap_or("absent"),
            commit_sha.as_deref().unwrap_or("absent"),
        ))
    }
}

/// Whether a detached database's commit is still reachable from a local ref.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Reachability {
    Reachable,
    /// Git proved the commit is gone, or that no ref reaches it.
    Unreachable,
    /// Git could not answer; the reason is reported and the database kept.
    Unknown(String),
}

/// Integrity check, fail closed (STD-02 §R31): only git's `NotFound` for the
/// commit, or a complete walk of every ref that finds none reaching it, is
/// proof. An ambiguous prefix, an object-database error, a shallow history or
/// an unreadable ref makes the answer unknown.
///
/// The prefix is looked up as an object id only, never as a ref name, so a
/// branch that happens to be spelled like the prefix cannot stand in for it.
fn detached_commit_reachability(repo: &Repository, commit_prefix: &str) -> Reachability {
    let detached_commit = match repo.find_commit_by_prefix(commit_prefix) {
        Ok(commit) => commit.id(),
        Err(error) if error.code() == git2::ErrorCode::NotFound => {
            return Reachability::Unreachable;
        }
        Err(error) => {
            return Reachability::Unknown(format!(
                "resolve detached commit {commit_prefix}: {}",
                error.message()
            ));
        }
    };

    let refs = match repo.references() {
        Ok(refs) => refs,
        Err(error) => {
            return Reachability::Unknown(format!("list git refs: {}", error.message()));
        }
    };
    for reference in refs {
        let reference = match reference {
            Ok(reference) => reference,
            Err(error) => {
                return Reachability::Unknown(format!("read git ref: {}", error.message()));
            }
        };
        if !reference.name_bytes().starts_with(b"refs/") {
            continue;
        }
        let ref_commit = match reference.peel_to_commit() {
            Ok(commit) => commit.id(),
            // A dangling symbolic ref, or a tag of a tree or blob, reaches no
            // commit at all.
            Err(error)
                if matches!(
                    error.code(),
                    git2::ErrorCode::NotFound
                        | git2::ErrorCode::Peel
                        | git2::ErrorCode::InvalidSpec
                ) =>
            {
                continue;
            }
            Err(error) => {
                return Reachability::Unknown(format!(
                    "peel git ref {}: {}",
                    String::from_utf8_lossy(reference.name_bytes()),
                    error.message()
                ));
            }
        };
        if ref_commit == detached_commit {
            return Reachability::Reachable;
        }
        match repo.graph_descendant_of(ref_commit, detached_commit) {
            Ok(true) => return Reachability::Reachable,
            Ok(false) => {}
            Err(error) => {
                return Reachability::Unknown(format!(
                    "check reachability from {}: {}",
                    String::from_utf8_lossy(reference.name_bytes()),
                    error.message()
                ));
            }
        }
    }

    Reachability::Unreachable
}

/// Why `clean` would delete, deleted or kept a graph database file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CleanReason {
    /// Written by a strictly older extractor version.
    OlderExtractorVersion,
    /// A detached-HEAD index whose commit Git reports gone or unreachable.
    UnreachableDetachedCommit,
    /// Written by a newer extractor version, so it belongs to a newer binary.
    NewerExtractorVersion,
    /// Current: this extractor version, and reachable when detached.
    Current,
    /// Another process holds the database's lock.
    Locked,
    /// Staleness could not be verified; `detail` says why.
    Unverifiable,
}

/// One graph database path `clean` decided about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CleanItem {
    /// File the decision applies to. A kept family is named by its `.db` path.
    pub path: PathBuf,
    /// Why it would be deleted, was deleted, or was kept.
    pub reason: CleanReason,
    /// What could not be verified, for an `unverifiable` item.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl CleanItem {
    fn new(path: PathBuf, reason: CleanReason, detail: Option<String>) -> Self {
        Self {
            path,
            reason,
            detail,
        }
    }
}

/// What [`plan_clean_old_databases`] or [`clean_old_databases`] decided.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CleanReport {
    /// Directory scanned for graph database files.
    pub graph_dir: PathBuf,
    /// Files the plan deletes. After an apply, exactly the files removed.
    pub would_delete: Vec<CleanItem>,
    /// Database families left in place, with the reason each was kept.
    pub kept: Vec<CleanItem>,
    /// Whether anything was actually deleted (`clean --confirm` or `sync`).
    pub applied: bool,
    /// Files deleted; empty for a plan.
    pub deleted: Vec<PathBuf>,
}
