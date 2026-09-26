//! Rebuildable SQLite index for verified and Git-only delivery history.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use git2::{Oid, Repository};
use orbit_graph_extract::history::{
    CHANGE_EXTRACTOR_VERSION, CurrentRevisionResolution, CurrentSymbolStatus,
    DEFAULT_HISTORY_SYNC_LIMIT, DELIVERY_IMPORT_SCHEMA_VERSION, DeliveredChange, DeliveryEvidence,
    DeliveryImport, FileChange, FileChangeKind, Provenance, RevisionSide, SymbolIdentity,
    TaskAssociation, TaskTextAvailability, TemporalFact, TemporalStatus, branch_tip,
    extract_delivery, repository_identity, validate_task_association,
};
use orbit_graph_extract::languages;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::Serialize;

use crate::GraphError;

/// Version of the history SQLite schema.
///
/// Version 4 adds `history_path_lineage`: each delivery's own rename and
/// deletion steps, persisted at ingest so recommendation can follow a
/// historical path forward without query-time rename detection. Opening a v4
/// index beside a compatible v3 index copies the v3 deliveries and cursors once
/// (see [`HistoryIndex::open`]); `history rebuild` also repopulates lineage.
pub const HISTORY_INDEX_SCHEMA_VERSION: u32 = 4;

/// Previous schema version whose payloads can be re-inserted without re-extraction.
const PREVIOUS_HISTORY_INDEX_SCHEMA_VERSION: u32 = 3;

/// `history_meta` key recording that the one-time previous-schema copy was attempted.
const LEGACY_COPY_META_KEY: &str = "legacy_copy";

/// Handle to the repository-local, rebuildable delivery-history index.
#[derive(Debug, Clone)]
pub struct HistoryIndex {
    repo_root: PathBuf,
    db_path: PathBuf,
    repository: String,
    landing_branch: String,
    /// Opened by a read-only constructor: connections are read-only and
    /// every write is refused.
    read_only: bool,
}

/// Outcome of an idempotent delivery import.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HistoryImportReport {
    /// Imported source-system delivery identifier.
    pub delivery_id: String,
    /// False when an identical delivery was already indexed.
    pub inserted: bool,
    /// Number of actual changed files.
    pub files: usize,
    /// Number of conservative symbol-change records.
    pub symbols: usize,
}

/// Outcome of an incremental first-parent Git history sync.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HistorySyncReport {
    /// Number of first-parent commits extracted.
    pub commits_indexed: usize,
    /// Number of new Git-only delivery rows.
    pub deliveries_inserted: usize,
    /// Cursor observed before the operation.
    pub cursor_before: Option<String>,
    /// Complete branch tip installed atomically with the delivery rows, if caught up.
    pub cursor_after: Option<String>,
    /// Frozen branch tip whose first-parent chain is being bootstrapped.
    pub snapshot_tip: String,
    /// Oldest not-yet-visited commit for the next bounded request.
    pub resume_from: Option<String>,
    /// True only when traversal reached the prior cursor or first parent root.
    pub complete: bool,
}

/// Current state of one repository/landing-branch history scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HistoryStatus {
    /// Stable repository identity.
    pub repository: String,
    /// First-parent landing branch scope.
    pub landing_branch: String,
    /// Rebuildable SQLite database path.
    pub database_path: PathBuf,
    /// Active history schema version.
    pub schema_version: u32,
    /// Active historical extractor version.
    pub extractor_version: u32,
    /// Last atomically indexed landing-branch tip.
    pub cursor: Option<String>,
    /// Frozen tip of an in-progress bounded bootstrap.
    pub bootstrap_tip: Option<String>,
    /// Oldest not-yet-visited commit for the next bootstrap request.
    pub bootstrap_resume_from: Option<String>,
    /// False while only a suffix of the frozen first-parent history is indexed.
    pub complete: bool,
    /// Total delivery rows in this scope.
    pub deliveries: usize,
    /// Deliveries backed by supplied verified delivery evidence.
    pub verified_deliveries: usize,
    /// Deliveries inferred only from Git first-parent history.
    pub git_only_deliveries: usize,
    /// Task memberships, which may be ambiguous across a multi-task delivery.
    pub task_associations: usize,
}

/// Outcome of an explicit scope rebuild from first-parent Git history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HistoryRebuildReport {
    /// Number of pre-rebuild delivery rows removed in the same transaction.
    pub removed_deliveries: usize,
    /// Git-only first-parent rebuild result.
    pub sync: HistorySyncReport,
}

/// One persisted path-lineage step recorded from a delivery's own before/after diff.
///
/// `new_path` is `Some` for a rename (the file continues under the new path)
/// and `None` for a deletion (the path has no descendant after the delivery).
/// Copies are not lineage: the source path stays live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PathLineageStep {
    /// Delivery whose diff recorded the step.
    pub(crate) delivery_id: String,
    /// Immutable landed commit of that delivery.
    pub(crate) after_revision: String,
    /// Path in the delivery's before tree.
    pub(crate) old_path: String,
    /// Descendant path in the delivery's after tree, absent for a deletion.
    pub(crate) new_path: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct HistoryScope {
    cursor: Option<String>,
    bootstrap_tip: Option<String>,
    bootstrap_frontier: Option<String>,
}

struct PendingBatch {
    commits: Vec<Oid>,
    complete: bool,
    resume_from: Option<Oid>,
}

impl HistoryIndex {
    /// Open or initialize the local history index for `landing_branch`.
    ///
    /// This is the writer's open, for `import`, `sync` and `rebuild`. A
    /// reader that must not create anything uses
    /// [`HistoryIndex::open_read_only`].
    pub fn open(repo_root: &Path, landing_branch: &str) -> Result<Self, GraphError> {
        Self::open_with_optional_index_dir(repo_root, landing_branch, None)
    }

    /// Open the existing local history index strictly for reading.
    ///
    /// Nothing is created, initialized, locked, copied or chmodded, so this
    /// works against a read-only `.orbit-graph/` directory (STD-01 §R31). An
    /// index that `orbit-graph history sync` has not built is
    /// [`GraphError::IndexMissing`], and one with other stored versions is an
    /// error naming `history rebuild`. Writes through the handle are refused.
    pub fn open_read_only(repo_root: &Path, landing_branch: &str) -> Result<Self, GraphError> {
        Self::open_read_only_with_optional_index_dir(repo_root, landing_branch, None)
    }

    /// Open the existing history index in orbit-graph's per-repository
    /// directory under `$ORBIT_PLUGIN_STATE` strictly for reading.
    pub(crate) fn open_read_only_with_index_dir(
        repo_root: &Path,
        landing_branch: &str,
        index_dir: &Path,
    ) -> Result<Self, GraphError> {
        Self::open_read_only_with_optional_index_dir(repo_root, landing_branch, Some(index_dir))
    }

    fn open_read_only_with_optional_index_dir(
        repo_root: &Path,
        landing_branch: &str,
        index_dir: Option<&Path>,
    ) -> Result<Self, GraphError> {
        let mut index = Self::resolve(repo_root, landing_branch, index_dir)?;
        index.read_only = true;
        if !index.db_path.is_file() {
            return Err(GraphError::IndexMissing {
                path: index.db_path.clone(),
                reason: format!(
                    "no history index for branch {} of {} at {}; run \
                     `orbit-graph history sync --branch {}`",
                    index.landing_branch,
                    index.repo_root.display(),
                    index.db_path.display(),
                    index.landing_branch
                ),
            });
        }
        let conn = index.open_connection()?;
        index.validate_versions(&conn)?;
        Ok(index)
    }

