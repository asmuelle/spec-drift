use super::DriftAnalyzer;
use crate::context::ProjectContext;
use crate::domain::{Divergence, RuleId, Severity, SpecClaim};
use crate::parsers::MarkdownParser;
use regex::Regex;

/// DocsAnalyzer — enforces the `symbol_absence` rule.
///
/// Strategy: every Markdown inline-code span is checked against an identifier
/// regex. If it looks like a Rust symbol path (`Type::method`, `fn_name`,
/// `module::Ty`), the leaf identifier is looked up against the `CodeFact`
/// index built from the Rust AST. Missing leaves become divergences.
pub struct DocsAnalyzer {
    ident_re: Regex,
}

impl Default for DocsAnalyzer {
    fn default() -> Self {
        Self {
            ident_re: Regex::new(r"^([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*)(\(\))?$")
                .expect("static regex"),
        }
    }
}

impl DriftAnalyzer for DocsAnalyzer {
    fn analyze(&self, ctx: &ProjectContext) -> Vec<Divergence> {
        let mut out = Vec::new();

        for md in &ctx.markdown_files {
            let claims = match MarkdownParser::parse(md) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("spec-drift: skipping {} (docs): {e}", md.display());
                    continue;
                }
            };

            for claim in claims {
                let Some(symbol) = self.extract_symbol(&claim) else {
                    continue;
                };
                let leaf = symbol.rsplit("::").next().unwrap_or(&symbol);

                // Skip stdlib / language keywords that look like identifiers.
                if is_language_intrinsic(leaf) {
                    continue;
                }

                if ctx.facts_named(leaf).next().is_none() {
                    out.push(Divergence {
                        rule: RuleId::SymbolAbsence,
                        severity: Severity::Critical,
                        location: claim.location.clone(),
                        stated: format!("`{symbol}` exists in the codebase"),
                        reality: format!(
                            "no symbol named `{leaf}` found in the parsed Rust sources"
                        ),
                        risk: "New developers and AI agents will reach for a non-existent API."
                            .to_string(),
                        attribution: None,
                    });
                }
            }
        }

        out
    }
}

impl DocsAnalyzer {
    /// Extract a Rust-shaped symbol reference from a Markdown inline code span.
    ///
    /// Returns `Some(symbol)` only when the span is clearly a Rust symbol:
    /// - ends in `()` (a function/method call), OR
    /// - contains a `::` path (qualified path), OR
    /// - starts with an uppercase letter (type name).
    ///
    /// Bare lowercase words like `ignore`, `syn`, `HEAD` are ignored to avoid
    /// flagging ordinary English prose, crate names, filenames, and tool names
    /// that happen to be code-fenced.
    fn extract_symbol(&self, claim: &SpecClaim) -> Option<String> {
        let text = claim.text.trim();
        let caps = self.ident_re.captures(text)?;
        let symbol = caps.get(1)?.as_str().to_string();
        let has_call_parens = caps.get(2).is_some();
        let has_path_sep = symbol.contains("::");
        // A type-looking name is CamelCase: starts uppercase AND contains at
        // least one lowercase letter. This rejects all-caps words like
        // `README`, `HEAD`, `AGENTS` that are common in English prose but
        // aren't Rust type names.
        let looks_like_type = symbol.starts_with(|c: char| c.is_ascii_uppercase())
            && symbol.chars().any(|c| c.is_ascii_lowercase());

        if has_call_parens || has_path_sep || looks_like_type {
            Some(symbol)
        } else {
            None
        }
    }
}

