// A library prints nothing: diagnostics go through `tracing` (STD-02 §R15).
#![deny(clippy::print_stderr, clippy::print_stdout)]
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]

//! SQLite-backed source-code graph store and query API.
//!
//! This crate owns the durable graph database path contract, sync policy, and
//! public query surface. It can be embedded as a library or used through the
//! `orbit-graph` JSON command-line interface.

use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use git2::Repository;
use rusqlite::{Connection, params};
use serde::Serialize;

mod evaluation;
/// Pure extraction contracts and language-specific extractors.
mod extract;
mod live_evaluation;
mod lock;
pub mod plugin;
mod query;
mod recommend;
mod store;
mod sync;

// `docs/usage.md`, included so `cargo test --doc` compiles its Rust example
// against this API and a stale example fails the build (STD-04 §R14).
#[cfg(doctest)]
#[doc = include_str!("../../../docs/usage.md")]
mod usage_doc {}

pub use extract::history::{
    CHANGE_EXTRACTOR_VERSION, CurrentRevisionResolution, CurrentSymbolStatus,
    DEFAULT_HISTORY_SYNC_LIMIT, DELIVERY_IMPORT_SCHEMA_VERSION, DeliveredChange, DeliveryEvidence,
    DeliveryImport, FileChange, FileChangeKind, FileFallbackReason, Provenance, RevisionSide,
    SymbolAttribution, SymbolChange, SymbolIdentity, SymbolMatchConfidence, TaskAssociation,
    TaskTextAvailability, TemporalFact, TemporalStatus,
};
pub use extract::{Selector, SelectorParseError};
pub use store::history::{
    HISTORY_INDEX_SCHEMA_VERSION, HistoryImportReport, HistoryIndex, HistoryRebuildReport,
    HistoryStatus, HistorySyncReport,
};

pub use evaluation::{
    EVALUATION_CORPUS_SCHEMA_VERSION, EVALUATION_SCHEMA_VERSION, EvaluationCorpus,
    EvaluationReport, evaluate_corpus,
};
pub use live_evaluation::{LiveGitEvaluation, LiveGitReport, evaluate_live_git};
pub use query::{
    DEFAULT_SEARCH_LIMIT, DEFAULT_SHOW_MAX_BYTES, Match, NodeMetadata, NodeView, SearchKind,
    SearchQuery, SearchResult, SourceSpan,
};
pub use recommend::{
    DEFAULT_RECOMMENDATION_LIMIT, HybridTaskHit, Recommendation, RecommendationAssociation,
    RecommendationCounts, RecommendationEngine, RecommendationFallback, RecommendationFreshness,
    RecommendationFreshnessStatus, RecommendationInput, RecommendationLevel, RecommendationReason,
    RecommendationRequest, RecommendationResult, RecommendationVariant,
};

#[cfg(test)]
mod tests;

/// Extractor/storage compatibility version embedded in graph database names.
///
/// Bump this when extractor output or storage expectations change
/// incompatibly. Older graph DB files then become invisible to the active
/// graph handle and are removed by the next `orbit-graph sync` or `clean --confirm`
/// whose lock on them is free; a newer version's files are never removed.
///
/// A bump also changes the committed `direct-call` sample export, which
/// records this number: regenerate it in the same change with
/// `UPDATE_GOLDENS=1 cargo test -p orbit-graph-explorer --test derived_artifacts --locked`.
// L-0052: FTS population invariants require a fresh DB when old indexes may be empty.
/// Version 5 rebuilds stored refs so qualified cross-file and explicit-import
/// resolution records stable symbol hints.
///
/// Version 6 rebuilds stored refs again: the Rust and Python call extractors
/// now recurse into method-chain receivers (previously dropped) and no
/// longer emit a chain receiver's full source text as a bogus callee name
/// for turbofish method calls (`x.collect::<Vec<_>>()`).
///
/// Version 7 rebuilds stored refs once more: Rust `impl` blocks are indexed
/// under the implemented type's name, and pass 2 now excludes them from ref
/// candidate lookups, so a type with any impl block resolves cross-file
/// references instead of degrading every one of them to `fuzzy_name`.
///
/// Version 8 rebuilds stored refs once more: a method call whose receiver type
/// the extractor cannot determine (`args.execute()`) is recorded with its
/// receiver, and pass 2 no longer resolves such a call by short name alone, so
/// a dispatcher no longer reports its own dispatch lines as `exact` inbound
/// references to itself.
///
/// Version 9 includes nested Rust and Python function definitions, so their
/// symbols and call ownership are present in rebuilt snapshots.
///
/// Version 10 rebuilds stored refs once more: calls on an impl's literal `self`
/// receiver and `Self::method()` now retain that impl type, so they can resolve
/// to the intended method rather than joining the name-only fallback.
///
/// Version 11 records `runtime_invocation` refs for Python `subprocess` and
/// Rust `Command::new` / `Command::cargo_bin` calls that name the program they
/// start, so [`Graph::runtime_invocations`] has rows to report.
///
/// Version 12 adds indexes on the store's foreign-key child columns and on
/// `imports(from_file)`, which reference resolution and per-file rewrites
/// look up; the stored rows are unchanged.
///
/// Version 13 rebuilds stored refs once more: Rust calls written inside macro
/// invocation arguments (`assert!(f(x))`, `vec![g(y)]`, `format!("{}", h())`)
/// are recovered from the macro's token tree, and a function passed by name
/// as a call argument (`.map(skill_link_roots)`) is recorded as a call.
///
/// Version 14 rebuilds stored refs once more: a Rust method call on a local
/// binding whose type is spelled (a typed parameter or `let`, or a
/// `T::new(..)`/`T { .. }` initialiser) records `<T>::method`, and pass 2
/// resolves `<T>::m`, `<T as Trait>::m`, and scoped `T::m(..)` targets against
/// that type's inherent, trait-impl, or trait-declared member instead of by
/// short name.
///
/// Version 15 stores each ref's resolution inputs (`extracted_qualified`,
/// `unresolved_receiver`, `spelled_path`), so an incremental sync can
/// re-resolve refs in unchanged files whose target was defined, removed or
/// renamed elsewhere.
///
/// Version 16 rebuilds stored refs so cross-file resolution only chooses
/// symbols from the ref's language.
///
/// Version 17 skips files larger than 4 MiB and files whose tree-sitter parse
/// exceeds its per-file deadline, so neither gets rows.
///
/// Version 18 makes pass 2's commit the only point where a file becomes
/// current, so an interrupted sync is repaired by the next one. It also
/// rebuilds indexes that earlier interrupted syncs left with files marked
/// current but missing their refs.
pub const EXTRACTOR_VERSION: u32 = 18;