    /// Open or initialize the history index in orbit-graph's per-repository
    /// directory under `$ORBIT_PLUGIN_STATE`.
    pub(crate) fn open_with_index_dir(
        repo_root: &Path,
        landing_branch: &str,
        index_dir: &Path,
    ) -> Result<Self, GraphError> {
        Self::open_with_optional_index_dir(repo_root, landing_branch, Some(index_dir))
    }

    fn open_with_optional_index_dir(
        repo_root: &Path,
        landing_branch: &str,
        index_dir: Option<&Path>,
    ) -> Result<Self, GraphError> {
        let index = Self::resolve(repo_root, landing_branch, index_dir)?;
        // An explicit index directory is orbit-graph's own per-repository
        // directory under `$ORBIT_PLUGIN_STATE`; otherwise the index lives in
        // the worktree's scratch directory.
        let owner = match index_dir {
            Some(_) => super::IndexDirOwner::PluginState,
            None => super::IndexDirOwner::Scratch {
                worktree_root: index.repo_root.as_path(),
            },
        };
        if let Some(parent) = index.db_path.parent() {
            super::create_index_dir(parent, owner, "create history index directory")?;
        }
        let _lock = HistoryLock::acquire(index.db_path.as_path())?;
        let mut conn = index.open_connection()?;
        initialize_schema(&conn)?;
        index.validate_versions(&conn)?;
        index.copy_previous_schema_once(&mut conn)?;
        Ok(index)
    }

    /// Resolves the repository, branch scope and database path, touching
    /// nothing on disk.
    fn resolve(
        repo_root: &Path,
        landing_branch: &str,
        index_dir: Option<&Path>,
    ) -> Result<Self, GraphError> {
        let repo = Repository::discover(repo_root).map_err(|error| {
            GraphError::invalid_data("open history Git repository", error.to_string())
        })?;
        let workdir = repo.workdir().ok_or_else(|| {
            GraphError::invalid_data(
                "open history Git repository",
                "bare repositories are unsupported",
            )
        })?;
        let repo_root = workdir
            .canonicalize()
            .map_err(|source| GraphError::io("canonicalize history repository", workdir, source))?;
        let repository = repository_identity(&repo)?;
        let landing_branch = normalize_branch(landing_branch);
        if landing_branch.is_empty() {
            return Err(GraphError::invalid_data(
                "open history index",
                "landing branch must be non-empty",
            ));
        }
        let index_dir = index_dir.map_or_else(|| repo_root.join(".orbit-graph"), Path::to_path_buf);
        let db_path = index_dir.join(format!(
            "change-history.{HISTORY_INDEX_SCHEMA_VERSION}.sqlite3"
        ));
        let index = Self {
            repo_root,
            db_path,
            repository,
            landing_branch,
            read_only: false,
        };
        Ok(index)
    }

    fn refuse_read_only(&self, operation: &'static str) -> Result<(), GraphError> {
        if self.read_only {
            return Err(GraphError::invalid_data(
                operation,
                format!(
                    "{} was opened read-only; write it with `orbit-graph history sync`, \
                     `import` or `rebuild`",
                    self.db_path.display()
                ),
            ));
        }
        Ok(())
    }

