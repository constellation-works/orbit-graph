//! Rust tree-sitter extraction.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use tree_sitter::{Node, Parser};

use super::common::{
    dedup_imports, dedup_refs, dedup_relations, dedup_symbols, normalize_path, parse_source,
};
use crate::{ExtractedFile, Extractor, RawCommand, RawImport, RawRef, RawRelation, RawSymbol};

mod commands;

/// Extracts Rust source files into raw graph rows.
pub struct RustExtractor;

impl Extractor for RustExtractor {
    fn lang(&self) -> &'static str {
        "rust"
    }

    fn supports(&self, path: &Path) -> bool {
        path.extension().and_then(|ext| ext.to_str()) == Some("rs")
    }

    fn extract(&self, path: &Path, bytes: &[u8]) -> ExtractedFile {
        let Ok(source) = std::str::from_utf8(bytes) else {
            return ExtractedFile::default();
        };

        let mut parser = Parser::new();
        if parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .is_err()
        {
            return ExtractedFile::default();
        }

        let Some(tree) = parse_source(&mut parser, source) else {
            return ExtractedFile::default();
        };

        let mut state = ExtractionState::new(path);
        let module = ModuleScope::root();
        let root = tree.root_node();
        state.type_aliases = use_type_aliases(root, source);
        extract_items(root, source, &module, None, None, &mut state);
        commands::extract_commands(root, source, &mut state);
        state.finish()
    }
}

struct ExtractionState {
    file_path: String,
    symbols: Vec<RawSymbol>,
    refs: Vec<RawRef>,
    relations: Vec<RawRelation>,
    imports: Vec<RawImport>,
    commands: Vec<RawCommand>,
    /// [`pattern_bindings`] per binding scope node id, computed on first use.
    local_bindings: HashMap<usize, HashSet<String>>,
    /// [`typed_bindings`] per function node id, computed on first use.
    binding_types: HashMap<usize, HashMap<String, Option<String>>>,
    /// `use path::Type as Alias` renames in this file: alias to the written
    /// path (see [`use_type_aliases`]).
    type_aliases: HashMap<String, String>,
}

impl ExtractionState {
    fn new(path: &Path) -> Self {
        Self {
            file_path: normalize_path(path),
            symbols: Vec::new(),
            refs: Vec::new(),
            relations: Vec::new(),
            imports: Vec::new(),
            commands: Vec::new(),
            local_bindings: HashMap::new(),
            binding_types: HashMap::new(),
            type_aliases: HashMap::new(),
        }
    }

    /// Type of the local binding `name` visible at `node`, when the enclosing
    /// function binds that name exactly once and the binding spells its type
    /// (see [`typed_bindings`]).
    fn binding_type(
        &mut self,
        node: Node,
        name: &str,
        source: &str,
        module: &ModuleScope,
    ) -> Option<String> {
        let scope = binding_scope(node);
        let aliases = &self.type_aliases;
        self.binding_types
            .entry(scope.id())
            .or_insert_with(|| typed_bindings(scope, source, module, aliases))
            .get(name)
            .cloned()
            .flatten()
    }

    /// `<T>::method` for a method call whose receiver is a plain local
    /// binding of known type `T`.
    fn typed_receiver_target(
        &mut self,
        receiver: Node,
        method: &str,
        source: &str,
        module: &ModuleScope,
    ) -> Option<String> {
        if receiver.kind() != "identifier" {
            return None;
        }
        let name = node_text(receiver, source);
        let type_name = self.binding_type(receiver, &name, source, module)?;
        Some(format!("<{type_name}>::{method}"))
    }

    /// Whether a pattern in the function enclosing `node` binds `name`.
    fn is_local_binding(&mut self, node: Node, name: &str, source: &str) -> bool {
        let scope = binding_scope(node);
        self.local_bindings
            .entry(scope.id())
            .or_insert_with(|| pattern_bindings(scope, source))
            .contains(name)
    }

    fn finish(mut self) -> ExtractedFile {
        dedup_symbols(&mut self.symbols);
        dedup_refs(&mut self.refs);
        dedup_relations(&mut self.relations);
        dedup_imports(&mut self.imports);
        dedup_commands(&mut self.commands);
        ExtractedFile {
            symbols: self.symbols,
            refs: self.refs,
            relations: self.relations,
            imports: self.imports,
            strings: Vec::new(),
            configs: Vec::new(),
            commands: self.commands,
        }
    }

    fn push_symbol(
        &mut self,
        node: Node,
        source: &str,
        name: String,
        qualified: String,
        kind: &'static str,
        parent_symbol: Option<String>,
    ) {
        self.symbols.push(RawSymbol {
            file_path: self.file_path.clone(),
            name,
            qualified,
            kind: kind.to_string(),
            span_start: node.start_byte(),
            span_end: node.end_byte(),
            signature: signature_for(node, source),
            parent_symbol,
        });
    }

    fn push_ref(
        &mut self,
        node: Node,
        source: &str,
        target_qualified: Option<String>,
        kind: &'static str,
        confidence: &'static str,
    ) {
        self.push_ref_with_receiver(node, source, target_qualified, kind, confidence, None);
    }

    /// [`Self::push_ref`] plus the receiver expression of a method call whose
    /// receiver type this extractor cannot determine. See
    /// [`RawRef::unresolved_receiver`].
    fn push_ref_with_receiver(
        &mut self,
        node: Node,
        source: &str,
        target_qualified: Option<String>,
        kind: &'static str,
        confidence: &'static str,
        unresolved_receiver: Option<String>,
    ) {
        let Some(target_name) = target_name(node, source) else {
            return;
        };
        if target_name.is_empty() || is_ignored_type_name(&target_name) {
            return;
        }

        self.refs.push(RawRef {
            from_file: self.file_path.clone(),
            from_span_start: node.start_byte(),
            from_span_end: node.end_byte(),
            target_name,
            target_qualified,
            kind: kind.to_string(),
            confidence: confidence.to_string(),
            unresolved_receiver,
            // This extractor labels a target `import_resolved` exactly when
            // it is the path written at the site (a scoped call, a qualified
            // type, a `use` path); derived targets are `fuzzy_name`.
            spelled_path: confidence == "import_resolved",
        });
    }

    fn push_command(&mut self, name: String, node: Node, handler_symbol: Option<String>) {
        if name.is_empty() {
            return;
        }

        self.commands.push(RawCommand {
            name,
            file_path: self.file_path.clone(),
            span_start: node.start_byte(),
            handler_symbol,
        });
    }
}

#[derive(Debug, Clone, Default)]
struct ModuleScope {
    segments: Vec<String>,
}

impl ModuleScope {
    fn root() -> Self {
        Self::default()
    }