/// SQLite schema version used by the graph store.
///
/// # Examples
///
/// ```
/// use orbit_graph::STORE_SCHEMA_VERSION;
///
/// assert!(STORE_SCHEMA_VERSION >= 1);
/// ```
pub const STORE_SCHEMA_VERSION: u32 = store::schema::SCHEMA_VERSION;

/// Default graph distance used by callers that do not supply `--depth`.
pub const DEFAULT_IMPACT_DEPTH: u8 = 3;

/// Default call-tree distance used by command traces when depth is omitted.
pub const DEFAULT_TRACE_DEPTH: u8 = 5;

/// Maximum number of impacted symbols returned by bounded traversals.
pub const IMPACT_NODE_CAP: usize = 200;

/// Maximum number of trace nodes returned by command traces.
pub const TRACE_NODE_CAP: usize = 200;

/// Opaque handle to a worktree-scoped graph database.
pub struct Graph {
    db_path: GraphDbPath,
    worktree_root: PathBuf,
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
    /// no other database; [`clean_old_databases`] does. A reader that must
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
    /// not [`STORE_SCHEMA_VERSION`] is [`GraphError::IndexIncompatible`].
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
    /// directory under `$ORBIT_PLUGIN_STATE`, which is marked with a
    /// `.gitignore` like the default scratch directory.
    pub(crate) fn open_in_plugin_state(
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
    pub(crate) fn open_read_only(worktree_root: &Path, db_path: &Path) -> Result<Self, GraphError> {
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
    /// `max_bytes` bounds the returned source slice. [`DEFAULT_SHOW_MAX_BYTES`]
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

/// Internal result of resolving a Selector to a symbol's file and span for
/// span-containment queries like callees.
#[derive(Debug, Clone)]
pub(crate) struct SymbolSpan {
    pub(crate) file_path: String,
    pub(crate) span_start: i64,
    pub(crate) span_end: i64,
}

/// Resolve a Selector to a single symbol's (file_path, span) if it exists in
/// the graph. Returns None for selectors that do not map to a stored symbol
/// (including non-Symbol variants and unknown names). Used by read queries
/// that then perform containment or adjacency lookups.
pub(crate) fn resolve_symbol_span(
    conn: &Connection,
    sel: &Selector,
) -> Result<Option<SymbolSpan>, GraphError> {
    let Selector::Symbol { path, symbol, kind } = sel else {
        return Ok(None);
    };

    // Match on either short name or qualified; apply kind filter when provided.
    // Paths in DB are normalized (slash-separated, relative to worktree).
    // An exact qualified match wins over a short-name match, so a printed
    // selector such as `#helper` names the top-level `helper`, not a nested
    // `inner::helper` that happens to have a lower id (STD-01 §R32).
    let mut sql = String::from(
        "SELECT file_path, span_start, span_end FROM symbols
         WHERE file_path = ?1 AND (name = ?2 OR qualified = ?2)",
    );
    let has_kind = !kind.trim().is_empty();
    if has_kind {
        sql.push_str(" AND kind = ?3");
    }
    sql.push_str(" ORDER BY CASE WHEN qualified = ?2 THEN 0 ELSE 1 END, id LIMIT 1");

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|source| GraphError::sqlite("prepare symbol resolve for query", source))?;

    let row = if has_kind {
        stmt.query_row(params![path, symbol, kind.trim()], |r| {
            Ok(SymbolSpan {
                file_path: r.get(0)?,
                span_start: r.get(1)?,
                span_end: r.get(2)?,
            })
        })
    } else {
        stmt.query_row(params![path, symbol], |r| {
            Ok(SymbolSpan {
                file_path: r.get(0)?,
                span_start: r.get(1)?,
                span_end: r.get(2)?,
            })
        })
    };

    match row {
        Ok(s) => Ok(Some(s)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(source) => Err(GraphError::sqlite("resolve symbol for query", source)),
    }
}

/// Graph crate error surface.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum GraphError {
    /// A filesystem operation failed while opening graph storage.
    Io {
        /// Operation being performed.
        operation: &'static str,
        /// Filesystem path involved in the failed operation.
        path: PathBuf,
        /// Source error rendered as text for cloneable error propagation.
        reason: String,
    },
    /// A SQLite operation failed while opening or initializing graph storage.
    Sqlite {
        /// Operation being performed.
        operation: &'static str,
        /// Source error rendered as text for cloneable error propagation.
        reason: String,
    },
    /// Stored or discovered graph data was invalid.
    InvalidData {
        /// Operation being performed.
        operation: &'static str,
        /// Validation failure rendered as text for cloneable error propagation.
        reason: String,
    },
    /// A read found no index where it looked; nothing was created.
    IndexMissing {
        /// Path of the index that does not exist yet.
        path: PathBuf,
        /// Actionable message naming the command that builds the index.
        reason: String,
    },
    /// An index exists but its stored schema identity is not the one this
    /// binary reads, so it is neither read nor written.
    IndexIncompatible {
        /// Path of the incompatible index.
        path: PathBuf,
        /// Actionable message naming the stored and expected identities.
        reason: String,
    },
    /// Placeholder variant until storage, sync, and query errors are defined.
    Unimplemented,
}

impl GraphError {
    /// Build an [`GraphError::Io`] failure for `operation` on `path`.
    ///
    /// The variant is `#[non_exhaustive]`, so this constructor is also how
    /// callers outside the crate — the `orbit-graph` CLI among them — report a
    /// filesystem failure in the graph error vocabulary.
    pub fn io(operation: &'static str, path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            operation,
            path: path.into(),
            reason: source.to_string(),
        }
    }

    pub(crate) fn sqlite(operation: &'static str, source: rusqlite::Error) -> Self {
        Self::sqlite_message(operation, source.to_string())
    }

    pub(crate) fn sqlite_message(operation: &'static str, reason: impl Into<String>) -> Self {
        Self::Sqlite {
            operation,
            reason: reason.into(),
        }
    }

    /// Build a [`GraphError::InvalidData`] failure for `operation`.
    ///
    /// Public for the same reason as [`GraphError::io`]: the variant cannot be
    /// constructed literally outside this crate.
    pub fn invalid_data(operation: &'static str, reason: impl Into<String>) -> Self {
        Self::InvalidData {
            operation,
            reason: reason.into(),
        }
    }
}

impl Display for GraphError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io {
                operation,
                path,
                reason,
            } => write!(f, "{operation} at {}: {reason}", path.display()),
            Self::Sqlite { operation, reason } => write!(f, "{operation}: {reason}"),
            Self::InvalidData { operation, reason } => write!(f, "{operation}: {reason}"),
            Self::IndexMissing { reason, .. } | Self::IndexIncompatible { reason, .. } => {
                f.write_str(reason)
            }
            Self::Unimplemented => f.write_str("graph operation is not implemented"),
        }
    }
}

