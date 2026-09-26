//! Graph SQLite schema from `GRAPH_SPEC.md` section 6.2.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::GraphError;

pub(crate) const SCHEMA_VERSION: u32 = 1;

pub(crate) struct InitialMeta<'a> {
    pub(crate) extractor_version: u32,
    pub(crate) branch: &'a str,
    pub(crate) commit_sha: &'a str,
}

pub(crate) const SCHEMA_SQL: &str = r#"
-- Files actually indexed (post-orbitignore, post-language-filter).
CREATE TABLE files (
    path           TEXT PRIMARY KEY,
    content_hash   BLOB NOT NULL,      -- blake3 of file bytes
    mtime_ns       INTEGER NOT NULL,
    lang           TEXT NOT NULL,      -- "rust", "typescript", "python", ...
    byte_len       INTEGER NOT NULL,
    extracted_at   INTEGER NOT NULL
) STRICT;

-- Symbols defined in files.
CREATE TABLE symbols (
    id             INTEGER PRIMARY KEY,
    file_path      TEXT NOT NULL REFERENCES files(path) ON DELETE CASCADE,
    name           TEXT NOT NULL,      -- "run_due_schedulers"
    qualified      TEXT NOT NULL,      -- "orbit_core::scheduler::run_due_schedulers"
    kind           TEXT NOT NULL,      -- "function" | "struct" | "enum" | "trait" |
                                       -- "impl" | "method" | "module" | "const" |
                                       -- "test" | "type_alias"
    span_start     INTEGER NOT NULL,   -- byte offset into file at content_hash
    span_end       INTEGER NOT NULL,   -- exclusive byte offset
    signature      TEXT,               -- one-line normalized signature
    parent_symbol  INTEGER REFERENCES symbols(id) ON DELETE CASCADE
) STRICT;
-- symbols.id is a plain rowid (no AUTOINCREMENT): it is NOT stable across
-- re-extracts, and ids freed by deleting a file's rows are reused, possibly
-- for different symbols. Use `qualified` for stable cross-build identity.
-- See §6.3.

CREATE INDEX symbols_name      ON symbols(name);
CREATE INDEX symbols_qualified ON symbols(qualified);
CREATE INDEX symbols_file      ON symbols(file_path);
CREATE INDEX symbols_parent    ON symbols(parent_symbol) WHERE parent_symbol IS NOT NULL;

-- Textual references from a source location to a symbol name.
-- Covers callers, type users, `use` statements, trait bounds — anything
-- anchored to (file, span). Resolution to a concrete symbol is by
-- `target_qualified` lookup, NOT by FK on symbols.id. `target_symbol_hint`
-- is a build-time cache that may go stale after incremental sync; queries
-- that need correctness re-resolve via `target_qualified`. See §6.3.
CREATE TABLE refs (
    id                  INTEGER PRIMARY KEY,
    from_file           TEXT NOT NULL REFERENCES files(path) ON DELETE CASCADE,
    from_span_start     INTEGER NOT NULL,   -- byte offset
    from_span_end       INTEGER NOT NULL,   -- exclusive
    target_name         TEXT NOT NULL,      -- short name; fallback for fuzzy
    target_qualified    TEXT,               -- best-effort qualified name (authoritative)
    target_symbol_hint  INTEGER,            -- non-authoritative; no FK
    kind                TEXT NOT NULL,      -- "call" | "type" | "use" | "trait_bound" |
                                            -- "runtime_invocation" (target_name is an
                                            -- opaque program string, never resolved)
    confidence          TEXT NOT NULL,      -- see §11
    -- Resolution inputs, kept so an incremental sync can re-resolve this ref
    -- when a definition it may name changes in another file.
    extracted_qualified TEXT,               -- the extractor's qualified path, before resolution
    unresolved_receiver TEXT,               -- receiver of a method call of unknown type
    spelled_path        INTEGER NOT NULL DEFAULT 0 -- 1 when extracted_qualified is the written path
) STRICT;

