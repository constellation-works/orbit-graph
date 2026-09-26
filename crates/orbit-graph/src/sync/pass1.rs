//! Pass 1 extracts files in parallel, then serializes SQLite writes.
//!
//! Extraction is CPU-bound and independent per file, so this pass uses rayon to
//! parse new or modified files concurrently. SQLite remains a single-writer
//! boundary, so extracted rows are written in a deterministic serial loop. The
//! changed files are processed in bounded chunks ([`Pass1Limits`]): one chunk
//! is extracted in parallel, then written, before the next is read, so pass 1
//! holds at most one chunk of extracted rows (`STD-03 §R22`). Each file has one
//! immediate SQLite transaction that deletes the prior row and inserts all
//! Pass 1 rows, while `ExtractedFile::refs` and command rows stay in memory for
//! Pass 2 and the final command transaction instead of being staged in the
//! frozen schema.
//!
//! A file written here is not yet current (`STD-03 §R8`): its row carries
//! [`PENDING_CONTENT_HASH`] and [`PENDING_MTIME_NS`], which no file on disk
//! matches, and pass 2 stamps the real values in the transaction that writes
//! its refs. Before its first write, pass 1 also marks the database as holding
//! an unfinished sync ([`super::mark_sync_pending`]). If the sync stops before
//! pass 2 commits, the next scan finds those files changed and extracts them
//! again, and the marker makes that sync re-resolve every stored ref.
//!
//! A file whose extraction fails or panics is not written; it is reported in
//! [`Pass1Output::failed`] (`STD-02 §R32`) and keeps any rows it already has.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use orbit_graph_extract::{
    ExtractedFile, RawCommand, RawConfig, RawImport, RawRef, RawRelation, RawString, RawSymbol,
    languages,
};
use rayon::prelude::*;
use rusqlite::{Connection, Transaction, TransactionBehavior, params};

use super::scanner::{Diff, MAX_FILE_BYTES, mtime_ns, normalize_path, read_capped};
use super::{io_error_kind, mark_sync_pending};
use crate::{GraphError, SyncFailure, SyncMode, SyncObserver, SyncPhase, SyncProgress};

/// `files.content_hash` of a file pass 1 wrote and pass 2 has not yet made
/// current. No file hashes to it, so the scanner always sees such a file as
/// changed.
pub(crate) const PENDING_CONTENT_HASH: &[u8] = &[];
/// `files.mtime_ns` of a file pass 1 wrote and pass 2 has not yet made
/// current, so the scanner's mtime fast path never skips it.
pub(crate) const PENDING_MTIME_NS: i64 = 0;
/// [`ExtractFileError::kind`] of a file that grew past [`MAX_FILE_BYTES`]
/// after the scan measured it; it is reported as skipped, not failed.
const OVERSIZE_KIND: &str = "oversize";

/// Longest one file's tree-sitter parse may take before the file counts as
/// an extraction failure (`STD-03 §R22`).
pub(crate) const DEFAULT_PARSE_TIMEOUT: Duration = Duration::from_secs(10);

/// Bounds on how much pass 1 extracts before writing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Pass1Limits {
    /// Most files extracted in one chunk.
    pub(crate) chunk_files: usize,
    /// Most source bytes, by file size on disk, extracted in one chunk. A
    /// single file above it still forms its own chunk; the byte cap bounds it.
    pub(crate) chunk_bytes: u64,
}

impl Default for Pass1Limits {
    fn default() -> Self {
        Self {
            chunk_files: 128,
            chunk_bytes: 8 * MAX_FILE_BYTES,
        }
    }
}