impl std::error::Error for GraphError {}

/// Sync mode requested by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncMode {
    /// Incremental sync driven by file metadata and content hashes.
    Auto,
    /// Full sync that rehashes and re-extracts all indexable files.
    Full,
}

/// Policy controlling whether reads refresh the graph before querying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncPolicy {
    /// Never auto-sync; callers invoke [`Graph::sync`] explicitly.
    Manual,
    /// Sync inline on every query.
    OnRead,
    /// Sync inline only if the last successful sync is older than `window`.
    Windowed {
        /// Maximum age of the last successful sync before reads refresh.
        window: Duration,
    },
    /// Run an initial sync at open, then keep the index fresh with a background watcher.
    Watch {
        /// Event coalescing window before a background sync starts.
        debounce: Duration,
    },
}

/// Summary returned after a graph sync completes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncReport {
    /// Number of files present in the graph after sync.
    pub files_indexed: usize,
    /// Number of files inserted or refreshed by this sync.
    pub files_changed: usize,
    /// Number of files removed from the graph by this sync.
    pub files_removed: usize,
    /// Wall-clock duration spent syncing.
    pub duration: Duration,
    /// Paths this sync could not read or extract, one entry each. The sync
    /// isolated them and indexed everything else; an already indexed path
    /// among them keeps its previous rows.
    pub failed: Vec<SyncFailure>,
    /// Paths this sync deliberately did not index, such as files larger than
    /// the 4 MiB byte cap.
    pub skipped: Vec<SyncSkip>,
    /// The graph database this sync wrote.
    pub database_path: PathBuf,
    /// The branch the graph database indexes.
    pub branch: String,
}