CREATE INDEX refs_target_qualified ON refs(target_qualified) WHERE target_qualified IS NOT NULL;
CREATE INDEX refs_target_name      ON refs(target_name);
CREATE INDEX refs_from_file        ON refs(from_file);

-- Symbol-to-symbol structural edges. No file:span source location.
-- Covers `impl Trait for Type`, class `extends`, interface `implements`,
-- supertype links. Both endpoints are qualified names; resolve to symbol
-- IDs at read time. Anchored to the file containing the relation's
-- definition site (e.g. the file with the `impl` block) for cascade.
CREATE TABLE relations (
    id              INTEGER PRIMARY KEY,
    from_qualified  TEXT NOT NULL,          -- concrete type / subclass
    to_qualified    TEXT NOT NULL,          -- trait / superclass / interface
    kind            TEXT NOT NULL,          -- "impl" | "extends" | "implements"
    def_file        TEXT NOT NULL REFERENCES files(path) ON DELETE CASCADE,
    def_span_start  INTEGER NOT NULL,
    def_span_end    INTEGER NOT NULL,
    confidence      TEXT NOT NULL
) STRICT;

CREATE INDEX relations_from ON relations(from_qualified);
CREATE INDEX relations_to   ON relations(to_qualified);
CREATE INDEX relations_kind ON relations(kind);
CREATE INDEX relations_def_file ON relations(def_file);

-- Imports / use statements. Module-level dependency edges.
-- `target_path` is a language-specific opaque string. For Rust it's a
-- `::`-joined path ("orbit_core::scheduler"); for TS it's the import
-- specifier ("./utils/foo", "@orbit/core"); for Python it's the dotted
-- module path. Comparison is exact-string only; cross-language matching
-- is not in scope.
CREATE TABLE imports (
    from_file      TEXT NOT NULL REFERENCES files(path) ON DELETE CASCADE,
    target_path    TEXT NOT NULL,
    target_symbol  TEXT                -- "Scheduler" or NULL for whole-module
) STRICT;

CREATE INDEX imports_from_file ON imports(from_file);

-- Clap / CLI command surface, extracted structurally.
CREATE TABLE commands (
    name           TEXT PRIMARY KEY,
    file_path      TEXT NOT NULL REFERENCES files(path) ON DELETE CASCADE,
    span_start     INTEGER NOT NULL,
    handler_symbol INTEGER REFERENCES symbols(id)
) STRICT;

CREATE INDEX commands_file    ON commands(file_path);
CREATE INDEX commands_handler ON commands(handler_symbol) WHERE handler_symbol IS NOT NULL;

-- Notable string literals — error messages, log lines, route paths.
-- Filter: length >= 6, not all ASCII punctuation, not pure format string.
CREATE TABLE strings (
    id             INTEGER PRIMARY KEY,
    file_path      TEXT NOT NULL REFERENCES files(path) ON DELETE CASCADE,
    line           INTEGER NOT NULL,
    value          TEXT NOT NULL,
    context_symbol INTEGER REFERENCES symbols(id)
) STRICT;

CREATE INDEX strings_file           ON strings(file_path);
CREATE INDEX strings_context_symbol ON strings(context_symbol) WHERE context_symbol IS NOT NULL;

-- Config keys: YAML / TOML / JSON / env var references.
CREATE TABLE configs (
    id             INTEGER PRIMARY KEY,
    file_path      TEXT NOT NULL REFERENCES files(path) ON DELETE CASCADE,
    line           INTEGER NOT NULL,
    key            TEXT NOT NULL,
    kind           TEXT NOT NULL       -- "yaml" | "toml" | "json" | "env" | "serde"
) STRICT;

CREATE INDEX configs_file ON configs(file_path);

-- Full-text search across the three high-value surfaces.
CREATE VIRTUAL TABLE symbols_fts USING fts5(name, qualified, signature, content='symbols');
CREATE VIRTUAL TABLE strings_fts USING fts5(value, content='strings');
CREATE VIRTUAL TABLE configs_fts USING fts5(key, content='configs');

-- Metadata.
CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT;
"#;