pub(crate) struct Pass1Output {
    pub(crate) refs: Vec<ExtractedFileRefs>,
    pub(crate) files_written: usize,
    pub(crate) files_removed: usize,
    pub(crate) files_indexed: usize,
    /// Whether `observer.is_cancelled()` stopped this pass before every file
    /// was processed.
    pub(crate) cancelled: bool,
    /// Total files this sync touched, written or removed. Equal to
    /// `files_written + files_removed` when `cancelled` is `false`; carried
    /// forward as pass 2's frozen `files_seen`/`files_indexed`.
    pub(crate) total_files: usize,
    /// Path of the last file pass 1 touched, if any; carried forward as pass
    /// 2's frozen `current_path`.
    pub(crate) last_touched_path: Option<String>,
    /// Files whose extraction failed or panicked, one entry each.
    pub(crate) failed: Vec<SyncFailure>,
    /// Files that grew past [`MAX_FILE_BYTES`] after the scan measured them.
    pub(crate) oversize: Vec<PathBuf>,
}

/// A file pass 1 wrote: the refs pass 2 resolves for it, and the content
/// identity pass 2 stamps on its row to make it current.
pub(crate) struct ExtractedFileRefs {
    pub(crate) file_path: String,
    pub(crate) refs: Vec<RawRef>,
    pub(crate) content_hash: Vec<u8>,
    pub(crate) mtime_ns: i64,
}

pub(crate) fn run(
    db_path: &Path,
    worktree_root: &Path,
    _mode: SyncMode,
    diff: &Diff,
    observer: Option<&dyn SyncObserver>,
) -> Result<Pass1Output, GraphError> {
    run_with_backend(
        db_path,
        worktree_root,
        _mode,
        diff,
        &DefaultExtractorBackend::default(),
        observer,
        Pass1Limits::default(),
    )
}

fn run_with_backend(
    db_path: &Path,
    worktree_root: &Path,
    _mode: SyncMode,
    diff: &Diff,
    backend: &dyn ExtractorBackend,
    observer: Option<&dyn SyncObserver>,
    limits: Pass1Limits,
) -> Result<Pass1Output, GraphError> {
    let changed = changed_files(diff);
    let chunks = plan_chunks(worktree_root, &changed, limits);

    let mut conn = open_writer_connection(db_path)?;
    mark_sync_pending(&mut conn)?;

    // Files this sync will touch: every deletion plus every changed file,
    // less each one whose extraction fails, as chunks reveal them.
    let mut total_files = diff.deleted.len() + changed.len();
    let mut files_touched = 0usize;
    let mut last_touched_path = None;
    if let Some(observer) = observer {
        observer.on_progress(&SyncProgress {
            phase: SyncPhase::Extracting,
            files_seen: total_files,
            files_indexed: files_touched,
            current_path: None,
            units_done: files_touched,
            units_total: total_files,
        });
    }

    let mut cancelled = false;
    let mut files_removed = 0;
    for rel_path in &diff.deleted {
        if observer.is_some_and(|observer| observer.is_cancelled()) {
            cancelled = true;
            break;
        }
        delete_file_transaction(&mut conn, rel_path)?;
        files_removed += 1;
        files_touched += 1;
        last_touched_path = Some(normalize_path(rel_path));
        if let Some(observer) = observer {
            observer.on_progress(&SyncProgress {
                phase: SyncPhase::Extracting,
                files_seen: total_files,
                files_indexed: files_touched,
                current_path: last_touched_path.clone(),
                units_done: files_touched,
                units_total: total_files,
            });
        }
    }

    let mut refs = Vec::new();
    let mut commands = Vec::new();
    let mut files_written = 0;
    let mut failed = Vec::new();
    let mut oversize = Vec::new();
    for chunk in chunks {
        if cancelled || observer.is_some_and(|observer| observer.is_cancelled()) {
            cancelled = true;
            break;
        }
        let chunk_len = chunk.len();
        let (mut extracted, errors) =
            extract_changed_files(worktree_root, &changed[chunk], backend);
        total_files -= chunk_len - extracted.len();
        for (path, error) in errors {
            if error.kind == OVERSIZE_KIND {
                oversize.push(path);
            } else {
                failed.push(error.into_failure(&path));
            }
        }
        extracted.sort_by(|left, right| left.path.cmp(&right.path));
        for mut file in extracted {
            if observer.is_some_and(|observer| observer.is_cancelled()) {
                cancelled = true;
                break;
            }
            let file_refs = std::mem::take(&mut file.rows.refs);
            let file_commands = std::mem::take(&mut file.rows.commands);
            write_file_transaction(&mut conn, &file)?;
            refs.push(ExtractedFileRefs {
                file_path: file.file_path.clone(),
                refs: file_refs,
                content_hash: std::mem::take(&mut file.content_hash),
                mtime_ns: file.mtime_ns,
            });
            commands.extend(file_commands);
            files_written += 1;
            files_touched += 1;
            last_touched_path = Some(file.file_path.clone());
            if let Some(observer) = observer {
                observer.on_progress(&SyncProgress {
                    phase: SyncPhase::Extracting,
                    files_seen: total_files,
                    files_indexed: files_touched,
                    current_path: last_touched_path.clone(),
                    units_done: files_touched,
                    units_total: total_files,
                });
            }
        }
    }
    insert_commands_transaction(&mut conn, &commands)?;

    let files_indexed = count_files(&conn)?;

    Ok(Pass1Output {
        refs,
        files_written,
        files_removed,
        files_indexed,
        cancelled,
        total_files,
        last_touched_path,
        failed,
        oversize,
    })
}