    fn child(&self, name: &str) -> Self {
        let mut segments = self.segments.clone();
        segments.push(name.to_string());
        Self { segments }
    }

    fn qualify(&self, name: &str) -> String {
        if self.segments.is_empty() || is_already_qualified(name) {
            name.to_string()
        } else {
            format!("{}::{name}", self.segments.join("::"))
        }
    }
}

fn extract_items(
    node: Node,
    source: &str,
    module: &ModuleScope,
    parent_symbol: Option<&str>,
    method_parent: Option<&str>,
    state: &mut ExtractionState,
) {
    let mut cursor = node.walk();
    let mut pending_attrs = Vec::new();

    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "attribute_item" | "inner_attribute_item" => {
                pending_attrs.push(node_text(child, source));
            }
            "function_item" => {
                extract_function(
                    child,
                    source,
                    module,
                    parent_symbol,
                    method_parent,
                    &pending_attrs,
                    state,
                );
                pending_attrs.clear();
            }
            "function_signature_item" => {
                extract_function_signature(
                    child,
                    source,
                    module,
                    parent_symbol,
                    method_parent,
                    state,
                );
                pending_attrs.clear();
            }
            "struct_item" => {
                extract_named_item(child, source, module, parent_symbol, "struct", state);
                collect_declaration_refs(child, source, module, state);
                pending_attrs.clear();
            }
            "enum_item" => {
                extract_named_item(child, source, module, parent_symbol, "enum", state);
                collect_declaration_refs(child, source, module, state);
                pending_attrs.clear();
            }
            "trait_item" => {
                extract_trait(child, source, module, parent_symbol, state);
                pending_attrs.clear();
            }
            "impl_item" => {
                extract_impl(child, source, module, state);
                pending_attrs.clear();
            }
            "mod_item" => {
                extract_mod(child, source, module, parent_symbol, state);
                pending_attrs.clear();
            }
            "type_item" => {
                extract_named_item(child, source, module, parent_symbol, "type_alias", state);
                collect_declaration_refs(child, source, module, state);
                pending_attrs.clear();
            }
            "const_item" | "static_item" => {
                extract_named_item(child, source, module, parent_symbol, "const", state);
                collect_declaration_refs(child, source, module, state);
                pending_attrs.clear();
            }
            "use_declaration" => {
                extract_use(child, source, state);
                pending_attrs.clear();
            }
            _ => {
                collect_expression_refs(child, source, module, state);
                pending_attrs.clear();
            }
        }
    }
}

fn extract_function(
    node: Node,
    source: &str,
    module: &ModuleScope,
    parent_symbol: Option<&str>,
    method_parent: Option<&str>,
    attrs: &[String],
    state: &mut ExtractionState,
) {
    let Some(name) = get_name(node, source) else {
        return;
    };

    let parent = method_parent.or(parent_symbol);
    let kind = if has_test_attr(attrs) {
        "test"
    } else if method_parent.is_some() {
        "method"
    } else {
        "function"
    };
    let qualified = qualify_member_or_module(module, parent, &name);
    state.push_symbol(
        node,
        source,
        name,
        qualified.clone(),
        kind,
        parent.map(ToOwned::to_owned),
    );
    collect_signature_refs(node, source, module, state);
    if let Some(body) = node.child_by_field_name("body") {
        // A block can define local functions. Extract them as children of this
        // function so their spans own their calls instead of leaving them
        // invisible to the graph (and attributing their calls to this body).
        extract_items(body, source, module, Some(&qualified), None, state);
    }
}

fn extract_function_signature(
    node: Node,
    source: &str,
    module: &ModuleScope,
    parent_symbol: Option<&str>,
    method_parent: Option<&str>,
    state: &mut ExtractionState,
) {
    let Some(name) = get_name(node, source) else {
        return;
    };
    let qualified = qualify_member_or_module(module, method_parent, &name);
    state.push_symbol(
        node,
        source,
        name,
        qualified,
        "method",
        method_parent.or(parent_symbol).map(ToOwned::to_owned),
    );
    collect_signature_refs(node, source, module, state);
}

fn extract_named_item(
    node: Node,
    source: &str,
    module: &ModuleScope,
    parent_symbol: Option<&str>,
    kind: &'static str,
    state: &mut ExtractionState,
) {
    let Some(name) = get_name(node, source) else {
        return;
    };
    let qualified = module.qualify(&name);
    state.push_symbol(
        node,
        source,
        name,
        qualified,
        kind,
        parent_symbol.map(ToOwned::to_owned),
    );
}

fn extract_trait(
    node: Node,
    source: &str,
    module: &ModuleScope,
    parent_symbol: Option<&str>,
    state: &mut ExtractionState,
) {
    let Some(name) = get_name(node, source) else {
        return;
    };
    let qualified = module.qualify(&name);
    state.push_symbol(
        node,
        source,
        name,
        qualified.clone(),
        "trait",
        parent_symbol.map(ToOwned::to_owned),
    );
    collect_trait_bound_fields(node, source, module, state);
    collect_signature_refs(node, source, module, state);
    if let Some(body) = node.child_by_field_name("body") {
        extract_items(
            body,
            source,
            module,
            Some(&qualified),
            Some(&qualified),
            state,
        );
    }
}

fn extract_impl(node: Node, source: &str, module: &ModuleScope, state: &mut ExtractionState) {
    let Some(type_node) = node.child_by_field_name("type") else {
        return;
    };
    let Some(type_name) = type_qualified_name(type_node, source, module) else {
        return;
    };
    let trait_name = node
        .child_by_field_name("trait")
        .and_then(|trait_node| type_qualified_name(trait_node, source, module));

    let impl_qualified = match trait_name.as_deref() {
        Some(trait_name) => format!("<{type_name} as {trait_name}>"),
        None => format!("<{type_name}>"),
    };
    state.push_symbol(
        node,
        source,
        type_name.clone(),
        impl_qualified.clone(),
        "impl",
        None,
    );

    if let Some(trait_name) = trait_name {
        state.relations.push(RawRelation {
            from_qualified: type_name,
            to_qualified: trait_name,
            kind: "impl".to_string(),
            def_file: state.file_path.clone(),
            def_span_start: node.start_byte(),
            def_span_end: node.end_byte(),
            confidence: "exact".to_string(),
        });
    }

    collect_trait_bound_fields(node, source, module, state);
    collect_type_refs(type_node, source, module, "type", state);
    if let Some(trait_node) = node.child_by_field_name("trait") {
        collect_type_refs(trait_node, source, module, "type", state);
    }
    if let Some(body) = node.child_by_field_name("body") {
        extract_items(
            body,
            source,
            module,
            Some(&impl_qualified),
            Some(&impl_qualified),
            state,
        );
    }
}