pub(crate) fn database_is_empty(conn: &Connection) -> Result<bool, GraphError> {
    let object_count: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get(0),
        )
        .map_err(|source| GraphError::sqlite("check graph schema objects", source))?;
    Ok(object_count == 0)
}

/// Creates the schema and its `meta` rows unless another connection already
/// has. The emptiness check runs inside the `IMMEDIATE` transaction, so of
/// several processes opening a new database at once exactly one creates the
/// tables and the rest find them.
pub(crate) fn initialize_if_empty(
    conn: &mut Connection,
    meta: &InitialMeta<'_>,
) -> Result<(), GraphError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|source| GraphError::sqlite("begin graph schema transaction", source))?;
    if !database_is_empty(&tx)? {
        return Ok(());
    }
    tx.execute_batch(SCHEMA_SQL)
        .map_err(|source| GraphError::sqlite("initialize graph schema", source))?;
    insert_meta_rows(&tx, meta)?;
    tx.commit()
        .map_err(|source| GraphError::sqlite("commit graph schema transaction", source))?;
    Ok(())
}

/// Checks the schema identity stored in `meta.schema_version` against
/// [`SCHEMA_VERSION`].
///
/// A database that was never initialized is [`GraphError::IndexMissing`]; one
/// with no identity, or another identity, is [`GraphError::IndexIncompatible`]
/// naming the file. Neither is ever read or written as if it were current
/// (STD-03 §R10).
pub(crate) fn validate_identity(conn: &Connection, db_path: &Path) -> Result<(), GraphError> {
    if database_is_empty(conn)? {
        return Err(GraphError::IndexMissing {
            path: db_path.to_path_buf(),
            reason: format!(
                "the graph index at {} was never initialized; run `orbit-graph sync`",
                db_path.display()
            ),
        });
    }
    let has_meta: bool = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'meta'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|count| count > 0)
        .map_err(|source| GraphError::sqlite("check graph metadata table", source))?;
    let stored = if has_meta {
        conn.query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|source| GraphError::sqlite("read graph schema version", source))?
    } else {
        None
    };
    let expected = SCHEMA_VERSION.to_string();
    match stored {
        Some(stored) if stored == expected => Ok(()),
        stored => Err(incompatible_schema(db_path, stored.as_deref())),
    }
}

fn incompatible_schema(db_path: &Path, stored: Option<&str>) -> GraphError {
    let path = db_path.display();
    let reason = match stored.map(|value| value.parse::<u32>()) {
        Some(Ok(stored)) if stored > SCHEMA_VERSION => format!(
            "the graph index at {path} has schema_version {stored}, newer than the \
             {SCHEMA_VERSION} this orbit-graph reads; use the orbit-graph that wrote it, \
             which this one never modifies"
        ),
        Some(Ok(stored)) => format!(
            "the graph index at {path} has schema_version {stored}, older than the \
             {SCHEMA_VERSION} this orbit-graph reads; delete {path} and run `orbit-graph sync` \
             to rebuild it"
        ),
        Some(Err(_)) | None => format!(
            "the graph index at {path} records {} instead of schema_version {SCHEMA_VERSION}; \
             delete {path} and run `orbit-graph sync` to rebuild it",
            stored.map_or_else(
                || "no schema_version".to_string(),
                |value| format!("schema_version {value:?}")
            )
        ),
    };
    GraphError::IndexIncompatible {
        path: db_path.to_path_buf(),
        reason,
    }
}

fn insert_meta_rows(conn: &Connection, meta: &InitialMeta<'_>) -> Result<(), GraphError> {
    let rows = [
        ("extractor_version", meta.extractor_version.to_string()),
        ("schema_version", SCHEMA_VERSION.to_string()),
        ("branch", meta.branch.to_string()),
        ("commit_sha", meta.commit_sha.to_string()),
        ("last_full_build_at", "0".to_string()),
        ("last_incremental_at", "0".to_string()),
    ];

    for (key, value) in rows {
        conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)",
            params![key, value],
        )
        .map_err(|source| GraphError::sqlite("insert graph metadata", source))?;
    }

    Ok(())
}
