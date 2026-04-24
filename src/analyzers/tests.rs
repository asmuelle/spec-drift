use super::DriftAnalyzer;
use crate::context::ProjectContext;
use crate::domain::{Divergence, Location, RuleId, Severity};
use crate::error::SpecDriftError;
use std::path::Path;

/// TestsAnalyzer — enforces the `lying_test` rule (heuristic).
///
/// A test that *names* a negative assertion (contains "cannot", "rejects",
/// "forbidden", "returns_error", "fails", ...) but has a body that only makes
/// positive assertions (plain `assert!(x)` on an `is_ok()`-ish expression, or
/// no assertions at all) is almost certainly lying. The heuristic is
/// intentionally conservative and only fires when intent and assertion
/// plainly disagree.
#[derive(Default)]
pub struct TestsAnalyzer;

impl DriftAnalyzer for TestsAnalyzer {
    fn analyze(&self, ctx: &ProjectContext) -> Vec<Divergence> {
        let mut out = Vec::new();
        for rs in &ctx.rust_files {
            match scan_file(rs) {
                Ok(mut divs) => out.append(&mut divs),
                Err(e) => eprintln!("spec-drift: skipping {} in tests pillar: {e}", rs.display()),
            }
        }
        out
    }
}

fn scan_file(path: &Path) -> Result<Vec<Divergence>, SpecDriftError> {
    let source = std::fs::read_to_string(path).map_err(|e| SpecDriftError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let file = syn::parse_file(&source).map_err(|e| SpecDriftError::RustParse {
        path: path.to_path_buf(),
        source: e,
    })?;

    let mut out = Vec::new();
    visit_items(&file.items, path, &mut out);
    Ok(out)
}

fn visit_items(items: &[syn::Item], path: &Path, out: &mut Vec<Divergence>) {
    for item in items {
        match item {
            syn::Item::Fn(f) if is_test_fn(&f.attrs) => inspect_test(f, path, out),
            syn::Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    visit_items(inner, path, out);
                }
            }
            _ => {}
        }
    }
}

fn is_test_fn(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        let p = a.path();
        p.is_ident("test") || p.segments.last().is_some_and(|s| s.ident == "test")
    })
}

fn inspect_test(f: &syn::ItemFn, path: &Path, out: &mut Vec<Divergence>) {
    let name = f.sig.ident.to_string();
    if !intent_is_negative(&name) {
        return;
    }

    let (count, has_negative) = scan_assertions(&f.block.stmts);

    // A negatively-named test must either assert nothing (empty test) or
    // contain at least one negative assertion. If it has only positive
    // assertions, the name and the check disagree → drift.
    if count == 0 || !has_negative {
        let line = f.sig.ident.span().start().line as u32;
        let reason = if count == 0 {
            "has no assertions"
        } else {
            "only contains positive assertions"
        };
        out.push(Divergence {
            rule: RuleId::LyingTest,
            severity: Severity::Critical,
            location: Location::new(path, line),
            stated: format!("`{name}` verifies a negative / failure path"),
            reality: format!("`{name}` {reason}"),
            risk: "The test is green but proves nothing about the stated contract.".to_string(),
            attribution: None,
        });
    }
}

fn intent_is_negative(name: &str) -> bool {
    let n = name.to_ascii_lowercase();

    // Tests that describe *detecting* a drift / violation / error are not
    // negative-path tests of the subject — they are positive tests of the
    // detector. `flags_missing_symbol` means "the analyzer flags a missing
    // symbol", not "the subject is missing its symbol". Skip these.
    const DETECTION_PREFIXES: &[&str] = &[
        "flags_",
        "detects_",
        "finds_",
        "reports_",
        "surfaces_",
        "identifies_",
        "catches_",
        "warns_",
        "does_not_",
        "doesnt_",
    ];
    if DETECTION_PREFIXES.iter().any(|p| n.starts_with(p)) {
        return false;
    }

    // Note: "missing" is intentionally omitted — too ambiguous in names like
    // `missing_file_is_not_an_error` where it describes the input, not the
    // expected outcome. `rejects_missing_field` is still caught via "rejects".
    const NEGATIVE_HINTS: &[&str] = &[
        "cannot",
        "rejects",
        "forbidden",
        "unauthorized",
        "denied",
        "returns_error",
        "returns_err",
        "fails",
        "invalid",
        "not_allowed",
        "panics",
    ];
    NEGATIVE_HINTS.iter().any(|h| n.contains(h))
}

fn scan_assertions(stmts: &[syn::Stmt]) -> (usize, bool) {
    let mut count = 0usize;
    let mut has_neg = false;
    for stmt in stmts {
        walk_stmt(stmt, &mut count, &mut has_neg);
    }
    (count, has_neg)
}