fn changed_files(diff: &Diff) -> Vec<PathBuf> {
    let mut changed = Vec::with_capacity(diff.modified.len() + diff.new.len());
    changed.extend(diff.modified.iter().cloned());
    changed.extend(diff.new.iter().cloned());
    changed.sort();
    changed.dedup();
    changed
}

/// Splits `changed` into consecutive chunks within `limits`, sized by each
/// file's length on disk (an unreadable file counts as empty; its extraction
/// fails on its own).
fn plan_chunks(
    worktree_root: &Path,
    changed: &[PathBuf],
    limits: Pass1Limits,
) -> Vec<Range<usize>> {
    let chunk_files = limits.chunk_files.max(1);
    let mut chunks = Vec::new();
    let mut start = 0;
    let mut bytes = 0u64;
    for (index, rel_path) in changed.iter().enumerate() {
        let len = fs::metadata(worktree_root.join(rel_path)).map_or(0, |metadata| metadata.len());
        if index > start
            && (index - start >= chunk_files || bytes.saturating_add(len) > limits.chunk_bytes)
        {
            chunks.push(start..index);
            start = index;
            bytes = 0;
        }
        bytes = bytes.saturating_add(len);
    }
    if start < changed.len() {
        chunks.push(start..changed.len());
    }
    chunks
}

/// Extracts `changed` in parallel. Returns the extracted files and, for each
/// file whose extraction failed or panicked, its path and error.
fn extract_changed_files(
    worktree_root: &Path,
    changed: &[PathBuf],
    backend: &dyn ExtractorBackend,
) -> (Vec<ExtractedSourceFile>, Vec<(PathBuf, ExtractFileError)>) {
    install_silenceable_panic_hook();
    let results = changed
        .par_iter()
        .map(|rel_path| {
            let _silenced = SilencedPanics::enter();
            let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                backend.extract(worktree_root, rel_path)
            })) {
                Ok(result) => result,
                Err(payload) => Err(ExtractFileError::new(
                    "extract source file",
                    format!(
                        "extractor panicked: {}",
                        panic_payload_message(payload.as_ref())
                    ),
                )
                .with_kind("panic")),
            };
            result.map_err(|error| {
                warn_extraction_failure(rel_path, error.to_string());
                (rel_path.clone(), error)
            })
        })
        .collect::<Vec<_>>();
    let mut extracted = Vec::new();
    let mut errors = Vec::new();
    for result in results {
        match result {
            Ok(file) => extracted.push(file),
            Err(error) => errors.push(error),
        }
    }
    (extracted, errors)
}

