//! Pass 2 resolves raw refs after Pass 1 has written files, symbols, and imports.
//!
//! Resolution follows the documented confidence ladder in strict order:
//! same-file exact matches, explicit imports, qualified cross-file matches,
//! same-module matches, then fuzzy name-only refs. One rung comes first and
//! short-circuits the rest: a Rust ref whose target names a member of a type
//! (`<T>::m`, `<T as Trait>::m`, `path::T::m`) is resolved by the type-member
//! rung in [`type_member`], which narrows to the type's module before picking
//! the inherent, trait-impl, or trait-declaration member. Its answer, fuzzy
//! included, is final, except that a receiver or `self` call whose type has
//! no indexed member and no written path continues down the ladder. All refs
//! for the files refreshed by the current sync are rewritten in one SQLite
//! transaction.
//!
//! An unchanged file's ref can still depend on a changed file. Apart from
//! trait impl blocks (below), the ladder only considers candidates named like
//! the ref, and it reads from a candidate's file only its language and the
//! `(name, qualified, kind)` of its symbols (the file's module
//! prefixes derive from every symbol in it). So a ref outside the changed
//! files can resolve differently after the change only if its `target_name`
//! names a symbol of a changed or removed file whose definitions under that
//! name differ, before versus after, or any symbol of a changed file whose
//! module prefixes differ, or any member of a trait whose impl blocks a
//! changed file adds or removes (the type-member rung resolves `<T>::m` to a
//! trait's default body through `impl Trait for T`, which is not named `m`).
//! Its stored `target_symbol_hint` also goes stale if
//! it points at a symbol row the rewrite replaced. An incremental sync
//! re-resolves exactly those refs in the same transaction, from the resolution
//! inputs stored with each ref (`extracted_qualified`, `unresolved_receiver`,
//! `spelled_path`), so every ref ends up as a full sync would store it.
//!
//! This transaction is also the single point where the files pass 1 wrote
//! become current (`STD-03 §R8`): it stamps their real content hash and mtime,
//! clears the database's unfinished-sync marker, and records the sync time.
//! When a sync starts and finds that marker, an earlier sync stopped between
//! pass 1 and this commit, and what the files it rewrote or removed defined
//! beforehand is lost. So that sync re-resolves every stored ref outside the
//! files it rewrites ([`Reresolve::All`]) instead of only the dependents, and
//! finishes the recovery before anything else is committed (`STD-03 §R9`).
//!
//! Two rungs match on the bare short name alone: the same-file rung and the
//! same-module rung. A method call whose receiver type the extractor could not
//! determine (`args.execute()`, recorded with
//! [`RawRef::unresolved_receiver`]) carries no evidence about which type's
//! method is meant, so those two rungs skip it: a dispatcher that calls
//! `args.execute()` must not resolve to its own `execute` method, and a list
//! `.append(...)` must not resolve to a same-module `append` function. Such
//! refs still resolve through the import and qualified rungs, which match a
//! qualified path, and otherwise land at `fuzzy_name`, where name-only
//! matching is labelled as such.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::extract::RawRef;
use rusqlite::{Connection, Transaction, TransactionBehavior, params};

use super::pass1::ExtractedFileRefs;
use crate::{GraphError, SyncMode, SyncObserver, SyncPhase, SyncProgress};
use type_member::{MemberTarget, call_module_qualifier, is_member_symbol, names_free_item};

const CONFIDENCE_EXACT: &str = "exact";
const CONFIDENCE_IMPORT_RESOLVED: &str = "import_resolved";
const CONFIDENCE_SAME_MODULE: &str = "same_module";
const CONFIDENCE_FUZZY_NAME: &str = "fuzzy_name";
const RUNTIME_INVOCATION_KIND: &str = "runtime_invocation";

/// How many refs pass 2 resolves between progress reports. Reporting per ref
/// would call the observer far too often on a large corpus; this cadence
/// keeps the call count bounded while still moving visibly.
const RESOLVE_PROGRESS_CADENCE: usize = 500;

/// Which stored refs outside the rewritten files pass 2 re-resolves.
#[derive(Debug)]
pub(crate) enum Reresolve<'a> {
    /// The refs that depend on what the modified and removed files defined
    /// before pass 1 rewrote them (see [`Definitions::load`]) compared with
    /// what they define now.
    Dependents(&'a Definitions),
    /// Every stored ref: what an interrupted sync's files defined before is
    /// unknown. A full sync also uses it; it rewrites every readable file, so
    /// only the refs of files it could not read remain to re-resolve.
    All,
}

