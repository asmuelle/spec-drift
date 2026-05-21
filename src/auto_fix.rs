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
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
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
    // The `stated` field renders the old name claimed in docs, e.g.
    // "`Client::new` exists in the codebase".
    // The `reality` field explains it doesn't exist.
    // Try to find a similar name in the codebase.
    let old_name = extract_stated_symbol(&divergence.stated)?;
    if old_name.is_empty() {
        return None;
    }

    // Search for similar symbols (simple fuzzy match)
    let candidates = find_similar_symbols(workspace_root, &old_name);
    let replacement = candidates.first()?;

    Some(AutoFix {
        file: divergence.location.file.clone(),
        line: divergence.location.line,
        description: format!(
            "Replace `{}` with `{}` in documentation",
            old_name, replacement
        ),
        old_text: Some(old_name),
        new_text: Some(replacement.clone()),
        auto_applicable: candidates.len() == 1,
    })
}

fn extract_stated_symbol(stated: &str) -> Option<String> {
    let raw = stated
        .split_once('`')
        .and_then(|(_, rest)| rest.split_once('`').map(|(symbol, _)| symbol))
        .unwrap_or(stated)
        .trim();
    let symbol = raw.strip_suffix("()").unwrap_or(raw).trim();
    (!symbol.is_empty()).then(|| symbol.to_string())
}

fn suggest_ghost_command_fix(divergence: &Divergence) -> Option<AutoFix> {
    // stated: "cargo --package old-crate" or "cargo --bin old-bin"
    // reality: "package/bin not found in workspace"
    let stated = &divergence.stated;

    // Extract the package/bin name from the command
    let is_package = stated.contains("--package");
    let prefix = if is_package { "--package" } else { "--bin" };

    // Find what comes after --package or --bin
    let cmd = stated
        .split_whitespace()
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
            if !current.is_empty()
                && current
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphabetic() || c == '_')
            {
                tokens.push(current.clone());
            }
            current.clear();
        }
    }
    if !current.is_empty()
        && current
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
    {
        tokens.push(current);
    }
    tokens
}

/// Identifier similarity after normalizing separators.
fn similarity(a: &str, b: &str) -> f64 {
    let a = normalize_identifier(a);
    let b = normalize_identifier(b);
    let max_len = a.chars().count().max(b.chars().count());
    if max_len == 0 {
        return 0.0;
    }
    if a == b {
        return 1.0;
    }
    let distance = levenshtein(&a, &b);
    let edit_score = 1.0 - (distance as f64 / max_len as f64);
    let common_prefix = a.chars().zip(b.chars()).take_while(|(a, b)| a == b).count();
    let prefix_score = common_prefix as f64 / max_len as f64;
    edit_score * prefix_score
}

fn normalize_identifier(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0; b.len() + 1];

    for (i, ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (curr[j] + 1).min(prev[j + 1] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[b.len()]
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
            if let (Some(old), Some(new)) = (&fix.old_text, &fix.new_text)
                && apply_text_fix(workspace_root, &fix.file, fix.line, old, new)
            {
                eprintln!(
                    "  Fixed: {} (line {}) — {}",
                    fix.file.display(),
                    fix.line,
                    fix.description
                );
                applied += 1;
            }
        }
    }
    applied
}

fn apply_text_fix(workspace_root: &Path, file: &Path, line: u32, old: &str, new: &str) -> bool {
    let path = resolve_fix_path(workspace_root, file);
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return false;
    };
    let mut lines: Vec<String> = contents.lines().map(str::to_string).collect();
    let idx = (line as usize).saturating_sub(1);
    if idx >= lines.len() {
        return false;
    }
    let Some(pos) = lines[idx].find(old) else {
        return false;
    };
    lines[idx].replace_range(pos..pos + old.len(), new);
    let mut output = lines.join("\n");
    if contents.ends_with('\n') {
        output.push('\n');
    }
    std::fs::write(path, output).is_ok()
}

fn resolve_fix_path(workspace_root: &Path, file: &Path) -> std::path::PathBuf {
    if file.is_absolute() {
        file.to_path_buf()
    } else {
        workspace_root.join(file)
    }
}

