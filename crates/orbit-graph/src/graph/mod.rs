use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

use crate::db_path::detached_commit_prefix;
use crate::{
    CalleeEdge, CalleeOpts, CalleeReport, Confidence, DepsResult, GraphDbPath, GraphError,
    ImpactDirection, ImpactResult, ImplementorsResult, NodeView, OverviewFormat, OverviewResult,
    ProgramName, RefOpts, RefResult, RuntimeInvocation, SearchQuery, SearchResult, Selector,
    SyncMode, SyncObserver, SyncOutcome, SyncPolicy, SyncReport, TraceResult, query, store, sync,
};

/// Opaque handle to a worktree-scoped graph database.
pub struct Graph {
    db_path: GraphDbPath,
    pub(crate) worktree_root: PathBuf,
    policy: SyncPolicy,
    /// Opened by a read-only constructor: [`Graph::sync`] is refused.
    read_only: bool,
    read_conn: Mutex<Connection>,
    last_auto_sync_at: Mutex<i64>,
    _watcher: Option<sync::watcher::SyncWatcher>,
}

impl Graph {
    /// Open the graph database for `worktree_root` using `policy`, creating
    /// and initializing it when it does not exist yet.
    ///
    /// This is the writer's open: use it to [`sync`](Self::sync). It removes
    /// no other database; [`crate::clean_old_databases`] does. A reader that must
    /// not create anything uses [`Graph::open_existing_read_only`].
    pub fn open(worktree_root: &Path, policy: SyncPolicy) -> Result<Self, GraphError> {
        // Phase 4 query methods will call this; keep the dispatcher live under dead-code lints.
        let _ensure_synced: fn(&Self) -> Result<(), GraphError> = Self::ensure_synced;
        let opened = store::open(worktree_root, policy)?;
        Self::from_opened(worktree_root, policy, opened)
    }