fn extract_mod(
    node: Node,
    source: &str,
    module: &ModuleScope,
    parent_symbol: Option<&str>,
    state: &mut ExtractionState,
) {
    let Some(name) = get_name(node, source) else {
        return;
    };
    let qualified = module.qualify(&name);
    state.push_symbol(
        node,
        source,
        name.clone(),
        qualified.clone(),
        "module",
        parent_symbol.map(ToOwned::to_owned),
    );
    if let Some(body) = node.child_by_field_name("body") {
        let child_module = module.child(&name);
        extract_items(body, source, &child_module, Some(&qualified), None, state);
    }
}

fn collect_signature_refs(
    node: Node,
    source: &str,
    module: &ModuleScope,
    state: &mut ExtractionState,
) {
    if let Some(params) = node.child_by_field_name("parameters") {
        collect_type_refs(params, source, module, "type", state);
    }
    if let Some(return_type) = node.child_by_field_name("return_type") {
        collect_type_refs(return_type, source, module, "type", state);
    }
    if let Some(type_parameters) = node.child_by_field_name("type_parameters") {
        collect_trait_bound_fields(type_parameters, source, module, state);
    }
    collect_trait_bound_fields(node, source, module, state);
}

fn collect_declaration_refs(
    node: Node,
    source: &str,
    module: &ModuleScope,
    state: &mut ExtractionState,
) {
    if let Some(type_parameters) = node.child_by_field_name("type_parameters") {
        collect_trait_bound_fields(type_parameters, source, module, state);
    }
    if let Some(type_node) = node.child_by_field_name("type") {
        collect_type_refs(type_node, source, module, "type", state);
    }
    if let Some(body) = node.child_by_field_name("body") {
        collect_type_refs(body, source, module, "type", state);
    }
    if let Some(value) = node.child_by_field_name("value") {
        collect_expression_refs(value, source, module, state);
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "where_clause" {
            collect_trait_bound_fields(child, source, module, state);
        }
    }
}

fn collect_trait_bound_fields(
    node: Node,
    source: &str,
    module: &ModuleScope,
    state: &mut ExtractionState,
) {
    if node.kind() == "trait_bounds" {
        collect_trait_bound_refs(node, source, module, state);
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "trait_bounds" {
            collect_trait_bound_refs(child, source, module, state);
        } else {
            collect_trait_bound_fields(child, source, module, state);
        }
    }
}

fn collect_trait_bound_refs(
    node: Node,
    source: &str,
    module: &ModuleScope,
    state: &mut ExtractionState,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "lifetime" => {}
            "removed_trait_bound" | "higher_ranked_trait_bound" => {
                collect_trait_bound_refs(child, source, module, state);
            }
            _ if is_type_reference_node(child) => {
                state.push_ref(
                    child,
                    source,
                    type_qualified_name(child, source, module),
                    "trait_bound",
                    confidence_for_type(child, source),
                );
                collect_type_refs(child, source, module, "type", state);
            }
            _ => collect_trait_bound_refs(child, source, module, state),
        }
    }
}

fn collect_type_refs(
    node: Node,
    source: &str,
    module: &ModuleScope,
    kind: &'static str,
    state: &mut ExtractionState,
) {
    match node.kind() {
        "primitive_type" | "lifetime" | "identifier" | "field_identifier" | "self" | "crate"
        | "super" => return,
        "type_identifier" | "scoped_type_identifier" => {
            state.push_ref(
                node,
                source,
                type_qualified_name(node, source, module),
                kind,
                confidence_for_type(node, source),
            );
            return;
        }
        "generic_type" | "generic_type_with_turbofish" => {
            if let Some(type_node) = node.child_by_field_name("type") {
                collect_type_refs(type_node, source, module, kind, state);
            }
            if let Some(arguments) = node.child_by_field_name("type_arguments") {
                collect_type_refs(arguments, source, module, kind, state);
            }
            return;
        }
        "trait_bounds" => {
            collect_trait_bound_refs(node, source, module, state);
            return;
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_type_refs(child, source, module, kind, state);
    }
}

fn collect_expression_refs(
    node: Node,
    source: &str,
    module: &ModuleScope,
    state: &mut ExtractionState,
) {
    if node.kind() == "call_expression" {
        collect_call_ref(node, source, module, state);
        if let Some(arguments) = node.child_by_field_name("arguments") {
            collect_function_value_refs(arguments, source, module, state);
            collect_expression_refs(arguments, source, module, state);
        }
        return;
    }
    if node.kind() == "macro_invocation" {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() == "token_tree" {
                collect_macro_token_refs(child, source, module, state);
            }
        }
        return;
    }
    if is_type_reference_node(node) {
        collect_type_refs(node, source, module, "type", state);
        return;
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "function_item"
            | "function_signature_item"
            | "struct_item"
            | "enum_item"
            | "trait_item"
            | "impl_item"
            | "mod_item"
            | "type_item"
            | "const_item"
            | "static_item"
            | "use_declaration" => {}
            _ => collect_expression_refs(child, source, module, state),
        }
    }
}

/// Records a `call` ref for each function passed by name as a call argument
/// (`.map(skill_link_roots)`, `.and_then(Self::parse)`, `run(module::step)`):
/// the callee invokes it on the caller's behalf, so without this edge the
/// function looks uncalled.
///
/// A bare identifier argument is far more often a local value than a
/// function, and the extractor has no type information, so one is only
/// recorded when it is spelled like a function (starts with a lowercase
/// letter or `_`) and no pattern in the enclosing function binds that name
/// (parameters, `let`, closure parameters, `for`, `match`/`if let` arms).
/// Paths (`a::b`) name items, never locals, and only need the spelling check.
fn collect_function_value_refs(
    arguments: Node,
    source: &str,
    module: &ModuleScope,
    state: &mut ExtractionState,
) {
    let mut cursor = arguments.walk();
    for argument in arguments.named_children(&mut cursor) {
        match argument.kind() {
            "identifier" => {
                let name = node_text(argument, source);
                if !is_function_like_name(&name) || state.is_local_binding(argument, &name, source)
                {
                    continue;
                }
                state.push_ref(
                    argument,
                    source,
                    Some(module.qualify(&name)),
                    "call",
                    "fuzzy_name",
                );
            }
            "scoped_identifier" => {
                let is_function_like = argument
                    .child_by_field_name("name")
                    .is_some_and(|name| is_function_like_name(&node_text(name, source)));
                if is_function_like {
                    push_call_target_ref(argument, source, module, state);
                }
            }
            _ => {}
        }
    }
}