pub fn slice_markdown_section(
    file_path: &Path,
    start_line: u32,
) -> Option<(String, std::ops::Range<usize>)> {
    let source = std::fs::read_to_string(file_path).ok()?;

    let mut sections = Vec::new();
    let mut current_line = 1;
    let mut current_offset = 0;
    let mut current_text = String::new();

    let parser = Parser::new_ext(&source, Options::all()).into_offset_iter();
    let mut in_heading = false;
    let mut heading_level = 0u8;

    for (event, range) in parser {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                let lvl = match level {
                    pulldown_cmark::HeadingLevel::H2 => 2,
                    pulldown_cmark::HeadingLevel::H3 => 3,
                    _ => 0,
                };
                if lvl == 2 || lvl == 3 {
                    if !current_text.trim().is_empty() {
                        sections.push((current_line, current_offset..range.start));
                        current_line = line_of_offset(&source, range.start);
                        current_offset = range.start;
                        current_text.clear();
                    } else {
                        current_line = line_of_offset(&source, range.start);
                        current_offset = range.start;
                    }
                    in_heading = true;
                    heading_level = lvl;
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                in_heading = false;
                heading_level = 0;
            }
            Event::Text(t) => {
                if in_heading && (heading_level == 2 || heading_level == 3) {
                    current_text.push_str(&t);
                    current_text.push('\n');
                } else if !in_heading {
                    current_text.push_str(&t);
                    current_text.push(' ');
                }
            }
            Event::Code(t) => {
                current_text.push('`');
                current_text.push_str(&t);
                current_text.push('`');
                current_text.push(' ');
            }
            _ => {}
        }
    }
    if !current_text.trim().is_empty() || current_offset < source.len() {
        sections.push((current_line, current_offset..source.len()));
    }

    for (line, range) in sections {
        if line == start_line {
            let text = source[range.clone()].to_string();
            return Some((text, range));
        }
    }

    None
}

fn line_of_offset(src: &str, offset: usize) -> u32 {
    let end = offset.min(src.len());
    (src[..end].bytes().filter(|&b| b == b'\n').count() as u32) + 1
}

pub fn slice_example_narrative(file_path: &Path) -> Option<(String, std::ops::Range<usize>)> {
    let src = std::fs::read_to_string(file_path).ok()?;
    let mut start_offset = None;
    let mut current_offset = 0;

    let mut narrative_text = String::new();
    let mut temp_empty_lines = String::new();
    let mut last_comment_offset = 0;

    for line in src.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("//!") {
            if start_offset.is_none() {
                start_offset = Some(current_offset);
            }
            if !temp_empty_lines.is_empty() {
                narrative_text.push_str(&temp_empty_lines);
                temp_empty_lines.clear();
            }
            narrative_text.push_str(rest.trim());
            narrative_text.push('\n');
            last_comment_offset = current_offset + line.len();
        } else if let Some(rest) = trimmed.strip_prefix("//") {
            if start_offset.is_none() {
                start_offset = Some(current_offset);
            }
            if !temp_empty_lines.is_empty() {
                narrative_text.push_str(&temp_empty_lines);
                temp_empty_lines.clear();
            }
            narrative_text.push_str(rest.trim());
            narrative_text.push('\n');
            last_comment_offset = current_offset + line.len();
        } else if trimmed.is_empty() || trimmed == "\r" {
            if start_offset.is_some() {
                temp_empty_lines.push('\n');
            }
        } else {
            break;
        }
        current_offset += line.len();
    }

    let trimmed_narrative = narrative_text.trim().to_string();
    if trimmed_narrative.len() < 16 {
        return None;
    }

    let start = start_offset?;
    let end = last_comment_offset;
    if start >= end {
        return None;
    }

    Some((trimmed_narrative, start..end))
}

pub fn build_markdown_correction_prompt(
    original_section: &str,
    code_context: &str,
    reality: &str,
) -> (String, String) {
    let system = "You are an expert technical writer and programmer. Your task is to update a specific Markdown section of documentation so that it accurately describes the current implementation in the source code.\n\
                  You MUST return ONLY the updated Markdown section (including its heading). Do not include any explanation, conversational text, or markdown code block wrapper (like ```markdown ... ```).\n\
                  Preserve the original formatting, headings, style, and tone as much as possible, only editing the parts that are outdated.".to_string();

    let user = format!(
        "Here is the original Markdown section:\n\
         ---\n\
         {}\n\
         ---\n\n\
         Here is the current implementation in the source code:\n\
         ---\n\
         {}\n\
         ---\n\n\
         According to our analysis, this documentation section has drifted from reality because:\n\
         {}\n\n\
         Please write the corrected Markdown section. Remember to return ONLY the raw corrected Markdown (including its heading, if it had one), with no conversational wrapper.",
        original_section, code_context, reality
    );
    (system, user)
}

pub fn build_example_narrative_prompt(
    original_narrative: &str,
    public_signatures: &str,
    reality: &str,
) -> (String, String) {
    let system = "You are an expert technical writer and programmer. Your task is to update the narrative comment block at the top of a Rust example file so that it accurately describes the current public API of the library.\n\
                  You MUST return ONLY the updated narrative text. Do not include any comment prefixes (like //! or //), conversational text, explanations, or code blocks.\n\
                  Preserve the original style and tone as much as possible, only editing the parts that are outdated.".to_string();

    let user = format!(
        "Here is the original example narrative (excluding comment prefixes):\n\
         ---\n\
         {}\n\
         ---\n\n\
         Here are the signatures of currently-public functions in the library:\n\
         ---\n\
         {}\n\
         ---\n\n\
         According to our analysis, this narrative has drifted from the public API because:\n\
         {}\n\n\
         Please write the corrected narrative text (without comment prefixes). Remember to return ONLY the raw corrected narrative, with no conversational wrapper.",
        original_narrative, public_signatures, reality
    );
    (system, user)
}