    /// Populate a fresh index from a compatible previous-schema database once.
    ///
    /// Schema v4 only adds derived lineage rows, so v3 delivery payloads (which
    /// include externally verified imports that `history rebuild` cannot
    /// recreate) are re-inserted unchanged and their lineage derived on insert.
    /// The previous database is read, never modified. The attempt is recorded
    /// in `history_meta` so it runs at most once per database, and it is skipped
    /// when the new index already holds any scope or delivery.
    fn copy_previous_schema_once(&self, conn: &mut Connection) -> Result<(), GraphError> {
        // Decide and act inside one IMMEDIATE transaction (in addition to the
        // sidecar lock held by `open`), so concurrent openers of an empty index
        // cannot both pass the checks and copy twice.
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| GraphError::sqlite("begin history legacy copy", source))?;
        let attempted: Option<String> = tx
            .query_row(
                "SELECT value FROM history_meta WHERE key=?1",
                [LEGACY_COPY_META_KEY],
                |row| row.get(0),
            )
            .optional()
            .map_err(|source| GraphError::sqlite("read history legacy copy state", source))?;
        if attempted.is_some() {
            return Ok(());
        }
        let populated: i64 = tx
            .query_row(
                "SELECT (SELECT count(*) FROM history_deliveries) + (SELECT count(*) FROM history_scopes)",
                [],
                |row| row.get(0),
            )
            .map_err(|source| GraphError::sqlite("check history legacy copy target", source))?;
        let previous = self.db_path.with_file_name(format!(
            "change-history.{PREVIOUS_HISTORY_INDEX_SCHEMA_VERSION}.sqlite3"
        ));
        let outcome = if populated > 0 {
            "skipped: index already populated".to_string()
        } else if !previous.is_file() {
            "skipped: no previous index".to_string()
        } else {
            match read_previous_schema(previous.as_path())? {
                Some((scopes, deliveries)) => {
                    for change in &deliveries {
                        insert_delivery(&tx, change)?;
                    }
                    for (repository, branch, scope) in &scopes {
                        tx.execute(
                            "INSERT INTO history_scopes(repository,landing_branch,cursor,bootstrap_tip,bootstrap_frontier) VALUES(?1,?2,?3,?4,?5)",
                            params![repository, branch, scope.cursor, scope.bootstrap_tip, scope.bootstrap_frontier],
                        )
                        .map_err(|source| GraphError::sqlite("copy history scope", source))?;
                    }
                    format!(
                        "copied {} deliveries and {} scopes from schema {PREVIOUS_HISTORY_INDEX_SCHEMA_VERSION}",
                        deliveries.len(),
                        scopes.len()
                    )
                }
                None => "skipped: previous index has incompatible versions".to_string(),
            }
        };
        record_legacy_copy(&tx, outcome.as_str())?;
        tx.commit()
            .map_err(|source| GraphError::sqlite("commit history legacy copy", source))
    }

    /// Stable repository identity expected by import envelopes.
    pub fn repository(&self) -> &str {
        self.repository.as_str()
    }

    /// Landing branch scoped by this handle.
    pub fn landing_branch(&self) -> &str {
        self.landing_branch.as_str()
    }

    /// SQLite path under `.orbit-graph/`.
    pub fn database_path(&self) -> &Path {
        self.db_path.as_path()
    }

    /// Canonical repository root backing this history scope.
    pub(crate) fn repo_root(&self) -> &Path {
        self.repo_root.as_path()
    }

    /// Atomically validate, extract, and import one versioned delivery envelope.
    pub fn import(&self, mut delivery: DeliveryImport) -> Result<HistoryImportReport, GraphError> {
        self.refuse_read_only("import history delivery")?;
        normalize_tasks(&mut delivery.tasks)?;
        delivery.landing_branch = normalize_branch(delivery.landing_branch.as_str());
        self.ensure_scope(&delivery)?;
        let repo = Repository::open(self.repo_root.as_path()).map_err(|error| {
            GraphError::invalid_data("open repository for history import", error.to_string())
        })?;
        let extracted = extract_delivery(&repo, delivery)?;
        let _lock = HistoryLock::acquire(self.db_path.as_path())?;
        let mut conn = self.open_connection()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| GraphError::sqlite("begin history import", source))?;
        let report = insert_delivery(&tx, &extracted)?;
        tx.commit()
            .map_err(|source| GraphError::sqlite("commit history import", source))?;
        Ok(report)
    }

    /// Validate and extract an envelope without mutating the derived history index.
    ///
    /// Chronological evaluators use this to derive held-out truth without making
    /// the target delivery available to ranking.
    pub fn preview(&self, mut delivery: DeliveryImport) -> Result<DeliveredChange, GraphError> {
        normalize_tasks(&mut delivery.tasks)?;
        delivery.landing_branch = normalize_branch(delivery.landing_branch.as_str());
        self.ensure_scope(&delivery)?;
        let repo = Repository::open(self.repo_root.as_path()).map_err(|error| {
            GraphError::invalid_data("open repository for history preview", error.to_string())
        })?;
        Ok(extract_delivery(&repo, delivery)?)
    }

    /// Incrementally index first-parent commits, bounded by `limit`.
    pub fn sync(&self, limit: Option<usize>) -> Result<HistorySyncReport, GraphError> {
        self.refuse_read_only("sync history")?;
        self.sync_impl(limit.unwrap_or(DEFAULT_HISTORY_SYNC_LIMIT))
    }

    /// Atomically clear this branch scope and rebuild it from first-parent Git history.
    /// Verified delivery envelopes are external source records and must be re-imported.
    pub fn rebuild(&self, limit: Option<usize>) -> Result<HistoryRebuildReport, GraphError> {
        self.refuse_read_only("rebuild history")?;
        let limit = limit.unwrap_or(DEFAULT_HISTORY_SYNC_LIMIT);
        if limit == 0 {
            return Err(GraphError::invalid_data(
                "rebuild history",
                "limit must be greater than zero",
            ));
        }
        let repo = Repository::open(self.repo_root.as_path()).map_err(|error| {
            GraphError::invalid_data("open repository for history rebuild", error.to_string())
        })?;
        let commits = self.pending_commits(&repo, None, limit)?;
        let tip = branch_tip(&repo, self.landing_branch.as_str())?;
        let extracted = self.extract_git_deliveries(&repo, commits.as_slice())?;
        let _lock = HistoryLock::acquire(self.db_path.as_path())?;
        let mut conn = self.open_connection()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| GraphError::sqlite("begin history rebuild", source))?;
        let removed = delete_scope(&tx, self.repository.as_str(), self.landing_branch.as_str())?;
        for change in &extracted {
            insert_delivery(&tx, change)?;
        }
        write_cursor(
            &tx,
            self.repository.as_str(),
            self.landing_branch.as_str(),
            tip,
        )?;
        tx.commit()
            .map_err(|source| GraphError::sqlite("commit history rebuild", source))?;
        Ok(HistoryRebuildReport {
            removed_deliveries: removed,
            sync: HistorySyncReport {
                commits_indexed: extracted.len(),
                deliveries_inserted: extracted.len(),
                cursor_before: None,
                cursor_after: Some(tip.to_string()),
                snapshot_tip: tip.to_string(),
                resume_from: None,
                complete: true,
            },
        })
    }

    /// Read counts and the incremental cursor without consulting Orbit state.
    pub fn status(&self) -> Result<HistoryStatus, GraphError> {
        let conn = self.open_connection()?;
        let scope = read_scope(
            &conn,
            self.repository.as_str(),
            self.landing_branch.as_str(),
        )?;
        let (deliveries, verified, git_only): (i64, i64, i64) = conn.query_row(
            "SELECT count(*), coalesce(sum(CASE WHEN evidence='verified_delivery' THEN 1 ELSE 0 END),0), coalesce(sum(CASE WHEN evidence='git_only' THEN 1 ELSE 0 END),0) FROM history_deliveries WHERE repository=?1 AND landing_branch=?2",
            params![self.repository, self.landing_branch],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).map_err(|source| GraphError::sqlite("read history delivery status", source))?;
        let tasks: i64 = conn
            .query_row(
                "SELECT count(*) FROM history_tasks WHERE repository=?1 AND landing_branch=?2",
                params![self.repository, self.landing_branch],
                |row| row.get(0),
            )
            .map_err(|source| GraphError::sqlite("read history task status", source))?;
        let repo = Repository::open(self.repo_root.as_path()).map_err(|error| {
            GraphError::invalid_data("open repository for history status", error.to_string())
        })?;
        let tip = branch_tip(&repo, self.landing_branch.as_str())?.to_string();
        let complete =
            scope.bootstrap_tip.is_none() && scope.cursor.as_deref() == Some(tip.as_str());
        Ok(HistoryStatus {
            repository: self.repository.clone(),
            landing_branch: self.landing_branch.clone(),
            database_path: self.db_path.clone(),
            schema_version: HISTORY_INDEX_SCHEMA_VERSION,
            extractor_version: CHANGE_EXTRACTOR_VERSION,
            cursor: scope.cursor,
            bootstrap_tip: scope.bootstrap_tip.clone(),
            bootstrap_resume_from: scope.bootstrap_frontier,
            complete,
            deliveries: usize_from_i64(deliveries, "delivery count")?,
            verified_deliveries: usize_from_i64(verified, "verified delivery count")?,
            git_only_deliveries: usize_from_i64(git_only, "Git-only delivery count")?,
            task_associations: usize_from_i64(tasks, "task association count")?,
        })
    }

    /// Load extracted deliveries for later ranking without exposing SQLite details.
    pub fn deliveries(&self) -> Result<Vec<DeliveredChange>, GraphError> {
        let conn = self.open_connection()?;
        let mut stmt = conn.prepare(
            "SELECT payload_json FROM history_deliveries WHERE repository=?1 AND landing_branch=?2 ORDER BY after_revision, delivery_id"
        ).map_err(|source| GraphError::sqlite("prepare history delivery read", source))?;
        let rows = stmt
            .query_map(params![self.repository, self.landing_branch], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|source| GraphError::sqlite("query history deliveries", source))?;
        let mut deliveries = Vec::new();
        for row in rows {
            let payload =
                row.map_err(|source| GraphError::sqlite("read history delivery row", source))?;
            deliveries.push(serde_json::from_str(payload.as_str()).map_err(|error| {
                GraphError::invalid_data("decode indexed history delivery", error.to_string())
            })?);
        }
        Ok(deliveries)
    }

    /// Load persisted rename/deletion lineage steps for this scope.
    pub(crate) fn path_lineage(&self) -> Result<Vec<PathLineageStep>, GraphError> {
        let conn = self.open_connection()?;
        let mut stmt = conn
            .prepare(
                "SELECT l.delivery_id, d.after_revision, l.old_path, l.new_path FROM history_path_lineage l JOIN history_deliveries d ON d.repository=l.repository AND d.landing_branch=l.landing_branch AND d.delivery_id=l.delivery_id WHERE l.repository=?1 AND l.landing_branch=?2 ORDER BY l.delivery_id, l.ordinal",
            )
            .map_err(|source| GraphError::sqlite("prepare history lineage read", source))?;
        stmt.query_map(params![self.repository, self.landing_branch], |row| {
            Ok(PathLineageStep {
                delivery_id: row.get(0)?,
                after_revision: row.get(1)?,
                old_path: row.get(2)?,
                new_path: row.get(3)?,
            })
        })
        .map_err(|source| GraphError::sqlite("query history lineage", source))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| GraphError::sqlite("read history lineage row", source))
    }

    /// Return an indexed delivery by its stable source boundary identifier.
    pub fn delivery(&self, delivery_id: &str) -> Result<Option<DeliveredChange>, GraphError> {
        let conn = self.open_connection()?;
        let payload: Option<String> = conn
            .query_row(
                "SELECT payload_json FROM history_deliveries WHERE repository=?1 AND landing_branch=?2 AND delivery_id=?3",
                params![self.repository, self.landing_branch, delivery_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|source| GraphError::sqlite("read history delivery by id", source))?;
        payload
            .map(|payload| {
                serde_json::from_str(payload.as_str()).map_err(|error| {
                    GraphError::invalid_data("decode indexed history delivery", error.to_string())
                })
            })
            .transpose()
    }

    /// Resolve a historical identity against the current landing-branch tree.
    ///
    /// Only a unique same-path, same-qualified-name, same-kind match is live.
    /// Deleted and ambiguous historical symbols are never returned as destinations.
    pub fn resolve_current_symbol(
        &self,
        historical: &SymbolIdentity,
    ) -> Result<CurrentRevisionResolution, GraphError> {
        let repo = Repository::open(self.repo_root.as_path()).map_err(|error| {
            GraphError::invalid_data(
                "open repository for current symbol resolution",
                error.to_string(),
            )
        })?;
        let tip = branch_tip(&repo, self.landing_branch.as_str())?;
        let commit = repo
            .find_commit(tip)
            .map_err(git_error("load current landing commit"))?;
        let tree = commit
            .tree()
            .map_err(git_error("load current landing tree"))?;
        let path = Path::new(historical.file_path.as_str());
        let entry = match tree.get_path(path) {
            Ok(entry) => entry,
            Err(error) if error.code() == git2::ErrorCode::NotFound => {
                return Ok(current_resolution(
                    historical,
                    tip,
                    CurrentSymbolStatus::Deleted,
                    Vec::new(),
                    "historical file is absent from the current tree",
                ));
            }
            Err(error) => return Err(git_error("read current symbol path")(error)),
        };
        let blob = entry
            .to_object(&repo)
            .and_then(|object| object.peel_to_blob())
            .map_err(git_error("read current symbol blob"))?;
        let extractors = languages::extractors();
        let Some(extractor) = extractors.iter().find(|extractor| extractor.supports(path)) else {
            return Ok(current_resolution(
                historical,
                tip,
                CurrentSymbolStatus::Unavailable,
                Vec::new(),
                "current file language is unsupported",
            ));
        };
        if blob.content().contains(&0) {
            return Ok(current_resolution(
                historical,
                tip,
                CurrentSymbolStatus::Unavailable,
                Vec::new(),
                "current file is binary",
            ));
        }
        let extracted = extractor.extract(path, blob.content());
        if extracted.symbols.is_empty() {
            return Ok(current_resolution(
                historical,
                tip,
                CurrentSymbolStatus::Unavailable,
                Vec::new(),
                "current parse or extraction is uncertain",
            ));
        }
        let matches = extracted
            .symbols
            .into_iter()
            .filter(|symbol| {
                symbol.qualified == historical.qualified && symbol.kind == historical.kind
            })
            .map(|symbol| SymbolIdentity {
                revision: tip.to_string(),
                side: RevisionSide::After,
                file_path: historical.file_path.clone(),
                name: symbol.name,
                qualified: symbol.qualified,
                kind: symbol.kind,
                span_start: symbol.span_start,
                span_end: symbol.span_end,
                signature: symbol.signature,
                parent: symbol.parent_symbol,
            })
            .collect::<Vec<_>>();
        let (status, reason) = match matches.len() {
            0 => (
                CurrentSymbolStatus::Deleted,
                "historical symbol is absent from the current tree",
            ),
            1 => (
                CurrentSymbolStatus::Live,
                "unique qualified name and kind in the current tree",
            ),
            _ => (
                CurrentSymbolStatus::Ambiguous,
                "multiple current symbols share the qualified name and kind",
            ),
        };
        Ok(current_resolution(historical, tip, status, matches, reason))
    }

    fn sync_impl(&self, limit: usize) -> Result<HistorySyncReport, GraphError> {
        if limit == 0 {
            return Err(GraphError::invalid_data(
                "sync history",
                "limit must be greater than zero",
            ));
        }
        let repo = Repository::open(self.repo_root.as_path()).map_err(|error| {
            GraphError::invalid_data("open repository for history sync", error.to_string())
        })?;
        let conn = self.open_connection()?;
        let scope = read_scope(
            &conn,
            self.repository.as_str(),
            self.landing_branch.as_str(),
        )?;
        drop(conn);
        let cursor_oid = scope
            .cursor
            .as_deref()
            .map(|value| {
                Oid::from_str(value).map_err(|error| {
                    GraphError::invalid_data("read history cursor", error.to_string())
                })
            })
            .transpose()?;
        let tip = branch_tip(&repo, self.landing_branch.as_str())?;
        let snapshot_tip = scope
            .bootstrap_tip
            .as_deref()
            .map(|value| Oid::from_str(value).map_err(git_error("parse history bootstrap tip")))
            .transpose()?
            .unwrap_or(tip);
        if snapshot_tip != tip && !is_first_parent_ancestor(&repo, snapshot_tip, tip, limit)? {
            return Err(GraphError::invalid_data(
                "validate history bootstrap tip",
                "landing branch diverged while a bounded bootstrap was in progress",
            ));
        }
        let start = scope
            .bootstrap_frontier
            .as_deref()
            .map(|value| {
                Oid::from_str(value).map_err(git_error("parse history bootstrap frontier"))
            })
            .transpose()?
            .unwrap_or(snapshot_tip);
        let batch = self.pending_batch(&repo, cursor_oid, start, limit)?;
        let commits = batch.commits;
        let extracted = self.extract_git_deliveries(&repo, commits.as_slice())?;
        let _lock = HistoryLock::acquire(self.db_path.as_path())?;
        let mut conn = self.open_connection()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| GraphError::sqlite("begin history sync", source))?;
        let current_scope =
            read_scope(&tx, self.repository.as_str(), self.landing_branch.as_str())?;
        if current_scope != scope {
            return Err(GraphError::invalid_data(
                "sync history cursor",
                "history cursor changed concurrently; retry from fresh status",
            ));
        }
        let mut inserted = 0;
        for change in &extracted {
            if insert_delivery(&tx, change)?.inserted {
                inserted += 1;
            }
            #[cfg(test)]
            if inserted == 1 && should_interrupt_sync(self.repo_root.as_path()) {
                return Err(GraphError::invalid_data(
                    "sync history",
                    "simulated interruption after first delivery write",
                ));
            }
        }
        let cursor_after = if batch.complete {
            write_scope(
                &tx,
                self.repository.as_str(),
                self.landing_branch.as_str(),
                Some(snapshot_tip),
                None,
                None,
            )?;
            Some(snapshot_tip.to_string())
        } else {
            write_scope(
                &tx,
                self.repository.as_str(),
                self.landing_branch.as_str(),
                cursor_oid,
                Some(snapshot_tip),
                batch.resume_from,
            )?;
            scope.cursor.clone()
        };
        tx.commit()
            .map_err(|source| GraphError::sqlite("commit history sync", source))?;
        Ok(HistorySyncReport {
            commits_indexed: extracted.len(),
            deliveries_inserted: inserted,
            cursor_before: scope.cursor,
            cursor_after,
            snapshot_tip: snapshot_tip.to_string(),
            resume_from: batch.resume_from.map(|oid| oid.to_string()),
            complete: batch.complete,
        })
    }

    fn pending_batch(
        &self,
        repo: &Repository,
        cursor: Option<Oid>,
        start: Oid,
        limit: usize,
    ) -> Result<PendingBatch, GraphError> {
        if cursor == Some(start) {
            return Ok(PendingBatch {
                commits: Vec::new(),
                complete: true,
                resume_from: None,
            });
        }
        let mut commits = Vec::new();
        let mut current = repo
            .find_commit(start)
            .map_err(git_error("load history batch frontier"))?;
        loop {
            if Some(current.id()) == cursor {
                return Ok(PendingBatch {
                    commits,
                    complete: true,
                    resume_from: None,
                });
            }
            if current.parent_count() == 0 {
                if cursor.is_some() {
                    return Err(GraphError::invalid_data(
                        "validate history cursor",
                        "stored cursor is not on the frozen landing-branch first-parent chain",
                    ));
                }
                return Ok(PendingBatch {
                    commits,
                    complete: true,
                    resume_from: None,
                });
            }
            commits.push(current.id());
            let parent = current
                .parent(0)
                .map_err(git_error("walk bounded first-parent history"))?;
            if commits.len() == limit {
                return Ok(PendingBatch {
                    commits,
                    complete: false,
                    resume_from: Some(parent.id()),
                });
            }
            current = parent;
        }
    }

    fn pending_commits(
        &self,
        repo: &Repository,
        cursor: Option<Oid>,
        limit: usize,
    ) -> Result<Vec<Oid>, GraphError> {
        let tip = branch_tip(repo, self.landing_branch.as_str())?;
        if cursor == Some(tip) {
            return Ok(Vec::new());
        }
        let mut pending = Vec::new();
        let mut current = repo
            .find_commit(tip)
            .map_err(git_error("load history branch tip"))?;
        loop {
            if Some(current.id()) == cursor {
                break;
            }
            if pending.len() == limit {
                return Err(GraphError::invalid_data(
                    "traverse first-parent history",
                    format!(
                        "history exceeds bounded traversal limit {limit}; increase --limit explicitly"
                    ),
                ));
            }
            if current.parent_count() == 0 {
                if cursor.is_some() {
                    return Err(GraphError::invalid_data(
                        "validate history cursor",
                        "stored cursor is not on the landing branch first-parent chain (history diverged or was rebound)",
                    ));
                }
                break;
            }
            pending.push(current.id());
            current = current
                .parent(0)
                .map_err(git_error("walk first-parent history"))?;
        }
        pending.reverse();
        Ok(pending)
    }

    fn extract_git_deliveries(
        &self,
        repo: &Repository,
        commits: &[Oid],
    ) -> Result<Vec<DeliveredChange>, GraphError> {
        commits
            .iter()
            .map(|oid| {
                let commit = repo
                    .find_commit(*oid)
                    .map_err(git_error("load Git-only delivery commit"))?;
                let parent = commit
                    .parent(0)
                    .map_err(git_error("load Git-only delivery parent"))?;
                let message = commit.message().unwrap_or_default();
                let task_ids = task_ids_from_message(message);
                let subject = message
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                let commit_at = format!("unix:{}", commit.time().seconds());
                let captured_at = ingestion_timestamp()?;
                let tasks = task_ids
                    .into_iter()
                    .map(|task_id| TaskAssociation {
                        task_id,
                        title: subject.clone(),
                        description: String::new(),
                        acceptance_criteria: Vec::new(),
                        source: Provenance {
                            system: "git_trailer".into(),
                            record_id: Some(oid.to_string()),
                        },
                        created_at: TemporalFact {
                            status: TemporalStatus::Unavailable,
                            timestamp: None,
                            source: Provenance {
                                system: "git_trailer".into(),
                                record_id: Some(oid.to_string()),
                            },
                        },
                        snapshot_available_at: TemporalFact {
                            status: TemporalStatus::Uncertain,
                            timestamp: Some(commit_at.clone()),
                            source: Provenance {
                                system: "git_commit".into(),
                                record_id: Some(oid.to_string()),
                            },
                        },
                        text_availability: TaskTextAvailability::Uncertain,
                        captured_at: captured_at.clone(),
                    })
                    .collect();
                Ok(extract_delivery(
                    repo,
                    DeliveryImport {
                        schema_version: DELIVERY_IMPORT_SCHEMA_VERSION,
                        repository: self.repository.clone(),
                        landing_branch: self.landing_branch.clone(),
                        before_revision: parent.id().to_string(),
                        after_revision: oid.to_string(),
                        delivery_id: format!("git:{oid}"),
                        evidence: DeliveryEvidence::GitOnly,
                        source: Provenance {
                            system: "git_first_parent".into(),
                            record_id: Some(oid.to_string()),
                        },
                        delivered_at: TemporalFact {
                            status: TemporalStatus::Uncertain,
                            timestamp: Some(commit_at),
                            source: Provenance {
                                system: "git_commit".into(),
                                record_id: Some(oid.to_string()),
                            },
                        },
                        captured_at,
                        tasks,
                    },
                )?)
            })
            .collect()
    }

    fn ensure_scope(&self, delivery: &DeliveryImport) -> Result<(), GraphError> {
        if delivery.repository != self.repository
            || normalize_branch(delivery.landing_branch.as_str()) != self.landing_branch
        {
            return Err(GraphError::invalid_data(
                "validate history import scope",
                format!(
                    "repository/branch mismatch: expected {} {}",
                    self.repository, self.landing_branch
                ),
            ));
        }
        Ok(())
    }

    fn open_connection(&self) -> Result<Connection, GraphError> {
        if self.read_only {
            return super::open_observational(self.db_path.as_path(), "open history index");
        }
        // Create privately before SQLite opens the path, and tighten older
        // databases that SQLite may have created with the process umask.
        let _file = open_private_history_file(self.db_path.as_path())?;
        let conn = Connection::open(self.db_path.as_path())
            .map_err(|source| GraphError::sqlite("open history index", source))?;
        conn.pragma_update(None, "busy_timeout", 5_000)
            .map_err(|source| GraphError::sqlite("set history busy timeout", source))?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(|source| GraphError::sqlite("enable history foreign keys", source))?;
        Ok(conn)
    }

    fn validate_versions(&self, conn: &Connection) -> Result<(), GraphError> {
        for (key, expected) in [
            ("schema_version", HISTORY_INDEX_SCHEMA_VERSION),
            ("extractor_version", CHANGE_EXTRACTOR_VERSION),
            ("import_schema_version", DELIVERY_IMPORT_SCHEMA_VERSION),
        ] {
            let actual: String = conn
                .query_row(
                    "SELECT value FROM history_meta WHERE key=?1",
                    [key],
                    |row| row.get(0),
                )
                .map_err(|source| GraphError::sqlite("read history index version", source))?;
            if actual != expected.to_string() {
                return Err(GraphError::invalid_data(
                    "validate history index version",
                    format!("{key} is {actual}; expected {expected}; run history-rebuild"),
                ));
            }
        }
        Ok(())
    }
}