thread_local! {
    /// Whether this thread is inside an extraction whose panics are caught
    /// and reported as warnings.
    static SILENCE_PANICS: Cell<bool> = const { Cell::new(false) };
}

/// Installs, once per process, a panic hook that stays quiet on a thread
/// inside [`SilencedPanics`] and otherwise defers to the hook it replaced.
///
/// L-0049: `catch_unwind` still runs the panic hook before an extractor panic
/// becomes a warning. The flag is per thread, so concurrent syncs do not
/// serialize on the hook and panics on unrelated threads are still reported.
fn install_silenceable_panic_hook() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if !panics_silenced() {
                previous(info);
            }
        }));
    });
}

fn panics_silenced() -> bool {
    SILENCE_PANICS.try_with(Cell::get).unwrap_or(false)
}

/// Silences panics on the current thread until dropped.
#[must_use = "panics are silenced only while the guard lives"]
struct SilencedPanics {
    previous: bool,
}

impl SilencedPanics {
    fn enter() -> Self {
        Self {
            previous: SILENCE_PANICS.replace(true),
        }
    }
}

impl Drop for SilencedPanics {
    fn drop(&mut self) {
        SILENCE_PANICS.set(self.previous);
    }
}

fn warn_extraction_failure(path: &Path, error: String) {
    tracing::warn!(
        path = %path.display(),
        error = %error,
        "skipping file after graph extraction failure"
    );
}

fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "unknown panic payload".to_string()
}

trait ExtractorBackend: Sync {
    fn extract(
        &self,
        worktree_root: &Path,
        rel_path: &Path,
    ) -> Result<ExtractedSourceFile, ExtractFileError>;
}

struct DefaultExtractorBackend {
    /// Per-file tree-sitter parse deadline.
    parse_timeout: Duration,
}

impl Default for DefaultExtractorBackend {
    fn default() -> Self {
        Self {
            parse_timeout: DEFAULT_PARSE_TIMEOUT,
        }
    }
}

impl ExtractorBackend for DefaultExtractorBackend {
    fn extract(
        &self,
        worktree_root: &Path,
        rel_path: &Path,
    ) -> Result<ExtractedSourceFile, ExtractFileError> {
        let path = worktree_root.join(rel_path);
        let bytes = read_capped(path.as_path())
            .map_err(|source| {
                ExtractFileError::new("read source file", format!("{}: {source}", path.display()))
                    .with_kind(io_error_kind(&source))
            })?
            .ok_or_else(|| {
                ExtractFileError::new(
                    "read source file",
                    format!("{} exceeds the {MAX_FILE_BYTES}-byte cap", path.display()),
                )
                .with_kind(OVERSIZE_KIND)
            })?;
        let mtime_ns = mtime_ns(path.as_path()).map_err(|error| {
            ExtractFileError::new("read source file mtime", error.to_string()).with_kind("io")
        })?;
        let extractors = languages::extractors();
        let extractor = extractors
            .iter()
            .find(|extractor| extractor.supports(rel_path))
            .ok_or_else(|| {
                ExtractFileError::new(
                    "select extractor",
                    format!("no registered extractor for {}", rel_path.display()),
                )
                .with_kind("unsupported")
            })?;
        let (rows, deadline_exceeded) = languages::with_parse_timeout(self.parse_timeout, || {
            extractor.extract(rel_path, &bytes)
        });
        if deadline_exceeded {
            return Err(ExtractFileError::new(
                "parse source file",
                format!(
                    "parse exceeded the {} ms deadline",
                    self.parse_timeout.as_millis()
                ),
            )
            .with_kind("parse_timeout"));
        }
        Ok(ExtractedSourceFile {
            path: rel_path.to_path_buf(),
            file_path: normalize_path(rel_path),
            lang: extractor.lang(),
            content_hash: blake3::hash(&bytes).as_bytes().to_vec(),
            mtime_ns,
            byte_len: usize_to_i64("convert source byte length", bytes.len()).map_err(|error| {
                ExtractFileError::new("convert source byte length", error.to_string())
            })?,
            extracted_at: now_epoch_nanos("record extraction timestamp").map_err(|error| {
                ExtractFileError::new("record extraction timestamp", error.to_string())
            })?,
            rows,
        })
    }
}