/// Whether an identifier follows Rust's function naming convention. Types,
/// enum variants, and constants are capitalised and never match.
fn is_function_like_name(name: &str) -> bool {
    name.starts_with(|ch: char| ch.is_ascii_lowercase() || ch == '_')
        && name != "_"
        && !is_reserved_word(name)
}

/// Words that tree-sitter's token-tree grammar yields as `identifier` although
/// they are Rust keywords (the grammar lists only some keywords as tokens).
fn is_reserved_word(name: &str) -> bool {
    matches!(
        name,
        "in" | "move"
            | "ref"
            | "dyn"
            | "else"
            | "extern"
            | "box"
            | "do"
            | "final"
            | "macro"
            | "override"
            | "priv"
            | "typeof"
            | "unsized"
            | "virtual"
            | "yield"
            | "try"
            | "abstract"
            | "become"
            | "true"
            | "false"
    )
}

/// Names bound by any pattern inside `scope`: parameters, `let`, `for`,
/// `match` arms, `if let`/`while let` conditions, and closure parameters.
/// Over-collecting (e.g. a tuple-struct path inside a pattern) only makes the
/// function-value heuristic more conservative.
fn pattern_bindings(scope: Node, source: &str) -> HashSet<String> {
    let mut bindings = HashSet::new();
    let mut stack = vec![scope];
    while let Some(node) = stack.pop() {
        if let Some(pattern) = node.child_by_field_name("pattern") {
            collect_identifiers(pattern, source, &mut bindings);
        }
        if node.kind() == "closure_parameters" {
            collect_identifiers(node, source, &mut bindings);
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    bindings
}

fn collect_identifiers(node: Node, source: &str, names: &mut HashSet<String>) {
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "identifier" | "shorthand_field_identifier") {
            names.insert(node_text(node, source));
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
}

/// The node whose patterns can bind a name visible at `node`: the nearest
/// enclosing function, or else the outermost item (a `const`/`static`
/// initializer's closures).
fn binding_scope(node: Node) -> Node {
    let mut scope = node;
    let mut current = node;
    while let Some(parent) = current.parent() {
        if current.kind() == "function_item" {
            return current;
        }
        if parent.kind() != "source_file" {
            scope = parent;
        }
        current = parent;
    }
    scope
}

/// Type parameter names in scope at a function: its own and those of the
/// enclosing `impl`/`trait` blocks.
fn type_parameter_names(function: Node, source: &str) -> HashSet<String> {
    let mut names = HashSet::new();
    let mut current = Some(function);
    while let Some(node) = current {
        if matches!(node.kind(), "function_item" | "impl_item" | "trait_item")
            && let Some(parameters) = node.child_by_field_name("type_parameters")
        {
            let mut cursor = parameters.walk();
            for parameter in parameters.named_children(&mut cursor) {
                if let Some(name) = parameter.child_by_field_name("name") {
                    names.insert(node_text(name, source));
                }
            }
        }
        current = node.parent();
    }
    names
}

/// `use path::Type as Alias` renames anywhere in the file, alias to the
/// written path (`Bar` to `crate::a::Foo`). Only type-like (capitalised)
/// aliases are kept: they are the ones a typed receiver or a `Type::m(..)`
/// path can spell.
fn use_type_aliases(root: Node, source: &str) -> HashMap<String, String> {
    fn collect(node: Node, source: &str, prefix: &[String], aliases: &mut HashMap<String, String>) {
        match node.kind() {
            "scoped_use_list" => {
                let mut next = prefix.to_vec();
                if let Some(path) = node.child_by_field_name("path") {
                    next.extend(path_segments(path, source));
                }
                if let Some(list) = node.child_by_field_name("list") {
                    collect(list, source, &next, aliases);
                }
            }
            "use_list" => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    collect(child, source, prefix, aliases);
                }
            }
            "use_as_clause" => {
                let (Some(alias), Some(path)) = (
                    node.child_by_field_name("alias"),
                    node.child_by_field_name("path"),
                ) else {
                    return;
                };
                let alias = node_text(alias, source);
                let mut segments = prefix.to_vec();
                segments.extend(path_segments(path, source));
                if alias.starts_with(|ch: char| ch.is_ascii_uppercase())
                    && let Some(path) = join_segments(&segments)
                {
                    aliases.insert(alias, path);
                }
            }
            _ => {}
        }
    }

    let mut aliases = HashMap::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "use_declaration" => {
                if let Some(argument) = node.child_by_field_name("argument") {
                    collect(argument, source, &[], &mut aliases);
                }
            }
            "source_file" | "mod_item" | "declaration_list" => {
                let mut cursor = node.walk();
                stack.extend(node.named_children(&mut cursor));
            }
            _ => {}
        }
    }
    aliases
}

/// `path` with a leading `use .. as Alias` name replaced by the path it
/// renames (`Bar::new` to `crate::a::Foo::new`).
fn dealias_path(aliases: &HashMap<String, String>, path: String) -> String {
    let (head, rest) = match path.split_once("::") {
        Some((head, rest)) => (head, Some(rest)),
        None => (path.as_str(), None),
    };
    match (aliases.get(head), rest) {
        (Some(real), Some(rest)) => format!("{real}::{rest}"),
        (Some(real), None) => real.clone(),
        (None, _) => path,
    }
}