const HISTORY_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS history_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS history_scopes (
  repository TEXT NOT NULL, landing_branch TEXT NOT NULL, cursor TEXT,
  bootstrap_tip TEXT, bootstrap_frontier TEXT,
  PRIMARY KEY(repository, landing_branch)
) STRICT;
CREATE TABLE IF NOT EXISTS history_deliveries (
  repository TEXT NOT NULL, landing_branch TEXT NOT NULL, delivery_id TEXT NOT NULL,
  before_revision TEXT NOT NULL, after_revision TEXT NOT NULL, evidence TEXT NOT NULL,
  source_system TEXT NOT NULL, source_record_id TEXT,
  delivered_status TEXT NOT NULL, delivered_at TEXT,
  delivered_source_system TEXT NOT NULL, delivered_source_record_id TEXT,
  captured_at TEXT NOT NULL,
  task_count INTEGER NOT NULL, file_count INTEGER NOT NULL, symbol_count INTEGER NOT NULL,
  payload_json TEXT NOT NULL,
  PRIMARY KEY(repository, landing_branch, delivery_id)
) STRICT;
CREATE INDEX IF NOT EXISTS history_deliveries_after ON history_deliveries(repository, landing_branch, after_revision);
CREATE TABLE IF NOT EXISTS history_tasks (
  repository TEXT NOT NULL, landing_branch TEXT NOT NULL, delivery_id TEXT NOT NULL,
  task_id TEXT NOT NULL, title TEXT NOT NULL, description TEXT NOT NULL,
  acceptance_criteria_json TEXT NOT NULL, source_system TEXT NOT NULL,
  source_record_id TEXT,
  created_status TEXT NOT NULL, created_at TEXT,
  created_source_system TEXT NOT NULL, created_source_record_id TEXT,
  snapshot_status TEXT NOT NULL, snapshot_available_at TEXT,
  snapshot_source_system TEXT NOT NULL, snapshot_source_record_id TEXT,
  text_availability TEXT NOT NULL, captured_at TEXT NOT NULL,
  PRIMARY KEY(repository, landing_branch, delivery_id, task_id),
  FOREIGN KEY(repository, landing_branch, delivery_id)
    REFERENCES history_deliveries(repository, landing_branch, delivery_id) ON DELETE CASCADE
) STRICT;
CREATE TABLE IF NOT EXISTS history_files (
  repository TEXT NOT NULL, landing_branch TEXT NOT NULL, delivery_id TEXT NOT NULL,
  ordinal INTEGER NOT NULL, old_path TEXT, new_path TEXT, change_kind TEXT NOT NULL,
  additions INTEGER NOT NULL, deletions INTEGER NOT NULL,
  before_fallback TEXT, after_fallback TEXT,
  PRIMARY KEY(repository, landing_branch, delivery_id, ordinal),
  FOREIGN KEY(repository, landing_branch, delivery_id)
    REFERENCES history_deliveries(repository, landing_branch, delivery_id) ON DELETE CASCADE
) STRICT;
CREATE TABLE IF NOT EXISTS history_symbols (
  repository TEXT NOT NULL, landing_branch TEXT NOT NULL, delivery_id TEXT NOT NULL,
  file_ordinal INTEGER NOT NULL, symbol_ordinal INTEGER NOT NULL,
  side TEXT NOT NULL, revision TEXT NOT NULL, file_path TEXT NOT NULL,
  name TEXT NOT NULL, qualified TEXT NOT NULL, kind TEXT NOT NULL,
  span_start INTEGER NOT NULL, span_end INTEGER NOT NULL, signature TEXT, parent TEXT,
  changed_lines_json TEXT NOT NULL, match_confidence TEXT NOT NULL,
  match_reason TEXT NOT NULL, live_after INTEGER NOT NULL,
  PRIMARY KEY(repository, landing_branch, delivery_id, file_ordinal, symbol_ordinal, side),
  FOREIGN KEY(repository, landing_branch, delivery_id, file_ordinal)
    REFERENCES history_files(repository, landing_branch, delivery_id, ordinal) ON DELETE CASCADE
) STRICT;
CREATE TABLE IF NOT EXISTS history_path_lineage (
  repository TEXT NOT NULL, landing_branch TEXT NOT NULL, delivery_id TEXT NOT NULL,
  ordinal INTEGER NOT NULL, old_path TEXT NOT NULL, new_path TEXT,
  PRIMARY KEY(repository, landing_branch, delivery_id, ordinal),
  FOREIGN KEY(repository, landing_branch, delivery_id)
    REFERENCES history_deliveries(repository, landing_branch, delivery_id) ON DELETE CASCADE
) STRICT;
"#;

