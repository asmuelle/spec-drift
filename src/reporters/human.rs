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
            out.push('\n');
        }
        out
    }
}