/// A path a sync could not read or extract. See [`SyncReport::failed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncFailure {
    /// Worktree-relative path, `/`-separated.
    pub path: String,
    /// What the sync was doing, such as `scan directory` or
    /// `read file for content hash`.
    pub operation: String,
    /// Stable class of the error: `permission_denied`, `not_found`, `io`,
    /// `invalid_data`, `parse_timeout`, `unsupported` or `panic`.
    pub error_kind: String,
    /// The error as reported, for diagnosis.
    pub message: String,
}

/// A path a sync deliberately did not index. See [`SyncReport::skipped`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncSkip {
    /// Worktree-relative path, `/`-separated.
    pub path: String,
    /// Why it was skipped; `oversize` for a file above the byte cap.
    pub reason: String,
}

/// Phase of [`Graph::sync_with_observer`] a [`SyncProgress`] report describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncPhase {
    /// Pass 1: changed files are read, parsed, and written; symbols, imports,
    /// and raw refs are extracted. `units_done`/`units_total` mirror
    /// `files_indexed`/`files_seen` in this phase.
    Extracting,
    /// Pass 2: raw refs extracted from every touched file are resolved
    /// against the confidence ladder and written. `files_seen`,
    /// `files_indexed`, and `current_path` are frozen at pass 1's final
    /// values during this phase; `units_done`/`units_total` count refs
    /// resolved so far and refs to resolve.
    Resolving,
}

impl SyncPhase {
    /// Stable label used by callers that serialize this phase, such as the
    /// change-explorer service.
    pub fn label(self) -> &'static str {
        match self {
            Self::Extracting => "extracting",
            Self::Resolving => "resolving",
        }
    }
}

/// Progress observed while [`Graph::sync_with_observer`] processes files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncProgress {
    /// Phase this report describes.
    pub phase: SyncPhase,
    /// Total files this sync will touch, written or removed. Pass 1 extracts
    /// in bounded chunks, so during [`SyncPhase::Extracting`] this starts as
    /// every changed or removed file and drops by each file whose extraction
    /// fails as its chunk is extracted. Frozen at pass 1's final count once
    /// `phase` is [`SyncPhase::Resolving`].
    pub files_seen: usize,
    /// Files this sync has processed so far; increases monotonically up to
    /// `files_seen`. Frozen at pass 1's final count once `phase` is
    /// [`SyncPhase::Resolving`].
    pub files_indexed: usize,
    /// Path of the file most recently processed, once any has been. Frozen
    /// at pass 1's last file once `phase` is [`SyncPhase::Resolving`].
    pub current_path: Option<String>,
    /// Units processed so far within `phase`; increases monotonically up to
    /// `units_total` and resets at the start of each new phase.
    pub units_done: usize,
    /// Total units `phase` will process.
    pub units_total: usize,
}

/// Observes progress across both passes of [`Graph::sync_with_observer`] and
/// can request cancellation of pass 1.
///
/// The cancel check runs only between files during pass 1 (extraction). A
/// file becomes current only when pass 2 commits its references, so a
/// cancelled sync, which skips pass 2, leaves every file it touched looking
/// unsynced: the next sync, incremental or full, extracts those files again,
/// resolves their references, and re-resolves the references elsewhere that
/// the interrupted sync may have affected.
///
/// Pass 2 (reference resolution) is not cancellable: once pass 1 completes
/// without cancellation, pass 2 resolves every collected ref to completion
/// inside one SQLite transaction before this call returns. A cancellation
/// request that arrives while pass 2 is running has no effect on that sync;
/// it takes effect at the next sync's pass 1.
pub trait SyncObserver: Send + Sync {
    /// Called once before the first file of pass 1 (with the total already
    /// known), again after each file pass 1 touches, and — once pass 1
    /// completes without cancellation — once before pass 2 starts resolving
    /// refs, at a bounded cadence while it resolves them, and once after the
    /// last one.
    fn on_progress(&self, progress: &SyncProgress);
    /// Checked between files during pass 1; once this returns `true`, no
    /// further file in pass 1 starts. Not polled during pass 2.
    fn is_cancelled(&self) -> bool;
}

/// Outcome of a sync run through [`Graph::sync_with_observer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncOutcome {
    /// The sync processed every file it found.
    Completed(SyncReport),
    /// The observer requested cancellation; the report covers exactly the
    /// files pass 1 processed before that point. None of them is current
    /// until a later sync completes (see [`SyncObserver`]).
    Cancelled(SyncReport),
}