fn initialize_schema(conn: &Connection) -> Result<(), GraphError> {
    conn.execute_batch(HISTORY_SCHEMA)
        .map_err(|source| GraphError::sqlite("initialize history schema", source))?;
    for (key, value) in [
        ("schema_version", HISTORY_INDEX_SCHEMA_VERSION),
        ("extractor_version", CHANGE_EXTRACTOR_VERSION),
        ("import_schema_version", DELIVERY_IMPORT_SCHEMA_VERSION),
    ] {
        conn.execute(
            "INSERT OR IGNORE INTO history_meta(key,value) VALUES(?1,?2)",
            params![key, value.to_string()],
        )
        .map_err(|source| GraphError::sqlite("initialize history version", source))?;
    }
    Ok(())
}

fn insert_delivery(
    tx: &Transaction<'_>,
    change: &DeliveredChange,
) -> Result<HistoryImportReport, GraphError> {
    let delivery = &change.delivery;
    let payload = serde_json::to_string(change)
        .map_err(|error| GraphError::invalid_data("encode history delivery", error.to_string()))?;
    let existing: Option<(String, String, String)> = tx.query_row(
        "SELECT before_revision, after_revision, payload_json FROM history_deliveries WHERE repository=?1 AND landing_branch=?2 AND delivery_id=?3",
        params![delivery.repository, delivery.landing_branch, delivery.delivery_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).optional().map_err(|source| GraphError::sqlite("check duplicate history delivery", source))?;
    let symbol_count = change.files.iter().map(|file| file.symbols.len()).sum();
    if let Some((before, after, existing_payload)) = existing {
        if before != delivery.before_revision
            || after != delivery.after_revision
            || existing_payload != payload
        {
            return Err(GraphError::invalid_data(
                "deduplicate history delivery",
                format!(
                    "delivery_id {} already exists with different content",
                    delivery.delivery_id
                ),
            ));
        }
        return Ok(HistoryImportReport {
            delivery_id: delivery.delivery_id.clone(),
            inserted: false,
            files: change.files.len(),
            symbols: symbol_count,
        });
    }
    tx.execute(
        "INSERT INTO history_deliveries(repository,landing_branch,delivery_id,before_revision,after_revision,evidence,source_system,source_record_id,delivered_status,delivered_at,delivered_source_system,delivered_source_record_id,captured_at,task_count,file_count,symbol_count,payload_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
        params![delivery.repository, delivery.landing_branch, delivery.delivery_id, delivery.before_revision, delivery.after_revision, delivery.evidence.as_str(), delivery.source.system, delivery.source.record_id, temporal_status(delivery.delivered_at.status), delivery.delivered_at.timestamp, delivery.delivered_at.source.system, delivery.delivered_at.source.record_id, delivery.captured_at, i64_len(delivery.tasks.len())?, i64_len(change.files.len())?, i64_len(symbol_count)?, payload],
    ).map_err(|source| GraphError::sqlite("insert history delivery", source))?;
    for task in &delivery.tasks {
        let criteria = serde_json::to_string(&task.acceptance_criteria).map_err(|error| {
            GraphError::invalid_data("encode history task criteria", error.to_string())
        })?;
        tx.execute(
            "INSERT INTO history_tasks(repository,landing_branch,delivery_id,task_id,title,description,acceptance_criteria_json,source_system,source_record_id,created_status,created_at,created_source_system,created_source_record_id,snapshot_status,snapshot_available_at,snapshot_source_system,snapshot_source_record_id,text_availability,captured_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
            params![delivery.repository, delivery.landing_branch, delivery.delivery_id, task.task_id, task.title, task.description, criteria, task.source.system, task.source.record_id, temporal_status(task.created_at.status), task.created_at.timestamp, task.created_at.source.system, task.created_at.source.record_id, temporal_status(task.snapshot_available_at.status), task.snapshot_available_at.timestamp, task.snapshot_available_at.source.system, task.snapshot_available_at.source.record_id, task_text_availability(task.text_availability), task.captured_at],
        ).map_err(|source| GraphError::sqlite("insert history task association", source))?;
    }
    for (file_ordinal, file) in change.files.iter().enumerate() {
        tx.execute(
            "INSERT INTO history_files(repository,landing_branch,delivery_id,ordinal,old_path,new_path,change_kind,additions,deletions,before_fallback,after_fallback) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![delivery.repository, delivery.landing_branch, delivery.delivery_id, i64_len(file_ordinal)?, file.old_path, file.new_path, file.kind.as_str(), i64_len(file.additions)?, i64_len(file.deletions)?, file.before_fallback.map(|reason| reason.as_str()), file.after_fallback.map(|reason| reason.as_str())],
        ).map_err(|source| GraphError::sqlite("insert history file", source))?;
        if let Some((old_path, new_path)) = lineage_step(file) {
            tx.execute(
                "INSERT INTO history_path_lineage(repository,landing_branch,delivery_id,ordinal,old_path,new_path) VALUES(?1,?2,?3,?4,?5,?6)",
                params![delivery.repository, delivery.landing_branch, delivery.delivery_id, i64_len(file_ordinal)?, old_path, new_path],
            ).map_err(|source| GraphError::sqlite("insert history path lineage", source))?;
        }
        for (symbol_ordinal, symbol) in file.symbols.iter().enumerate() {
            for item in [symbol.before.as_ref(), symbol.after.as_ref()]
                .into_iter()
                .flatten()
            {
                let lines = serde_json::to_string(&item.changed_lines).map_err(|error| {
                    GraphError::invalid_data("encode history changed lines", error.to_string())
                })?;
                tx.execute(
                    "INSERT INTO history_symbols(repository,landing_branch,delivery_id,file_ordinal,symbol_ordinal,side,revision,file_path,name,qualified,kind,span_start,span_end,signature,parent,changed_lines_json,match_confidence,match_reason,live_after) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
                    params![delivery.repository, delivery.landing_branch, delivery.delivery_id, i64_len(file_ordinal)?, i64_len(symbol_ordinal)?, item.symbol.side.as_str(), item.symbol.revision, item.symbol.file_path, item.symbol.name, item.symbol.qualified, item.symbol.kind, i64_len(item.symbol.span_start)?, i64_len(item.symbol.span_end)?, item.symbol.signature, item.symbol.parent, lines, symbol.match_confidence.as_str(), symbol.reason, i64::from(symbol.live_after)],
                ).map_err(|source| GraphError::sqlite("insert history symbol", source))?;
            }
        }
    }
    Ok(HistoryImportReport {
        delivery_id: delivery.delivery_id.clone(),
        inserted: true,
        files: change.files.len(),
        symbols: symbol_count,
    })
}

/// Lineage recorded for one extracted file change: renames continue the old
/// path under the new one; deletions end it. Other kinds keep their path.
fn lineage_step(file: &FileChange) -> Option<(&str, Option<&str>)> {
    let old_path = file.old_path.as_deref()?;
    match file.kind {
        FileChangeKind::Renamed => {
            let new_path = file.new_path.as_deref()?;
            (new_path != old_path).then_some((old_path, Some(new_path)))
        }
        FileChangeKind::Deleted => Some((old_path, None)),
        _ => None,
    }
}

type PreviousSchemaContents = (Vec<(String, String, HistoryScope)>, Vec<DeliveredChange>);

/// Read every scope and delivery payload from a previous-schema database whose
/// extractor and import contracts match the running binary. Returns `None`
/// when the versions differ, so the caller records a skipped copy instead of
/// importing payloads with a different meaning.
fn read_previous_schema(path: &Path) -> Result<Option<PreviousSchemaContents>, GraphError> {
    let _lock = HistoryLock::acquire(path)?;
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|source| GraphError::sqlite("open previous history index", source))?;
    conn.pragma_update(None, "busy_timeout", 5_000)
        .map_err(|source| GraphError::sqlite("set previous history busy timeout", source))?;
    for (key, expected) in [
        ("schema_version", PREVIOUS_HISTORY_INDEX_SCHEMA_VERSION),
        ("extractor_version", CHANGE_EXTRACTOR_VERSION),
        ("import_schema_version", DELIVERY_IMPORT_SCHEMA_VERSION),
    ] {
        let actual: Option<String> = conn
            .query_row(
                "SELECT value FROM history_meta WHERE key=?1",
                [key],
                |row| row.get(0),
            )
            .optional()
            .map_err(|source| GraphError::sqlite("read previous history version", source))?;
        if actual != Some(expected.to_string()) {
            return Ok(None);
        }
    }
    let mut scopes_stmt = conn
        .prepare("SELECT repository,landing_branch,cursor,bootstrap_tip,bootstrap_frontier FROM history_scopes ORDER BY repository, landing_branch")
        .map_err(|source| GraphError::sqlite("prepare previous history scopes", source))?;
    let scopes = scopes_stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                HistoryScope {
                    cursor: row.get(2)?,
                    bootstrap_tip: row.get(3)?,
                    bootstrap_frontier: row.get(4)?,
                },
            ))
        })
        .map_err(|source| GraphError::sqlite("query previous history scopes", source))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| GraphError::sqlite("read previous history scope", source))?;
    let mut deliveries_stmt = conn
        .prepare("SELECT payload_json FROM history_deliveries ORDER BY repository, landing_branch, delivery_id")
        .map_err(|source| GraphError::sqlite("prepare previous history deliveries", source))?;
    let payloads = deliveries_stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|source| GraphError::sqlite("query previous history deliveries", source))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| GraphError::sqlite("read previous history delivery", source))?;
    let deliveries = payloads
        .iter()
        .map(|payload| {
            serde_json::from_str(payload.as_str()).map_err(|error| {
                GraphError::invalid_data("decode previous history delivery", error.to_string())
            })
        })
        .collect::<Result<Vec<DeliveredChange>, _>>()?;
    Ok(Some((scopes, deliveries)))
}

