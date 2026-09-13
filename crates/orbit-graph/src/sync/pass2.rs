//! Pass 2 resolves raw refs after Pass 1 has written files, symbols, and imports.
//!
//! Resolution deliberately follows the documented confidence ladder in strict
//! order: same-file exact matches, explicit imports, qualified cross-file
//! matches, same-module matches, then fuzzy name-only refs. All refs for the
//! files refreshed by the current sync are rewritten in one SQLite transaction;
//! unchanged files' refs are not touched during incremental syncs.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::extract::RawRef;
use rusqlite::{Connection, Transaction, TransactionBehavior, params};

use super::pass1::ExtractedFileRefs;
use crate::{GraphError, SyncMode};

const CONFIDENCE_EXACT: &str = "exact";
const CONFIDENCE_IMPORT_RESOLVED: &str = "import_resolved";
const CONFIDENCE_SAME_MODULE: &str = "same_module";
const CONFIDENCE_FUZZY_NAME: &str = "fuzzy_name";

pub(crate) fn run(
    db_path: &Path,
    mode: SyncMode,
    refs_by_file: Vec<ExtractedFileRefs>,
) -> Result<(), GraphError> {
    let mut conn = open_writer_connection(db_path)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|source| GraphError::sqlite("begin pass2 refs transaction", source))?;

    for file_refs in refs_by_file {
        delete_refs_for_file(&tx, &file_refs.file_path)?;
        for raw_ref in &file_refs.refs {
            let resolved = resolve_ref(&tx, &file_refs.file_path, raw_ref)?;
            insert_ref(&tx, &file_refs.file_path, raw_ref, &resolved)?;
        }
    }

    update_sync_meta(&tx, mode)?;
    tx.commit()
        .map_err(|source| GraphError::sqlite("commit pass2 refs transaction", source))?;
    Ok(())
}

fn open_writer_connection(db_path: &Path) -> Result<Connection, GraphError> {
    let conn = Connection::open(db_path)
        .map_err(|source| GraphError::sqlite("open graph database for pass2 writes", source))?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|source| GraphError::sqlite("enable foreign keys for pass2 writes", source))?;
    Ok(conn)
}

fn delete_refs_for_file(tx: &Transaction<'_>, from_file: &str) -> Result<(), GraphError> {
    tx.prepare_cached("DELETE FROM refs WHERE from_file = ?1")
        .map_err(|source| GraphError::sqlite("prepare pass2 ref delete", source))?
        .execute(params![from_file])
        .map_err(|source| GraphError::sqlite("delete prior refs for file", source))?;
    Ok(())
}

fn insert_ref(
    tx: &Transaction<'_>,
    from_file: &str,
    raw_ref: &RawRef,
    resolved: &ResolvedRef,
) -> Result<(), GraphError> {
    tx.prepare_cached(
        "INSERT INTO refs (
            from_file, from_span_start, from_span_end, target_name, target_qualified,
            target_symbol_hint, kind, confidence
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )
    .map_err(|source| GraphError::sqlite("prepare pass2 ref insert", source))?
    .execute(params![
        from_file,
        usize_to_i64("convert ref span start", raw_ref.from_span_start)?,
        usize_to_i64("convert ref span end", raw_ref.from_span_end)?,
        raw_ref.target_name,
        resolved.target_qualified,
        resolved.target_symbol_hint,
        raw_ref.kind,
        resolved.confidence,
    ])
    .map_err(|source| GraphError::sqlite("insert resolved ref row", source))?;
    Ok(())
}

fn resolve_ref(
    tx: &Transaction<'_>,
    from_file: &str,
    raw_ref: &RawRef,
) -> Result<ResolvedRef, GraphError> {
    if let Some(candidate) = resolve_exact(tx, from_file, raw_ref)? {
        return Ok(ResolvedRef::candidate(candidate, CONFIDENCE_EXACT));
    }
    match resolve_import(tx, from_file, raw_ref)? {
        ImportResolution::Unique(candidate) => {
            return Ok(ResolvedRef::candidate(
                candidate,
                CONFIDENCE_IMPORT_RESOLVED,
            ));
        }
        ImportResolution::Ambiguous => {
            return Ok(ResolvedRef {
                target_qualified: None,
                target_symbol_hint: None,
                confidence: CONFIDENCE_FUZZY_NAME,
            });
        }
        ImportResolution::None => {}
    }
    if let Some(candidate) = resolve_qualified(tx, raw_ref)? {
        return Ok(ResolvedRef::candidate(candidate, CONFIDENCE_EXACT));
    }
    if let Some(candidate) = resolve_same_module(tx, from_file, raw_ref)? {
        return Ok(ResolvedRef::candidate(candidate, CONFIDENCE_SAME_MODULE));
    }
    Ok(ResolvedRef {
        target_qualified: None,
        target_symbol_hint: None,
        confidence: CONFIDENCE_FUZZY_NAME,
    })
}

