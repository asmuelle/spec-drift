use super::DriftAnalyzer;
use crate::config::ConstraintRule;
use crate::context::ProjectContext;
use crate::domain::{Divergence, Location, RuleId, Severity};
use quote::ToTokens;
use std::path::Path;

/// ConstraintAnalyzer — enforces `constraint_violation` (heuristic, Warning).
///
/// Strategy: the user declares invariants in `spec-drift.toml` as
/// `[[rules.constraint_violation]]` entries. The only shape supported today
/// is a return-type constraint:
///
/// ```toml
/// [[rules.constraint_violation]]
/// name = "handlers_return_api_result"
/// glob = "src/handlers/**"
/// return_type = "Result<_, ApiError>"
/// ```
///
/// For every `fn` under `glob`, the analyzer compares the function's return
/// type to `return_type`. Matching is syntactic: whitespace is normalized and
/// `_` in the pattern matches *any* single tokenized segment. This keeps the
/// rule deterministic without requiring full type resolution.
pub struct ConstraintAnalyzer {
    rules: Vec<ConstraintRule>,
}

impl ConstraintAnalyzer {
    pub fn new(rules: Vec<ConstraintRule>) -> Self {
        Self { rules }
    }
}

impl DriftAnalyzer for ConstraintAnalyzer {
    fn id(&self) -> &'static str {
        "constraint_violation"
    }

    fn analyze(&self, ctx: &ProjectContext) -> Vec<Divergence> {
        if self.rules.is_empty() {
            return Vec::new();
        }

        let mut out = Vec::new();
        for rule in &self.rules {
            for rs in &ctx.rust_files {
                let rel = ctx.rel(rs);
                if !rule.glob.is_match(rel) {
                    continue;
                }
                check_file(rs, rule, &mut out);
            }
        }
        out
    }
}

fn check_file(path: &Path, rule: &ConstraintRule, out: &mut Vec<Divergence>) {
    let Ok(source) = std::fs::read_to_string(path) else {
        return;
    };
    let Ok(file) = syn::parse_file(&source) else {
        return;
    };

    for item in &file.items {
        inspect_item(item, path, rule, out);
    }
}

fn inspect_item(item: &syn::Item, path: &Path, rule: &ConstraintRule, out: &mut Vec<Divergence>) {
    match item {
        syn::Item::Fn(f) => {
            compare_return_type(
                &f.sig,
                &f.sig.ident.to_string(),
                path,
                rule,
                out,
            );
        }
        syn::Item::Impl(i) => {
            for impl_item in &i.items {
                if let syn::ImplItem::Fn(f) = impl_item {
                    compare_return_type(
                        &f.sig,
                        &f.sig.ident.to_string(),
                        path,
                        rule,
                        out,
                    );
                }
            }
        }
        syn::Item::Mod(m) => {
            if let Some((_, items)) = &m.content {
                for item in items {
                    inspect_item(item, path, rule, out);
                }
            }
        }
        _ => {}
    }
}

fn compare_return_type(
    sig: &syn::Signature,
    fn_name: &str,
    path: &Path,
    rule: &ConstraintRule,
    out: &mut Vec<Divergence>,
) {
    let Some(expected) = &rule.return_type else {
        return;
    };

    let actual = match &sig.output {
        syn::ReturnType::Default => "()".to_string(),
        syn::ReturnType::Type(_, ty) => ty.to_token_stream().to_string(),
    };

    if matches_with_wildcard(expected, &actual) {
        return;
    }

    let line = ident_line(&sig.ident);
    out.push(Divergence {
        rule: RuleId::ConstraintViolation,
        severity: Severity::Warning,
        location: Location::new(path, line),
        stated: format!(
            "constraint `{}`: `{}` should return `{}`",
            rule.name, fn_name, expected
        ),
        reality: format!(
            "`{}` returns `{}`",
            fn_name,
            normalize(&actual)
        ),
        risk: "Functions under this glob have drifted from the stated contract.".to_string(),
    });
}

fn ident_line(ident: &syn::Ident) -> u32 {
    ident.span().start().line as u32
}

/// Compare two type strings after whitespace normalization, treating `_` in
/// the expected pattern as a match-any single-segment wildcard.
fn matches_with_wildcard(expected: &str, actual: &str) -> bool {
    let e = normalize(expected);
    let a = normalize(actual);
    if e == a {
        return true;
    }
    // Fast path: no wildcard to worry about.
    if !e.contains('_') {
        return false;
    }
    wildcard_eq(&e, &a)
}

