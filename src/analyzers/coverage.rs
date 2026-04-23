use super::DriftAnalyzer;
use crate::context::ProjectContext;
use crate::domain::{Divergence, FactKind, Location, RuleId, Severity};
use crate::parsers::MarkdownParser;
use regex::Regex;
use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;

/// MissingCoverageAnalyzer — enforces `missing_coverage` (heuristic, Notice).
///
/// Strategy: for every function-shaped inline code span in Markdown (`fn_name()`
/// or `Type::method()`) where the target function *exists* in the AST, look for
/// at least one occurrence of the name in a test-scope file. Test-scope is any
/// file under `tests/` or any `.rs` file containing a `#[test]` attribute.
///
/// The rule intentionally only fires for symbols that *do* exist — otherwise
/// `symbol_absence` would already have flagged them.
pub struct MissingCoverageAnalyzer;

impl Default for MissingCoverageAnalyzer {
    fn default() -> Self {
        Self
    }
}

impl DriftAnalyzer for MissingCoverageAnalyzer {
    fn id(&self) -> &'static str {
        "missing_coverage"
    }

    fn analyze(&self, ctx: &ProjectContext) -> Vec<Divergence> {
        let test_sources = load_test_sources(&ctx.rust_files);
        if test_sources.is_empty() {
            // No tests at all — `missing_coverage` would fire on every claim.
            // That's noise; prefer silence until the user has at least some tests.
            return Vec::new();
        }

        let mut out = Vec::new();
        let mut already_flagged: HashSet<(std::path::PathBuf, u32, String)> = HashSet::new();

        for md in &ctx.markdown_files {
            let Ok(claims) = MarkdownParser::parse(md) else {
                continue;
            };
            for claim in claims {
                let Some(leaf) = extract_callable_leaf(&claim.text) else {
                    continue;
                };

                // Only flag if the symbol actually exists in the codebase as a
                // function. Otherwise symbol_absence handles it.
                let exists_as_fn = ctx
                    .facts_named(&leaf)
                    .any(|f| matches!(f.kind, FactKind::Function));
                if !exists_as_fn {
                    continue;
                }

                if tests_reference(&test_sources, &leaf) {
                    continue;
                }

                let key = (claim.location.file.clone(), claim.location.line, leaf.clone());
                if !already_flagged.insert(key) {
                    continue;
                }

                out.push(Divergence {
                    rule: RuleId::MissingCoverage,
                    severity: Severity::Notice,
                    location: Location::new(
                        claim.location.file.clone(),
                        claim.location.line,
                    ),
                    stated: format!("`{leaf}` is a capability the project exposes"),
                    reality: format!("no test references `{leaf}` by name"),
                    risk: "Capability claimed in the docs has no guard-rail in tests.".to_string(),
                });
            }
        }

        out
    }
}

fn callable_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Matches `name()` or `Type::method()` (argument list optional), capturing
        // the leaf identifier.
        Regex::new(
            r"^(?:[A-Za-z_][A-Za-z0-9_]*::)*([A-Za-z_][A-Za-z0-9_]*)\([^)]*\)$",
        )
        .unwrap()
    })
}

fn extract_callable_leaf(span: &str) -> Option<String> {
    let caps = callable_re().captures(span.trim())?;
    Some(caps.get(1)?.as_str().to_string())
}

fn load_test_sources(rust_files: &[std::path::PathBuf]) -> Vec<String> {
    rust_files
        .iter()
        .filter(|p| is_test_file(p))
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .collect()
}

fn is_test_file(path: &Path) -> bool {
    // `tests/` integration tests.
    if path.components().any(|c| c.as_os_str() == "tests") {
        return true;
    }
    // Inline `#[cfg(test)]` / `#[test]` in any file.
    match std::fs::read_to_string(path) {
        Ok(src) => src.contains("#[test]") || src.contains("#[cfg(test)]"),
        Err(_) => false,
    }
}