fn resolve_exact(
    tx: &Transaction<'_>,
    from_file: &str,
    raw_ref: &RawRef,
) -> Result<Option<SymbolCandidate>, GraphError> {
    let candidates = symbols_in_file_by_name(tx, from_file, &raw_ref.target_name)?;
    if candidates.is_empty() {
        return Ok(None);
    }

    if let Some(target_qualified) = raw_ref.target_qualified.as_deref() {
        let qualified_matches = candidates
            .iter()
            .filter(|candidate| candidate.qualified == target_qualified)
            .cloned()
            .collect::<Vec<_>>();
        if unique_candidate(&qualified_matches).is_some() {
            return Ok(qualified_matches.into_iter().next());
        }
    }

    Ok(unique_candidate(&candidates))
}

fn resolve_qualified(
    tx: &Transaction<'_>,
    raw_ref: &RawRef,
) -> Result<Option<SymbolCandidate>, GraphError> {
    if raw_ref.kind == "use" {
        return Ok(None);
    }
    let Some(target) = raw_ref.target_qualified.as_deref() else {
        return Ok(None);
    };
    if !target.contains("::") && !target.contains('.') {
        return Ok(None);
    }
    let mut candidates = Vec::new();
    for candidate in symbols_by_name(tx, &raw_ref.target_name)? {
        if candidate_matches_qualified(tx, &candidate, target)? {
            candidates.push(candidate);
        }
    }
    Ok(unique_candidate(&candidates))
}

fn resolve_import(
    tx: &Transaction<'_>,
    from_file: &str,
    raw_ref: &RawRef,
) -> Result<ImportResolution, GraphError> {
    let imports = imports_for_file(tx, from_file)?;
    for explicit in [true, false] {
        let mut matches = BTreeMap::new();
        for import in imports.iter().filter(|import| {
            (import.target_symbol.as_deref() == Some(raw_ref.target_name.as_str())) == explicit
        }) {
            let Some(module_path) = import_module_for_ref(from_file, import, raw_ref) else {
                continue;
            };
            for candidate in symbols_by_name(tx, &raw_ref.target_name)? {
                if candidate_matches_module(tx, &candidate, &module_path)? {
                    matches.insert(candidate.id, candidate);
                }
            }
        }
        let mut candidates = matches.into_values();
        match (candidates.next(), candidates.next()) {
            (Some(candidate), None) => return Ok(ImportResolution::Unique(candidate)),
            (Some(_), Some(_)) => return Ok(ImportResolution::Ambiguous),
            _ => {}
        }
    }
    Ok(ImportResolution::None)
}

fn resolve_same_module(
    tx: &Transaction<'_>,
    from_file: &str,
    raw_ref: &RawRef,
) -> Result<Option<SymbolCandidate>, GraphError> {
    let prefixes = module_prefixes_for_file(tx, from_file)?;
    if prefixes.is_empty() {
        return Ok(None);
    }

    let candidates = symbols_by_name(tx, &raw_ref.target_name)?
        .into_iter()
        .filter(|candidate| candidate.file_path != from_file)
        .filter(|candidate| {
            module_prefixes_for_candidate(tx, candidate)
                .is_ok_and(|candidate_prefixes| !prefixes.is_disjoint(&candidate_prefixes))
        })
        .collect::<Vec<_>>();

    Ok(unique_candidate(&candidates))
}

/// Symbols a textual ref can resolve to. Rust `impl` blocks are stored under the
/// implemented type's `name` (qualified as `<Type>` or `<Type as Trait>`), but a
/// `call`/`type`/`use`/`trait_bound` ref never targets an impl block itself, so they
/// are excluded here. Otherwise a type with even one impl block yields several
/// same-named candidates and every uniqueness test in the ladder fails.
fn symbols_in_file_by_name(
    tx: &Transaction<'_>,
    from_file: &str,
    name: &str,
) -> Result<Vec<SymbolCandidate>, GraphError> {
    let mut stmt = tx
        .prepare_cached(
            "SELECT id, file_path, name, qualified FROM symbols
             WHERE file_path = ?1 AND name = ?2 AND kind <> 'impl'
             ORDER BY qualified, id",
        )
        .map_err(|source| GraphError::sqlite("prepare exact symbol lookup", source))?;
    let rows = stmt
        .query_map(params![from_file, name], symbol_candidate_from_row)
        .map_err(|source| GraphError::sqlite("query symbols for ref resolution", source))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|source| GraphError::sqlite("collect symbols for ref resolution", source))
}