pub fn format_as_comments(text: &str, prefix: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            out.push_str(prefix);
            out.push('\n');
        } else {
            out.push_str(prefix);
            out.push(' ');
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

pub fn collect_public_signatures(ctx: &crate::context::ProjectContext) -> String {
    let mut names: Vec<String> = ctx
        .code_facts
        .iter()
        .filter(|f| {
            matches!(f.kind, crate::domain::FactKind::Function)
                && !f
                    .location
                    .file
                    .components()
                    .any(|c| c.as_os_str() == "tests" || c.as_os_str() == "examples")
        })
        .map(|f| f.name.clone())
        .collect();
    names.sort();
    names.dedup();
    names.join("\n")
}

pub fn build_outdated_logic_context(
    ctx: &crate::context::ProjectContext,
    section_text: &str,
) -> String {
    let mentioned = mentioned_identifiers(section_text);
    let bodies: Vec<(String, String)> = mentioned
        .iter()
        .filter_map(|name| fetch_fn_body(ctx, name).map(|b| (name.clone(), b)))
        .collect();

    let mut context = String::new();
    for (name, body) in bodies {
        context.push_str(&format!("### fn {name}\n```rust\n{body}\n```\n"));
    }
    context
}

fn mentioned_identifiers(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in text.split('`') {
        let trimmed = part.trim().trim_end_matches("()");
        if is_ident(trimmed) {
            out.push(trimmed.to_string());
        }
        // Handle qualified paths: grab the leaf.
        if let Some(leaf) = trimmed.rsplit("::").next()
            && is_ident(leaf)
            && leaf != trimmed
        {
            out.push(leaf.to_string());
        }
    }
    out.sort();
    out.dedup();
    out
}

fn is_ident(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let mut chars = s.chars();
    let first = chars.next().unwrap();
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn fetch_fn_body(ctx: &crate::context::ProjectContext, name: &str) -> Option<String> {
    let fact = ctx
        .facts_named(name)
        .find(|f| matches!(f.kind, crate::domain::FactKind::Function))?;
    let src = std::fs::read_to_string(&fact.location.file).ok()?;
    const WINDOW: usize = 50;
    let lines: Vec<&str> = src.lines().collect();
    let start = fact.location.line.saturating_sub(1) as usize;
    let end = (start + WINDOW).min(lines.len());
    Some(lines[start..end].join("\n"))
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
    fn extracts_symbol_from_rendered_stated_text() {
        assert_eq!(
            extract_stated_symbol("`Client::new` exists in the codebase").as_deref(),
            Some("Client::new")
        );
        assert_eq!(
            extract_stated_symbol("`connect_to_db()` exists in the codebase").as_deref(),
            Some("connect_to_db")
        );
    }

    #[test]
    fn apply_text_fix_resolves_relative_path_against_workspace_root() {
        let tmp = tempfile::tempdir().unwrap();
        let readme = tmp.path().join("README.md");
        std::fs::write(&readme, "Use `old_name()`.\n").unwrap();

        assert!(apply_text_fix(
            tmp.path(),
            Path::new("README.md"),
            1,
            "old_name",
            "new_name",
        ));

        let out = std::fs::read_to_string(readme).unwrap();
        assert_eq!(out, "Use `new_name()`.\n");
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

    #[test]
    fn test_slice_markdown_section() {
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("README.md");
        let content = "\
# Title
Intro text is here.

## Section 1
This is the text for section 1.

### Sub Section 2
And text for section 2.
";
        std::fs::write(&file_path, content).unwrap();

        // Slice Section 1 (starts on line 4, line counting of ## Section 1)
        let slice1 = slice_markdown_section(&file_path, 4).unwrap();
        assert!(slice1.0.contains("## Section 1"));
        assert!(slice1.0.contains("This is the text for section 1."));
        assert_eq!(
            &content[slice1.1],
            "## Section 1\nThis is the text for section 1.\n\n"
        );

        // Slice Sub Section 2 (starts on line 7)
        let slice2 = slice_markdown_section(&file_path, 7).unwrap();
        assert!(slice2.0.contains("### Sub Section 2"));
        assert!(slice2.0.contains("And text for section 2."));
        assert_eq!(
            &content[slice2.1],
            "### Sub Section 2\nAnd text for section 2.\n"
        );
    }

    #[test]
    fn test_slice_example_narrative() {
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("demo.rs");
        let content = "\
//! This example demonstrates a legacy connection.
//! And this is the second line of the narrative.

fn main() {}
";
        std::fs::write(&file_path, content).unwrap();

        let slice = slice_example_narrative(&file_path).unwrap();
        assert_eq!(
            slice.0,
            "This example demonstrates a legacy connection.\nAnd this is the second line of the narrative."
        );
        assert_eq!(
            &content[slice.1],
            "//! This example demonstrates a legacy connection.\n//! And this is the second line of the narrative.\n"
        );
    }
}
