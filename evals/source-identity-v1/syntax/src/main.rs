//! Offline syntax only. Input bytes are parsed, never compiled or expanded.
use serde_json::{json, Value};
use std::io::{self, Read, Write};
use syn::{spanned::Spanned, visit::Visit, Attribute, Ident, Item, Path, Type};

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("Rust syntax error: {0}")]
    Syntax(#[from] syn::Error),
    #[error("unsupported Rust context: {0}")]
    Unsupported(String),
    #[error("source read failed: {0}")]
    Read(#[from] io::Error),
}
impl From<&str> for Error {
    fn from(message: &str) -> Self {
        Self::Unsupported(message.into())
    }
}
impl From<String> for Error {
    fn from(message: String) -> Self {
        Self::Unsupported(message)
    }
}
type Result<T> = std::result::Result<T, Error>;

fn path(path: &Path) -> Result<String> {
    if path.leading_colon.is_some() {
        return Err("absolute extern paths are unsupported".into());
    }
    Ok(path
        .segments
        .iter()
        .map(|p| p.ident.to_string())
        .collect::<Vec<_>>()
        .join("::"))
}

// Only attributes whose binding effects we explicitly understand are admitted.
// cfg preserves a named blocker; arbitrary attribute/derive macros can emit new
// bindings and therefore cannot be reduced to a blocker for the written name.
fn attributes(attrs: &[Attribute]) -> Result<(Option<String>, Option<String>)> {
    let mut uncertain = None;
    let mut module_path = None;
    for attr in attrs {
        let name = path(attr.path())?;
        match name.as_str() {
            "cfg" => uncertain = Some("conditional declaration (cfg is not evaluated)".into()),
            "path" => {
                if module_path.is_some() {
                    return Err("duplicate path attribute".into());
                }
                if let syn::Meta::NameValue(meta) = &attr.meta {
                    if let syn::Expr::Lit(lit) = &meta.value {
                        if let syn::Lit::Str(value) = &lit.lit {
                            module_path = Some(value.value());
                            continue;
                        }
                    }
                }
                return Err("module path must be a literal string".into());
            }
            "derive" => {
                let derives = attr
                    .parse_args_with(
                        syn::punctuated::Punctuated::<Path, syn::Token![,]>::parse_terminated,
                    )
                    .map_err(Error::Syntax)?;
                for derive in derives {
                    // Standard derives generate trait impls, not scored written methods.
                    if !matches!(
                        path(&derive)?.as_str(),
                        "Clone"
                            | "Copy"
                            | "Debug"
                            | "Default"
                            | "Eq"
                            | "PartialEq"
                            | "Ord"
                            | "PartialOrd"
                            | "Hash"
                    ) {
                        return Err("custom derive can generate unknown bindings".into());
                    }
                }
            }
            "doc" | "allow" | "warn" | "deny" | "forbid" | "expect" | "repr" | "inline"
            | "cold" | "must_use" | "deprecated" | "non_exhaustive" | "no_mangle"
            | "export_name" | "link_section" | "track_caller" => {}
            _ => {
                return Err(format!("unsupported attribute {name}; binding effects unknown").into())
            }
        }
    }
    Ok((uncertain, module_path))
}

fn named(kind: &str, name: &Ident, span: proc_macro2::Span, attrs: &[Attribute]) -> Result<Value> {
    let (uncertain, module_path) = attributes(attrs)?;
    if kind != "mod" && module_path.is_some() {
        return Err("path attribute requires an external module".into());
    }
    let start = name.span().start();
    Ok(
        json!({"kind": kind, "name": name.to_string(), "line": start.line,
        "column": start.column + 1, "end_line": span.end().line,
        "unsupported": uncertain, "path": module_path}),
    )
}

fn items(input: &[Item]) -> Result<Vec<Value>> {
    input.iter().map(item).collect()
}

fn item(input: &Item) -> Result<Value> {
    match input {
        Item::Fn(x) => named("fn", &x.sig.ident, x.span(), &x.attrs),
        Item::Struct(x) => named("struct", &x.ident, x.span(), &x.attrs),
        Item::Enum(x) => named("enum", &x.ident, x.span(), &x.attrs),
        Item::Trait(x) => {
            let mut value = named("trait", &x.ident, x.span(), &x.attrs)?;
            let children = x
                .items
                .iter()
                .map(|child| match child {
                    syn::TraitItem::Fn(f) => named("fn", &f.sig.ident, f.span(), &f.attrs),
                    syn::TraitItem::Const(c) => named("unsupported", &c.ident, c.span(), &c.attrs),
                    syn::TraitItem::Type(t) => named("unsupported", &t.ident, t.span(), &t.attrs),
                    _ => Err("trait macro/verbatim item leaves bindings unknown".into()),
                })
                .collect::<Result<Vec<_>>>()?;
            value["children"] = json!(children);
            Ok(value)
        }
        Item::Impl(x) => {
            let (uncertain, module_path) = attributes(&x.attrs)?;
            if module_path.is_some() || x.defaultness.is_some() {
                return Err("unsupported impl attributes/specialization".into());
            }
            let owner = match x.self_ty.as_ref() {
                Type::Path(p) if p.qself.is_none() => path(&p.path)?,
                _ => return Err("impl owner is not a named local type".into()),
            };
            if x.generics
                .type_params()
                .any(|p| owner.split("::").next() == Some(p.ident.to_string().as_str()))
            {
                return Err("blanket impl owner is a generic parameter".into());
            }
            let trait_path = x
                .trait_
                .as_ref()
                .map(|(negative, p, _)| {
                    if negative.is_some() {
                        Err("negative impl is unsupported".into())
                    } else {
                        path(p)
                    }
                })
                .transpose()?;
            let children = x
                .items
                .iter()
                .map(|child| match child {
                    syn::ImplItem::Fn(f) => named("fn", &f.sig.ident, f.span(), &f.attrs),
                    syn::ImplItem::Const(c) => named("unsupported", &c.ident, c.span(), &c.attrs),
                    syn::ImplItem::Type(t) => named("unsupported", &t.ident, t.span(), &t.attrs),
                    _ => Err("impl macro/verbatim item leaves bindings unknown".into()),
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(json!({"kind": "impl", "owner": owner, "trait": trait_path,
                "unsupported": uncertain, "children": children}))
        }
        Item::Mod(x) => {
            let mut value = named("mod", &x.ident, x.span(), &x.attrs)?;
            value["children"] = match &x.content {
                Some((_, content)) => json!(items(content)?),
                None => Value::Null,
            };
            Ok(value)
        }
        Item::Use(x) => {
            let (uncertain, module_path) = attributes(&x.attrs)?;
            if uncertain.is_some() || module_path.is_some() || x.leading_colon.is_some() {
                return Err("conditional/absolute import unsupported".into());
            }
            let mut tree = &x.tree;
            let mut parts = Vec::new();
            while let syn::UseTree::Path(p) = tree {
                parts.push(p.ident.to_string());
                tree = &p.tree;
            }
            let alias = match tree {
                syn::UseTree::Name(n) => {
                    parts.push(n.ident.to_string());
                    n.ident.to_string()
                }
                syn::UseTree::Rename(n) => {
                    parts.push(n.ident.to_string());
                    n.rename.to_string()
                }
                _ => return Err("group/glob imports leave bindings unknown".into()),
            };
            Ok(json!({"kind": "use", "name": alias, "target": parts.join("::")}))
        }
        Item::Const(x) => named("unsupported", &x.ident, x.span(), &x.attrs),
        Item::Static(x) => named("unsupported", &x.ident, x.span(), &x.attrs),
        Item::Type(x) => named("unsupported", &x.ident, x.span(), &x.attrs),
        _ => Err("item macro/foreign/unsupported scope leaves bindings unknown".into()),
    }
}

// syn intentionally preserves some unrecognized syntax as Verbatim. Never
// accept those escape hatches as proof of syntax validity, even in a body.
#[derive(Default)]
struct NoVerbatim(bool);
impl<'ast> Visit<'ast> for NoVerbatim {
    fn visit_expr(&mut self, node: &'ast syn::Expr) {
        self.0 |= matches!(node, syn::Expr::Verbatim(_));
        syn::visit::visit_expr(self, node);
    }
    fn visit_type(&mut self, node: &'ast Type) {
        self.0 |= matches!(node, Type::Verbatim(_));
        syn::visit::visit_type(self, node);
    }
    fn visit_pat(&mut self, node: &'ast syn::Pat) {
        self.0 |= matches!(node, syn::Pat::Verbatim(_));
        syn::visit::visit_pat(self, node);
    }
    fn visit_impl_item(&mut self, node: &'ast syn::ImplItem) {
        self.0 |= matches!(node, syn::ImplItem::Verbatim(_));
        syn::visit::visit_impl_item(self, node);
    }
    fn visit_trait_item(&mut self, node: &'ast syn::TraitItem) {
        self.0 |= matches!(node, syn::TraitItem::Verbatim(_));
        syn::visit::visit_trait_item(self, node);
    }
    fn visit_foreign_item(&mut self, node: &'ast syn::ForeignItem) {
        self.0 |= matches!(node, syn::ForeignItem::Verbatim(_));
        syn::visit::visit_foreign_item(self, node);
    }
    fn visit_item(&mut self, node: &'ast Item) {
        self.0 |= matches!(node, Item::Verbatim(_));
        syn::visit::visit_item(self, node);
    }
}

fn parse(source: &str) -> Result<Value> {
    let file = syn::parse_file(source)?;
    let mut visitor = NoVerbatim::default();
    visitor.visit_file(&file);
    if visitor.0 {
        return Err("unrecognized verbatim Rust syntax".into());
    }
    let (uncertain, module_path) = attributes(&file.attrs)?;
    if uncertain.is_some() || module_path.is_some() {
        return Err("conditional/path crate attribute unsupported".into());
    }
    Ok(json!({"items": items(&file.items)?, "sources": {
        "syntax/Cargo.toml": include_str!("../Cargo.toml"),
        "syntax/Cargo.lock": include_str!("../Cargo.lock"),
        "syntax/src/main.rs": include_str!("main.rs")
    }}))
}

fn main() {
    let mut source = String::new();
    let result = io::stdin()
        .take(1_048_577)
        .read_to_string(&mut source)
        .map_err(Error::Read)
        .and_then(|_| {
            if source.len() > 1_048_576 {
                Err("source size limit exceeded".into())
            } else {
                parse(&source)
            }
        });
    let output = match result {
        Ok(value) => value,
        Err(error) => json!({"error": error.to_string()}),
    };
    // This private JSON protocol is consumed by Python; no source is executed.
    if let Err(error) = writeln!(io::stdout().lock(), "{output}") {
        if error.kind() != io::ErrorKind::BrokenPipe {
            std::process::exit(1);
        }
    }
}