    /// Open the existing graph database for `worktree_root` strictly for
    /// reading.
    ///
    /// Nothing is created, initialized, migrated, locked, synchronized or
    /// deleted, so this works against a read-only `.orbit-graph/` directory
    /// (STD-01 §R31). A database that `orbit-graph sync` has not built is
    /// [`GraphError::IndexMissing`], and one whose stored schema identity is
    /// not [`crate::STORE_SCHEMA_VERSION`] is [`GraphError::IndexIncompatible`].
    /// [`Graph::sync`] on the returned handle is refused.
    ///
    /// # Examples
    ///
    /// ```
    /// use orbit_graph::{Graph, GraphError, SyncMode, SyncPolicy};
    ///
    /// let dir = tempfile::tempdir()?;
    /// assert!(matches!(
    ///     Graph::open_existing_read_only(dir.path()),
    ///     Err(GraphError::IndexMissing { .. })
    /// ));
    /// assert!(!dir.path().join(".orbit-graph").exists());
    ///
    /// Graph::open(dir.path(), SyncPolicy::Manual)?.sync(SyncMode::Full)?;
    /// let graph = Graph::open_existing_read_only(dir.path())?;
    /// assert!(graph.sync(SyncMode::Auto).is_err());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn open_existing_read_only(worktree_root: &Path) -> Result<Self, GraphError> {
        let db_path = store::resolve_worktree_db_path(worktree_root)?;
        if !db_path.path().is_file() {
            return Err(store::missing_graph_index(worktree_root, db_path.path()));
        }
        Self::open_observed(worktree_root, db_path)
    }

    /// Read the graph selected by a checkout identity already pinned by the caller.
    pub(crate) fn open_existing_for_pinned_target(
        worktree_root: &Path,
        branch: &str,
        target: &str,
    ) -> Result<Self, GraphError> {
        let db_path = crate::resolve_db_path_for_commit(
            worktree_root,
            branch,
            target,
            crate::EXTRACTOR_VERSION,
        );
        if !db_path.path().is_file() {
            return Err(store::missing_graph_index(worktree_root, db_path.path()));
        }
        Self::open_observed(worktree_root, db_path)
    }

    /// Open a graph for a synthetic or detached tree identified by `revision`.
    ///
    /// The database uses the existing `detached-<short-sha>` naming contract.
    ///
    /// # Examples
    ///
    /// ```
    /// use orbit_graph::{Graph, SyncPolicy};
    ///
    /// let dir = tempfile::tempdir()?;
    /// let revision = "0123456789abcdef";
    /// let graph = Graph::open_with_revision(dir.path(), revision, SyncPolicy::Manual)?;
    /// assert!(
    ///     graph
    ///         .db_path()
    ///         .path()
    ///         .to_string_lossy()
    ///         .contains("detached-0123456789ab")
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn open_with_revision(
        worktree_root: &Path,
        revision: &str,
        policy: SyncPolicy,
    ) -> Result<Self, GraphError> {
        if detached_commit_prefix(revision).is_none() {
            return Err(GraphError::invalid_data(
                "validate graph revision identity",
                "revision identity must begin with at least 12 hexadecimal characters",
            ));
        }
        let opened = store::open_for_revision(worktree_root, revision)?;
        Self::from_opened(worktree_root, policy, opened)
    }

    /// Open a graph that indexes `worktree_root` at a caller-supplied database path.
    ///
    /// Missing parent directories are created. The path must name a file.
    /// orbit-graph writes nothing else into a caller-supplied directory: it
    /// is not marked with a `.gitignore`.
    ///
    /// # Examples
    ///
    /// ```
    /// use orbit_graph::{Graph, SyncPolicy};
    ///
    /// let dir = tempfile::tempdir()?;
    /// let db_path = dir.path().join("custom").join("graph.db");
    /// let graph = Graph::open_with_db_path(dir.path(), &db_path, SyncPolicy::Manual)?;
    /// assert_eq!(graph.db_path().path(), db_path);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn open_with_db_path(
        worktree_root: &Path,
        db_path: &Path,
        policy: SyncPolicy,
    ) -> Result<Self, GraphError> {
        let opened =
            store::open_with_db_path(worktree_root, db_path, store::IndexDirOwner::Caller)?;
        Self::from_opened(worktree_root, policy, opened)
    }

    /// Open a graph whose database lives in orbit-graph's own per-repository
    /// state directory, which is created owner-only and marked with a
    /// `.gitignore` like the default scratch directory.
    ///
    /// Public only for the `orbit-graph` CLI's plugin protocol, which chooses
    /// that directory; library callers use [`Graph::open_with_db_path`].
    #[doc(hidden)]
    pub fn open_in_plugin_state(
        worktree_root: &Path,
        db_path: &Path,
        policy: SyncPolicy,
    ) -> Result<Self, GraphError> {
        let opened =
            store::open_with_db_path(worktree_root, db_path, store::IndexDirOwner::PluginState)?;
        Self::from_opened(worktree_root, policy, opened)
    }

    /// Open a complete graph database at `db_path` strictly for reading, as
    /// [`Graph::open_existing_read_only`] does for the worktree's own
    /// database. The plugin's published code-graph generations use a
    /// rollback journal, so they open as ordinary read-only connections.
    ///
    /// Nothing is created, migrated or locked (STD-01 §R31).
    ///
    /// # Errors
    ///
    /// [`GraphError::InvalidData`] when `db_path` is not a file or `worktree_root`
    /// is not a Git worktree, and an error when the database's stored schema
    /// identity is not the one this build reads.
    pub fn open_read_only(worktree_root: &Path, db_path: &Path) -> Result<Self, GraphError> {
        let db_path = store::existing_db_path(worktree_root, db_path)?;
        Self::open_observed(worktree_root, db_path)
    }

    fn open_observed(worktree_root: &Path, db_path: GraphDbPath) -> Result<Self, GraphError> {
        let read_conn = store::open_observational(db_path.path(), "open graph database read-only")?;
        store::schema::validate_identity(&read_conn, db_path.path())?;
        let last_auto_sync_at = read_last_incremental_at(
            &read_conn,
            "read graph last incremental sync metadata at open",
        )?;
        Ok(Self {
            db_path,
            worktree_root: worktree_root.to_path_buf(),
            policy: SyncPolicy::Manual,
            read_only: true,
            read_conn: Mutex::new(read_conn),
            last_auto_sync_at: Mutex::new(last_auto_sync_at),
            _watcher: None,
        })
    }

    fn from_opened(
        worktree_root: &Path,
        policy: SyncPolicy,
        opened: store::OpenedGraph,
    ) -> Result<Self, GraphError> {
        let read_conn = open_read_connection(opened.db_path.path(), "open graph read connection")?;
        let last_auto_sync_at = read_last_incremental_at(
            &read_conn,
            "read graph last incremental sync metadata at open",
        )?;
        let watcher = if let SyncPolicy::Watch { debounce } = policy {
            Some(sync::watcher::SyncWatcher::start(
                opened.db_path.path().to_path_buf(),
                worktree_root.to_path_buf(),
                debounce,
            )?)
        } else {
            None
        };

        let graph = Self {
            db_path: opened.db_path,
            worktree_root: worktree_root.to_path_buf(),
            policy,
            read_only: false,
            read_conn: Mutex::new(read_conn),
            last_auto_sync_at: Mutex::new(last_auto_sync_at),
            _watcher: watcher,
        };
        if matches!(policy, SyncPolicy::Watch { .. }) {
            graph.sync(SyncMode::Auto)?;
        }
        Ok(graph)
    }

    /// Synchronize indexed rows with files on disk.
    ///
    /// A path that cannot be read or extracted does not fail the sync: it is
    /// listed in [`SyncReport::failed`] and everything else is indexed. A
    /// sync interrupted by a crash, a kill, an error or a cancellation is
    /// repaired by the next sync, incremental or full.
    ///
    /// A handle from a read-only constructor refuses to sync.
    pub fn sync(&self, mode: SyncMode) -> Result<SyncReport, GraphError> {
        self.refuse_read_only()?;
        let mut report = sync::run(self.db_path.path(), self.worktree_root.as_path(), mode)?;
        if mode == SyncMode::Auto {
            self.record_auto_sync_now()?;
        }
        self.name_target(&mut report);
        Ok(report)
    }

    fn refuse_read_only(&self) -> Result<(), GraphError> {
        if self.read_only {
            return Err(GraphError::invalid_data(
                "sync graph",
                format!(
                    "{} was opened read-only; sync it with `orbit-graph sync` or Graph::open",
                    self.db_path.path().display()
                ),
            ));
        }
        Ok(())
    }

    /// Names the database and branch this handle writes in `report`.
    fn name_target(&self, report: &mut SyncReport) {
        report.database_path = self.db_path.path().to_path_buf();
        report.branch = self.db_path.branch().to_string();
    }

    /// Synchronize like [`Graph::sync`], but report per-file progress to
    /// `observer` and stop early once it requests cancellation.
    ///
    /// This method is strictly additive: `Graph::sync` keeps its own
    /// behavior, error surface, and coalescing across concurrent callers on
    /// the same database. This method does not coalesce, and is intended for
    /// a single dedicated indexing thread, such as the change-explorer
    /// service's cold-build worker, that owns its database exclusively.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::fs;
    ///
    /// use orbit_graph::{Graph, SyncMode, SyncObserver, SyncOutcome, SyncPolicy, SyncProgress};
    ///
    /// struct CancelImmediately;
    ///
    /// impl SyncObserver for CancelImmediately {
    ///     fn on_progress(&self, _progress: &SyncProgress) {}
    ///     fn is_cancelled(&self) -> bool {
    ///         true
    ///     }
    /// }
    ///
    /// let dir = tempfile::tempdir()?;
    /// fs::create_dir_all(dir.path().join("src"))?;
    /// fs::write(dir.path().join("src/a.rs"), "pub fn a() -> i32 { 1 }\n")?;
    /// fs::write(dir.path().join("src/b.rs"), "pub fn b() -> i32 { 2 }\n")?;
    ///
    /// let graph = Graph::open(dir.path(), SyncPolicy::Manual)?;
    /// let outcome = graph.sync_with_observer(SyncMode::Full, &CancelImmediately)?;
    /// // Requesting cancellation up front still leaves the store consistent:
    /// // either variant carries a report of exactly the files processed so far.
    /// let report = match outcome {
    ///     SyncOutcome::Completed(report) | SyncOutcome::Cancelled(report) => report,
    /// };
    /// assert!(report.files_indexed <= 2);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn sync_with_observer(
        &self,
        mode: SyncMode,
        observer: &dyn SyncObserver,
    ) -> Result<SyncOutcome, GraphError> {
        self.refuse_read_only()?;
        let mut outcome = sync::run_with_observer(
            self.db_path.path(),
            self.worktree_root.as_path(),
            mode,
            observer,
        )?;
        if mode == SyncMode::Auto && matches!(outcome, SyncOutcome::Completed(_)) {
            self.record_auto_sync_now()?;
        }
        match &mut outcome {
            SyncOutcome::Completed(report) | SyncOutcome::Cancelled(report) => {
                self.name_target(report);
            }
        }
        Ok(outcome)
    }

    /// Return the resolved database path backing this graph handle.
    pub fn db_path(&self) -> &GraphDbPath {
        &self.db_path
    }

    /// Return the source root indexed by this graph handle.
    pub fn worktree_root(&self) -> &Path {
        self.worktree_root.as_path()
    }

    pub(crate) fn ensure_synced(&self) -> Result<(), GraphError> {
        match self.policy {
            SyncPolicy::Manual => Ok(()),
            SyncPolicy::OnRead => self.sync(SyncMode::Auto).map(|_| ()),
            SyncPolicy::Watch { .. } => Ok(()),
            SyncPolicy::Windowed { window } => {
                if sync_window_elapsed(self.last_auto_sync_at(), window)? {
                    self.sync(SyncMode::Auto)?;
                }
                Ok(())
            }
        }
    }

    fn last_auto_sync_at(&self) -> i64 {
        *self
            .last_auto_sync_at
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn record_auto_sync_now(&self) -> Result<(), GraphError> {
        let now = now_epoch_nanos("record graph auto sync timestamp")?;
        let mut last_auto_sync_at = self
            .last_auto_sync_at
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *last_auto_sync_at = now;
        Ok(())
    }

    pub(crate) fn with_read_connection<T>(
        &self,
        run: impl FnOnce(&Connection) -> Result<T, GraphError>,
    ) -> Result<T, GraphError> {
        let conn = self
            .read_conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        run(&conn)
    }

    /// Search indexed symbols, strings, and config keys.
    pub fn search(&self, q: &SearchQuery) -> Result<SearchResult, GraphError> {
        self.ensure_synced()?;
        query::search::run(self, q)
    }

    /// Show the node or source view addressed by `sel`.
    ///
    /// `max_bytes` bounds the returned source slice. [`crate::DEFAULT_SHOW_MAX_BYTES`]
    /// is the intended CLI/MCP default.
    pub fn show(&self, sel: &Selector, max_bytes: usize) -> Result<Option<NodeView>, GraphError> {
        self.ensure_synced()?;
        query::show::run(self, sel, max_bytes)
    }

    /// Return inbound references and relations for `sel`.
    pub fn refs(&self, sel: &Selector, opts: &RefOpts) -> Result<RefResult, GraphError> {
        self.ensure_synced()?;
        query::refs::run(self, sel, opts)
    }

    /// Return every outbound call edge from `sel`, unresolved ones included
    /// (nothing is hidden, so there is no count to report).
    pub fn callees(&self, sel: &Selector) -> Result<Vec<CalleeEdge>, GraphError> {
        self.callees_with_options(sel, &CalleeOpts::all())
    }

    /// Return outbound call edges from `sel` filtered by `opts`.
    ///
    /// This drops [`CalleeReport::hidden_unresolved`]. A caller that sets
    /// [`CalleeOpts::hide_unresolved`] and shows the result to anyone should
    /// call [`Graph::callees_report`] instead and report what it hid; that
    /// method is the one place the filter is applied.
    pub fn callees_with_options(
        &self,
        sel: &Selector,
        opts: &CalleeOpts,
    ) -> Result<Vec<CalleeEdge>, GraphError> {
        Ok(self.callees_report(sel, opts)?.callees)
    }

    /// Return outbound call edges from `sel` filtered by `opts`, with the
    /// number of unresolved edges [`CalleeOpts::hide_unresolved`] omitted.
    pub fn callees_report(
        &self,
        sel: &Selector,
        opts: &CalleeOpts,
    ) -> Result<CalleeReport, GraphError> {
        self.ensure_synced()?;
        query::callees::run(self, sel, opts)
    }

    /// Return the bounded impact set around `sel`.
    pub fn impact(
        &self,
        sel: &Selector,
        depth: u8,
        min_confidence: Confidence,
    ) -> Result<ImpactResult, GraphError> {
        self.impact_with_direction(sel, depth, min_confidence, ImpactDirection::Both)
    }

    /// Return the bounded impact set around `sel` in `direction`.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::fs;
    ///
    /// use orbit_graph::{Confidence, Graph, ImpactDirection, Selector, SyncMode, SyncPolicy};
    ///
    /// let dir = tempfile::tempdir()?;
    /// fs::create_dir_all(dir.path().join("src"))?;
    /// fs::write(
    ///     dir.path().join("src/lib.rs"),
    ///     "pub fn helper() -> i32 { 1 }\npub fn entry() -> i32 { helper() }\n",
    /// )?;
    ///
    /// let graph = Graph::open(dir.path(), SyncPolicy::Manual)?;
    /// graph.sync(SyncMode::Full)?;
    ///
    /// let selector: Selector = "symbol:src/lib.rs#entry:function".parse()?;
    /// let impact = graph.impact_with_direction(
    ///     &selector,
    ///     3,
    ///     Confidence::default(),
    ///     ImpactDirection::Outbound,
    /// )?;
    /// assert!(
    ///     impact
    ///         .touched
    ///         .iter()
    ///         .any(|entry| entry.qualified_name.contains("helper"))
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn impact_with_direction(
        &self,
        sel: &Selector,
        depth: u8,
        min_confidence: Confidence,
        direction: ImpactDirection,
    ) -> Result<ImpactResult, GraphError> {
        self.ensure_synced()?;
        query::impact::run(self, sel, depth, min_confidence, direction)
    }

    /// Trace the call tree rooted at a command handler.
    pub fn trace(
        &self,
        command: &str,
        depth: u8,
        min_confidence: Confidence,
    ) -> Result<TraceResult, GraphError> {
        self.ensure_synced()?;
        query::trace::run(self, command, depth, min_confidence)
    }

    /// Summarize indexed files and symbols, optionally scoped to a `dir:` or
    /// `file:` selector. Passing `None` summarizes the whole worktree.
    pub fn overview(
        &self,
        scope: Option<&Selector>,
        format: OverviewFormat,
    ) -> Result<OverviewResult, GraphError> {
        self.ensure_synced()?;
        query::overview::run(self, scope, format)
    }

    /// Return the concrete types implementing the trait addressed by `sel`.
    pub fn implementors(&self, sel: &Selector) -> Result<ImplementorsResult, GraphError> {
        self.ensure_synced()?;
        query::implementors::run(self, sel)
    }

    /// Return outbound module/import edges for the files addressed by `sel`
    /// (a `file:` or `dir:` selector).
    pub fn deps(&self, sel: &Selector) -> Result<DepsResult, GraphError> {
        self.ensure_synced()?;
        query::deps::run(self, sel)
    }

    /// Return every recorded runtime-invocation site: a call that starts a
    /// program the syntax names, such as `subprocess.run(["prog", ...])` or
    /// `Command::new(env!("CARGO_BIN_EXE_prog"))`.
    ///
    /// Each entry's `program` is the opaque program string as written (for an
    /// interpreter launch, the `.py` script path); it is never resolved to a
    /// symbol. Ordered by file, then source position.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::fs;
    ///
    /// use orbit_graph::{Graph, SyncMode, SyncPolicy};
    ///
    /// let dir = tempfile::tempdir()?;
    /// fs::create_dir_all(dir.path().join("tests"))?;
    /// fs::write(
    ///     dir.path().join("tests/cli.rs"),
    ///     "#[test]\nfn runs() {\n    Command::new(env!(\"CARGO_BIN_EXE_tool\"));\n}\n",
    /// )?;
    ///
    /// let graph = Graph::open(dir.path(), SyncPolicy::Manual)?;
    /// graph.sync(SyncMode::Full)?;
    ///
    /// let invocations = graph.runtime_invocations()?;
    /// assert_eq!(invocations.len(), 1);
    /// assert_eq!(invocations[0].program, "tool");
    /// assert_eq!(invocations[0].line, 3);
    /// assert_eq!(invocations[0].symbol.as_ref().map(|symbol| symbol.name.as_str()), Some("runs"));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn runtime_invocations(&self) -> Result<Vec<RuntimeInvocation>, GraphError> {
        self.ensure_synced()?;
        query::runtime::invocations(self)
    }

    /// Return the program names the source file at `path` (worktree-relative)
    /// ships under, read from its nearest manifests: the nearest `Cargo.toml`
    /// with a `[package]` (package name, `[[bin]]` names, `src/bin` targets)
    /// and the nearest `pyproject.toml` (`[project.scripts]` and
    /// `[tool.poetry.scripts]` keys).
    ///
    /// Manifests are read from the worktree at query time; one that is missing
    /// or does not parse contributes nothing. A path that leaves the worktree
    /// is refused.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::fs;
    ///
    /// use orbit_graph::{Graph, ProgramNameSource, SyncPolicy};
    ///
    /// let dir = tempfile::tempdir()?;
    /// fs::create_dir_all(dir.path().join("src"))?;
    /// fs::write(
    ///     dir.path().join("Cargo.toml"),
    ///     "[package]\nname = \"tool\"\n\n[[bin]]\nname = \"tool-cli\"\npath = \"src/main.rs\"\n",
    /// )?;
    ///
    /// let graph = Graph::open(dir.path(), SyncPolicy::Manual)?;
    /// let names = graph.program_names("src/lib.rs")?;
    /// assert!(names.iter().any(|name| name.name == "tool"
    ///     && name.source == ProgramNameSource::CargoPackage));
    /// assert!(names.iter().any(|name| name.name == "tool-cli"
    ///     && name.source == ProgramNameSource::CargoBin));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn program_names(&self, path: &str) -> Result<Vec<ProgramName>, GraphError> {
        query::runtime::program_names(self, path)
    }
}

