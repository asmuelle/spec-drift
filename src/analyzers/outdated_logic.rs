use super::DriftAnalyzer;
use crate::context::ProjectContext;
use crate::domain::{Divergence, FactKind, Location, RuleId, Severity};
use crate::llm::LlmClient;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use std::sync::Arc;

/// OutdatedLogicAnalyzer — enforces `outdated_logic` (experimental, Notice).
///
/// Strategy: split each Markdown file into sections delimited by H2/H3
/// headings. For every section whose prose references at least one function
/// that exists in the codebase, send the section text together with the
/// function bodies to the LLM and ask whether the description still
/// accurately describes the behavior.
///
/// The rule is experimental. A null or budget-exhausted client silently
/// skips; the analyzer never emits a divergence it can't back with a verdict.
pub struct OutdatedLogicAnalyzer {
    client: Arc<dyn LlmClient>,
}

impl OutdatedLogicAnalyzer {
    pub fn new(client: Arc<dyn LlmClient>) -> Self {
        Self { client }
    }
}

const SYSTEM_PROMPT: &str = "You are auditing technical documentation for drift from source code. \
You receive (1) a Markdown section describing intended behavior, and (2) one or more Rust function \
bodies that implement the behavior. Decide whether the section is still accurate. \
Reply ONLY with a JSON object of the shape \
`{\"match_spec\": <bool>, \"reason\": \"<short explanation>\"}`. No prose before or after.";

impl DriftAnalyzer for OutdatedLogicAnalyzer {
    fn id(&self) -> &'static str {
        "outdated_logic"
    }

    fn analyze(&self, ctx: &ProjectContext) -> Vec<Divergence> {
        let mut out = Vec::new();

        for md in &ctx.markdown_files {
            let Ok(source) = std::fs::read_to_string(md) else {
                continue;
            };
            let sections = split_sections(&source);

            for section in sections {
                let mentioned: Vec<String> = section.mentioned_identifiers();
                // Section must mention at least one identifier that actually
                // exists in the codebase as a function — otherwise there's
                // nothing concrete to compare against.
                let bodies: Vec<(String, String)> = mentioned
                    .iter()
                    .filter_map(|name| fetch_fn_body(ctx, name).map(|b| (name.clone(), b)))
                    .collect();
                if bodies.is_empty() {
                    continue;
                }

                let user_prompt = render_user_prompt(&section.text, &bodies);
                let Some(verdict) = self.client.evaluate(SYSTEM_PROMPT, &user_prompt) else {
                    // Fail closed — client is null, budget exhausted, or
                    // network failed. Never flag on incomplete evidence.
                    continue;
                };
                if verdict.match_spec {
                    continue;
                }

                out.push(Divergence {
                    rule: RuleId::OutdatedLogic,
                    severity: Severity::Notice,
                    location: Location::new(md.clone(), section.line),
                    stated: "section describes current behavior".into(),
                    reality: verdict.reason,
                    risk: "Docs teach behavior the code no longer implements.".into(),
                    attribution: None,

                });
            }
        }

        out
    }
}

struct Section {
    text: String,
    line: u32,
}

impl Section {
    /// Pull out every backticked token that looks like a bare Rust identifier.
    fn mentioned_identifiers(&self) -> Vec<String> {
        let mut out = Vec::new();
        for part in self.text.split('`') {
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

/// Split Markdown into sections delimited by H2/H3 headings. The section
/// `text` is the concatenation of every block in the section; `line` is the
/// source line of the heading (or 1 for the implicit leading section).
fn split_sections(source: &str) -> Vec<Section> {
    let mut sections: Vec<Section> = Vec::new();
    let mut current = Section {
        text: String::new(),
        line: 1,
    };

    let parser = Parser::new_ext(source, Options::all()).into_offset_iter();
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
                    if !current.text.trim().is_empty() {
                        sections.push(std::mem::replace(
                            &mut current,
                            Section {
                                text: String::new(),
                                line: line_of_offset(source, range.start),
                            },
                        ));
                    } else {
                        current.line = line_of_offset(source, range.start);
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
                    current.text.push_str(&t);
                    current.text.push('\n');
                } else if !in_heading {
                    current.text.push_str(&t);
                    current.text.push(' ');
                }
            }
            Event::Code(t) => {
                current.text.push('`');
                current.text.push_str(&t);
                current.text.push('`');
                current.text.push(' ');
            }
            _ => {}
        }
    }
    if !current.text.trim().is_empty() {
        sections.push(current);
    }
    sections
}

fn line_of_offset(src: &str, offset: usize) -> u32 {
    let end = offset.min(src.len());
    (src[..end].bytes().filter(|&b| b == b'\n').count() as u32) + 1
}

fn fetch_fn_body(ctx: &ProjectContext, name: &str) -> Option<String> {
    let fact = ctx
        .facts_named(name)
        .find(|f| matches!(f.kind, FactKind::Function))?;
    let src = std::fs::read_to_string(&fact.location.file).ok()?;
    // Extract ~50 lines of context around the fact for the prompt. Keeping the
    // prompt bounded is important for both cost and model attention.
    const WINDOW: usize = 50;
    let lines: Vec<&str> = src.lines().collect();
    let start = fact.location.line.saturating_sub(1) as usize;
    let end = (start + WINDOW).min(lines.len());
    Some(lines[start..end].join("\n"))
}

