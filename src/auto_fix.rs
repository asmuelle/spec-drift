//! Auto-fix suggestions for deterministic drift rules.
//!
//! Some rules are mechanically fixable:
//! - `symbol_absence`: doc references a renamed/missing symbol → suggest the new name
//! - `ghost_command`: CI references a deleted crate/binary → suggest removal
//! - `compile_failure`: example doesn't compile → surface the rustc diagnostic
//!
//! This module produces `AutoFix` structs that either auto-apply
//! (with `--fix`) or are surfaced as suggestions in the report.

use crate::domain::{Divergence, RuleId};
use std::path::Path;

/// A suggested fix for a divergence.
#[derive(Debug, Clone)]
pub struct AutoFix {
    /// File to modify
    pub file: std::path::PathBuf,
    /// Line number where the fix applies
    pub line: u32,
    /// Description of what to change
    pub description: String,
    /// The old text to replace (if a simple substitution)
    pub old_text: Option<String>,
    /// The replacement text
    pub new_text: Option<String>,
    /// Whether this fix can be auto-applied safely
    pub auto_applicable: bool,
}

/// Try to generate an auto-fix for a divergence.
///
/// Only deterministic rules with mechanical fixes are supported.
pub fn suggest_fix(divergence: &Divergence, workspace_root: &Path) -> Option<AutoFix> {
    match divergence.rule {
        RuleId::SymbolAbsence => suggest_symbol_fix(divergence, workspace_root),
        RuleId::GhostCommand => suggest_ghost_command_fix(divergence),
        RuleId::CompileFailure => suggest_compile_fix(divergence),
        _ => None,
    }
}

fn suggest_symbol_fix(divergence: &Divergence, workspace_root: &Path) -> Option<AutoFix> {
    // The `stated` field contains the old name claimed in docs.
    // The `reality` field explains it doesn't exist.
    // Try to find a similar name in the codebase.
    let old_name = divergence.stated.trim();
    if old_name.is_empty() {
        return None;
    }

    // Search for similar symbols (simple fuzzy match)
    let candidates = find_similar_symbols(workspace_root, old_name);
    let replacement = candidates.first()?;

    Some(AutoFix {
        file: divergence.location.file.clone(),
        line: divergence.location.line,
        description: format!(
            "Replace `{}` with `{}` in documentation",
            old_name, replacement
        ),
        old_text: Some(old_name.to_string()),
        new_text: Some(replacement.clone()),
        auto_applicable: candidates.len() == 1,
    })
}

fn suggest_ghost_command_fix(divergence: &Divergence) -> Option<AutoFix> {
    // stated: "cargo --package old-crate" or "cargo --bin old-bin"
    // reality: "package/bin not found in workspace"
    let stated = &divergence.stated;

    // Extract the package/bin name from the command
    let is_package = stated.contains("--package");
    let prefix = if is_package { "--package" } else { "--bin" };

    // Find what comes after --package or --bin
    let cmd = stated.split_whitespace()
        .skip_while(|w| *w != prefix)
        .nth(1)?;

    Some(AutoFix {
        file: divergence.location.file.clone(),
        line: divergence.location.line,
        description: format!(
            "Remove reference to {prefix} `{cmd}` which no longer exists in the workspace"
        ),
        old_text: Some(format!("{prefix} {cmd}")),
        new_text: None, // Can't auto-determine replacement
        auto_applicable: false,
    })
}

fn suggest_compile_fix(divergence: &Divergence) -> Option<AutoFix> {
    // Reality contains the rustc error message
    let reality = &divergence.reality;
    if reality.is_empty() {
        return None;
    }

    Some(AutoFix {
        file: divergence.location.file.clone(),
        line: divergence.location.line,
        description: format!("Fix compilation error: {}", reality),
        old_text: None,
        new_text: None,
        auto_applicable: false,
    })
}

/// Find Rust symbols in the workspace that are similar to `name`.
fn find_similar_symbols(root: &Path, name: &str) -> Vec<String> {
    use std::collections::BTreeSet;
    let mut candidates = BTreeSet::new();
    let lower = name.to_lowercase();

    // Walk workspace .rs files looking for similar function/struct/enum names
    if let Ok(entries) = std::fs::read_dir(root.join("src")) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            if let Ok(contents) = std::fs::read_to_string(&path) {
                for token in tokenize_rust(&contents) {
                    if token == name {
                        continue; // Skip exact match (it's the old name)
                    }
                    if similarity(&token.to_lowercase(), &lower) > 0.6 {
                        candidates.insert(token);
                    }
                }
            }
        }
    }

    candidates.into_iter().collect()
}

