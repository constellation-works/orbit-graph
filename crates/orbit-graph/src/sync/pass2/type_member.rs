//! Resolution of Rust refs that name a member of a known type.
//!
//! The Rust extractor records such a ref's type in `target_qualified`:
//! `<T>::m` for `self.m()` / `Self::m()` in `impl T` and for a method call on
//! a receiver whose type it can read (`runtime: &OrbitRuntime` makes
//! `runtime.enable_plugin()` `<OrbitRuntime>::enable_plugin`), and the path
//! as written for a scoped call (`ResolvedConfig::load(..)`,
//! `<T as Trait>::m(..)`).
//!
//! Rust symbols are qualified as `<T>::m` (inherent impl), `<T as Trait>::m`
//! (trait impl), or `Trait::m` (trait declaration), so the generic ladder's
//! string match misses trait impls and never narrows by type: a scoped
//! `ResolvedConfig::load(..)` fell through to name-only rungs and matched any
//! `load`. This rung matches on the Self type's name and the member name.
//! It first narrows to the modules that can hold the type (the written type
//! path, or the imports naming it), then prefers inherent methods over trait
//! methods as Rust's method lookup does, and only then narrows by the calling
//! file and module proximity. A type the file names through a path or import
//! with no indexed candidate is an external or aliased-away type and stays
//! fuzzy. `<dyn Trait>::m` targets only the trait's declaration. A member no
//! impl defines resolves to the declaration of a trait the type implements
//! (a default body).
//!
//! A ref whose type has no candidate at all keeps its old ladder when its
//! target came from a receiver or `self` (its receiver, if any, still keeps
//! it off the name-only rungs). A written `T::m(..)` path with a type-like
//! `T` can only name an associated item of `T`, so it never falls back to a
//! name-only match.

use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use rusqlite::Transaction;

use super::{
    CONFIDENCE_EXACT, CONFIDENCE_IMPORT_RESOLVED, CONFIDENCE_SAME_MODULE, ImportCandidate,
    NamedCandidate, ResolvedRef, Resolver, SymbolCandidate, TraitImpls, normalize_module_path,
    resolve_import_path, symbol_candidate_from_row,
};
use crate::GraphError;
use crate::extract::RawRef;

/// A ref target, or a symbol's qualified name, read as `Type::member`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MemberTarget {
    /// Module path written before the type (`crate::config` in
    /// `crate::config::ResolvedConfig::load`); empty when not written.
    type_path: String,
    /// Last path segment of the Self type, generics removed.
    type_name: String,
    /// Last path segment of the trait for `<T as Trait>::m`.
    trait_name: Option<String>,
    /// Whether the text was bracketed (`<T>::m`). Rust symbols qualify
    /// impl members this way and trait declarations as `Trait::m`.
    bracketed: bool,
    /// Whether the Self type is a trait object (`<dyn Trait>::m`, which the
    /// extractor also writes for an `impl Trait` receiver): the member is
    /// the trait's, never a same-named struct's inherent method.
    trait_object: bool,
    member: String,
}

impl MemberTarget {
    /// The member target of a Rust ref, if its `target_qualified` names one.
    pub(super) fn of_ref(from_file: &str, raw_ref: &RawRef) -> Option<Self> {
        if !from_file.ends_with(".rs") || raw_ref.kind == "use" {
            return None;
        }
        let target = Self::parse(raw_ref.target_qualified.as_deref()?)?;
        (target.member == raw_ref.target_name).then_some(target)
    }