/// Cross-file counterpart of [`symbols_in_file_by_name`]; applies the same
/// `impl` exclusion.
fn symbols_by_name(tx: &Transaction<'_>, name: &str) -> Result<Vec<SymbolCandidate>, GraphError> {
    let mut stmt = tx
        .prepare_cached(
            "SELECT id, file_path, name, qualified FROM symbols
             WHERE name = ?1 AND kind <> 'impl'
             ORDER BY qualified, id",
        )
        .map_err(|source| GraphError::sqlite("prepare name symbol lookup", source))?;
    let rows = stmt
        .query_map(params![name], symbol_candidate_from_row)
        .map_err(|source| GraphError::sqlite("query symbols for ref resolution", source))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|source| GraphError::sqlite("collect symbols for ref resolution", source))
}

fn module_prefixes_for_file(
    tx: &Transaction<'_>,
    from_file: &str,
) -> Result<BTreeSet<String>, GraphError> {
    let mut stmt = tx
        .prepare_cached(
            "SELECT id, file_path, name, qualified FROM symbols
             WHERE file_path = ?1
             ORDER BY qualified, id",
        )
        .map_err(|source| GraphError::sqlite("prepare module prefix symbol lookup", source))?;
    let rows = stmt
        .query_map(params![from_file], symbol_candidate_from_row)
        .map_err(|source| GraphError::sqlite("query symbols for ref resolution", source))?;
    let symbols = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| GraphError::sqlite("collect symbols for ref resolution", source))?;
    let mut prefixes = symbols
        .iter()
        .filter_map(|symbol| qualified_prefix_before_name(&symbol.qualified, &symbol.name))
        .map(|prefix| normalize_module_path(&prefix))
        .collect::<BTreeSet<_>>();
    let file_parts = file_module_parts(from_file);
    for start in 0..file_parts.len() {
        prefixes.insert(file_parts[start..].join("::"));
    }
    Ok(prefixes)
}

fn imports_for_file(
    tx: &Transaction<'_>,
    from_file: &str,
) -> Result<Vec<ImportCandidate>, GraphError> {
    let mut stmt = tx
        .prepare_cached(
            "SELECT target_path, target_symbol FROM imports
             WHERE from_file = ?1
             ORDER BY target_symbol IS NULL, target_path, target_symbol",
        )
        .map_err(|source| GraphError::sqlite("prepare import lookup", source))?;
    let rows = stmt
        .query_map(params![from_file], |row| {
            Ok(ImportCandidate {
                target_path: row.get(0)?,
                target_symbol: row.get(1)?,
            })
        })
        .map_err(|source| GraphError::sqlite("query imports for ref resolution", source))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|source| GraphError::sqlite("collect imports for ref resolution", source))
}

fn symbol_candidate_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SymbolCandidate> {
    Ok(SymbolCandidate {
        id: row.get(0)?,
        file_path: row.get(1)?,
        name: row.get(2)?,
        qualified: row.get(3)?,
    })
}

fn unique_candidate(candidates: &[SymbolCandidate]) -> Option<SymbolCandidate> {
    if candidates.len() == 1 {
        candidates.first().cloned()
    } else {
        None
    }
}

fn candidate_matches_qualified(
    tx: &Transaction<'_>,
    candidate: &SymbolCandidate,
    target: &str,
) -> Result<bool, GraphError> {
    let target = normalize_module_path(target);
    let qualified = normalize_module_path(&candidate.qualified);
    if qualified == target {
        return Ok(true);
    }
    Ok(module_prefixes_for_candidate(tx, candidate)?
        .into_iter()
        .any(|prefix| join_module_symbol(&prefix, &qualified) == target))
}

fn candidate_matches_module(
    tx: &Transaction<'_>,
    candidate: &SymbolCandidate,
    module: &str,
) -> Result<bool, GraphError> {
    let module = normalize_module_path(module);
    Ok(module_prefixes_for_candidate(tx, candidate)?
        .into_iter()
        .any(|prefix| prefix == module || prefix.ends_with(&format!("::{module}"))))
}

fn module_prefixes_for_candidate(
    tx: &Transaction<'_>,
    candidate: &SymbolCandidate,
) -> Result<BTreeSet<String>, GraphError> {
    let mut prefixes = module_prefixes_for_file(tx, &candidate.file_path)?;
    if let Some(prefix) = qualified_prefix_before_name(&candidate.qualified, &candidate.name) {
        prefixes.insert(normalize_module_path(&prefix));
    }
    Ok(prefixes)
}