fn record_legacy_copy(conn: &Connection, outcome: &str) -> Result<(), GraphError> {
    conn.execute(
        "INSERT OR REPLACE INTO history_meta(key,value) VALUES(?1,?2)",
        params![LEGACY_COPY_META_KEY, outcome],
    )
    .map(drop)
    .map_err(|source| GraphError::sqlite("record history legacy copy", source))
}

fn task_ids_from_message(message: &str) -> Vec<String> {
    let mut ids = BTreeSet::new();
    for line in message.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("task-id")
            || key.trim().eq_ignore_ascii_case("orbit-task")
        {
            ids.extend(
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string),
            );
        }
    }
    ids.into_iter().collect()
}

fn normalize_branch(branch: &str) -> String {
    branch
        .trim()
        .strip_prefix("refs/heads/")
        .unwrap_or(branch.trim())
        .to_string()
}

fn normalize_tasks(tasks: &mut Vec<TaskAssociation>) -> Result<(), GraphError> {
    tasks.sort_by(|left, right| left.task_id.cmp(&right.task_id));
    let mut normalized: Vec<TaskAssociation> = Vec::with_capacity(tasks.len());
    for task in tasks.drain(..) {
        validate_task_association(&task)?;
        if let Some(previous) = normalized.last()
            && previous.task_id == task.task_id
        {
            if previous == &task {
                continue;
            }
            return Err(GraphError::invalid_data(
                "deduplicate delivery task association",
                format!("task {} appears with conflicting snapshots", task.task_id),
            ));
        }
        normalized.push(task);
    }
    *tasks = normalized;
    Ok(())
}

