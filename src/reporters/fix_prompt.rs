use super::Reporter;
use crate::domain::Divergence;
use std::fmt::Write;

/// FixPromptReporter — emits a structured Markdown brief an LLM can consume to
/// propose concrete corrections for every divergence.
///
/// The prompt is deliberately literal: each divergence becomes its own task
/// block with file/line, the stated claim, the observed reality, and the risk.
/// It does not editorialize or suggest fixes itself — those are the AI's job.
pub struct FixPromptReporter;

impl Reporter for FixPromptReporter {
    fn render(&self, divergences: &[Divergence]) -> String {
        if divergences.is_empty() {
            return "# spec-drift correction prompt\n\nNo divergences detected. \
                Nothing to correct.\n"
                .to_string();
        }

        let mut out = String::new();
        writeln!(out, "# spec-drift correction prompt").unwrap();
        out.push('\n');
        writeln!(
            out,
            "You are maintaining a Rust project. `spec-drift` found {} divergence{} \
             between the project's specification surfaces (README, AGENTS.md, examples, CI) \
             and its code. For each item below, choose one of:",
            divergences.len(),
            if divergences.len() == 1 { "" } else { "s" }
        )
        .unwrap();
        writeln!(out, "1. Update the spec to match the code.").unwrap();
        writeln!(out, "2. Update the code to match the spec.").unwrap();
        writeln!(
            out,
            "3. Justify the divergence and silence the rule (inline or in `spec-drift.toml`)."
        )
        .unwrap();
        out.push('\n');
        writeln!(out, "Preserve formatting, do not invent facts, and keep edits minimal.")
            .unwrap();
        out.push('\n');

        for (i, d) in divergences.iter().enumerate() {
            writeln!(out, "## Task {} — `{}` ({})", i + 1, d.rule.as_str(), d.severity.glyph())
                .unwrap();
            writeln!(
                out,
                "- **Location:** `{}:{}`",
                d.location.file.display(),
                d.location.line
            )
            .unwrap();
            writeln!(out, "- **Stated:** {}", d.stated).unwrap();
            writeln!(out, "- **Reality:** {}", d.reality).unwrap();
            writeln!(out, "- **Risk:** {}", d.risk).unwrap();
            out.push('\n');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Location, RuleId, Severity};

    fn div() -> Divergence {
        Divergence {
            rule: RuleId::SymbolAbsence,
            severity: Severity::Critical,
            location: Location::new("README.md", 42),
            stated: "`Client::new` exists".into(),
            reality: "no `new` found in sources".into(),
            risk: "docs lie".into(),
        }
    }

    #[test]
    fn empty_input_returns_short_noop_brief() {
        let out = FixPromptReporter.render(&[]);
        assert!(out.contains("No divergences"));
    }

    #[test]
    fn renders_task_block_per_divergence() {
        let out = FixPromptReporter.render(&[div(), div()]);
        assert!(out.contains("## Task 1"));
        assert!(out.contains("## Task 2"));
        assert!(out.contains("`README.md:42`"));
        assert!(out.contains("symbol_absence"));
    }

    #[test]
    fn mentions_the_three_resolution_options() {
        let out = FixPromptReporter.render(&[div()]);
        assert!(out.contains("Update the spec"));
        assert!(out.contains("Update the code"));
        assert!(out.contains("silence the rule"));
    }
}