    /// Parses `<T>::m`, `<path::T as Trait>::m`, or `path::T::m`, where `T`
    /// is spelled like a type (leading uppercase) and `m` like a function.
    pub(super) fn parse(qualified: &str) -> Option<Self> {
        let (self_type, trait_name, member, bracketed) =
            if let Some(inner) = qualified.strip_prefix('<') {
                let close = matching_angle(inner)?;
                let member = inner[close + 1..].strip_prefix("::")?;
                let (self_type, trait_name) = match inner[..close].split_once(" as ") {
                    Some((self_type, trait_name)) => (self_type, Some(trait_name)),
                    None => (&inner[..close], None),
                };
                (self_type.trim(), trait_name.map(str::trim), member, true)
            } else {
                let (self_type, member) = qualified.rsplit_once("::")?;
                (self_type, None, member, false)
            };
        if member.contains("::") || !is_function_name(member) {
            return None;
        }
        let (self_type, trait_object) = match self_type.strip_prefix("dyn ") {
            Some(object) => (object.trim(), true),
            None => (self_type, false),
        };
        let self_type = strip_generics(self_type);
        let (type_path, type_name) = match self_type.rsplit_once("::") {
            Some((path, name)) => (path.to_string(), name),
            None => (String::new(), self_type),
        };
        if type_name == "Self" || !type_name.starts_with(|ch: char| ch.is_ascii_uppercase()) {
            return None;
        }
        let trait_name = trait_name.map(|name| {
            let name = strip_generics(name);
            name.rsplit("::").next().unwrap_or(name).to_string()
        });
        Some(Self {
            type_path,
            type_name: type_name.to_string(),
            trait_name,
            bracketed,
            trait_object,
            member: member.to_string(),
        })
    }

    /// How a candidate symbol relates to this target, or `None` when it is
    /// another type's member.
    fn tier_of(&self, candidate: &Self) -> Option<Tier> {
        if candidate.member != self.member || candidate.type_name != self.type_name {
            return None;
        }
        let tier = match (candidate.bracketed, &candidate.trait_name) {
            (true, None) => Tier::Inherent,
            (true, Some(_)) => Tier::TraitImpl,
            (false, _) => Tier::TraitDeclaration,
        };
        // A trait object's member is the trait's declaration (or a member of
        // an inherent `impl dyn Trait`).
        if self.trait_object && tier != Tier::TraitDeclaration && !candidate.trait_object {
            return None;
        }
        match (&self.trait_name, &candidate.trait_name) {
            (Some(wanted), Some(found)) if wanted != found => None,
            (Some(_), None) if tier == Tier::Inherent => None,
            _ => Some(tier),
        }
    }
}

/// Rust's method lookup tries inherent methods before trait methods; a trait
/// declaration is the target only when nothing implements the member for the
/// type (`dyn Trait` receivers, `Trait::m(..)` paths).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Tier {
    Inherent,
    TraitImpl,
    TraitDeclaration,
}

impl Resolver<'_, '_> {
    /// Resolves a ref whose target names a member of a known type. `None`
    /// means the ref should continue down the generic ladder.
    ///
    /// The order matters: the modules that can hold the type are fixed
    /// first (the written type path, or the imports that bring the type's
    /// name into the file), and candidates outside them are never
    /// considered, so an external `reqwest::Client` or an aliased-away name
    /// never resolves to a local look-alike. Only then is the tier (inherent
    /// before trait impl before trait declaration) picked, inside that
    /// narrowed set, and only then do the same-file, same-module, and
    /// uniqueness shortcuts apply.
    pub(super) fn resolve_type_member(
        &mut self,
        from_file: &str,
        imports: &[ImportCandidate],
        target: &MemberTarget,
    ) -> Result<Option<ResolvedRef>, GraphError> {
        let candidates = self.symbols_by_name(&target.member)?;
        let members = candidates
            .iter()
            .filter_map(|candidate| {
                MemberTarget::parse(&candidate.symbol.qualified)
                    .and_then(|parsed| target.tier_of(&parsed))
                    .map(|tier| (tier, candidate))
            })
            .collect::<Vec<_>>();
        let scope = type_scope(from_file, imports, target);

        let resolved = if scope.is_empty() {
            self.pick_unscoped(from_file, &members)?
        } else {
            let mut in_scope = Vec::new();
            for &(tier, candidate) in &members {
                if self.member_within_scope(candidate, &scope)? {
                    in_scope.push((tier, candidate));
                }
            }
            self.pick_scoped(from_file, &in_scope, members.len())?
        };
        if resolved.is_some() {
            return Ok(resolved);
        }
        if let Some(resolved) = self.resolve_trait_default(target, &scope)? {
            return Ok(Some(resolved));
        }
        if members.is_empty() && scope.is_empty() {
            // A receiver's or `self`'s type may get the member through
            // `Deref` or a type outside the index: keep the generic ladder
            // (a receiver still keeps it off name-only rungs). A written
            // `Type::member` path names nothing else.
            return Ok((!target.bracketed).then(ResolvedRef::fuzzy));
        }
        // The type is outside the index (an external crate's, or an alias
        // the index cannot see through), or several same-named types define
        // the member and nothing narrows it: name-only rungs could only guess.
        Ok(Some(ResolvedRef::fuzzy()))
    }