#[derive(Debug)]
struct ExtractFileError {
    operation: &'static str,
    reason: String,
    /// Stable class reported as [`SyncFailure::error_kind`].
    kind: &'static str,
}

impl ExtractFileError {
    fn new(operation: &'static str, reason: impl Into<String>) -> Self {
        Self {
            operation,
            reason: reason.into(),
            kind: "invalid_data",
        }
    }

    fn with_kind(mut self, kind: &'static str) -> Self {
        self.kind = kind;
        self
    }

    fn into_failure(self, rel_path: &Path) -> SyncFailure {
        SyncFailure {
            path: normalize_path(rel_path),
            operation: self.operation.to_string(),
            error_kind: self.kind.to_string(),
            message: self.reason,
        }
    }
}

impl Display for ExtractFileError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.operation, self.reason)
    }
}

struct ExtractedSourceFile {
    path: PathBuf,
    file_path: String,
    lang: &'static str,
    content_hash: Vec<u8>,
    mtime_ns: i64,
    byte_len: i64,
    extracted_at: i64,
    rows: ExtractedFile,
}

fn open_writer_connection(db_path: &Path) -> Result<Connection, GraphError> {
    let conn = Connection::open(db_path)
        .map_err(|source| GraphError::sqlite("open graph database for pass1 writes", source))?;
    crate::store::configure_sync_writer(&conn, "configure graph database for pass1 writes")?;
    Ok(conn)
}

fn delete_file_transaction(conn: &mut Connection, rel_path: &Path) -> Result<(), GraphError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|source| GraphError::sqlite("begin pass1 delete transaction", source))?;
    delete_fts_for_file(&tx, &normalize_path(rel_path))?;
    tx.execute(
        "DELETE FROM files WHERE path = ?1",
        params![normalize_path(rel_path)],
    )
    .map_err(|source| GraphError::sqlite("delete removed graph file", source))?;
    tx.commit()
        .map_err(|source| GraphError::sqlite("commit pass1 delete transaction", source))?;
    Ok(())
}

fn write_file_transaction(
    conn: &mut Connection,
    file: &ExtractedSourceFile,
) -> Result<(), GraphError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|source| GraphError::sqlite("begin pass1 file transaction", source))?;
    delete_fts_for_file(&tx, &file.file_path)?;
    tx.execute("DELETE FROM files WHERE path = ?1", params![file.file_path])
        .map_err(|source| GraphError::sqlite("delete prior graph file rows", source))?;
    // Not current until pass 2 stamps the real content hash and mtime.
    tx.execute(
        "INSERT INTO files (path, content_hash, mtime_ns, lang, byte_len, extracted_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            file.file_path,
            PENDING_CONTENT_HASH,
            PENDING_MTIME_NS,
            file.lang,
            file.byte_len,
            file.extracted_at
        ],
    )
    .map_err(|source| GraphError::sqlite("insert graph file row", source))?;

    let symbol_ids = insert_symbols(&tx, &file.rows.symbols)?;
    insert_imports(&tx, &file.rows.imports)?;
    insert_relations(&tx, &file.rows.relations)?;
    insert_strings(&tx, &file.rows.strings, &symbol_ids)?;
    insert_configs(&tx, &file.rows.configs)?;

    tx.commit()
        .map_err(|source| GraphError::sqlite("commit pass1 file transaction", source))?;
    Ok(())
}

fn insert_commands_transaction(
    conn: &mut Connection,
    commands: &[RawCommand],
) -> Result<(), GraphError> {
    if commands.is_empty() {
        return Ok(());
    }

    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|source| GraphError::sqlite("begin pass1 command transaction", source))?;
    insert_commands(&tx, commands)?;
    tx.commit()
        .map_err(|source| GraphError::sqlite("commit pass1 command transaction", source))?;
    Ok(())
}