fn tests_reference(sources: &[String], symbol: &str) -> bool {
    // Word-boundary match so `new` doesn't accidentally match `renew`.
    let pattern = format!(r"\b{}\b", regex::escape(symbol));
    let Ok(re) = Regex::new(&pattern) else {
        return false;
    };
    sources.iter().any(|s| re.is_match(s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::CodeFact;

    fn fact_fn(name: &str) -> CodeFact {
        CodeFact {
            location: Location::new("src/lib.rs", 1),
            kind: FactKind::Function,
            name: name.to_string(),
        }
    }

    #[test]
    fn flags_documented_fn_absent_from_tests() {
        let tmp = tempfile::tempdir().unwrap();
        let md = tmp.path().join("README.md");
        std::fs::write(&md, "Call `place_order()` to submit.\n").unwrap();

        let test_file = tmp.path().join("tests").join("api.rs");
        std::fs::create_dir_all(test_file.parent().unwrap()).unwrap();
        std::fs::write(&test_file, "#[test] fn t() { other_fn(); }\n").unwrap();

        let mut ctx = ProjectContext::new(tmp.path());
        ctx.markdown_files.push(md);
        ctx.rust_files.push(test_file);
        ctx.code_facts.push(fact_fn("place_order"));

        let divs = MissingCoverageAnalyzer.analyze(&ctx);
        assert_eq!(divs.len(), 1);
        assert_eq!(divs[0].rule, RuleId::MissingCoverage);
        assert_eq!(divs[0].severity, Severity::Notice);
    }

    #[test]
    fn passes_when_test_mentions_symbol() {
        let tmp = tempfile::tempdir().unwrap();
        let md = tmp.path().join("README.md");
        std::fs::write(&md, "Call `place_order()` to submit.\n").unwrap();

        let test_file = tmp.path().join("tests").join("api.rs");
        std::fs::create_dir_all(test_file.parent().unwrap()).unwrap();
        std::fs::write(
            &test_file,
            "#[test] fn t() { let _ = place_order(); }\n",
        )
        .unwrap();

        let mut ctx = ProjectContext::new(tmp.path());
        ctx.markdown_files.push(md);
        ctx.rust_files.push(test_file);
        ctx.code_facts.push(fact_fn("place_order"));

        assert!(MissingCoverageAnalyzer.analyze(&ctx).is_empty());
    }

    #[test]
    fn does_not_flag_missing_symbols() {
        // If the symbol doesn't exist at all, that's symbol_absence's job.
        let tmp = tempfile::tempdir().unwrap();
        let md = tmp.path().join("README.md");
        std::fs::write(&md, "Call `ghost_fn()` to vanish.\n").unwrap();

        let test_file = tmp.path().join("tests").join("api.rs");
        std::fs::create_dir_all(test_file.parent().unwrap()).unwrap();
        std::fs::write(&test_file, "#[test] fn t() {}\n").unwrap();

        let mut ctx = ProjectContext::new(tmp.path());
        ctx.markdown_files.push(md);
        ctx.rust_files.push(test_file);
        // No code fact — ghost_fn doesn't exist.

        assert!(MissingCoverageAnalyzer.analyze(&ctx).is_empty());
    }

    #[test]
    fn word_boundary_prevents_substring_false_positive() {
        // `new` must not match `renew`.
        let tmp = tempfile::tempdir().unwrap();
        let md = tmp.path().join("README.md");
        std::fs::write(&md, "Call `new()`.\n").unwrap();

        let test_file = tmp.path().join("tests").join("api.rs");
        std::fs::create_dir_all(test_file.parent().unwrap()).unwrap();
        std::fs::write(&test_file, "#[test] fn t() { renew(); }\n").unwrap();

        let mut ctx = ProjectContext::new(tmp.path());
        ctx.markdown_files.push(md);
        ctx.rust_files.push(test_file);
        ctx.code_facts.push(fact_fn("new"));

        let divs = MissingCoverageAnalyzer.analyze(&ctx);
        assert_eq!(divs.len(), 1);
    }

    #[test]
    fn silent_when_no_tests_exist() {
        let tmp = tempfile::tempdir().unwrap();
        let md = tmp.path().join("README.md");
        std::fs::write(&md, "Call `f()`.\n").unwrap();

        let mut ctx = ProjectContext::new(tmp.path());
        ctx.markdown_files.push(md);
        ctx.code_facts.push(fact_fn("f"));
        // No rust_files — project has no tests yet.

        assert!(MissingCoverageAnalyzer.analyze(&ctx).is_empty());
    }
}