    /// Picks among members of a type the file names through a written path
    /// or an import. `total` counts same-named members before scoping.
    fn pick_scoped(
        &mut self,
        from_file: &str,
        in_scope: &[(Tier, &NamedCandidate)],
        total: usize,
    ) -> Result<Option<ResolvedRef>, GraphError> {
        let pool = best_tier(in_scope);
        if let [only] = pool.as_slice() {
            let confidence = if total == 1 {
                CONFIDENCE_EXACT
            } else {
                CONFIDENCE_IMPORT_RESOLVED
            };
            return Ok(Some(ResolvedRef::candidate(
                only.symbol.clone(),
                confidence,
            )));
        }
        self.pick_nearest(from_file, &pool)
    }

    /// Picks among members of a type the file names without a path or an
    /// import: a type of the calling file or module, a glob import, or a
    /// prelude type. Narrowed to the calling file, then its module, before
    /// the tier is picked.
    fn pick_unscoped(
        &mut self,
        from_file: &str,
        members: &[(Tier, &NamedCandidate)],
    ) -> Result<Option<ResolvedRef>, GraphError> {
        if let [(_, only)] = members {
            return Ok(Some(ResolvedRef::candidate(
                only.symbol.clone(),
                CONFIDENCE_EXACT,
            )));
        }
        let same_file = members
            .iter()
            .filter(|(_, candidate)| candidate.symbol.file_path == from_file)
            .copied()
            .collect::<Vec<_>>();
        if !same_file.is_empty() {
            return Ok(unique(&best_tier(&same_file))
                .map(|only| ResolvedRef::candidate(only.symbol.clone(), CONFIDENCE_EXACT)));
        }
        let prefixes = self.module_prefixes_for_file(from_file)?;
        let mut nearby = Vec::new();
        for &(tier, candidate) in members {
            if self.candidate_shares_module(candidate, &prefixes)? {
                nearby.push((tier, candidate));
            }
        }
        if !nearby.is_empty() {
            return Ok(unique(&best_tier(&nearby))
                .map(|only| ResolvedRef::candidate(only.symbol.clone(), CONFIDENCE_SAME_MODULE)));
        }
        // Same-named types elsewhere only: the tier may pick among one
        // type's own members, never between different types.
        let pool = best_tier(members);
        Ok(unique(&pool)
            .filter(|only| {
                members
                    .iter()
                    .all(|(_, candidate)| candidate.symbol.file_path == only.symbol.file_path)
            })
            .map(|only| ResolvedRef::candidate(only.symbol.clone(), CONFIDENCE_EXACT)))
    }

    /// Several in-scope members of the best tier: the calling file's own,
    /// then the one sharing the calling file's module.
    fn pick_nearest(
        &mut self,
        from_file: &str,
        pool: &[&NamedCandidate],
    ) -> Result<Option<ResolvedRef>, GraphError> {
        let same_file = pool
            .iter()
            .filter(|candidate| candidate.symbol.file_path == from_file)
            .collect::<Vec<_>>();
        if let [only] = same_file.as_slice() {
            return Ok(Some(ResolvedRef::candidate(
                only.symbol.clone(),
                CONFIDENCE_EXACT,
            )));
        }
        let prefixes = self.module_prefixes_for_file(from_file)?;
        let mut nearby = Vec::new();
        for candidate in pool {
            if self.candidate_shares_module(candidate, &prefixes)? {
                nearby.push(candidate.symbol.clone());
            }
        }
        if let [only] = nearby.as_slice() {
            return Ok(Some(ResolvedRef::candidate(
                only.clone(),
                CONFIDENCE_SAME_MODULE,
            )));
        }
        Ok(None)
    }