/// Rewrites the refs of the files Pass 1 wrote and makes those files current,
/// then re-resolves the refs elsewhere that `reresolve` selects.
pub(crate) fn run(
    db_path: &Path,
    mode: SyncMode,
    refs_by_file: Vec<ExtractedFileRefs>,
    reresolve: Reresolve<'_>,
    observer: Option<&dyn SyncObserver>,
    files_seen: usize,
    current_path: Option<String>,
) -> Result<(), GraphError> {
    let mut conn = open_writer_connection(db_path)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|source| GraphError::sqlite("begin pass2 refs transaction", source))?;

    let units_total: usize = refs_by_file
        .iter()
        .map(|file_refs| file_refs.refs.len())
        .sum();
    let mut units_done = 0usize;
    if let Some(observer) = observer {
        report_resolving_progress(observer, files_seen, &current_path, units_done, units_total);
    }

    let mut resolver = Resolver::new(&tx);
    let mut rewritten_files = BTreeSet::new();
    let midway = refs_by_file.len() / 2;
    for (index, file_refs) in refs_by_file.into_iter().enumerate() {
        if index == midway && index > 0 {
            super::inject_fault(super::FaultPoint::MidPass2);
        }
        rewritten_files.insert(file_refs.file_path.clone());
        delete_refs_for_file(&tx, &file_refs.file_path)?;
        mark_current(&tx, &file_refs)?;
        let resolved = resolver.resolve_file(&file_refs.file_path, &file_refs.refs)?;
        for (raw_ref, resolved) in file_refs.refs.iter().zip(&resolved) {
            insert_ref(&tx, &file_refs.file_path, raw_ref, resolved)?;
            units_done += 1;
            if let Some(observer) = observer
                && units_done.is_multiple_of(RESOLVE_PROGRESS_CADENCE)
            {
                report_resolving_progress(
                    observer,
                    files_seen,
                    &current_path,
                    units_done,
                    units_total,
                );
            }
        }
    }

    match reresolve {
        Reresolve::Dependents(before) => {
            let dependents = Dependents::between(&tx, before, &rewritten_files)?;
            refresh_dependent_refs(&tx, &mut resolver, &dependents, &rewritten_files)?;
        }
        Reresolve::All => {
            refresh_all_refs(&tx, &mut resolver, &rewritten_files)?;
        }
    }

    if let Some(observer) = observer {
        report_resolving_progress(observer, files_seen, &current_path, units_done, units_total);
    }

    update_sync_meta(&tx, mode)?;
    super::clear_sync_pending(&tx)?;
    tx.commit()
        .map_err(|source| GraphError::sqlite("commit pass2 refs transaction", source))?;
    Ok(())
}

/// What a set of files defines: each file's symbol rows. Captured before
/// Pass 1 rewrites or removes the files, and compared with what they define
/// afterwards. See [`run`].
#[derive(Debug, Default)]
pub(crate) struct Definitions {
    by_file: BTreeMap<String, Vec<DefinedSymbol>>,
}

impl Definitions {
    /// Reads the symbols `file_paths` define now.
    pub(crate) fn load<'a>(
        db_path: &Path,
        file_paths: impl IntoIterator<Item = &'a str>,
    ) -> Result<Self, GraphError> {
        let conn =
            crate::open_read_connection(db_path, "open graph database for prior definitions")?;
        let mut by_file = BTreeMap::new();
        for file_path in file_paths {
            by_file.insert(file_path.to_string(), defined_symbols(&conn, file_path)?);
        }
        Ok(Self { by_file })
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct DefinedSymbol {
    name: String,
    qualified: String,
    kind: String,
    id: i64,
}

fn defined_symbols(conn: &Connection, file_path: &str) -> Result<Vec<DefinedSymbol>, GraphError> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT name, qualified, kind, id FROM symbols
             WHERE file_path = ?1
             ORDER BY name, qualified, kind, id",
        )
        .map_err(|source| GraphError::sqlite("prepare defined symbol lookup", source))?;
    let rows = stmt
        .query_map(params![file_path], |row| {
            Ok(DefinedSymbol {
                name: row.get(0)?,
                qualified: row.get(1)?,
                kind: row.get(2)?,
                id: row.get(3)?,
            })
        })
        .map_err(|source| GraphError::sqlite("query defined symbols", source))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|source| GraphError::sqlite("collect defined symbols", source))
}

/// The refs outside the rewritten files that an incremental sync must
/// re-resolve: every ref whose `target_name` is in `names`, and every ref whose
/// hint points at one of the `replaced` symbol rows.
///
/// The type-member rung also reads trait impl blocks (`impl Greet for Foo`)
/// by their Self type, not by the ref's name: `foo.hi()` (`<Foo>::hi`)
/// resolves to the default body `Greet::hi` only while such an impl exists.
/// So when a trait impl block changes, every member its trait declares is
/// added to `names`.
#[derive(Debug, Default)]
struct Dependents {
    names: BTreeSet<String>,
    replaced: HashMap<i64, String>,
    /// Traits whose impl blocks changed; expanded into `names` by
    /// [`Dependents::between`].
    changed_impl_traits: BTreeSet<String>,
}

