//! Inline-ignore filtering.
//!
//! A divergence is suppressed when the source file contains, on or one line
//! above the reported line, either:
//!
//! - `spec-drift: ignore-rule <rule-id>` (works in any comment syntax), or
//! - `#[allow(spec_drift::<rule-id>)]` (Rust-idiomatic form).
//!
//! "Silence must be specific and auditable" — markers without a rule id are
//! ignored.

use crate::domain::{Divergence, RuleId};
use std::collections::HashMap;
use std::path::PathBuf;

/// Drop divergences that have an inline ignore marker naming their rule.
pub fn apply_inline_ignores(divs: Vec<Divergence>) -> Vec<Divergence> {
    let mut cache: HashMap<PathBuf, Vec<String>> = HashMap::new();
    divs.into_iter()
        .filter(|d| !is_suppressed(d, &mut cache))
        .collect()
}

fn is_suppressed(d: &Divergence, cache: &mut HashMap<PathBuf, Vec<String>>) -> bool {
    let lines = cache
        .entry(d.location.file.clone())
        .or_insert_with(|| load_lines(&d.location.file));

    let target = d.location.line as usize;
    if target == 0 {
        return false;
    }
    // Check the reported line and up to three lines above. Attributes
    // (`#[allow]`, `#[test]`, ...) often stack several lines above the fn
    // signature, and HTML comments / `//` comments sit on the line above the
    // claim they annotate.
    // Vec is 0-indexed; Location::line is 1-indexed, so reported line is at
    // Vec index (line - 1). We also look at the 3 preceding lines.
    let candidate_offsets = [1usize, 2, 3, 4];
    candidate_offsets
        .iter()
        .filter_map(|off| target.checked_sub(*off).and_then(|i| lines.get(i)))
        .any(|line| line_silences_rule(line, d.rule))
}

fn load_lines(path: &std::path::Path) -> Vec<String> {
    match std::fs::read_to_string(path) {
        Ok(s) => s.lines().map(|l| l.to_string()).collect(),
        Err(_) => Vec::new(),
    }
}

fn line_silences_rule(line: &str, rule: RuleId) -> bool {
    let id = rule.as_str();
    // Generic marker — works inside `//`, `#`, `<!-- -->`, etc.
    let generic = format!("spec-drift: ignore-rule {id}");
    if line.contains(&generic) {
        return true;
    }
    // Rust-idiomatic: `#[allow(spec_drift::<rule>)]`
    let rust_form = format!("spec_drift::{id}");
    line.contains("#[allow(") && line.contains(&rust_form)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Location, Severity};

    fn div(path: PathBuf, line: u32) -> Divergence {
        Divergence {
            rule: RuleId::SymbolAbsence,
            severity: Severity::Critical,
            location: Location::new(path, line),
            stated: "x".into(),
            reality: "y".into(),
            risk: "z".into(),
            attribution: None,
        }
    }

    #[test]
    fn generic_marker_on_previous_line_suppresses() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("README.md");
        std::fs::write(
            &f,
            "<!-- spec-drift: ignore-rule symbol_absence -->\nUse `NoSuchThing()` to start.\n",
        )
        .unwrap();

        let out = apply_inline_ignores(vec![div(f, 2)]);
        assert!(out.is_empty());
    }

    #[test]
    fn rust_allow_form_suppresses_lying_test() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("lib.rs");
        std::fs::write(
            &f,
            "#[allow(spec_drift::lying_test)]\n#[test]\nfn rejects_nothing() {}\n",
        )
        .unwrap();

        let d = Divergence {
            rule: RuleId::LyingTest,
            ..div(f, 3)
        };
        assert!(apply_inline_ignores(vec![d]).is_empty());
    }

    #[test]
    fn marker_for_different_rule_does_not_suppress() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("x.md");
        std::fs::write(
            &f,
            "<!-- spec-drift: ignore-rule ghost_command -->\n`X()` missing.\n",
        )
        .unwrap();
        let d = div(f, 2);
        assert_eq!(apply_inline_ignores(vec![d]).len(), 1);
    }

    #[test]
    fn unannotated_file_keeps_divergence() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("plain.md");
        std::fs::write(&f, "nothing to see\n").unwrap();
        assert_eq!(apply_inline_ignores(vec![div(f, 1)]).len(), 1);
    }

    #[test]
    fn missing_file_is_not_an_error() {
        let out = apply_inline_ignores(vec![div(PathBuf::from("/nope/nope.md"), 1)]);
        // Falls back to "not suppressed" when the file can't be read.
        assert_eq!(out.len(), 1);
    }
}