    /// Whether a member candidate's type lies under one of the `scope`
    /// modules, through its file's module or the type path its own
    /// qualified name spells (`<inner::Foo>::m`).
    fn member_within_scope(
        &mut self,
        candidate: &NamedCandidate,
        scope: &BTreeSet<String>,
    ) -> Result<bool, GraphError> {
        let own_path = MemberTarget::parse(&candidate.symbol.qualified)
            .map(|parsed| normalize_module_path(&parsed.type_path))
            .unwrap_or_default();
        for module in scope {
            if !own_path.is_empty() && prefix_within_module(&own_path, module) {
                return Ok(true);
            }
            if self.candidate_within_module(candidate, module)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// A member the type gets from a trait's default body: `Foo::hi()` or
    /// `foo.hi()`, where `impl Greet for Foo {}` does not override `hi`,
    /// resolves to the declaration `Greet::hi`. Only traits that some
    /// `impl Trait for T` implements (inside `scope`, when there is one)
    /// are considered.
    fn resolve_trait_default(
        &mut self,
        target: &MemberTarget,
        scope: &BTreeSet<String>,
    ) -> Result<Option<ResolvedRef>, GraphError> {
        if target.trait_object {
            return Ok(None);
        }
        let impls = self.trait_impls_of(&target.type_name)?;
        let mut traits = BTreeSet::new();
        let mut impl_files = BTreeSet::new();
        for (trait_name, candidate) in impls.iter() {
            if target
                .trait_name
                .as_ref()
                .is_some_and(|wanted| wanted != trait_name)
            {
                continue;
            }
            if !scope.is_empty() && !self.member_within_scope(candidate, scope)? {
                continue;
            }
            traits.insert(trait_name.clone());
            impl_files.insert(candidate.symbol.file_path.clone());
        }
        if traits.is_empty() {
            return Ok(None);
        }
        let declarations = self
            .symbols_by_name(&target.member)?
            .iter()
            .filter(|candidate| {
                MemberTarget::parse(&candidate.symbol.qualified)
                    .is_some_and(|parsed| !parsed.bracketed && traits.contains(&parsed.type_name))
            })
            .map(|candidate| candidate.symbol.clone())
            .collect::<Vec<_>>();
        let [only] = declarations.as_slice() else {
            return Ok(None);
        };
        // One trait the type implements declares the member. That is the
        // target when the type itself is certain: named by a path or an
        // import, or implemented in one file only.
        if !scope.is_empty() || impl_files.len() == 1 {
            return Ok(Some(ResolvedRef::candidate(only.clone(), CONFIDENCE_EXACT)));
        }
        Ok(None)
    }

    /// The `impl Trait for T` blocks whose Self type's last segment is
    /// `type_name`, each with its trait's last segment. All trait impl
    /// symbols are loaded once per pass, on first use.
    fn trait_impls_of(&mut self, type_name: &str) -> Result<TraitImpls, GraphError> {
        if self.trait_impls.is_none() {
            let mut by_type: HashMap<String, Vec<(String, NamedCandidate)>> = HashMap::new();
            for symbol in trait_impl_symbols(self.tx)? {
                let Some((self_name, trait_name)) = impl_type_and_trait(&symbol.qualified) else {
                    continue;
                };
                by_type
                    .entry(self_name)
                    .or_default()
                    .push((trait_name, NamedCandidate::new(symbol, "rust".to_string())));
            }
            self.trait_impls = Some(
                by_type
                    .into_iter()
                    .map(|(name, impls)| (name, Rc::from(impls)))
                    .collect(),
            );
        }
        Ok(self
            .trait_impls
            .as_ref()
            .and_then(|impls| impls.get(type_name))
            .map_or_else(|| Rc::from(Vec::new()), Rc::clone))
    }
}

/// `("Foo", "Greet")` for an impl block qualified `<a::Foo<T> as b::Greet>`.
fn impl_type_and_trait(qualified: &str) -> Option<(String, String)> {
    let inner = qualified.strip_prefix('<')?.strip_suffix('>')?;
    let (self_type, trait_path) = inner.split_once(" as ")?;
    let last = |path: &str| {
        let path = strip_generics(path.trim());
        path.rsplit("::").next().unwrap_or(path).to_string()
    };
    Some((last(self_type), last(trait_path)))
}

/// The trait an impl block implements (`Greet` for `<a::Foo as b::Greet>`),
/// or `None` for an inherent impl or anything else.
pub(super) fn impl_trait_name(qualified: &str) -> Option<String> {
    impl_type_and_trait(qualified).map(|(_, trait_name)| trait_name)
}

/// Names of the members every trait whose last path segment is `trait_name`
/// declares (`hi` for `Greet::hi`). A superset is harmless: an incremental
/// sync re-resolves the refs with these names and keeps any that did not
/// change.
pub(super) fn trait_member_names(
    tx: &Transaction<'_>,
    trait_name: &str,
) -> Result<BTreeSet<String>, GraphError> {
    let mut stmt = tx
        .prepare_cached(
            "SELECT DISTINCT name, qualified FROM symbols
             WHERE kind <> 'impl' AND instr(qualified, ?1) > 0",
        )
        .map_err(|source| GraphError::sqlite("prepare trait member lookup", source))?;
    let rows = stmt
        .query_map([format!("{trait_name}::")], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|source| GraphError::sqlite("query trait members", source))?;
    let mut names = BTreeSet::new();
    for row in rows {
        let (name, qualified) =
            row.map_err(|source| GraphError::sqlite("read trait member", source))?;
        if MemberTarget::parse(&qualified)
            .is_some_and(|member| !member.bracketed && member.type_name == trait_name)
        {
            names.insert(name);
        }
    }
    Ok(names)
}

/// Every Rust trait impl block (`<T as Trait>`).
fn trait_impl_symbols(tx: &Transaction<'_>) -> Result<Vec<SymbolCandidate>, GraphError> {
    let mut stmt = tx
        .prepare_cached(
            "SELECT id, file_path, name, qualified FROM symbols
             WHERE kind = 'impl' AND qualified LIKE '<% as %>'
             ORDER BY qualified, id",
        )
        .map_err(|source| GraphError::sqlite("prepare trait impl lookup", source))?;
    let rows = stmt
        .query_map([], symbol_candidate_from_row)
        .map_err(|source| GraphError::sqlite("query trait impls for ref resolution", source))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|source| GraphError::sqlite("collect trait impls for ref resolution", source))
}

/// Modules that can hold the type a member target names: the module path
/// written before it (`crate::b` in `crate::b::Foo::m`), else the modules of
/// the imports that bring the type's name into the file. Empty when the file
/// names the type neither way. Normalized, with `-` read as `_`.
fn type_scope(
    from_file: &str,
    imports: &[ImportCandidate],
    target: &MemberTarget,
) -> BTreeSet<String> {
    let mut modules = BTreeSet::new();
    if target.type_path.is_empty() {
        for import in imports {
            if import.target_symbol.as_deref() == Some(target.type_name.as_str()) {
                modules.insert(resolve_import_path(from_file, &import.target_path));
            }
        }
    } else {
        modules.insert(resolve_import_path(from_file, &target.type_path));
    }
    modules
        .into_iter()
        .map(|module| normalize_module_path(&module).replace('-', "_"))
        .filter(|module| !module.is_empty())
        .collect()
}

/// The candidates of the best (lowest) tier present.
fn best_tier<'c>(members: &[(Tier, &'c NamedCandidate)]) -> Vec<&'c NamedCandidate> {
    let Some(best) = members.iter().map(|(tier, _)| *tier).min() else {
        return Vec::new();
    };
    members
        .iter()
        .filter(|(tier, _)| *tier == best)
        .map(|(_, candidate)| *candidate)
        .collect()
}