/// Resolved, worktree-scoped graph database path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphDbPath {
    path: PathBuf,
    branch: String,
    extractor_version: u32,
}

impl GraphDbPath {
    fn new(path: PathBuf, branch: String, extractor_version: u32) -> Self {
        Self {
            path,
            branch,
            extractor_version,
        }
    }

    /// Return the canonical SQLite database path.
    pub fn path(&self) -> &Path {
        self.path.as_path()
    }

    /// Return the unsanitized branch name represented by this database path.
    pub fn branch(&self) -> &str {
        self.branch.as_str()
    }

    /// Return the extractor version embedded in the database filename.
    pub fn extractor_version(&self) -> u32 {
        self.extractor_version
    }

    /// Return the graph store schema version.
    pub const fn schema_version(&self) -> u32 {
        STORE_SCHEMA_VERSION
    }
}

/// Resolve the graph database path the worktree's current branch or detached
/// commit selects, as [`Graph::open`] would, without creating anything.
///
/// Only an unborn branch, or a directory outside any Git repository, selects
/// the `HEAD` family; any other failure to read `HEAD` is an error.
///
/// # Examples
///
/// ```
/// use orbit_graph::{EXTRACTOR_VERSION, resolve_worktree_db_path};
///
/// let dir = tempfile::tempdir()?;
/// let db_path = resolve_worktree_db_path(dir.path())?;
/// assert_eq!(db_path.branch(), "HEAD");
/// assert_eq!(db_path.extractor_version(), EXTRACTOR_VERSION);
/// assert!(!db_path.path().exists());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn resolve_worktree_db_path(worktree_root: &Path) -> Result<GraphDbPath, GraphError> {
    store::resolve_worktree_db_path(worktree_root)
}

/// Resolve the canonical graph database path for a worktree and branch.
///
/// The filename sanitizes the branch with a conservative filesystem-safe
/// allowlist, while the returned [`GraphDbPath`] keeps the raw branch name for
/// future `meta.branch` storage.
pub fn resolve_db_path(worktree_root: &Path, branch: &str, extractor_version: u32) -> GraphDbPath {
    resolve_db_path_for_commit(worktree_root, branch, "", extractor_version)
}

/// Resolve the canonical graph database path, using per-commit filenames for detached HEAD.
///
/// Branch-attached graphs keep the branch-scoped filename. Detached HEAD graphs
/// use `detached-<short-sha>.<version>.db` when a commit SHA is available so
/// concurrent detached checkouts on different commits do not churn the same DB.
pub fn resolve_db_path_for_commit(
    worktree_root: &Path,
    branch: &str,
    commit_sha: &str,
    extractor_version: u32,
) -> GraphDbPath {
    let filename_stem = graph_db_filename_stem(branch, commit_sha);
    let filename = format!("{filename_stem}.{extractor_version}.db");
    GraphDbPath::new(
        worktree_root.join(".orbit-graph").join(filename),
        branch.to_string(),
        extractor_version,
    )
}

fn graph_db_filename_stem(branch: &str, commit_sha: &str) -> String {
    if branch == "HEAD" {
        detached_commit_prefix(commit_sha)
            .map(|prefix| format!("detached-{prefix}"))
            .unwrap_or_else(|| sanitize_branch_for_filename(branch))
    } else {
        sanitize_branch_for_filename(branch)
    }
}

fn detached_commit_prefix(commit_sha: &str) -> Option<&str> {
    let commit_sha = commit_sha.trim();
    let prefix = commit_sha.get(..12)?;
    if prefix.chars().all(|ch| ch.is_ascii_hexdigit()) {
        Some(prefix)
    } else {
        None
    }
}

fn sanitize_branch_for_filename(branch: &str) -> String {
    if branch.is_empty() {
        return "_".to_string();
    }

    let chars = branch.chars().collect::<Vec<_>>();
    let mut sanitized = String::with_capacity(branch.len());
    for (index, ch) in chars.iter().copied().enumerate() {
        let is_dot = ch == '.';
        let is_double_dot = is_dot
            && ((index > 0 && chars[index - 1] == '.')
                || (index + 1 < chars.len() && chars[index + 1] == '.'));
        let allowed = ch.is_ascii_alphanumeric()
            || ch == '_'
            || ch == '-'
            || (index > 0 && is_dot && !is_double_dot);

        sanitized.push(if allowed { ch } else { '_' });
    }
    sanitized
}

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

/// Reference query options for [`Graph::refs`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefOpts {
    /// Minimum confidence included in returned results.
    pub confidence: RefConfidence,
    /// Optional kind filter for textual refs or structural relations.
    pub kind: Option<RefKind>,
}

/// Confidence floor and output value for graph references.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RefConfidence {
    /// Same file, unambiguous match on name and qualified path.
    Exact,
    /// Cross-file reference resolved through an explicit import.
    ImportResolved,
    /// Cross-file reference resolved within the same module namespace.
    #[default]
    SameModule,
    /// Name-only match with ambiguous or weak resolution.
    FuzzyName,
}