fn import_module_for_ref(
    from_file: &str,
    import: &ImportCandidate,
    raw_ref: &RawRef,
) -> Option<String> {
    let target_qualified = raw_ref.target_qualified.as_deref().unwrap_or_default();
    if import.target_symbol.as_deref() == Some(raw_ref.target_name.as_str()) {
        return Some(resolve_import_path(from_file, &import.target_path));
    }
    let qualifier = normalize_module_path(target_qualified)
        .strip_suffix(&format!("::{}", raw_ref.target_name))?
        .to_string();
    let local = import.target_symbol.as_deref().unwrap_or_else(|| {
        import
            .target_path
            .rsplit([':', '.'])
            .find(|part| !part.is_empty())
            .unwrap_or("")
    });

    if qualifier.rsplit("::").next() != Some(local) {
        return None;
    }
    let mut module = resolve_import_path(from_file, &import.target_path);
    if from_file.ends_with(".rs") && import.target_symbol.is_some() {
        module = join_module_symbol(&module, local);
    }
    Some(module)
}

fn resolve_import_path(from_file: &str, path: &str) -> String {
    let mut parts = normalize_module_path(path)
        .split("::")
        .filter(|part| !part.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if parts.first().is_some_and(|part| part == "crate") {
        parts.remove(0);
        return parts.join("::");
    }
    if parts
        .first()
        .is_some_and(|part| part == "self" || part == "super")
    {
        let mut base = file_module_parts(from_file);
        while parts.first().is_some_and(|part| part == "super") {
            parts.remove(0);
            base.pop();
        }
        if parts.first().is_some_and(|part| part == "self") {
            parts.remove(0);
        }
        base.extend(parts);
        return base.join("::");
    }
    parts.join("::")
}

fn normalize_module_path(path: &str) -> String {
    path.replace('.', "::")
        .split("::")
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("::")
}

fn join_module_symbol(module: &str, symbol: &str) -> String {
    if module.is_empty() {
        symbol.to_string()
    } else {
        format!("{module}::{symbol}")
    }
}

fn file_module_parts(file_path: &str) -> Vec<String> {
    let is_rust = file_path.ends_with(".rs");
    let stem = file_path
        .strip_suffix(".rs")
        .or_else(|| file_path.strip_suffix(".py"))
        .unwrap_or(file_path);
    let mut parts = stem
        .split('/')
        .filter(|part| !part.is_empty() && *part != "src")
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if parts.last().is_some_and(|part| {
        (is_rust && (part == "lib" || part == "main" || part == "mod"))
            || (!is_rust && part == "__init__")
    }) {
        parts.pop();
    }
    parts
}

fn qualified_prefix_before_name(qualified: &str, name: &str) -> Option<String> {
    if qualified == name {
        return Some(String::new());
    }
    let prefix = qualified.strip_suffix(name)?;
    let trimmed = normalize_qualified_prefix(prefix);
    if trimmed.len() == prefix.len() {
        None
    } else {
        Some(trimmed)
    }
}

fn normalize_qualified_prefix(prefix: &str) -> String {
    prefix
        .trim_end_matches(|ch: char| !is_qualified_word(ch))
        .to_string()
}

fn is_qualified_word(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | '-')
}

fn update_sync_meta(tx: &Transaction<'_>, mode: SyncMode) -> Result<(), GraphError> {
    let key = match mode {
        SyncMode::Auto => "last_incremental_at",
        SyncMode::Full => "last_full_build_at",
    };
    tx.prepare_cached("UPDATE meta SET value = ?1 WHERE key = ?2")
        .map_err(|source| GraphError::sqlite("prepare graph sync metadata update", source))?
        .execute(params![
            now_epoch_nanos("record sync timestamp")?.to_string(),
            key
        ])
        .map_err(|source| GraphError::sqlite("update graph sync metadata", source))?;
    Ok(())
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

#[derive(Debug, Clone)]
struct ResolvedRef {
    target_qualified: Option<String>,
    target_symbol_hint: Option<i64>,
    confidence: &'static str,
}

impl ResolvedRef {
    fn candidate(candidate: SymbolCandidate, confidence: &'static str) -> Self {
        Self {
            target_qualified: Some(candidate.qualified),
            target_symbol_hint: Some(candidate.id),
            confidence,
        }
    }
}

#[derive(Debug, Clone)]
struct SymbolCandidate {
    id: i64,
    file_path: String,
    name: String,
    qualified: String,
}

#[derive(Debug, Clone)]
struct ImportCandidate {
    target_path: String,
    target_symbol: Option<String>,
}

enum ImportResolution {
    Unique(SymbolCandidate),
    Ambiguous,
    None,
}

#[cfg(test)]
#[path = "tests/pass2.rs"]
mod tests;