fn unique<'c>(pool: &[&'c NamedCandidate]) -> Option<&'c NamedCandidate> {
    match pool {
        [only] => Some(only),
        _ => None,
    }
}

/// Whether a Rust ref names a free item: a plain or module-path call
/// (`f()`, `plugin::enable_plugin()`), or a type. Such a path can never
/// denote an impl or trait member, so the name-only and import rungs must
/// not match it to one (`plugin::enable_plugin(self, ..)` inside
/// `OrbitRuntime::enable_plugin` is not a call of itself).
///
/// `self.m()`/`Self::m()` and receiver calls are excluded: their member may
/// legitimately come from a trait's declaration.
pub(super) fn names_free_item(from_file: &str, raw_ref: &RawRef) -> bool {
    if !from_file.ends_with(".rs") || raw_ref.kind == "use" || raw_ref.unresolved_receiver.is_some()
    {
        return false;
    }
    // Plain and path calls always carry their spelling; a call without one is
    // `self.m()` outside an impl (a trait's default body).
    raw_ref.target_qualified.as_deref().is_some_and(|target| {
        !target.starts_with('<')
            && !target.starts_with("Self::")
            && MemberTarget::parse(target).is_none()
    })
}

/// Whether a symbol's qualified name makes it a type's or trait's member
/// (`<T>::m`, `<T as Trait>::m`, `Trait::m`).
pub(super) fn is_member_symbol(qualified: &str) -> bool {
    MemberTarget::parse(qualified).is_some()
}