/// Types of the names bound in `scope` (a function, excluding nested items),
/// for typing method-call receivers.
///
/// A name maps to `Some(type)` only when it is bound once (or always with the
/// same type) and that binding spells the type: a parameter or `let` with a
/// type annotation, or a `let` initialised by `T::new(..)`, `T::default()`, or
/// a `T { .. }` literal. Every other binding (a `match`/`for`/`if let`
/// pattern, an untyped closure parameter, an inferred `let`) makes the name
/// untyped, so shadowing never attaches a stale type. See [`type_hint`].
///
/// A type named by a type parameter of the function or its enclosing
/// `impl`/`trait` (`x: T` in `fn g<T: Tr>`) is unknown, so it leaves the
/// name untyped; a `use .. as Alias` name is read as the type it renames.
fn typed_bindings(
    scope: Node,
    source: &str,
    module: &ModuleScope,
    aliases: &HashMap<String, String>,
) -> HashMap<String, Option<String>> {
    fn record(types: &mut HashMap<String, Option<String>>, name: String, hint: Option<String>) {
        types
            .entry(name)
            .and_modify(|existing| {
                if *existing != hint {
                    *existing = None;
                }
            })
            .or_insert(hint);
    }
    fn record_untyped(types: &mut HashMap<String, Option<String>>, pattern: Node, source: &str) {
        let mut names = HashSet::new();
        collect_identifiers(pattern, source, &mut names);
        for name in names {
            record(types, name, None);
        }
    }

    let generics = type_parameter_names(scope, source);
    let known = |hint: String| -> Option<String> {
        let (object, path) = match hint.strip_prefix("dyn ") {
            Some(path) => ("dyn ", path),
            None => ("", hint.as_str()),
        };
        let root = path.split("::").next().unwrap_or(path);
        (!generics.contains(root))
            .then(|| format!("{object}{}", dealias_path(aliases, path.to_string())))
    };

    let mut types = HashMap::new();
    let mut stack = vec![scope];
    while let Some(node) = stack.pop() {
        let pattern = node.child_by_field_name("pattern");
        match node.kind() {
            "function_item" | "impl_item" | "mod_item" | "trait_item" if node != scope => {
                continue;
            }
            "parameter" | "let_declaration" => {
                if let Some(pattern) = pattern {
                    if pattern.kind() == "identifier" {
                        let hint = node
                            .child_by_field_name("type")
                            .and_then(|type_node| type_hint(type_node, source, module))
                            .or_else(|| {
                                node.child_by_field_name("value")
                                    .and_then(|value| constructed_type(value, source, module))
                            })
                            .and_then(known);
                        record(&mut types, node_text(pattern, source), hint);
                    } else {
                        record_untyped(&mut types, pattern, source);
                    }
                }
            }
            "closure_parameters" => {
                let mut cursor = node.walk();
                for parameter in node.named_children(&mut cursor) {
                    if parameter.kind() != "parameter" {
                        record_untyped(&mut types, parameter, source);
                    }
                }
            }
            _ => {
                if let Some(pattern) = pattern {
                    record_untyped(&mut types, pattern, source);
                }
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    types
}

/// The type a method call on a value of type `node` dispatches on: references
/// and the `Box`/`Arc`/`Rc` smart pointers are looked through (auto-deref),
/// `dyn Trait`/`impl Trait` give `dyn Trait` (the trait's members, never a
/// same-named struct's), `Self` the enclosing impl type,
/// and any other generic type its base (`Vec<T>` → `Vec`). Tuples, slices,
/// primitives, and function types give `None`.
fn type_hint(node: Node, source: &str, module: &ModuleScope) -> Option<String> {
    match node.kind() {
        "reference_type" => type_hint(node.child_by_field_name("type")?, source, module),
        "dynamic_type" | "abstract_type" => {
            let trait_name = type_hint(node.child_by_field_name("trait")?, source, module)?;
            (!trait_name.starts_with("dyn ")).then(|| format!("dyn {trait_name}"))
        }
        "generic_type" => {
            let base = node.child_by_field_name("type")?;
            let base_text = normalize_qualified_name(&node_text(base, source));
            let base_name = base_text.rsplit("::").next().unwrap_or(&base_text);
            if matches!(base_name, "Box" | "Arc" | "Rc") {
                let arguments = node.child_by_field_name("type_arguments")?;
                let mut cursor = arguments.walk();
                let inner = arguments
                    .named_children(&mut cursor)
                    .find(|argument| argument.kind() != "lifetime")?;
                type_hint(inner, source, module)
            } else {
                type_hint(base, source, module)
            }
        }
        "type_identifier" if node_text(node, source) == "Self" => {
            enclosing_impl_type(node, source, module)
        }
        "type_identifier" | "scoped_type_identifier" => {
            let text = normalize_qualified_name(&node_text(node, source));
            let name = text.rsplit("::").next().unwrap_or(&text);
            (name.starts_with(|ch: char| ch.is_ascii_uppercase()) && !is_ignored_type_name(name))
                .then_some(text)
        }
        _ => None,
    }
}

/// The type a `let` initialiser evidently constructs: `T::new(..)`,
/// `T::default()`, or a `T { .. }` literal.
fn constructed_type(value: Node, source: &str, module: &ModuleScope) -> Option<String> {
    match value.kind() {
        "call_expression" => {
            let function = value.child_by_field_name("function")?;
            if function.kind() != "scoped_identifier" {
                return None;
            }
            let constructor = function.child_by_field_name("name")?;
            if !matches!(node_text(constructor, source).as_str(), "new" | "default") {
                return None;
            }
            let path = function.child_by_field_name("path")?;
            if path.kind() == "identifier" && node_text(path, source) == "Self" {
                return enclosing_impl_type(value, source, module);
            }
            type_hint(path, source, module).or_else(|| {
                // A value path (`scoped_identifier`/`identifier`) spelled
                // like a type.
                let text = normalize_qualified_name(&node_text(path, source));
                let name = text.rsplit("::").next().unwrap_or(&text);
                (name.starts_with(|ch: char| ch.is_ascii_uppercase())
                    && !is_ignored_type_name(name))
                .then_some(text)
            })
        }
        "struct_expression" => type_hint(value.child_by_field_name("name")?, source, module),
        _ => None,
    }
}

/// Recovers call refs from a macro invocation's token tree, which tree-sitter
/// leaves unparsed: an identifier or path immediately followed by a
/// parenthesised token tree (`f(..)`, `a::b(..)`, `x.m(..)`, `f::<T>(..)`) is
/// read as a call, at any nesting depth (`assert!(!store.delete(id)?)`,
/// `vec![path(root, "a")]`, `format!("{}", render(x))`).
///
/// Recovered refs carry exactly what the same call written outside a macro
/// would: method calls keep their receiver text (so the ORB-12416 rule — no
/// name-only match for an unknown receiver — still applies), paths keep their
/// qualified spelling, and resolution confidence is still decided by pass 2's
/// ladder from the symbol table, never raised by the extractor. Declarations
/// spelled inside a macro (`fn f(..)`, `struct S(..)`) are skipped.
fn collect_macro_token_refs(
    tree: Node,
    source: &str,
    module: &ModuleScope,
    state: &mut ExtractionState,
) {
    let mut cursor = tree.walk();
    let tokens: Vec<Node> = tree.children(&mut cursor).collect();
    for (index, token) in tokens.iter().enumerate() {
        match token.kind() {
            "token_tree" => collect_macro_token_refs(*token, source, module, state),
            "identifier" if is_macro_call_head(&tokens, index) => {
                push_macro_call_ref(&tokens, index, source, module, state);
            }
            _ => {}
        }
    }
}

/// Whether the identifier at `index` is followed by call arguments: a `(`
/// token tree, optionally after a `::<..>` turbofish.
fn is_macro_call_head(tokens: &[Node], index: usize) -> bool {
    let mut next = index + 1;
    if tokens.get(next).is_some_and(|token| token.kind() == "::")
        && tokens
            .get(next + 1)
            .is_some_and(|token| token.kind() == "<")
    {
        let mut depth = 0i32;
        let mut cursor = next + 1;
        loop {
            let Some(token) = tokens.get(cursor) else {
                return false;
            };
            depth += match token.kind() {
                "<" => 1,
                "<<" => 2,
                ">" => -1,
                ">>" => -2,
                _ => 0,
            };
            cursor += 1;
            if depth <= 0 {
                break;
            }
        }
        next = cursor;
    }
    let Some(arguments) = tokens.get(next) else {
        return false;
    };
    arguments.kind() == "token_tree"
        && arguments
            .child(0)
            .is_some_and(|delimiter| delimiter.kind() == "(")
}

fn push_macro_call_ref(
    tokens: &[Node],
    index: usize,
    source: &str,
    module: &ModuleScope,
    state: &mut ExtractionState,
) {
    let name_node = tokens[index];
    let name = node_text(name_node, source);
    if is_reserved_word(&name) || is_ignored_type_name(&name) {
        return;
    }
    let previous = index.checked_sub(1).map(|previous| tokens[previous]);
    if previous.is_some_and(|previous| {
        matches!(
            previous.kind(),
            "fn" | "struct" | "enum" | "union" | "trait" | "mod" | "type" | "impl" | "const"
        )
    }) {
        return;
    }

    let (span_start, target_qualified, confidence, receiver) = match previous.map(|p| p.kind()) {
        Some(".") => {
            let receiver_end = index - 1;
            let receiver_start = macro_receiver_start(tokens, receiver_end);
            if receiver_start == receiver_end {
                return;
            }
            let receiver_text = source
                .get(tokens[receiver_start].start_byte()..tokens[receiver_end - 1].end_byte())
                .unwrap_or_default()
                .to_string();
            if receiver_text == "self" {
                let target = enclosing_impl_type(name_node, source, module)
                    .map(|type_name| format!("<{type_name}>::{name}"));
                (name_node.start_byte(), target, "fuzzy_name", None)
            } else {
                let target = (receiver_start + 1 == receiver_end)
                    .then(|| {
                        state.typed_receiver_target(tokens[receiver_start], &name, source, module)
                    })
                    .flatten();
                (
                    name_node.start_byte(),
                    target,
                    "fuzzy_name",
                    Some(receiver_text),
                )
            }
        }
        Some("::") => {
            let mut start = index;
            while start >= 2
                && tokens[start - 1].kind() == "::"
                && matches!(
                    tokens[start - 2].kind(),
                    "identifier" | "self" | "super" | "crate"
                )
            {
                start -= 2;
            }
            let path = source
                .get(tokens[start].start_byte()..name_node.end_byte())
                .map(normalize_qualified_name)
                .unwrap_or_default();
            let fully_spelled = start == 0 || tokens[start - 1].kind() != "::";
            if let Some(rest) = path.strip_prefix("Self::") {
                let target = enclosing_impl_type(name_node, source, module)
                    .map(|type_name| format!("<{type_name}>::{rest}"));
                (tokens[start].start_byte(), target, "fuzzy_name", None)
            } else if fully_spelled && path.contains("::") {
                (
                    tokens[start].start_byte(),
                    Some(dealias_path(&state.type_aliases, path)),
                    "import_resolved",
                    None,
                )
            } else {
                // `Vec::<u8>::new(..)` or `<T as Trait>::m(..)`: the path is
                // not recoverable from the tokens, so keep only the name.
                (tokens[start].start_byte(), None, "fuzzy_name", None)
            }
        }
        _ => (
            name_node.start_byte(),
            Some(module.qualify(&name)),
            "fuzzy_name",
            None,
        ),
    };

    state.refs.push(RawRef {
        from_file: state.file_path.clone(),
        from_span_start: span_start,
        from_span_end: name_node.end_byte(),
        target_name: name,
        target_qualified,
        kind: "call".to_string(),
        confidence: confidence.to_string(),
        unresolved_receiver: receiver,
        spelled_path: confidence == "import_resolved",
    });
}

/// Index of the first token of the postfix expression that ends just before
/// `dot` (the `.` of a method call): identifiers, `self`, literals, argument
/// or index token trees, and the `.`/`::`/`?`/`.await` that chain them.
fn macro_receiver_start(tokens: &[Node], dot: usize) -> usize {
    let mut start = dot;
    while start > 0 {
        let kind = tokens[start - 1].kind();
        let chains = matches!(
            kind,
            "identifier" | "self" | "super" | "crate" | "token_tree" | "." | "::" | "?" | "await"
        ) || kind.ends_with("_literal");
        if !chains {
            break;
        }
        start -= 1;
    }
    // A leading `.`/`::`/`?` cannot start an expression.
    while start < dot && matches!(tokens[start].kind(), "." | "::" | "?") {
        start += 1;
    }
    start
}

fn collect_call_ref(node: Node, source: &str, module: &ModuleScope, state: &mut ExtractionState) {
    let Some(function) = node.child_by_field_name("function") else {
        return;
    };
    collect_runtime_invocation(node, function, source, state);
    push_call_target_ref(function, source, module, state);
}

/// Records a `runtime_invocation` ref for a call that starts a program the
/// syntax names: `Command::new("<prog>")`,
/// `Command::new(env!("CARGO_BIN_EXE_<name>"))` (target `<name>`), and
/// `Command::cargo_bin("<name>")` (`assert_cmd`), under any path prefix
/// (`std::process::Command`, `assert_cmd::Command`, ...).
///
/// The target is the program string exactly as written and is never resolved
/// to a symbol: a consumer may only associate it with a program by name. The
/// ref is anchored at the whole call, so its span lies inside the enclosing
/// function.
fn collect_runtime_invocation(
    call: Node,
    function: Node,
    source: &str,
    state: &mut ExtractionState,
) {
    if function.kind() != "scoped_identifier" {
        return;
    }
    let path = normalize_qualified_name(&node_text(function, source));
    let mut segments = path.rsplit("::");
    let (Some(constructor), Some("Command")) = (segments.next(), segments.next()) else {
        return;
    };
    if !matches!(constructor, "new" | "cargo_bin") {
        return;
    }
    let Some(arguments) = call.child_by_field_name("arguments") else {
        return;
    };
    let mut cursor = arguments.walk();
    let Some(first) = arguments
        .named_children(&mut cursor)
        .find(|argument| !matches!(argument.kind(), "line_comment" | "block_comment"))
    else {
        return;
    };
    let program = match first.kind() {
        "string_literal" | "raw_string_literal" => rust_string_literal_value(first, source),
        "macro_invocation" if constructor == "new" => cargo_bin_exe_name(first, source),
        _ => None,
    };
    let Some(program) = program.filter(|program| !program.is_empty()) else {
        return;
    };
    state.refs.push(RawRef {
        from_file: state.file_path.clone(),
        from_span_start: call.start_byte(),
        from_span_end: call.end_byte(),
        target_name: program,
        target_qualified: None,
        kind: "runtime_invocation".to_string(),
        confidence: "fuzzy_name".to_string(),
        unresolved_receiver: None,
        spelled_path: false,
    });
}

/// Contents of a plain or raw string literal without escape processing.
fn rust_string_literal_value(node: Node, source: &str) -> Option<String> {
    let text = node_text(node, source);
    let body = text.trim_start_matches('r').trim_matches('#');
    body.strip_prefix('"')?
        .strip_suffix('"')
        .map(ToOwned::to_owned)
}

/// `<name>` from `env!("CARGO_BIN_EXE_<name>")`, the path Cargo gives an
/// integration test for the package's `<name>` binary.
fn cargo_bin_exe_name(node: Node, source: &str) -> Option<String> {
    let macro_name = node.child_by_field_name("macro")?;
    if node_text(macro_name, source) != "env" {
        return None;
    }
    let text = node_text(node, source);
    let (_, rest) = text.split_once("\"CARGO_BIN_EXE_")?;
    let (name, _) = rest.split_once('"')?;
    Some(name.to_string())
}

/// Pushes a ref for the call target at `function`, then recurses into any
/// nested receiver/path expression that can itself contain further calls:
/// method-chain receivers (`a().b()`), and `?`/`.await`-wrapped receivers
/// (`a()?.b()`, `a().await.b()`), which surface as the `value` of the
/// receiver's `field_expression` and are otherwise never visited because a
/// `call_expression`'s own traversal only descends into its `arguments`.
fn push_call_target_ref(
    function: Node,
    source: &str,
    module: &ModuleScope,
    state: &mut ExtractionState,
) {
    match function.kind() {
        "identifier" => state.push_ref(
            function,
            source,
            Some(module.qualify(&node_text(function, source))),
            "call",
            "fuzzy_name",
        ),
        "scoped_identifier" => {
            let self_target = self_call_target_qualified(function, source, module);
            let is_self_target = self_target.is_some();
            let target = self_target.unwrap_or_else(|| {
                dealias_path(
                    &state.type_aliases,
                    normalize_qualified_name(&node_text(function, source)),
                )
            });
            state.push_ref(
                function,
                source,
                Some(target),
                "call",
                if is_self_target {
                    "fuzzy_name"
                } else {
                    "import_resolved"
                },
            );
        }
        "field_expression" => {
            let value = function.child_by_field_name("value");
            let receiver = value.and_then(|value| unresolved_receiver_text(value, source));
            if let Some(field) = function.child_by_field_name("field") {
                let target = self_call_target_qualified(function, source, module).or_else(|| {
                    let method = node_text(field, source);
                    state.typed_receiver_target(value?, &method, source, module)
                });
                state.push_ref_with_receiver(field, source, target, "call", "fuzzy_name", receiver);
            }
            if let Some(value) = function.child_by_field_name("value") {
                collect_expression_refs(value, source, module, state);
            }
        }
        "generic_function" => {
            // Turbofish on a method call (`x.collect::<Vec<_>>()`) or on a
            // path call (`a::run::<T>()`): unwrap to the inner function
            // position instead of pushing the whole node's source text.
            if let Some(inner) = function.child_by_field_name("function") {
                push_call_target_ref(inner, source, module, state);
            }
        }
        "generic_type_with_turbofish" => {
            if let Some(type_node) = function.child_by_field_name("function") {
                state.push_ref(
                    type_node,
                    source,
                    type_qualified_name(type_node, source, module),
                    "call",
                    confidence_for_type(type_node, source),
                );
            } else {
                collect_expression_refs(function, source, module, state);
            }
        }
        _ => collect_expression_refs(function, source, module, state),
    }
}

/// Returns the exact method qualified name for a `self.method()` or
/// `Self::method()` call in an impl. Other receivers remain deliberately
/// untyped: local bindings and fields are outside this extractor's scope.
fn self_call_target_qualified(
    function: Node,
    source: &str,
    module: &ModuleScope,
) -> Option<String> {
    let is_self_call = match function.kind() {
        "field_expression" => function
            .child_by_field_name("value")
            .is_some_and(|value| node_text(value, source) == "self"),
        "scoped_identifier" => node_text(function, source).starts_with("Self::"),
        _ => false,
    };
    if !is_self_call {
        return None;
    }

    let name = function
        .child_by_field_name("field")
        .map(|field| node_text(field, source))
        .or_else(|| {
            node_text(function, source)
                .rsplit("::")
                .next()
                .map(ToOwned::to_owned)
        })?;
    let type_name = enclosing_impl_type(function, source, module)?;
    Some(format!("<{type_name}>::{name}"))
}

/// Qualified name of the type whose `impl` block encloses `node`.
fn enclosing_impl_type(node: Node, source: &str, module: &ModuleScope) -> Option<String> {
    let mut ancestor = Some(node);
    while let Some(node) = ancestor {
        if node.kind() == "impl_item" {
            let type_node = node.child_by_field_name("type")?;
            return type_qualified_name(type_node, source, module);
        }
        ancestor = node.parent();
    }
    None
}

/// Receiver text to record on a method-call ref, or `None` when the receiver
/// identifies the enclosing definition's own type (`self`, `Self`) and a
/// same-file method of that name is therefore a reasonable match.
///
/// Everything else — a local binding, a field, a chained call — names a value
/// whose type this extractor does not track, so the bare method name alone
/// must not be resolved by name. See [`RawRef::unresolved_receiver`].
fn unresolved_receiver_text(value: Node, source: &str) -> Option<String> {
    let text = node_text(value, source);
    if matches!(text.as_str(), "self" | "Self") {
        return None;
    }
    Some(text)
}

fn extract_use(node: Node, source: &str, state: &mut ExtractionState) {
    let Some(argument) = node.child_by_field_name("argument") else {
        return;
    };
    collect_use(argument, source, &[], state);
}

fn collect_use(node: Node, source: &str, prefix: &[String], state: &mut ExtractionState) {
    match node.kind() {
        "scoped_use_list" => {
            let mut next_prefix = prefix.to_vec();
            if let Some(path) = node.child_by_field_name("path") {
                next_prefix.extend(path_segments(path, source));
            }
            if let Some(list) = node.child_by_field_name("list") {
                collect_use(list, source, &next_prefix, state);
            }
        }
        "use_list" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_use(child, source, prefix, state);
            }
        }
        "use_as_clause" => {
            let Some(alias) = node.child_by_field_name("alias") else {
                return;
            };
            let Some(path) = node.child_by_field_name("path") else {
                return;
            };
            let mut source_segments = prefix.to_vec();
            source_segments.extend(path_segments(path, source));
            push_use_rows(
                alias,
                source,
                &source_segments,
                Some(node_text(alias, source)),
                state,
            );
        }
        "use_wildcard" => {
            let mut source_segments = prefix.to_vec();
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                source_segments.extend(path_segments(child, source));
            }
            if let Some(target_path) = join_segments(&source_segments) {
                state.imports.push(RawImport {
                    from_file: state.file_path.clone(),
                    target_path,
                    target_symbol: None,
                });
                state.push_ref(node, source, None, "use", "import_resolved");
            }
        }
        "identifier" | "crate" | "self" | "super" | "scoped_identifier" => {
            let mut source_segments = prefix.to_vec();
            source_segments.extend(path_segments(node, source));
            push_use_rows(node, source, &source_segments, None, state);
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_use(child, source, prefix, state);
            }
        }
    }
}