/// Confidence floor used by reference and traversal queries.
pub use RefConfidence as Confidence;

/// Filterable reference and relation kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RefKind {
    /// Function or method call reference.
    Call,
    /// Type usage reference.
    Type,
    /// Import or use-statement reference.
    Use,
    /// Trait-bound reference.
    TraitBound,
    /// Implementation relation.
    Impl,
    /// Inheritance relation.
    Extends,
    /// Interface implementation relation.
    Implements,
}

/// Reference query result returned by [`Graph::refs`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefResult {
    /// Resolved target symbol, or the unresolved selector target.
    pub target: RefTarget,
    /// Textual references anchored to a source file and span.
    pub refs: Vec<RefEntry>,
    /// Structural relations whose destination is the target symbol.
    pub relations: Vec<RelationEntry>,
    /// Number of candidate rows excluded by the confidence floor.
    pub skipped_low_confidence: usize,
    /// Whether `fallback` is present: `true` means the requested floor found
    /// no textual `refs` and the rows under `fallback.refs` are name-only
    /// matches. Always serialized, so a reader never has to infer it from a
    /// missing key.
    pub fallback_used: bool,
    /// Lower-confidence references surfaced because the precise floor found no
    /// textual `refs`. Present only when the precise result was empty and a
    /// lower-confidence (`fuzzy_name`) match exists — see [`RefFallback`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback: Option<RefFallback>,
}

/// Lower-confidence references surfaced when the precise floor returned no refs.
///
/// Cross-crate call sites routed through `pub use` re-exports resolve only at
/// `fuzzy_name` (name-only) confidence, which the default `same_module` floor
/// excludes. When the precise `refs` list is empty, the query falls back to the
/// fuzzy floor so a genuinely-referenced public API does not look unreferenced.
/// These matches are name-only and may include unrelated symbols sharing the name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefFallback {
    /// Confidence floor used to produce the fallback references (`fuzzy_name`).
    pub confidence: RefConfidence,
    /// Fallback references, each labelled with its own resolution confidence.
    pub refs: Vec<RefEntry>,
    /// Human-readable explanation of why these lower-confidence refs are shown.
    pub note: String,
}

/// Target metadata included in a [`RefResult`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefTarget {
    /// Short symbol name requested or resolved.
    pub name: String,
    /// Fully-qualified symbol name used as the graph query key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qualified: Option<String>,
}

/// Textual reference entry returned by [`Graph::refs`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefEntry {
    /// Source file containing the reference.
    pub file: String,
    /// One-based source line containing the reference.
    pub line: usize,
    /// Textual reference kind.
    pub kind: RefKind,
    /// Resolution confidence for this reference.
    pub confidence: RefConfidence,
    /// `symbol:` selector of the innermost indexed symbol enclosing the
    /// reference (the caller, for a call), or `None` when the reference lies
    /// outside every symbol span, such as a top-level statement.
    pub from_selector: Option<String>,
    /// The trimmed source line containing the reference, cut to at most 160
    /// characters with a trailing `…`.
    pub snippet: String,
}

/// Structural relation entry returned by [`Graph::refs`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RelationEntry {
    /// Qualified source symbol for the relation.
    pub from: String,
    /// Structural relation kind.
    pub kind: RefKind,
    /// Source file defining the relation.
    pub file: String,
    /// One-based source line defining the relation.
    pub line: usize,
    /// Resolution confidence for this relation.
    pub confidence: RefConfidence,
}

/// Outbound call edge returned by [`Graph::callees`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CalleeEdge {
    /// Short target name as written in the call site.
    pub target_name: String,
    /// Resolved qualified name (authoritative when present); None for fuzzy/unresolved.
    pub target_qualified: Option<String>,
    /// Resolution confidence assigned to the call edge.
    pub confidence: RefConfidence,
    /// One-based source line containing the call site.
    pub line: usize,
}

/// Options for [`Graph::callees_with_options`].
///
/// # Examples
///
/// ```
/// use std::fs;
///
/// use orbit_graph::{CalleeOpts, Confidence, Graph, Selector, SyncMode, SyncPolicy};
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
/// let opts = CalleeOpts {
///     confidence: Confidence::Exact,
///     kind: None,
///     hide_unresolved: false,
/// };
/// let callees = graph.callees_with_options(&selector, &opts)?;
/// assert_eq!(callees.len(), 1);
/// assert_eq!(callees[0].target_name, "helper");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CalleeOpts {
    /// Minimum confidence included in returned edges.
    pub confidence: RefConfidence,
    /// Optional edge-kind filter. Since this query returns calls, any non-call
    /// kind produces an empty result.
    pub kind: Option<RefKind>,
    /// Omit unresolved call edges (no `target_qualified`) whose call name has
    /// no indexed callable definition (a `function`, `method`, `class`, or
    /// `struct` of that name) anywhere in the graph, such as standard-library
    /// or prelude calls (`map_err`, `Ok`, `to_string`). They are counted in
    /// [`CalleeReport::hidden_unresolved`] instead.
    pub hide_unresolved: bool,
}