fn insert_symbols(
    tx: &Transaction<'_>,
    symbols: &[RawSymbol],
) -> Result<BTreeMap<String, i64>, GraphError> {
    let mut symbol_ids = BTreeMap::new();
    for symbol in symbols {
        tx.execute(
            "INSERT INTO symbols (
                file_path, name, qualified, kind, span_start, span_end, signature, parent_symbol
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)",
            params![
                symbol.file_path,
                symbol.name,
                symbol.qualified,
                symbol.kind,
                usize_to_i64("convert symbol span start", symbol.span_start)?,
                usize_to_i64("convert symbol span end", symbol.span_end)?,
                symbol.signature
            ],
        )
        .map_err(|source| GraphError::sqlite("insert graph symbol row", source))?;
        let symbol_id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO symbols_fts (rowid, name, qualified, signature)
             VALUES (?1, ?2, ?3, ?4)",
            params![symbol_id, symbol.name, symbol.qualified, symbol.signature],
        )
        .map_err(|source| GraphError::sqlite("insert graph symbol fts row", source))?;
        symbol_ids.insert(symbol.qualified.clone(), symbol_id);
    }

    for symbol in symbols {
        let Some(parent_qualified) = symbol.parent_symbol.as_ref() else {
            continue;
        };
        let Some(parent_id) = symbol_ids.get(parent_qualified) else {
            continue;
        };
        let Some(symbol_id) = symbol_ids.get(&symbol.qualified) else {
            continue;
        };
        tx.execute(
            "UPDATE symbols SET parent_symbol = ?1 WHERE id = ?2",
            params![parent_id, symbol_id],
        )
        .map_err(|source| GraphError::sqlite("link graph symbol parent", source))?;
    }

    Ok(symbol_ids)
}

fn insert_imports(tx: &Transaction<'_>, imports: &[RawImport]) -> Result<(), GraphError> {
    for import in imports {
        tx.execute(
            "INSERT INTO imports (from_file, target_path, target_symbol)
             VALUES (?1, ?2, ?3)",
            params![import.from_file, import.target_path, import.target_symbol],
        )
        .map_err(|source| GraphError::sqlite("insert graph import row", source))?;
    }
    Ok(())
}

fn insert_relations(tx: &Transaction<'_>, relations: &[RawRelation]) -> Result<(), GraphError> {
    for relation in relations {
        tx.execute(
            "INSERT INTO relations (
                from_qualified, to_qualified, kind, def_file, def_span_start, def_span_end,
                confidence
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                relation.from_qualified,
                relation.to_qualified,
                relation.kind,
                relation.def_file,
                usize_to_i64("convert relation span start", relation.def_span_start)?,
                usize_to_i64("convert relation span end", relation.def_span_end)?,
                relation.confidence
            ],
        )
        .map_err(|source| GraphError::sqlite("insert graph relation row", source))?;
    }
    Ok(())
}

fn insert_strings(
    tx: &Transaction<'_>,
    strings: &[RawString],
    symbol_ids: &BTreeMap<String, i64>,
) -> Result<(), GraphError> {
    for string in strings {
        let context_symbol = string
            .context_symbol
            .as_ref()
            .and_then(|qualified| symbol_ids.get(qualified))
            .copied();
        tx.execute(
            "INSERT INTO strings (file_path, line, value, context_symbol)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                string.file_path,
                usize_to_i64("convert string line", string.line)?,
                string.value,
                context_symbol
            ],
        )
        .map_err(|source| GraphError::sqlite("insert graph string row", source))?;
        let string_id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO strings_fts (rowid, value) VALUES (?1, ?2)",
            params![string_id, string.value],
        )
        .map_err(|source| GraphError::sqlite("insert graph string fts row", source))?;
    }
    Ok(())
}