fn push_use_rows(
    span_node: Node,
    source: &str,
    source_segments: &[String],
    alias: Option<String>,
    state: &mut ExtractionState,
) {
    let Some(source_path) = join_segments(source_segments) else {
        return;
    };
    let imported_name = alias.unwrap_or_else(|| import_name(source_segments));
    let target_path = import_target_path(source_segments);

    state.imports.push(RawImport {
        from_file: state.file_path.clone(),
        target_path,
        target_symbol: Some(imported_name),
    });
    state.push_ref(
        span_node,
        source,
        Some(source_path),
        "use",
        "import_resolved",
    );
}

fn get_name(node: Node, source: &str) -> Option<String> {
    node.child_by_field_name("name")
        .map(|name| node_text(name, source))
        .filter(|name| !name.is_empty())
}

fn node_text(node: Node, source: &str) -> String {
    node.utf8_text(source.as_bytes()).unwrap_or("").to_string()
}

fn signature_for(node: Node, source: &str) -> Option<String> {
    let end = node
        .child_by_field_name("body")
        .or_else(|| node.child_by_field_name("value"))
        .map_or_else(|| node.end_byte(), |body| body.start_byte());
    source
        .get(node.start_byte()..end)
        .map(normalize_signature)
        .filter(|signature| !signature.is_empty())
}

