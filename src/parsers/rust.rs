use crate::domain::{CodeFact, FactKind, Location};
use crate::error::SpecDriftError;
use std::path::Path;
use syn::spanned::Spanned;

pub struct RustParser;

impl RustParser {
    /// Parse `path` as a Rust file and extract every top-level (and nested)
    /// item as a [`CodeFact`]. Free functions inside `impl` blocks are included
    /// so method references like `Client::new` can be resolved.
    pub fn parse(path: &Path) -> Result<Vec<CodeFact>, SpecDriftError> {
        let source = std::fs::read_to_string(path).map_err(|e| SpecDriftError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;

        let file = syn::parse_file(&source).map_err(|e| SpecDriftError::RustParse {
            path: path.to_path_buf(),
            source: e,
        })?;

        let mut facts = Vec::new();
        collect_items(&file.items, path, &mut facts);
        Ok(facts)
    }
}

fn collect_items(items: &[syn::Item], path: &Path, facts: &mut Vec<CodeFact>) {
    for item in items {
        match item {
            syn::Item::Fn(f) => {
                push(
                    facts,
                    path,
                    FactKind::Function,
                    f.sig.ident.to_string(),
                    f.span(),
                );
            }
            syn::Item::Struct(s) => {
                push(facts, path, FactKind::Struct, s.ident.to_string(), s.span());
            }
            syn::Item::Enum(e) => {
                push(facts, path, FactKind::Enum, e.ident.to_string(), e.span());
            }
            syn::Item::Trait(t) => {
                push(facts, path, FactKind::Trait, t.ident.to_string(), t.span());
            }
            syn::Item::Type(t) => {
                push(
                    facts,
                    path,
                    FactKind::TypeAlias,
                    t.ident.to_string(),
                    t.span(),
                );
            }
            syn::Item::Const(c) => {
                push(
                    facts,
                    path,
                    FactKind::Constant,
                    c.ident.to_string(),
                    c.span(),
                );
            }
            syn::Item::Static(s) => {
                push(
                    facts,
                    path,
                    FactKind::Constant,
                    s.ident.to_string(),
                    s.span(),
                );
            }
            syn::Item::Macro(m) => {
                if let Some(ident) = m.ident.as_ref() {
                    push(facts, path, FactKind::Macro, ident.to_string(), m.span());
                }
            }
            syn::Item::Mod(m) => {
                push(facts, path, FactKind::Module, m.ident.to_string(), m.span());
                if let Some((_, items)) = &m.content {
                    collect_items(items, path, facts);
                }
            }
            syn::Item::Impl(i) => {
                for impl_item in &i.items {
                    if let syn::ImplItem::Fn(f) = impl_item {
                        push(
                            facts,
                            path,
                            FactKind::Function,
                            f.sig.ident.to_string(),
                            f.span(),
                        );
                    }
                }
            }
            _ => {}
        }
    }
}

fn push(
    facts: &mut Vec<CodeFact>,
    path: &Path,
    kind: FactKind,
    name: String,
    span: proc_macro2::Span,
) {
    facts.push(CodeFact {
        location: Location::new(path, span.start().line as u32),
        kind,
        name,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_fn_struct_enum_trait_impl_methods() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("lib.rs");
        std::fs::write(
            &path,
            r#"
                pub fn hello() {}
                pub struct Widget;
                pub enum Color { Red, Blue }
                pub trait Sing { fn sing(&self); }
                impl Widget { pub fn make() {} }
            "#,
        )
        .unwrap();

        let facts = RustParser::parse(&path).unwrap();
        let names: Vec<_> = facts.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"hello"));
        assert!(names.contains(&"Widget"));
        assert!(names.contains(&"Color"));
        assert!(names.contains(&"Sing"));
        assert!(names.contains(&"make"));
    }
}