impl Dependents {
    /// Compares what each changed or removed file defined before the sync
    /// with what it defines now.
    fn between(
        tx: &Transaction<'_>,
        before: &Definitions,
        rewritten_files: &BTreeSet<String>,
    ) -> Result<Self, GraphError> {
        let mut dependents = Self::default();
        let files = before
            .by_file
            .keys()
            .chain(rewritten_files)
            .collect::<BTreeSet<_>>();
        for file_path in files {
            let old = before
                .by_file
                .get(file_path)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let new = defined_symbols(tx, file_path)?;
            dependents.add_file(file_path, old, &new);
        }
        for trait_name in std::mem::take(&mut dependents.changed_impl_traits) {
            dependents
                .names
                .extend(type_member::trait_member_names(tx, &trait_name)?);
        }
        Ok(dependents)
    }

    fn add_file(&mut self, file_path: &str, old: &[DefinedSymbol], new: &[DefinedSymbol]) {
        for symbol in old {
            if !keeps_meaning(symbol, old, new) {
                self.replaced.insert(symbol.id, symbol.name.clone());
            }
        }

        // Every candidate in the file is matched against its module prefixes;
        // if they moved, any ref naming any of its symbols may resolve anew.
        let changed_names = if prefixes_of(file_path, old) != prefixes_of(file_path, new) {
            old.iter()
                .chain(new)
                .map(|symbol| symbol.name.as_str())
                .collect::<BTreeSet<_>>()
        } else {
            let old_by_name = definitions_by_name(old);
            let new_by_name = definitions_by_name(new);
            old_by_name
                .keys()
                .chain(new_by_name.keys())
                .filter(|name| old_by_name.get(*name) != new_by_name.get(*name))
                .copied()
                .collect()
        };
        for symbol in old.iter().chain(new) {
            if symbol.kind == "impl"
                && changed_names.contains(symbol.name.as_str())
                && let Some(trait_name) = type_member::impl_trait_name(&symbol.qualified)
            {
                self.changed_impl_traits.insert(trait_name);
            }
        }
        self.names
            .extend(changed_names.into_iter().map(str::to_string));
    }
}

/// Whether a hint to the old row `symbol.id` still names the same definition
/// after the file was rewritten.
///
/// pass1 deletes a rewritten file's symbol rows and inserts them again, and
/// `symbols.id` is a plain rowid (no `AUTOINCREMENT`), so SQLite hands the
/// freed ids out again: when the file held the highest ids, an unchanged
/// definition can come back under a new id while its old id now names a
/// different symbol. A hint is kept only when the row now holding its id is
/// the same definition, `(name, qualified, kind)`, and no other definition in
/// the file shares that `(name, qualified)`, since the ladder orders equal
/// candidates by id.
fn keeps_meaning(symbol: &DefinedSymbol, old: &[DefinedSymbol], new: &[DefinedSymbol]) -> bool {
    let same_identity =
        |other: &&DefinedSymbol| other.name == symbol.name && other.qualified == symbol.qualified;
    let Some(now) = new.iter().find(|other| other.id == symbol.id) else {
        return false;
    };
    now.name == symbol.name
        && now.qualified == symbol.qualified
        && now.kind == symbol.kind
        && old.iter().filter(same_identity).count() == 1
        && new.iter().filter(same_identity).count() == 1
}

/// A file's definitions grouped by name, as the ladder sees them: the
/// qualified name and kind of each, without the row id.
fn definitions_by_name(symbols: &[DefinedSymbol]) -> BTreeMap<&str, Vec<(&str, &str)>> {
    let mut by_name: BTreeMap<&str, Vec<(&str, &str)>> = BTreeMap::new();
    for symbol in symbols {
        by_name
            .entry(symbol.name.as_str())
            .or_default()
            .push((symbol.qualified.as_str(), symbol.kind.as_str()));
    }
    by_name
}

fn prefixes_of(file_path: &str, symbols: &[DefinedSymbol]) -> BTreeSet<String> {
    module_prefixes(
        file_path,
        symbols
            .iter()
            .map(|symbol| (symbol.qualified.as_str(), symbol.name.as_str())),
    )
}