fn normalize_signature(signature: &str) -> String {
    signature
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end_matches('=')
        .trim()
        .to_string()
}

fn has_test_attr(attrs: &[String]) -> bool {
    attrs.iter().any(|attr| {
        let compact: String = attr.chars().filter(|ch| !ch.is_whitespace()).collect();
        compact == "#[test]" || compact.ends_with("::test]") || compact.contains("(test")
    })
}

fn qualify_member_or_module(
    module: &ModuleScope,
    parent_symbol: Option<&str>,
    name: &str,
) -> String {
    match parent_symbol {
        Some(parent) => format!("{parent}::{name}"),
        None => module.qualify(name),
    }
}

fn type_qualified_name(node: Node, source: &str, module: &ModuleScope) -> Option<String> {
    let text = type_reference_text(node, source)?;
    if text.is_empty() || is_ignored_type_name(&text) {
        None
    } else if is_already_qualified(&text) {
        Some(normalize_qualified_name(&text))
    } else {
        Some(module.qualify(&text))
    }
}

fn type_reference_text(node: Node, source: &str) -> Option<String> {
    match node.kind() {
        "type_identifier" | "identifier" | "scoped_type_identifier" | "scoped_identifier" => {
            Some(normalize_qualified_name(&node_text(node, source)))
        }
        "generic_type" | "generic_type_with_turbofish" => node
            .child_by_field_name("type")
            .and_then(|type_node| type_reference_text(type_node, source)),
        "reference_type" | "pointer_type" | "array_type" | "tuple_type" | "unit_type" => {
            first_type_reference_child(node, source)
        }
        _ => {
            if is_type_reference_node(node) {
                first_type_reference_child(node, source)
            } else {
                None
            }
        }
    }
}