/// A small stop-list of identifiers that Rust docs commonly reference but that
/// `spec-drift` cannot resolve against a project-local AST (primitive types,
/// `std` items, common tool names and filenames). This is a heuristic — users
/// can extend or disable it via config once the rule engine lands.
fn is_language_intrinsic(name: &str) -> bool {
    matches!(
        name,
        // Primitives and keywords
        "bool"
            | "char"
            | "str"
            | "String"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "f32"
            | "f64"
            | "Option"
            | "Some"
            | "None"
            | "Result"
            | "Ok"
            | "Err"
            | "Vec"
            | "Box"
            | "Arc"
            | "Rc"
            | "RefCell"
            | "Cell"
            | "Mutex"
            | "RwLock"
            | "HashMap"
            | "HashSet"
            | "BTreeMap"
            | "BTreeSet"
            | "fn"
            | "impl"
            | "trait"
            | "struct"
            | "enum"
            | "pub"
            | "mod"
            | "use"
            | "self"
            | "Self"
            | "super"
            | "crate"
            // Common tool / config filenames that appear CamelCased in prose
            | "Makefile"
            | "GNUmakefile"
            | "Dockerfile"
            | "Cargo"
            | "Cargo.toml"
            | "Cargo.lock"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{CodeFact, FactKind, Location};

    fn fact(name: &str) -> CodeFact {
        CodeFact {
            location: Location::new("src/lib.rs", 1),
            kind: FactKind::Function,
            name: name.to_string(),
        }
    }

    #[test]
    fn flags_missing_symbol_referenced_in_markdown() {
        let tmp = tempfile::tempdir().unwrap();
        let md = tmp.path().join("README.md");
        std::fs::write(&md, "Use `connect_to_db()` to start.\n").unwrap();

        let mut ctx = ProjectContext::new(tmp.path());
        ctx.markdown_files.push(md.clone());
        ctx.code_facts.push(fact("init_connection"));

        let divergences = DocsAnalyzer::default().analyze(&ctx);
        assert_eq!(divergences.len(), 1);
        assert_eq!(divergences[0].rule, RuleId::SymbolAbsence);
        assert_eq!(divergences[0].location.line, 1);
    }

    #[test]
    fn does_not_flag_symbol_that_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let md = tmp.path().join("README.md");
        std::fs::write(&md, "Use `connect_to_db()` to start.\n").unwrap();

        let mut ctx = ProjectContext::new(tmp.path());
        ctx.markdown_files.push(md);
        ctx.code_facts.push(fact("connect_to_db"));

        assert!(DocsAnalyzer::default().analyze(&ctx).is_empty());
    }

    #[test]
    fn ignores_language_intrinsics() {
        let tmp = tempfile::tempdir().unwrap();
        let md = tmp.path().join("README.md");
        std::fs::write(&md, "Returns `Option<String>` on failure.\n").unwrap();

        let mut ctx = ProjectContext::new(tmp.path());
        ctx.markdown_files.push(md);

        // No code facts — but Option/String are intrinsics so must not flag.
        assert!(DocsAnalyzer::default().analyze(&ctx).is_empty());
    }

    #[test]
    fn ignores_bare_lowercase_words() {
        // Words like `ignore`, `syn`, `git2` in backticks are usually crate
        // names or prose, not API references. They must not generate drift.
        let tmp = tempfile::tempdir().unwrap();
        let md = tmp.path().join("README.md");
        std::fs::write(&md, "Uses the `ignore` crate and `syn` for parsing.\n").unwrap();

        let mut ctx = ProjectContext::new(tmp.path());
        ctx.markdown_files.push(md);
        assert!(DocsAnalyzer::default().analyze(&ctx).is_empty());
    }

    #[test]
    fn ignores_all_caps_words() {
        // `README`, `HEAD`, `AGENTS` look uppercase but aren't Rust type names.
        let tmp = tempfile::tempdir().unwrap();
        let md = tmp.path().join("README.md");
        std::fs::write(&md, "See `README` and `HEAD` and `AGENTS`.\n").unwrap();

        let mut ctx = ProjectContext::new(tmp.path());
        ctx.markdown_files.push(md);
        assert!(DocsAnalyzer::default().analyze(&ctx).is_empty());
    }

    #[test]
    fn resolves_method_path_by_leaf() {
        let tmp = tempfile::tempdir().unwrap();
        let md = tmp.path().join("README.md");
        std::fs::write(&md, "Call `Client::new()` to build.\n").unwrap();

        let mut ctx = ProjectContext::new(tmp.path());
        ctx.markdown_files.push(md);
        ctx.code_facts.push(fact("new"));

        assert!(DocsAnalyzer::default().analyze(&ctx).is_empty());
    }
}