fn insert_configs(tx: &Transaction<'_>, configs: &[RawConfig]) -> Result<(), GraphError> {
    for config in configs {
        tx.execute(
            "INSERT INTO configs (file_path, line, key, kind)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                config.file_path,
                usize_to_i64("convert config line", config.line)?,
                config.key,
                config.kind
            ],
        )
        .map_err(|source| GraphError::sqlite("insert graph config row", source))?;
        let config_id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO configs_fts (rowid, key) VALUES (?1, ?2)",
            params![config_id, config.key],
        )
        .map_err(|source| GraphError::sqlite("insert graph config fts row", source))?;
    }
    Ok(())
}

fn delete_fts_for_file(tx: &Transaction<'_>, file_path: &str) -> Result<(), GraphError> {
    // L-0056: cross-file command handler FKs must be cleared before refreshing symbol rows.
    tx.execute(
        "UPDATE commands
         SET handler_symbol = NULL
         WHERE handler_symbol IN (
             SELECT id FROM symbols WHERE file_path = ?1
         )",
        params![file_path],
    )
    .map_err(|source| GraphError::sqlite("clear command handlers for refreshed file", source))?;
    tx.execute(
        "DELETE FROM symbols_fts WHERE rowid IN (
            SELECT id FROM symbols WHERE file_path = ?1
         )",
        params![file_path],
    )
    .map_err(|source| GraphError::sqlite("delete prior symbol fts rows", source))?;
    tx.execute(
        "DELETE FROM strings_fts WHERE rowid IN (
            SELECT id FROM strings WHERE file_path = ?1
         )",
        params![file_path],
    )
    .map_err(|source| GraphError::sqlite("delete prior string fts rows", source))?;
    tx.execute(
        "DELETE FROM configs_fts WHERE rowid IN (
            SELECT id FROM configs WHERE file_path = ?1
         )",
        params![file_path],
    )
    .map_err(|source| GraphError::sqlite("delete prior config fts rows", source))?;
    Ok(())
}

fn insert_commands(tx: &Transaction<'_>, commands: &[RawCommand]) -> Result<(), GraphError> {
    for command in commands {
        let handler_symbol = match command.handler_symbol.as_ref() {
            Some(qualified) => unique_symbol_id_for_qualified(tx, qualified)?,
            None => None,
        };
        tx.execute(
            "INSERT INTO commands (name, file_path, span_start, handler_symbol)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                command.name,
                command.file_path,
                usize_to_i64("convert command span start", command.span_start)?,
                handler_symbol
            ],
        )
        .map_err(|source| GraphError::sqlite("insert graph command row", source))?;
    }
    Ok(())
}

fn unique_symbol_id_for_qualified(
    tx: &Transaction<'_>,
    qualified: &str,
) -> Result<Option<i64>, GraphError> {
    let mut stmt = tx
        .prepare_cached("SELECT id FROM symbols WHERE qualified = ?1 ORDER BY id LIMIT 2")
        .map_err(|source| GraphError::sqlite("prepare command handler symbol lookup", source))?;
    let ids = stmt
        .query_map(params![qualified], |row| row.get::<_, i64>(0))
        .map_err(|source| GraphError::sqlite("query command handler symbol", source))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| GraphError::sqlite("collect command handler symbol", source))?;
    if ids.len() == 1 {
        Ok(ids.first().copied())
    } else {
        Ok(None)
    }
}

fn count_files(conn: &Connection) -> Result<usize, GraphError> {
    let count = conn
        .query_row("SELECT count(*) FROM files", [], |row| row.get::<_, i64>(0))
        .map_err(|source| GraphError::sqlite("count graph files after pass1", source))?;
    usize::try_from(count).map_err(|source| {
        GraphError::invalid_data("count graph files after pass1", source.to_string())
    })
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

fn usize_to_i64(operation: &'static str, value: usize) -> Result<i64, GraphError> {
    i64::try_from(value).map_err(|error| GraphError::invalid_data(operation, error.to_string()))
}

#[cfg(test)]
#[path = "tests/pass1.rs"]
mod tests;