impl CalleeOpts {
    /// Return options that preserve the unfiltered [`Graph::callees`] behavior.
    pub const fn all() -> Self {
        Self {
            confidence: RefConfidence::FuzzyName,
            kind: None,
            hide_unresolved: false,
        }
    }
}

/// Outbound call edges plus what [`CalleeOpts::hide_unresolved`] omitted,
/// returned by [`Graph::callees_report`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CalleeReport {
    /// Returned call edges, in call-site order.
    pub callees: Vec<CalleeEdge>,
    /// Unresolved edges with no indexed definition that were omitted; `0`
    /// unless [`CalleeOpts::hide_unresolved`] is set.
    pub hidden_unresolved: usize,
}

/// Bounded impact result returned by [`Graph::impact`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImpactResult {
    /// Traversal direction used to build this result.
    #[serde(skip_serializing_if = "ImpactDirection::is_both")]
    pub direction: ImpactDirection,
    /// Impacted symbols in breadth-first order from the origin.
    pub touched: Vec<ImpactEntry>,
    /// Whether traversal stopped because [`IMPACT_NODE_CAP`] was reached.
    pub truncated: bool,
    /// Number of impacted symbols returned in `touched`.
    pub visited_nodes: usize,
    /// Whether `fallback` is present: `true` means the requested floor reached
    /// no node and `fallback.touched` holds name-only matches. Always
    /// serialized.
    pub fallback_used: bool,
    /// Lower-confidence impact surfaced because the precise floor found no
    /// touched nodes. Present only when a lower-confidence (`fuzzy_name`) match
    /// exists; see [`ImpactFallback`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback: Option<ImpactFallback>,
}

/// Lower-confidence impact surfaced when the precise floor returned no nodes.
///
/// `fuzzy_name` edges are name-only and may include unrelated symbols sharing
/// the same short name. The top-level [`ImpactResult`] remains the result for
/// the requested confidence floor; this fallback is an explicit hint that a
/// lower-confidence blast radius exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImpactFallback {
    /// Confidence floor used to produce the fallback impact (`fuzzy_name`).
    pub confidence: RefConfidence,
    /// Impacted symbols found at the fallback floor.
    pub touched: Vec<ImpactEntry>,
    /// Whether fallback traversal stopped because [`IMPACT_NODE_CAP`] was reached.
    pub truncated: bool,
    /// Number of impacted symbols returned in `touched`.
    pub visited_nodes: usize,
    /// Human-readable explanation of why these lower-confidence nodes are shown.
    pub note: String,
}

/// A symbol reached by [`Graph::impact`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImpactEntry {
    /// Qualified symbol name reached by the traversal.
    pub qualified_name: String,
    /// Whether `qualified_name` names a symbol or a file-attributed call site.
    #[serde(skip_serializing_if = "ImpactOrigin::is_symbol")]
    pub origin: ImpactOrigin,
    /// Breadth-first distance from the origin symbol.
    pub distance: usize,
    /// Edge kind used for the prior hop into this symbol.
    pub edge_kind: RefKind,
    /// Selector that `show`, `refs`, `callees`, or `impact` accept for this
    /// node: `symbol:<file>#<qualified>:<kind>` for an indexed symbol, or
    /// `file:<file>` for a file-attributed call site. `None` when the name
    /// has no indexed definition, such as a trait from another crate.
    pub selector: Option<String>,
    /// Workspace-relative file of the node, when indexed.
    pub file: Option<String>,
    /// One-based line: the symbol's definition line, or for a file-attributed
    /// node the first call site that reached it.
    pub line: Option<usize>,
}

/// Direction followed by an impact traversal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImpactDirection {
    /// Follow callers and reverse structural dependencies.
    Inbound,
    /// Follow callees and forward structural dependencies.
    Outbound,
    /// Follow both directions, preserving the historical behavior.
    #[default]
    Both,
}

impl ImpactDirection {
    fn is_both(&self) -> bool {
        *self == Self::Both
    }
}

/// Origin represented by an [`ImpactEntry`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImpactOrigin {
    /// An indexed symbol node.
    #[default]
    Symbol,
    /// A call site attributed to its file because it lies outside symbol spans.
    File,
}

impl ImpactOrigin {
    fn is_symbol(&self) -> bool {
        *self == Self::Symbol
    }
}

/// Command trace result returned by [`Graph::trace`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TraceResult {
    /// Root command handler node, or `None` when the command is unknown.
    pub root: Option<TraceNode>,
    /// Whether traversal stopped because [`TRACE_NODE_CAP`] was reached.
    pub truncated: bool,
    /// Number of nodes returned in the trace tree, including the root.
    pub visited_nodes: usize,
}