fn temporal_status(status: TemporalStatus) -> &'static str {
    match status {
        TemporalStatus::Known => "known",
        TemporalStatus::Uncertain => "uncertain",
        TemporalStatus::Unavailable => "unavailable",
    }
}

fn task_text_availability(availability: TaskTextAvailability) -> &'static str {
    match availability {
        TaskTextAvailability::KnownPreExecution => "known_pre_execution",
        TaskTextAvailability::PostExecution => "post_execution",
        TaskTextAvailability::Uncertain => "uncertain",
    }
}

fn ingestion_timestamp() -> Result<String, GraphError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| {
            GraphError::invalid_data("capture history ingestion time", error.to_string())
        })?;
    Ok(format!("unix:{}", duration.as_secs()))
}

fn current_resolution(
    historical: &SymbolIdentity,
    tip: Oid,
    status: CurrentSymbolStatus,
    matches: Vec<SymbolIdentity>,
    reason: &str,
) -> CurrentRevisionResolution {
    CurrentRevisionResolution {
        historical: historical.clone(),
        current_revision: Some(tip.to_string()),
        status,
        matches,
        reason: reason.to_string(),
    }
}

fn read_scope(
    conn: &Connection,
    repository: &str,
    branch: &str,
) -> Result<HistoryScope, GraphError> {
    conn.query_row(
        "SELECT cursor,bootstrap_tip,bootstrap_frontier FROM history_scopes WHERE repository=?1 AND landing_branch=?2",
        params![repository, branch],
        |row| {
            Ok(HistoryScope {
                cursor: row.get(0)?,
                bootstrap_tip: row.get(1)?,
                bootstrap_frontier: row.get(2)?,
            })
        },
    )
    .optional()
    .map(Option::unwrap_or_default)
    .map_err(|source| GraphError::sqlite("read history sync scope", source))
}