fn normalize(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Compare two normalized type strings where `_` in `pattern` matches a single
/// top-level type argument. Nested angle brackets and parens are tracked so
/// `Result<_, ApiError>` matches `Result<Vec<User>, ApiError>`.
fn wildcard_eq(pattern: &str, actual: &str) -> bool {
    let mut p_idx = 0;
    let mut a_idx = 0;
    let p = pattern.as_bytes();
    let a = actual.as_bytes();

    while p_idx < p.len() && a_idx < a.len() {
        if p[p_idx] == b'_' {
            // Match any non-empty segment that ends at a top-level `,`, `>` or
            // `)`. A "top-level" boundary is one where our depth counters are
            // back at zero relative to the start of this wildcard match.
            let mut depth_angle: i32 = 0;
            let mut depth_paren: i32 = 0;
            let mut matched = 0;
            while a_idx + matched < a.len() {
                let c = a[a_idx + matched];
                if depth_angle == 0
                    && depth_paren == 0
                    && (c == b',' || c == b'>' || c == b')')
                {
                    break;
                }
                match c {
                    b'<' => depth_angle += 1,
                    b'>' => depth_angle -= 1,
                    b'(' => depth_paren += 1,
                    b')' => depth_paren -= 1,
                    _ => {}
                }
                matched += 1;
            }
            if matched == 0 {
                return false;
            }
            a_idx += matched;
            p_idx += 1;
        } else if p[p_idx] == a[a_idx] {
            p_idx += 1;
            a_idx += 1;
        } else {
            return false;
        }
    }

    p_idx == p.len() && a_idx == a.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use globset::Glob;

    fn rule_for(glob: &str, return_type: &str) -> ConstraintRule {
        ConstraintRule {
            name: "test_rule".to_string(),
            glob: Glob::new(glob).unwrap().compile_matcher(),
            return_type: Some(return_type.to_string()),
        }
    }

    fn run(src: &str, rel_path: &str, rule: ConstraintRule) -> Vec<Divergence> {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(rel_path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, src).unwrap();

        let mut ctx = ProjectContext::new(tmp.path());
        ctx.rust_files.push(path);

        ConstraintAnalyzer::new(vec![rule]).analyze(&ctx)
    }

    #[test]
    fn flags_wrong_return_type() {
        let src = r#"
            pub fn list_users() -> Option<Vec<User>> { None }
        "#;
        let rule = rule_for("src/handlers/**", "Result<_, ApiError>");
        let divs = run(src, "src/handlers/users.rs", rule);
        assert_eq!(divs.len(), 1);
        assert_eq!(divs[0].rule, RuleId::ConstraintViolation);
        assert_eq!(divs[0].severity, Severity::Warning);
    }

    #[test]
    fn accepts_matching_return_type_with_wildcard() {
        let src = r#"
            pub fn list_users() -> Result<Vec<User>, ApiError> { todo!() }
        "#;
        let rule = rule_for("src/handlers/**", "Result<_, ApiError>");
        let divs = run(src, "src/handlers/users.rs", rule);
        assert!(divs.is_empty());
    }

    #[test]
    fn ignores_files_outside_glob() {
        let src = r#"
            pub fn helper() -> i32 { 1 }
        "#;
        let rule = rule_for("src/handlers/**", "Result<_, ApiError>");
        let divs = run(src, "src/utils/math.rs", rule);
        assert!(divs.is_empty());
    }

    #[test]
    fn checks_impl_methods() {
        let src = r#"
            pub struct Foo;
            impl Foo {
                pub fn not_a_result(&self) -> bool { true }
            }
        "#;
        let rule = rule_for("src/handlers/**", "Result<_, ApiError>");
        let divs = run(src, "src/handlers/foo.rs", rule);
        assert_eq!(divs.len(), 1);
    }

    #[test]
    fn wildcard_matches_any_single_segment() {
        assert!(matches_with_wildcard(
            "Result<_, ApiError>",
            "Result<Vec<User>, ApiError>"
        ));
        assert!(matches_with_wildcard(
            "Result<_, _>",
            "Result<User, MyError>"
        ));
        assert!(!matches_with_wildcard(
            "Result<_, ApiError>",
            "Result<User, DbError>"
        ));
    }
}