/// Re-resolves the stored refs outside `rewritten_files` that `dependents`
/// selects, and updates each row whose resolution changed. Returns the number
/// of rows updated.
fn refresh_dependent_refs(
    tx: &Transaction<'_>,
    resolver: &mut Resolver<'_, '_>,
    dependents: &Dependents,
    rewritten_files: &BTreeSet<String>,
) -> Result<usize, GraphError> {
    // A ref resolved to a symbol carries that symbol's name as its
    // `target_name`, so the refs hinting at a replaced row are among the refs
    // with the replaced symbols' names.
    let lookup_names = dependents
        .names
        .iter()
        .chain(dependents.replaced.values())
        .collect::<BTreeSet<_>>();
    let mut by_file: BTreeMap<String, Vec<StoredRef>> = BTreeMap::new();
    {
        let mut stmt = tx
            .prepare_cached(
                "SELECT id, from_file, from_span_start, from_span_end, target_name,
                        extracted_qualified, kind, unresolved_receiver,
                        target_qualified, target_symbol_hint, confidence, spelled_path
                 FROM refs
                 WHERE target_name = ?1
                 ORDER BY id",
            )
            .map_err(|source| GraphError::sqlite("prepare dependent ref lookup", source))?;
        for name in lookup_names {
            let rows = stmt
                .query_map(params![name], StoredRef::from_row)
                .map_err(|source| GraphError::sqlite("query dependent refs", source))?;
            for row in rows {
                let stored =
                    row.map_err(|source| GraphError::sqlite("read dependent ref", source))?;
                let depends = dependents.names.contains(&stored.raw.target_name)
                    || stored
                        .target_symbol_hint
                        .is_some_and(|hint| dependents.replaced.contains_key(&hint));
                if depends && !rewritten_files.contains(&stored.raw.from_file) {
                    by_file
                        .entry(stored.raw.from_file.clone())
                        .or_default()
                        .push(stored);
                }
            }
        }
    }

    let mut updated = 0;
    for (from_file, stored) in by_file {
        updated += re_resolve_stored(tx, resolver, &from_file, &stored)?;
    }
    Ok(updated)
}

/// Re-resolves every stored ref outside `rewritten_files`, one source file at
/// a time, and updates each row whose resolution changed. Returns the number
/// of rows updated.
fn refresh_all_refs(
    tx: &Transaction<'_>,
    resolver: &mut Resolver<'_, '_>,
    rewritten_files: &BTreeSet<String>,
) -> Result<usize, GraphError> {
    let from_files = {
        let mut stmt = tx
            .prepare("SELECT DISTINCT from_file FROM refs ORDER BY from_file")
            .map_err(|source| GraphError::sqlite("prepare stored ref file lookup", source))?;
        stmt.query_map([], |row| row.get::<_, String>(0))
            .map_err(|source| GraphError::sqlite("query stored ref files", source))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| GraphError::sqlite("collect stored ref files", source))?
    };
    let mut updated = 0;
    for from_file in from_files {
        if rewritten_files.contains(&from_file) {
            continue;
        }
        let stored = {
            let mut stmt = tx
                .prepare_cached(
                    "SELECT id, from_file, from_span_start, from_span_end, target_name,
                            extracted_qualified, kind, unresolved_receiver,
                            target_qualified, target_symbol_hint, confidence, spelled_path
                     FROM refs
                     WHERE from_file = ?1
                     ORDER BY id",
                )
                .map_err(|source| GraphError::sqlite("prepare stored ref lookup", source))?;
            stmt.query_map(params![from_file], StoredRef::from_row)
                .map_err(|source| GraphError::sqlite("query stored refs", source))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|source| GraphError::sqlite("read stored ref", source))?
        };
        updated += re_resolve_stored(tx, resolver, &from_file, &stored)?;
    }
    Ok(updated)
}

/// Re-resolves `stored`, refs of `from_file`, and updates each row whose
/// resolution changed. Returns the number of rows updated.
fn re_resolve_stored(
    tx: &Transaction<'_>,
    resolver: &mut Resolver<'_, '_>,
    from_file: &str,
    stored: &[StoredRef],
) -> Result<usize, GraphError> {
    let raw_refs = stored.iter().map(|row| row.raw.clone()).collect::<Vec<_>>();
    let resolved = resolver.resolve_file(from_file, &raw_refs)?;
    let mut updated = 0;
    for (row, resolved) in stored.iter().zip(resolved) {
        if row.target_qualified == resolved.target_qualified
            && row.target_symbol_hint == resolved.target_symbol_hint
            && row.confidence == resolved.confidence
        {
            continue;
        }
        tx.prepare_cached(
            "UPDATE refs
             SET target_qualified = ?1, target_symbol_hint = ?2, confidence = ?3
             WHERE id = ?4",
        )
        .map_err(|source| GraphError::sqlite("prepare dependent ref update", source))?
        .execute(params![
            resolved.target_qualified,
            resolved.target_symbol_hint,
            resolved.confidence,
            row.id
        ])
        .map_err(|source| GraphError::sqlite("update dependent ref", source))?;
        updated += 1;
    }
    Ok(updated)
}

/// A stored ref read back for re-resolution: its resolution inputs as a
/// [`RawRef`], plus the resolution currently stored.
struct StoredRef {
    id: i64,
    raw: RawRef,
    target_qualified: Option<String>,
    target_symbol_hint: Option<i64>,
    confidence: String,
}