fn write_cursor(
    conn: &Connection,
    repository: &str,
    branch: &str,
    cursor: Oid,
) -> Result<(), GraphError> {
    conn.execute(
        "INSERT INTO history_scopes(repository,landing_branch,cursor) VALUES(?1,?2,?3) ON CONFLICT(repository,landing_branch) DO UPDATE SET cursor=excluded.cursor",
        params![repository, branch, cursor.to_string()],
    ).map_err(|source| GraphError::sqlite("write history cursor", source))?;
    Ok(())
}

fn write_scope(
    conn: &Connection,
    repository: &str,
    branch: &str,
    cursor: Option<Oid>,
    bootstrap_tip: Option<Oid>,
    bootstrap_frontier: Option<Oid>,
) -> Result<(), GraphError> {
    conn.execute(
        "INSERT INTO history_scopes(repository,landing_branch,cursor,bootstrap_tip,bootstrap_frontier) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(repository,landing_branch) DO UPDATE SET cursor=excluded.cursor,bootstrap_tip=excluded.bootstrap_tip,bootstrap_frontier=excluded.bootstrap_frontier",
        params![
            repository,
            branch,
            cursor.map(|oid| oid.to_string()),
            bootstrap_tip.map(|oid| oid.to_string()),
            bootstrap_frontier.map(|oid| oid.to_string()),
        ],
    )
    .map_err(|source| GraphError::sqlite("write history sync scope", source))?;
    Ok(())
}

fn is_first_parent_ancestor(
    repo: &Repository,
    ancestor: Oid,
    descendant: Oid,
    limit: usize,
) -> Result<bool, GraphError> {
    let mut current = repo
        .find_commit(descendant)
        .map_err(git_error("load landing tip for first-parent validation"))?;
    for _ in 0..=limit {
        if current.id() == ancestor {
            return Ok(true);
        }
        if current.parent_count() == 0 {
            return Ok(false);
        }
        current = current
            .parent(0)
            .map_err(git_error("walk landing first-parent validation"))?;
    }
    Err(GraphError::invalid_data(
        "validate history bootstrap tip",
        format!("branch advanced beyond the bounded validation limit {limit}"),
    ))
}

fn delete_scope(conn: &Connection, repository: &str, branch: &str) -> Result<usize, GraphError> {
    let removed = conn
        .execute(
            "DELETE FROM history_deliveries WHERE repository=?1 AND landing_branch=?2",
            params![repository, branch],
        )
        .map_err(|source| GraphError::sqlite("clear history delivery scope", source))?;
    conn.execute(
        "DELETE FROM history_scopes WHERE repository=?1 AND landing_branch=?2",
        params![repository, branch],
    )
    .map_err(|source| GraphError::sqlite("clear history cursor scope", source))?;
    Ok(removed)
}

fn i64_len(value: usize) -> Result<i64, GraphError> {
    i64::try_from(value)
        .map_err(|error| GraphError::invalid_data("store history count", error.to_string()))
}

fn usize_from_i64(value: i64, field: &str) -> Result<usize, GraphError> {
    usize::try_from(value).map_err(|error| {
        GraphError::invalid_data("read history count", format!("{field}: {error}"))
    })
}

fn git_error(operation: &'static str) -> impl FnOnce(git2::Error) -> GraphError {
    move |error| GraphError::invalid_data(operation, error.to_string())
}

#[cfg(test)]
fn set_sync_interruption(repo_root: Option<PathBuf>) {
    let slot = SYNC_INTERRUPTION.get_or_init(|| std::sync::Mutex::new(None));
    *slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = repo_root;
}

#[cfg(test)]
fn should_interrupt_sync(repo_root: &Path) -> bool {
    SYNC_INTERRUPTION
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_deref()
        == Some(repo_root)
}

#[cfg(test)]
static SYNC_INTERRUPTION: std::sync::OnceLock<std::sync::Mutex<Option<PathBuf>>> =
    std::sync::OnceLock::new();

struct HistoryLock {
    _file: File,
}

impl HistoryLock {
    fn acquire(db_path: &Path) -> Result<Self, GraphError> {
        let path = db_path.with_extension("sqlite3.lock");
        let file = open_private_history_file(path.as_path())?;
        file.lock_exclusive()
            .map_err(|source| GraphError::io("lock history index", path, source))?;
        Ok(Self { _file: file })
    }
}

fn open_private_history_file(path: &Path) -> Result<File, GraphError> {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options
        .open(path)
        .map_err(|source| GraphError::io("open private history file", path, source))?;
    #[cfg(unix)]
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|source| GraphError::io("restrict history file permissions", path, source))?;
    Ok(file)
}

#[cfg(test)]
mod tests;