/// The module path a Rust module-path call spells before its name, with `-`
/// read as `_` (`std::process` for `std::process::id()`). `None` for anything
/// else, including a path rooted at `crate`/`self`/`super`: those name this
/// crate, whose modules are often reached through `use m as alias`
/// re-exports that the symbol table does not record, so the name-only rung
/// keeps its plain same-module rule for them.
pub(super) fn call_module_qualifier(from_file: &str, raw_ref: &RawRef) -> Option<String> {
    if !raw_ref.spelled_path || !names_free_item(from_file, raw_ref) {
        return None;
    }
    let target = normalize_module_path(raw_ref.target_qualified.as_deref()?);
    let (qualifier, _) = target.rsplit_once("::")?;
    let root = qualifier.split("::").next()?;
    (!matches!(root, "crate" | "self" | "super")).then(|| qualifier.replace('-', "_"))
}

/// Whether a module prefix is the module `qualifier` names or lies inside
/// it: a module commonly re-exports its children's items
/// (`plugin::enable_plugin` defined in `plugin/lifecycle.rs`).
fn prefix_within_module(prefix: &str, qualifier: &str) -> bool {
    let prefix = prefix.replace('-', "_");
    prefix == qualifier
        || prefix.ends_with(&format!("::{qualifier}"))
        || prefix.starts_with(&format!("{qualifier}::"))
        || prefix.contains(&format!("::{qualifier}::"))
}

impl Resolver<'_, '_> {
    /// Whether the candidate can be the item a module-path call names: one of
    /// its module prefixes is, or lies inside, the call's module `qualifier`.
    /// Keeps the name-only same-module rung from matching
    /// `std::process::id()` to an unrelated local `id` function.
    pub(super) fn candidate_within_module(
        &mut self,
        candidate: &NamedCandidate,
        qualifier: &str,
    ) -> Result<bool, GraphError> {
        if candidate
            .own_prefix
            .as_deref()
            .is_some_and(|prefix| prefix_within_module(prefix, qualifier))
        {
            return Ok(true);
        }
        Ok(self
            .module_prefixes_for_file(&candidate.symbol.file_path)?
            .iter()
            .any(|prefix| prefix_within_module(prefix, qualifier)))
    }
}

/// Index of the `>` closing the `<` that `text` follows.
fn matching_angle(text: &str) -> Option<usize> {
    let mut depth = 1usize;
    for (index, ch) in text.char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

/// `Foo<T>` → `Foo`, `a::Foo::<T>` → `a::Foo`.
fn strip_generics(text: &str) -> &str {
    let text = text.split('<').next().unwrap_or(text).trim();
    text.strip_suffix("::").unwrap_or(text)
}

fn is_function_name(name: &str) -> bool {
    name.starts_with(|ch: char| ch.is_ascii_lowercase() || ch == '_')
        && name.chars().all(|ch| ch.is_alphanumeric() || ch == '_')
}