impl StoredRef {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        let confidence: String = row.get(10)?;
        Ok(Self {
            id: row.get(0)?,
            raw: RawRef {
                from_file: row.get(1)?,
                from_span_start: i64_to_usize(row.get(2)?),
                from_span_end: i64_to_usize(row.get(3)?),
                target_name: row.get(4)?,
                target_qualified: row.get(5)?,
                kind: row.get(6)?,
                confidence: confidence.clone(),
                unresolved_receiver: row.get(7)?,
                spelled_path: row.get(11)?,
            },
            target_qualified: row.get(8)?,
            target_symbol_hint: row.get(9)?,
            confidence,
        })
    }
}

/// Span offsets are written from `usize`, so a negative value cannot occur;
/// spans are not resolution inputs, so clamping one is harmless.
fn i64_to_usize(value: i64) -> usize {
    usize::try_from(value).unwrap_or_default()
}

fn report_resolving_progress(
    observer: &dyn SyncObserver,
    files_seen: usize,
    current_path: &Option<String>,
    units_done: usize,
    units_total: usize,
) {
    observer.on_progress(&SyncProgress {
        phase: SyncPhase::Resolving,
        files_seen,
        files_indexed: files_seen,
        current_path: current_path.clone(),
        units_done,
        units_total,
    });
}

fn open_writer_connection(db_path: &Path) -> Result<Connection, GraphError> {
    let conn = Connection::open(db_path)
        .map_err(|source| GraphError::sqlite("open graph database for pass2 writes", source))?;
    crate::store::configure_sync_writer(&conn, "configure graph database for pass2 writes")?;
    Ok(conn)
}

/// Stamps the real content hash and mtime on a file pass 1 wrote with its
/// pending sentinels, making it current once this transaction commits.
fn mark_current(tx: &Transaction<'_>, file: &ExtractedFileRefs) -> Result<(), GraphError> {
    tx.prepare_cached("UPDATE files SET content_hash = ?1, mtime_ns = ?2 WHERE path = ?3")
        .map_err(|source| GraphError::sqlite("prepare graph file stamp", source))?
        .execute(params![file.content_hash, file.mtime_ns, file.file_path])
        .map_err(|source| GraphError::sqlite("stamp graph file current", source))?;
    Ok(())
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
            target_symbol_hint, kind, confidence, extracted_qualified, unresolved_receiver,
            spelled_path
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
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
        raw_ref.target_qualified,
        raw_ref.unresolved_receiver,
        raw_ref.spelled_path,
    ])
    .map_err(|source| GraphError::sqlite("insert resolved ref row", source))?;
    Ok(())
}

/// Everything a ref's resolution depends on besides the file it sits in.
///
/// [`Resolver::resolve_ref`] is a pure function of the calling file, the
/// symbol and import rows Pass 1 wrote (which Pass 2 never changes), and these
/// facts about the ref, which are all the ladder reads from a [`RawRef`]:
/// whether its kind is `runtime_invocation` (never resolved) or `use` (skips
/// the qualified rung), whether [`resolves_by_name_only`] holds (the same-file
/// and same-module rungs), whether the extractor spelled its path
/// ([`RawRef::spelled_path`], the same-module rung's module-path
/// check), and its `target_name` and `target_qualified` (every rung, the
/// type-member rung, and [`import_module_for_ref`]). Two refs in one file with equal keys
/// therefore resolve identically, so a file's refs are resolved once per
/// distinct key. A new ladder input must be added here AND stored in `refs`
/// ([`insert_ref`]) and read back by [`StoredRef::from_row`], because an
/// incremental sync re-resolves refs in unchanged files from their stored
/// rows alone.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RefKey {
    is_runtime_invocation: bool,
    is_use: bool,
    name_only: bool,
    spelled_path: bool,
    target_name: String,
    target_qualified: Option<String>,
}

impl RefKey {
    fn of(raw_ref: &RawRef) -> Self {
        Self {
            is_runtime_invocation: raw_ref.kind == RUNTIME_INVOCATION_KIND,
            is_use: raw_ref.kind == "use",
            name_only: resolves_by_name_only(raw_ref),
            spelled_path: raw_ref.spelled_path,
            target_name: raw_ref.target_name.clone(),
            target_qualified: raw_ref.target_qualified.clone(),
        }
    }
}

/// A type's trait impl blocks, each with its trait's last path segment.
type TraitImpls = Rc<[(String, NamedCandidate)]>;

/// Resolves refs against the symbols and imports Pass 1 wrote.
///
/// Pass 2 only rewrites `refs`, so the symbol and import rows it reads are
/// fixed for the whole pass. The resolver memoizes the lookups the ladder
/// repeats: each file's module prefixes, each name's cross-file candidates,
/// and each calling file's imports. Without the memo a common name such as
/// `new` reloaded every candidate file's symbols for every ref naming it.
struct Resolver<'a, 'conn> {
    tx: &'a Transaction<'conn>,
    file_prefixes: HashMap<String, Rc<BTreeSet<String>>>,
    candidates_by_name: HashMap<String, Rc<[NamedCandidate]>>,
    /// Trait impl blocks by Self type name, loaded on first use.
    trait_impls: Option<HashMap<String, TraitImpls>>,
    qualified_matches: HashMap<(String, String, String), Option<SymbolCandidate>>,
}

