use super::Reporter;
use crate::domain::Divergence;
use std::fmt::Write;

pub struct HumanReporter;

impl Reporter for HumanReporter {
    fn render(&self, divergences: &[Divergence]) -> String {
        if divergences.is_empty() {
            return "✅ SPEC DRIFT REPORT: no divergences found.\n".to_string();
        }

        let mut out = String::new();
        let n = divergences.len();
        writeln!(
            out,
            "📉 SPEC DRIFT REPORT: [{n} Divergence{} Found]",
            if n == 1 { "" } else { "s" },
        )
        .unwrap();
        out.push('\n');

        for d in divergences {
            writeln!(out, "{}: {}", d.severity.glyph(), d.rule.as_str()).unwrap();
            writeln!(
                out,
                "- File: {} (Line {})",
                d.location.file.display(),
                d.location.line
            )
            .unwrap();
            writeln!(out, "- Stated: {}", d.stated).unwrap();
            writeln!(out, "- Reality: {}", d.reality).unwrap();
            writeln!(out, "- Risk: {}", d.risk).unwrap();
            if let Some(a) = &d.attribution {
                writeln!(
                    out,
                    "- Blame: {} {} ({}): {}",
                    a.commit, a.author, a.date, a.summary
                )
                .unwrap();
            }
            out.push('\n');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Attribution, Location, RuleId, Severity};

    fn div_with(attr: Option<Attribution>) -> Divergence {
        Divergence {
            rule: RuleId::SymbolAbsence,
            severity: Severity::Critical,
            location: Location::new("README.md", 42),
            stated: "x".into(),
            reality: "y".into(),
            risk: "z".into(),
            attribution: attr,
        }
    }

    #[test]
    fn renders_blame_line_when_attribution_present() {
        let out = HumanReporter.render(&[div_with(Some(Attribution {
            commit: "abc1234".into(),
            author: "Ada".into(),
            date: "2024-01-02".into(),
            summary: "Write README".into(),
        }))]);
        assert!(out.contains("- Blame: abc1234 Ada (2024-01-02): Write README"));
    }

    #[test]
    fn omits_blame_line_when_attribution_missing() {
        let out = HumanReporter.render(&[div_with(None)]);
        assert!(!out.contains("- Blame:"));
    }
}