fn sync_window_elapsed(last_incremental_at: i64, window: Duration) -> Result<bool, GraphError> {
    if last_incremental_at < 0 {
        return Err(GraphError::invalid_data(
            "check graph sync policy window",
            format!("last_incremental_at is negative: {last_incremental_at}"),
        ));
    }
    if last_incremental_at == 0 {
        return Ok(true);
    }

    let now = now_epoch_nanos("check graph sync policy window")?;
    let elapsed = now.saturating_sub(last_incremental_at);
    Ok(u128::try_from(elapsed).map_err(|source| {
        GraphError::invalid_data("check graph sync policy window", source.to_string())
    })? > window.as_nanos())
}

/// Opens a read connection with the standard bounded busy wait and foreign
/// keys.
pub(crate) fn open_read_connection(
    db_path: &Path,
    operation: &'static str,
) -> Result<Connection, GraphError> {
    let conn = Connection::open(db_path).map_err(|source| GraphError::sqlite(operation, source))?;
    conn.pragma_update(None, "busy_timeout", 5_000)
        .map_err(|source| GraphError::sqlite("set busy_timeout for graph read", source))?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|source| GraphError::sqlite("enable foreign keys for graph read", source))?;
    Ok(conn)
}

fn read_last_incremental_at(conn: &Connection, operation: &'static str) -> Result<i64, GraphError> {
    let value = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'last_incremental_at'",
            [],
            |row| row.get::<_, String>(0),
        )
        .map_err(|source| GraphError::sqlite(operation, source))?;
    parse_epoch_nanos(operation, &value)
}

fn parse_epoch_nanos(operation: &'static str, value: &str) -> Result<i64, GraphError> {
    value
        .parse::<i64>()
        .map_err(|source| GraphError::invalid_data(operation, source.to_string()))
}

fn now_epoch_nanos(operation: &'static str) -> Result<i64, GraphError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| {
            GraphError::invalid_data(
                operation,
                format!("system time is before UNIX_EPOCH: {error}"),
            )
        })?;
    i64::try_from(duration.as_nanos())
        .map_err(|error| GraphError::invalid_data(operation, error.to_string()))
}

#[cfg(test)]
mod tests;