impl<'a, 'conn> Resolver<'a, 'conn> {
    fn new(tx: &'a Transaction<'conn>) -> Self {
        Self {
            tx,
            file_prefixes: HashMap::new(),
            candidates_by_name: HashMap::new(),
            trait_impls: None,
            qualified_matches: HashMap::new(),
        }
    }

    /// Resolves every ref of one file, in order.
    fn resolve_file(
        &mut self,
        from_file: &str,
        refs: &[RawRef],
    ) -> Result<Vec<ResolvedRef>, GraphError> {
        let language = self
            .tx
            .query_row(
                "SELECT lang FROM files WHERE path = ?1",
                params![from_file],
                |row| row.get::<_, String>(0),
            )
            .map_err(|source| GraphError::sqlite("read ref file language", source))?;
        let imports = imports_for_file(self.tx, from_file)?;
        let mut memo: HashMap<RefKey, ResolvedRef> = HashMap::new();
        let mut resolved = Vec::with_capacity(refs.len());
        for raw_ref in refs {
            let key = RefKey::of(raw_ref);
            if let Some(known) = memo.get(&key) {
                resolved.push(known.clone());
                continue;
            }
            let result = self.resolve_ref(from_file, &language, &imports, raw_ref)?;
            memo.insert(key, result.clone());
            resolved.push(result);
        }
        Ok(resolved)
    }

    fn resolve_ref(
        &mut self,
        from_file: &str,
        language: &str,
        imports: &[ImportCandidate],
        raw_ref: &RawRef,
    ) -> Result<ResolvedRef, GraphError> {
        // A runtime invocation names a program, not a symbol: it is stored as the
        // opaque program string and never climbs the resolution ladder, so a
        // program named like a function (`["git", ...]` beside `def git`) is not
        // mistaken for a call to it.
        if raw_ref.kind == RUNTIME_INVOCATION_KIND {
            return Ok(ResolvedRef::fuzzy());
        }
        if let Some(target) = MemberTarget::of_ref(from_file, raw_ref)
            && let Some(resolved) = self.resolve_type_member(from_file, imports, &target)?
        {
            return Ok(resolved);
        }
        if let Some(candidate) = resolve_exact(self.tx, from_file, raw_ref)? {
            return Ok(ResolvedRef::candidate(candidate, CONFIDENCE_EXACT));
        }
        match self.resolve_import(from_file, language, imports, raw_ref)? {
            ImportResolution::Unique(candidate) => {
                return Ok(ResolvedRef::candidate(
                    candidate,
                    CONFIDENCE_IMPORT_RESOLVED,
                ));
            }
            ImportResolution::Ambiguous => return Ok(ResolvedRef::fuzzy()),
            ImportResolution::None => {}
        }
        if let Some(candidate) = self.resolve_qualified(language, raw_ref)? {
            return Ok(ResolvedRef::candidate(candidate, CONFIDENCE_EXACT));
        }
        if let Some(candidate) = self.resolve_same_module(from_file, language, raw_ref)? {
            return Ok(ResolvedRef::candidate(candidate, CONFIDENCE_SAME_MODULE));
        }
        Ok(ResolvedRef::fuzzy())
    }

    fn resolve_qualified(
        &mut self,
        language: &str,
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
        let key = (
            language.to_string(),
            raw_ref.target_name.clone(),
            normalize_module_path(target),
        );
        if let Some(known) = self.qualified_matches.get(&key) {
            return Ok(known.clone());
        }
        let matched = self.unique_qualified_match(&key.0, &key.1, &key.2)?;
        self.qualified_matches.insert(key, matched.clone());
        Ok(matched)
    }

    /// The one candidate named `name` that `target` (already normalized)
    /// names in `language`, if exactly one does. Memoized per
    /// `(language, name, target)` by
    /// [`Self::resolve_qualified`]: the answer depends on nothing else.
    fn unique_qualified_match(
        &mut self,
        language: &str,
        name: &str,
        target: &str,
    ) -> Result<Option<SymbolCandidate>, GraphError> {
        let mut matched = None;
        for candidate in self.symbols_by_name(name)?.iter() {
            if candidate.language == language
                && self.candidate_matches_qualified(candidate, target)?
            {
                if matched.is_some() {
                    return Ok(None);
                }
                matched = Some(candidate.symbol.clone());
            }
        }
        Ok(matched)
    }