/// Simple tokenizer for Rust identifiers.
fn tokenize_rust(src: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in src.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            current.push(ch);
        } else {
            if !current.is_empty() && current.chars().next().map_or(false, |c| c.is_alphabetic() || c == '_') {
                tokens.push(current.clone());
            }
            current.clear();
        }
    }
    if !current.is_empty() && current.chars().next().map_or(false, |c| c.is_alphabetic() || c == '_') {
        tokens.push(current);
    }
    tokens
}

/// Simple string similarity based on common prefix/suffix length.
fn similarity(a: &str, b: &str) -> f64 {
    let common_prefix = a.chars().zip(b.chars()).take_while(|(a, b)| a == b).count();
    let max_len = a.len().max(b.len());
    if max_len == 0 { return 0.0; }
    common_prefix as f64 / max_len as f64
}

/// Auto-fix runner: apply all auto-applicable fixes.
///
/// Returns the number of fixes applied. Prints changes to stderr.
pub fn apply_fixes(divergences: &[Divergence], workspace_root: &Path) -> u32 {
    let mut applied = 0u32;
    for d in divergences {
        if let Some(fix) = suggest_fix(d, workspace_root) {
            if !fix.auto_applicable {
                continue;
            }
            if let (Some(old), Some(new)) = (&fix.old_text, &fix.new_text) {
                if apply_text_fix(&fix.file, fix.line, old, new) {
                    eprintln!("  Fixed: {} (line {}) — {}", fix.file.display(), fix.line, fix.description);
                    applied += 1;
                }
            }
        }
    }
    applied
}

fn apply_text_fix(file: &Path, line: u32, old: &str, new: &str) -> bool {
    let Ok(contents) = std::fs::read_to_string(file) else { return false };
    let lines: Vec<&str> = contents.lines().collect();
    let idx = (line as usize).saturating_sub(1);
    if idx >= lines.len() {
        return false;
    }
    let target = lines[idx];
    if let Some(pos) = target.find(old) {
        let mut new_line = target.to_string();
        new_line.replace_range(pos..pos + old.len(), new);
        let mut new_lines = lines.clone();
        new_lines[idx] = &new_line;
        // This won't work as-is due to borrow checking — need owned strings
        let new_contents: Vec<String> = lines.iter().enumerate().map(|(i, l)| {
            if i == idx {
                let mut s = l.to_string();
                if let Some(p) = s.find(old) {
                    s.replace_range(p..p + old.len(), new);
                }
                s
            } else {
                l.to_string()
            }
        }).collect();
        let output = new_contents.join("\n");
        // Preserve trailing newline
        let output = if contents.ends_with('\n') {
            output + "\n"
        } else {
            output
        };
        std::fs::write(file, output).is_ok()
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn similarity_scores() {
        assert!(similarity("connect_to_db", "connect_to_db") > 0.9);
        assert!(similarity("init_connection", "connect_to_db") < 0.5);
        assert!(similarity("login", "log_in") > 0.5);
    }

    #[test]
    fn tokenize_rust_extracts_identifiers() {
        let tokens = tokenize_rust("pub fn connect_to_db() -> Result<()> { let x = 1; }");
        assert!(tokens.contains(&"connect_to_db".to_string()));
        assert!(tokens.contains(&"Result".to_string()));
    }

    #[test]
    fn ghost_command_fix_extracts_package_name() {
        let d = Divergence {
            rule: RuleId::GhostCommand,
            severity: crate::domain::Severity::Warning,
            location: crate::domain::Location::new(".github/workflows/ci.yml", 10),
            stated: "cargo test --package old-crate --all-features".into(),
            reality: "package 'old-crate' not found in workspace".into(),
            risk: "CI jobs may silently skip or fail".into(),
            attribution: None,
        };
        let fix = suggest_ghost_command_fix(&d).unwrap();
        assert!(fix.description.contains("old-crate"));
    }
}