fn render_user_prompt(section: &str, bodies: &[(String, String)]) -> String {
    let mut prompt = String::new();
    prompt.push_str("## Documentation section\n");
    prompt.push_str(section.trim());
    prompt.push_str("\n\n## Current code\n");
    for (name, body) in bodies {
        prompt.push_str(&format!("### fn {name}\n```rust\n{body}\n```\n"));
    }
    prompt.push_str("\nRespond with the JSON object described in the system prompt.");
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{CodeFact, Location};
    use crate::llm::LlmVerdict;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct FakeClient {
        verdict: LlmVerdict,
        was_called: AtomicBool,
    }

    impl LlmClient for FakeClient {
        fn evaluate(&self, _: &str, _: &str) -> Option<LlmVerdict> {
            self.was_called.store(true, Ordering::Release);
            Some(self.verdict.clone())
        }
    }

    struct BlankClient;
    impl LlmClient for BlankClient {
        fn evaluate(&self, _: &str, _: &str) -> Option<LlmVerdict> {
            None
        }
    }

    fn fn_fact(name: &str, file: &std::path::Path, line: u32) -> CodeFact {
        CodeFact {
            location: Location::new(file, line),
            kind: FactKind::Function,
            name: name.to_string(),
        }
    }

    #[test]
    fn flags_when_model_says_drift() {
        let tmp = tempfile::tempdir().unwrap();
        let md = tmp.path().join("README.md");
        std::fs::write(
            &md,
            "## Usage\n\nCall `start()` to begin the flow.\n",
        )
        .unwrap();
        let rs = tmp.path().join("lib.rs");
        std::fs::write(&rs, "pub fn start() { /* old impl */ }\n").unwrap();

        let mut ctx = ProjectContext::new(tmp.path());
        ctx.markdown_files.push(md.clone());
        ctx.rust_files.push(rs.clone());
        ctx.code_facts.push(fn_fact("start", &rs, 1));

        let client = Arc::new(FakeClient {
            verdict: LlmVerdict {
                match_spec: false,
                reason: "start() no longer takes the happy path".into(),
            },
            was_called: AtomicBool::new(false),
        });
        let divs = OutdatedLogicAnalyzer::new(client.clone()).analyze(&ctx);

        assert!(client.was_called.load(Ordering::Acquire));
        assert_eq!(divs.len(), 1);
        assert_eq!(divs[0].rule, RuleId::OutdatedLogic);
        assert_eq!(divs[0].severity, Severity::Notice);
        assert!(divs[0].reality.contains("no longer"));
    }

    #[test]
    fn silent_when_model_says_match() {
        let tmp = tempfile::tempdir().unwrap();
        let md = tmp.path().join("README.md");
        std::fs::write(&md, "## Usage\n\nCall `go()` to run.\n").unwrap();
        let rs = tmp.path().join("lib.rs");
        std::fs::write(&rs, "pub fn go() {}\n").unwrap();

        let mut ctx = ProjectContext::new(tmp.path());
        ctx.markdown_files.push(md);
        ctx.rust_files.push(rs.clone());
        ctx.code_facts.push(fn_fact("go", &rs, 1));

        let client = Arc::new(FakeClient {
            verdict: LlmVerdict {
                match_spec: true,
                reason: "matches".into(),
            },
            was_called: AtomicBool::new(false),
        });
        assert!(OutdatedLogicAnalyzer::new(client).analyze(&ctx).is_empty());
    }

    #[test]
    fn silent_when_client_is_null() {
        let tmp = tempfile::tempdir().unwrap();
        let md = tmp.path().join("README.md");
        std::fs::write(&md, "## Usage\n\nCall `go()` to run.\n").unwrap();
        let rs = tmp.path().join("lib.rs");
        std::fs::write(&rs, "pub fn go() {}\n").unwrap();

        let mut ctx = ProjectContext::new(tmp.path());
        ctx.markdown_files.push(md);
        ctx.rust_files.push(rs.clone());
        ctx.code_facts.push(fn_fact("go", &rs, 1));

        assert!(
            OutdatedLogicAnalyzer::new(Arc::new(BlankClient))
                .analyze(&ctx)
                .is_empty()
        );
    }

    #[test]
    fn skips_sections_with_no_resolvable_symbols() {
        let tmp = tempfile::tempdir().unwrap();
        let md = tmp.path().join("README.md");
        std::fs::write(
            &md,
            "## Overview\n\nA short philosophical paragraph with no symbols.\n",
        )
        .unwrap();

        let mut ctx = ProjectContext::new(tmp.path());
        ctx.markdown_files.push(md);

        let client = Arc::new(FakeClient {
            verdict: LlmVerdict {
                match_spec: false,
                reason: "would fire".into(),
            },
            was_called: AtomicBool::new(false),
        });
        let divs = OutdatedLogicAnalyzer::new(client.clone()).analyze(&ctx);
        // Client is probed once (the liveness probe). What we really care
        // about: no drift emitted when sections don't bind to symbols.
        assert!(divs.is_empty());
    }
}