    fn resolve_import(
        &mut self,
        from_file: &str,
        language: &str,
        imports: &[ImportCandidate],
        raw_ref: &RawRef,
    ) -> Result<ImportResolution, GraphError> {
        let free_item = names_free_item(from_file, raw_ref);
        for explicit in [true, false] {
            let mut matches = BTreeMap::new();
            for import in imports.iter().filter(|import| {
                (import.target_symbol.as_deref() == Some(raw_ref.target_name.as_str())) == explicit
            }) {
                let Some(module_path) = import_module_for_ref(from_file, import, raw_ref) else {
                    continue;
                };
                let module = normalize_module_path(&module_path);
                for candidate in self.symbols_by_name(&raw_ref.target_name)?.iter() {
                    if candidate.language != language
                        || (free_item && is_member_symbol(&candidate.symbol.qualified))
                    {
                        continue;
                    }
                    if self.candidate_matches_module(candidate, &module)? {
                        matches.insert(candidate.symbol.id, candidate.symbol.clone());
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
        &mut self,
        from_file: &str,
        language: &str,
        raw_ref: &RawRef,
    ) -> Result<Option<SymbolCandidate>, GraphError> {
        if !resolves_by_name_only(raw_ref) {
            return Ok(None);
        }
        let prefixes = self.module_prefixes_for_file(from_file)?;
        if prefixes.is_empty() {
            return Ok(None);
        }

        let free_item = names_free_item(from_file, raw_ref);
        let qualifier = call_module_qualifier(from_file, raw_ref);
        let mut matched = None;
        for candidate in self.symbols_by_name(&raw_ref.target_name)?.iter() {
            if candidate.language != language
                || candidate.symbol.file_path == from_file
                || (free_item && is_member_symbol(&candidate.symbol.qualified))
            {
                continue;
            }
            if let Some(qualifier) = qualifier.as_deref()
                && !self.candidate_within_module(candidate, qualifier)?
            {
                continue;
            }
            if self.candidate_shares_module(candidate, &prefixes)? {
                if matched.is_some() {
                    return Ok(None);
                }
                matched = Some(candidate.symbol.clone());
            }
        }
        Ok(matched)
    }

    /// Cross-file counterpart of [`symbols_in_file_by_name`]; applies the same
    /// `impl` exclusion. Memoized per name for the pass.
    fn symbols_by_name(&mut self, name: &str) -> Result<Rc<[NamedCandidate]>, GraphError> {
        if let Some(candidates) = self.candidates_by_name.get(name) {
            return Ok(Rc::clone(candidates));
        }
        let candidates: Rc<[NamedCandidate]> = symbols_by_name(self.tx, name)?.into();
        self.candidates_by_name
            .insert(name.to_string(), Rc::clone(&candidates));
        Ok(candidates)
    }

    /// Memoized [`module_prefixes_for_file`].
    fn module_prefixes_for_file(
        &mut self,
        file_path: &str,
    ) -> Result<Rc<BTreeSet<String>>, GraphError> {
        if let Some(prefixes) = self.file_prefixes.get(file_path) {
            return Ok(Rc::clone(prefixes));
        }
        let prefixes = Rc::new(module_prefixes_for_file(self.tx, file_path)?);
        self.file_prefixes
            .insert(file_path.to_string(), Rc::clone(&prefixes));
        Ok(prefixes)
    }

    /// Whether `target` (already normalized) names the candidate, either
    /// directly or under one of the candidate's module prefixes.
    ///
    /// The candidate's prefixes are its file's prefixes plus the prefix of its
    /// own qualified name; the test is a disjunction over them, so the two
    /// sources are checked in turn rather than merged into a new set.
    fn candidate_matches_qualified(
        &mut self,
        candidate: &NamedCandidate,
        target: &str,
    ) -> Result<bool, GraphError> {
        let qualified = candidate.normalized_qualified.as_str();
        if qualified == target {
            return Ok(true);
        }
        let matches = |prefix: &str| is_module_symbol(target, prefix, qualified);
        if candidate.own_prefix.as_deref().is_some_and(matches) {
            return Ok(true);
        }
        Ok(self
            .module_prefixes_for_file(&candidate.symbol.file_path)?
            .iter()
            .any(|prefix| matches(prefix)))
    }

    /// Whether one of the candidate's module prefixes is `module` (already
    /// normalized) or ends with it.
    fn candidate_matches_module(
        &mut self,
        candidate: &NamedCandidate,
        module: &str,
    ) -> Result<bool, GraphError> {
        let matches = |prefix: &str| prefix == module || is_module_suffix(prefix, module);
        if candidate.own_prefix.as_deref().is_some_and(matches) {
            return Ok(true);
        }
        Ok(self
            .module_prefixes_for_file(&candidate.symbol.file_path)?
            .iter()
            .any(|prefix| matches(prefix)))
    }

    /// Whether the candidate's module prefixes intersect `prefixes`.
    fn candidate_shares_module(
        &mut self,
        candidate: &NamedCandidate,
        prefixes: &BTreeSet<String>,
    ) -> Result<bool, GraphError> {
        if candidate
            .own_prefix
            .as_ref()
            .is_some_and(|prefix| prefixes.contains(prefix))
        {
            return Ok(true);
        }
        let candidate_prefixes = self.module_prefixes_for_file(&candidate.symbol.file_path)?;
        Ok(!prefixes.is_disjoint(&candidate_prefixes))
    }
}

fn resolve_exact(
    tx: &Transaction<'_>,
    from_file: &str,
    raw_ref: &RawRef,
) -> Result<Option<SymbolCandidate>, GraphError> {
    let mut candidates = symbols_in_file_by_name(tx, from_file, &raw_ref.target_name)?;
    if names_free_item(from_file, raw_ref) {
        candidates.retain(|candidate| !is_member_symbol(&candidate.qualified));
    }
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

    if !resolves_by_name_only(raw_ref) {
        return Ok(None);
    }
    Ok(unique_candidate(&candidates))
}

/// Whether the ref may be matched on its short name alone. False for a method
/// call whose receiver type the extractor could not determine: the receiver,
/// not the calling file or module, decides which same-named method runs.
fn resolves_by_name_only(raw_ref: &RawRef) -> bool {
    raw_ref.unresolved_receiver.is_none()
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
fn symbols_by_name(tx: &Transaction<'_>, name: &str) -> Result<Vec<NamedCandidate>, GraphError> {
    let mut stmt = tx
        .prepare_cached(
            "SELECT s.id, s.file_path, s.name, s.qualified, f.lang
             FROM symbols s JOIN files f ON f.path = s.file_path
             WHERE s.name = ?1 AND s.kind <> 'impl'
             ORDER BY s.qualified, s.id",
        )
        .map_err(|source| GraphError::sqlite("prepare name symbol lookup", source))?;
    let rows = stmt
        .query_map(params![name], |row| {
            Ok(NamedCandidate::new(
                symbol_candidate_from_row(row)?,
                row.get(4)?,
            ))
        })
        .map_err(|source| GraphError::sqlite("query symbols for ref resolution", source))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|source| GraphError::sqlite("collect symbols for ref resolution", source))
}

/// Module paths a file's symbols live under: the prefixes of every symbol's
/// qualified name, plus each suffix of the file's own module path.
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
    Ok(module_prefixes(
        from_file,
        symbols
            .iter()
            .map(|symbol| (symbol.qualified.as_str(), symbol.name.as_str())),
    ))
}

/// Module prefixes of a file defining symbols with these `(qualified, name)`
/// pairs; see [`module_prefixes_for_file`].
fn module_prefixes<'a>(
    file_path: &str,
    symbols: impl Iterator<Item = (&'a str, &'a str)>,
) -> BTreeSet<String> {
    let mut prefixes = symbols
        .filter_map(|(qualified, name)| qualified_prefix_before_name(qualified, name))
        .map(|prefix| normalize_module_path(&prefix))
        .collect::<BTreeSet<_>>();
    let file_parts = file_module_parts(file_path);
    for start in 0..file_parts.len() {
        prefixes.insert(file_parts[start..].join("::"));
    }
    prefixes
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

/// Whether `target == join_module_symbol(module, symbol)`, without building
/// the joined string.
fn is_module_symbol(target: &str, module: &str, symbol: &str) -> bool {
    if module.is_empty() {
        return target == symbol;
    }
    target.len() == module.len() + 2 + symbol.len()
        && target.starts_with(module)
        && target[module.len()..].starts_with("::")
        && target.ends_with(symbol)
}

/// Whether `path` ends with `::{module}`, without building the suffix.
fn is_module_suffix(path: &str, module: &str) -> bool {
    path.len() >= module.len() + 2
        && path.ends_with(module)
        && path[..path.len() - module.len()].ends_with("::")
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
    fn fuzzy() -> Self {
        Self {
            target_qualified: None,
            target_symbol_hint: None,
            confidence: CONFIDENCE_FUZZY_NAME,
        }
    }

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

/// A cross-file candidate with its normalized qualified name and the module
/// prefix of that name computed once, since the ladder tests them for every
/// ref naming it.
#[derive(Debug)]
struct NamedCandidate {
    symbol: SymbolCandidate,
    language: String,
    normalized_qualified: String,
    own_prefix: Option<String>,
}

impl NamedCandidate {
    fn new(symbol: SymbolCandidate, language: String) -> Self {
        let own_prefix = qualified_prefix_before_name(&symbol.qualified, &symbol.name)
            .map(|prefix| normalize_module_path(&prefix));
        Self {
            normalized_qualified: normalize_module_path(&symbol.qualified),
            symbol,
            language,
            own_prefix,
        }
    }
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

mod type_member;

#[cfg(test)]
#[path = "tests/pass2.rs"]
mod tests;