fn first_type_reference_child(node: Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if let Some(text) = type_reference_text(child, source) {
            return Some(text);
        }
    }
    None
}

fn target_name(node: Node, source: &str) -> Option<String> {
    let text = match node.kind() {
        "generic_type" | "generic_type_with_turbofish" => node
            .child_by_field_name("type")
            .map(|type_node| node_text(type_node, source))?,
        _ => node_text(node, source),
    };
    let normalized = normalize_qualified_name(&text);
    normalized
        .rsplit("::")
        .next()
        .map(str::to_string)
        .filter(|name| !name.is_empty() && name != "*")
}

fn normalize_qualified_name(name: &str) -> String {
    name.split("::")
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("::")
}

fn is_already_qualified(name: &str) -> bool {
    name.contains("::") || name.starts_with('<')
}

fn is_ignored_type_name(name: &str) -> bool {
    matches!(
        name,
        "Self"
            | "self"
            | "str"
            | "bool"
            | "char"
            | "usize"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "f32"
            | "f64"
            | "()"
    )
}

fn is_type_reference_node(node: Node) -> bool {
    matches!(
        node.kind(),
        "type_identifier"
            | "scoped_type_identifier"
            | "generic_type"
            | "generic_type_with_turbofish"
            | "reference_type"
            | "pointer_type"
            | "array_type"
            | "tuple_type"
            | "unit_type"
    )
}

fn confidence_for_type(node: Node, source: &str) -> &'static str {
    let text = node_text(node, source);
    if text.contains("::") {
        "import_resolved"
    } else {
        "fuzzy_name"
    }
}

fn path_segments(node: Node, source: &str) -> Vec<String> {
    normalize_qualified_name(&node_text(node, source))
        .split("::")
        .filter(|segment| !segment.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn join_segments(segments: &[String]) -> Option<String> {
    if segments.is_empty() {
        None
    } else {
        Some(segments.join("::"))
    }
}

fn import_name(segments: &[String]) -> String {
    match segments.last().map(String::as_str) {
        Some("self") if segments.len() > 1 => segments[segments.len() - 2].clone(),
        Some(name) => name.to_string(),
        None => String::new(),
    }
}

fn import_target_path(segments: &[String]) -> String {
    if segments.len() <= 1 {
        return segments.first().cloned().unwrap_or_default();
    }
    segments[..segments.len() - 1].join("::")
}

fn dedup_commands(commands: &mut Vec<RawCommand>) {
    commands.sort_by(|left, right| {
        left.span_start
            .cmp(&right.span_start)
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.handler_symbol.cmp(&right.handler_symbol))
    });
    commands.dedup_by(|left, right| {
        left.file_path == right.file_path
            && left.name == right.name
            && left.span_start == right.span_start
            && left.handler_symbol == right.handler_symbol
    });
}