fn walk_stmt(stmt: &syn::Stmt, count: &mut usize, has_neg: &mut bool) {
    match stmt {
        syn::Stmt::Expr(e, _) => walk_expr(e, count, has_neg),
        syn::Stmt::Local(local) => {
            if let Some(init) = &local.init {
                walk_expr(&init.expr, count, has_neg);
            }
        }
        syn::Stmt::Macro(m) => {
            handle_macro(&m.mac, count, has_neg);
        }
        _ => {}
    }
}

fn walk_expr(expr: &syn::Expr, count: &mut usize, has_neg: &mut bool) {
    match expr {
        syn::Expr::Macro(m) => handle_macro(&m.mac, count, has_neg),
        syn::Expr::Block(b) => {
            for s in &b.block.stmts {
                walk_stmt(s, count, has_neg);
            }
        }
        syn::Expr::If(i) => {
            for s in &i.then_branch.stmts {
                walk_stmt(s, count, has_neg);
            }
            if let Some((_, else_expr)) = &i.else_branch {
                walk_expr(else_expr, count, has_neg);
            }
        }
        syn::Expr::Match(m) => {
            for arm in &m.arms {
                walk_expr(&arm.body, count, has_neg);
            }
        }
        _ => {}
    }
}

fn handle_macro(mac: &syn::Macro, count: &mut usize, has_neg: &mut bool) {
    let name = mac.path.segments.last().map(|s| s.ident.to_string());
    let Some(name) = name else { return };

    let is_assertion = matches!(
        name.as_str(),
        "assert"
            | "assert_eq"
            | "assert_ne"
            | "debug_assert"
            | "debug_assert_eq"
            | "debug_assert_ne"
            | "panic"
    );
    if !is_assertion {
        return;
    }

    *count += 1;
    let tokens = mac.tokens.to_string();
    if assertion_looks_negative(&name, &tokens) {
        *has_neg = true;
    }
}

fn assertion_looks_negative(name: &str, tokens: &str) -> bool {
    if name == "assert_ne" {
        return true;
    }
    if name == "panic" {
        return true;
    }
    // `syn` emits `TokenStream::to_string()` with spaces around punctuation
    // ("res . is_none ()"), so naive substring checks miss real negative
    // assertions. Normalize by stripping whitespace and lowercasing before
    // matching.
    let t: String = tokens
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    t.starts_with('!')
        || t.contains(".is_err")
        || t.contains(".is_none")
        || t.contains("matches!(")
        || t.contains("err(")
        || t.contains(".is_not_")
        || t.contains("403")
        || t.contains("401")
        || t.contains("404")
}

#[cfg(test)]
mod tests_inner {
    use super::*;

    fn run_on_source(src: &str) -> Vec<Divergence> {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("lib.rs");
        std::fs::write(&path, src).unwrap();

        let mut ctx = ProjectContext::new(tmp.path());
        ctx.rust_files.push(path);
        TestsAnalyzer.analyze(&ctx)
    }

    #[test]
    fn flags_negative_name_with_only_positive_assertion() {
        let src = r#"
            #[test]
            fn user_cannot_access_admin_panel() {
                assert!(true);
            }
        "#;
        let divs = run_on_source(src);
        assert_eq!(divs.len(), 1);
        assert_eq!(divs[0].rule, RuleId::LyingTest);
    }

    #[test]
    fn flags_negative_name_with_no_assertions() {
        let src = r#"
            #[test]
            fn rejects_invalid_payload() {
                let _ = 1;
            }
        "#;
        let divs = run_on_source(src);
        assert_eq!(divs.len(), 1);
    }

    #[test]
    fn passes_negative_name_with_is_err_assertion() {
        let src = r#"
            #[test]
            fn rejects_invalid_payload() {
                let res: Result<(), ()> = Err(());
                assert!(res.is_err());
            }
        "#;
        let divs = run_on_source(src);
        assert!(divs.is_empty());
    }

    #[test]
    fn ignores_positive_tests() {
        let src = r#"
            #[test]
            fn builds_a_widget() {
                assert!(true);
            }
        "#;
        let divs = run_on_source(src);
        assert!(divs.is_empty());
    }

    #[test]
    fn exempts_detection_prefixed_test_names() {
        // `flags_`, `detects_`, etc. describe the analyzer's job, not the
        // subject's failure. They must not be treated as negative-path tests.
        let src = r#"
            #[test]
            fn flags_missing_symbol_in_markdown() {
                let found = 1;
                assert_eq!(found, 1);
            }
        "#;
        let divs = run_on_source(src);
        assert!(divs.is_empty());
    }

    #[test]
    fn passes_negative_name_with_status_code_check() {
        let src = r#"
            #[test]
            fn user_cannot_access_admin_panel() {
                assert_eq!(status, 403);
            }
        "#;
        let divs = run_on_source(src);
        assert!(divs.is_empty());
    }
}