impl TraceResult {
    pub(crate) fn empty() -> Self {
        Self {
            root: None,
            truncated: false,
            visited_nodes: 0,
        }
    }
}

/// A node in a command-handler call tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TraceNode {
    /// Short name as written at the call site, or the handler symbol name for the root.
    pub name: String,
    /// Resolved qualified symbol name when the call target was resolved.
    pub qualified_name: Option<String>,
    /// Resolver confidence for the edge into this node; `None` for the root.
    pub confidence: Option<String>,
    /// Nested callees reached from this symbol.
    pub children: Vec<TraceNode>,
}

/// Output format for [`Graph::overview`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OverviewFormat {
    /// Aggregate counts plus the highest-symbol files, without per-file symbols.
    Summary,
    /// Aggregate counts plus every in-scope file and its symbols.
    Full,
}

/// Repository shape summary returned by [`Graph::overview`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OverviewResult {
    /// Format used to build this result.
    pub format: OverviewFormat,
    /// Scope path the summary was restricted to, or `None` for the whole worktree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Number of indexed files in scope.
    pub total_files: usize,
    /// Number of indexed symbols in scope.
    pub total_symbols: usize,
    /// File counts keyed by language.
    pub languages: BTreeMap<String, usize>,
    /// Symbol counts keyed by symbol kind.
    pub symbol_kinds: BTreeMap<String, usize>,
    /// Files in scope. In `summary` format these are the top files by symbol
    /// count with empty `symbols`; in `full` format every in-scope file with
    /// its symbols.
    pub files: Vec<OverviewFile>,
}

/// A file entry in an [`OverviewResult`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OverviewFile {
    /// Worktree-relative file path.
    pub path: String,
    /// Detected language.
    pub lang: String,
    /// Number of symbols defined in this file.
    pub symbol_count: usize,
    /// Symbols defined in this file; populated only in `full` format.
    pub symbols: Vec<OverviewSymbol>,
}

/// A symbol entry in an [`OverviewFile`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OverviewSymbol {
    /// Short symbol name.
    pub name: String,
    /// Symbol kind.
    pub kind: String,
    /// Fully-qualified symbol name.
    pub qualified: String,
}

/// Trait implementor result returned by [`Graph::implementors`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImplementorsResult {
    /// Trait name matched against, derived from the selector's trailing segment.
    pub trait_name: String,
    /// Concrete types implementing the trait.
    pub implementors: Vec<Implementor>,
}

/// A single trait implementor entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Implementor {
    /// Qualified name of the implementing type.
    pub type_qualified: String,
    /// Trait reference recorded at the impl site (`relations.to_qualified`).
    pub trait_matched: String,
    /// Structural relation kind (`impl` / `implements`).
    pub kind: RefKind,
    /// File defining the implementation.
    pub file: String,
}

/// Outbound module/import edge result returned by [`Graph::deps`].
///
/// Reports source-level import edges, not the Cargo crate dependency graph that
/// v1 `orbit.graph.deps` returned. Computed by the internal `query::deps`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DepsResult {
    /// Selector scope echoed back.
    pub scope: String,
    /// Outbound import edges in scope.
    pub imports: Vec<DepEdge>,
}

/// A single outbound import edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DepEdge {
    /// Source file that declares the import.
    pub from_file: String,
    /// Imported module path or specifier (language-specific opaque string).
    pub target_path: String,
    /// Imported symbol, or `None` for a whole-module import.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_symbol: Option<String>,
}

/// One runtime-invocation site returned by [`Graph::runtime_invocations`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuntimeInvocation {
    /// Source file containing the invoking call.
    pub file: String,
    /// One-based source line where the invoking call starts.
    pub line: usize,
    /// Program string as written: the first argv element, a shell command's
    /// first token, a Cargo binary name, or the `.py` script an interpreter
    /// launch runs. Opaque; never resolved to a symbol.
    pub program: String,
    /// Innermost indexed symbol whose span contains the call, or `None` when
    /// the call lies outside every symbol span.
    pub symbol: Option<RuntimeInvocationSymbol>,
}

/// Enclosing symbol of a [`RuntimeInvocation`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuntimeInvocationSymbol {
    /// Short symbol name.
    pub name: String,
    /// Symbol kind.
    pub kind: String,
    /// Fully-qualified symbol name.
    pub qualified: String,
}

/// One program name returned by [`Graph::program_names`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ProgramName {
    /// Program name as the manifest declares it.
    pub name: String,
    /// Which manifest entry declared it.
    pub source: ProgramNameSource,
    /// Worktree-relative path of the declaring manifest.
    pub manifest: String,
}

/// Manifest entry kinds that name a program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgramNameSource {
    /// `Cargo.toml` `[package].name`.
    CargoPackage,
    /// `Cargo.toml` `[[bin]].name`, or a `src/bin` target.
    CargoBin,
    /// A `pyproject.toml` `[project.scripts]` or `[tool.poetry.scripts]` key.
    PyprojectScript,
}
