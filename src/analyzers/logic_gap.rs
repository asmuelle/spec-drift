use super::DriftAnalyzer;
use crate::context::ProjectContext;
use crate::domain::{Divergence, FactKind, Location, RuleId, Severity};
use crate::llm::LlmClient;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// LogicGapAnalyzer — enforces `logic_gap` (experimental, Notice).
///
/// Strategy: for every `examples/*.rs` with a leading `//!` or `//` comment
/// that reads like a narrative ("this example demonstrates ..."), compare that
/// narrative against the public API surface of the library (every `pub fn`
/// declared under `src/`). When the model decides the narrative no longer
/// holds, emit a Notice.
///
/// The rule is experimental: null / budget-exhausted / network-failing
/// clients silently skip. `compile_failure` handles the deterministic case
/// where the example no longer compiles.
pub struct LogicGapAnalyzer {
    client: Arc<dyn LlmClient>,
}

impl LogicGapAnalyzer {
    pub fn new(client: Arc<dyn LlmClient>) -> Self {
        Self { client }
    }
}

const SYSTEM_PROMPT: &str = "You are auditing Rust example code for drift from a library's public \
API. You receive (1) the narrative comment at the top of an example file, and (2) the signatures \
of every currently-public function in the library. Decide whether the narrative still matches a \
valid usage pattern of the current API. Reply ONLY with a JSON object of the shape \
`{\"match_spec\": <bool>, \"reason\": \"<short explanation>\"}`. No prose before or after.";

impl DriftAnalyzer for LogicGapAnalyzer {
    fn analyze(&self, ctx: &ProjectContext) -> Vec<Divergence> {
        let public_surface = collect_public_signatures(ctx);
        if public_surface.is_empty() {
            // Nothing to compare against — no library, no gap.
            return Vec::new();
        }

        let mut out = Vec::new();

        for example in example_files(&ctx.rust_files) {
            let Some(narrative) = read_narrative(example) else {
                continue;
            };

            let user_prompt = render_prompt(&narrative, &public_surface);
            let Some(verdict) = self.client.evaluate(SYSTEM_PROMPT, &user_prompt) else {
                continue;
            };
            if verdict.match_spec {
                continue;
            }

            let rel = ctx.rel(example).to_path_buf();
            out.push(Divergence {
                rule: RuleId::LogicGap,
                severity: Severity::Notice,
                location: Location::new(rel, 1),
                stated: "example narrative describes current API usage".into(),
                reality: verdict.reason,
                risk: "Example teaches a pattern the public API no longer supports.".into(),
                attribution: None,
            });
        }

        out
    }
}

fn example_files(rust_files: &[PathBuf]) -> impl Iterator<Item = &Path> {
    rust_files
        .iter()
        .filter(|p| p.components().any(|c| c.as_os_str() == "examples"))
        .map(|p| p.as_path())
}

/// Extract the leading narrative: the contiguous run of `//!` / `//` lines at
/// the top of the file, joined into a single string. Returns `None` when the
/// example has no leading comment — nothing for the model to audit.
fn read_narrative(path: &Path) -> Option<String> {
    let src = std::fs::read_to_string(path).ok()?;
    let mut buf = String::new();
    for line in src.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("//!") {
            buf.push_str(rest.trim());
            buf.push('\n');
        } else if let Some(rest) = trimmed.strip_prefix("//") {
            buf.push_str(rest.trim());
            buf.push('\n');
        } else if trimmed.is_empty() {
            // Allow a blank line inside the comment block.
            if !buf.is_empty() {
                buf.push('\n');
            }
        } else {
            break;
        }
    }
    let trimmed = buf.trim();
    if trimmed.len() < 16 {
        // Too short to be a meaningful narrative.
        return None;
    }
    Some(trimmed.to_string())
}

fn collect_public_signatures(ctx: &ProjectContext) -> Vec<String> {
    // Use the cached `pub fn` code facts directly; we only need names, not
    // full signatures, for the model to judge whether the narrative still
    // applies. Names are cheap and keep the prompt bounded.
    let mut names: Vec<String> = ctx
        .code_facts
        .iter()
        .filter(|f| {
            matches!(f.kind, FactKind::Function)
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
    names
}

fn render_prompt(narrative: &str, public_fns: &[String]) -> String {
    let mut prompt = String::new();
    prompt.push_str("## Example narrative\n");
    prompt.push_str(narrative);
    prompt.push_str("\n\n## Public functions in the library\n");
    for name in public_fns {
        prompt.push_str("- ");
        prompt.push_str(name);
        prompt.push('\n');
    }
    prompt.push_str("\nRespond with the JSON object described in the system prompt.");
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{CodeFact, Location};
    use crate::llm::LlmVerdict;

    struct FakeClient(LlmVerdict);
    impl LlmClient for FakeClient {
        fn evaluate(&self, _: &str, _: &str) -> Option<LlmVerdict> {
            Some(self.0.clone())
        }
    }

    struct NullClient;
    impl LlmClient for NullClient {
        fn evaluate(&self, _: &str, _: &str) -> Option<LlmVerdict> {
            None
        }
    }

    fn pub_fn(name: &str) -> CodeFact {
        CodeFact {
            location: Location::new("src/lib.rs", 1),
            kind: FactKind::Function,
            name: name.to_string(),
        }
    }

    fn setup_example(narrative: &str) -> (tempfile::TempDir, ProjectContext) {
        let tmp = tempfile::tempdir().unwrap();
        let examples = tmp.path().join("examples");
        std::fs::create_dir(&examples).unwrap();
        let demo = examples.join("demo.rs");
        let body = format!("//! {narrative}\n\nfn main() {{}}\n");
        std::fs::write(&demo, body).unwrap();

        let mut ctx = ProjectContext::new(tmp.path());
        ctx.rust_files.push(demo);
        ctx.code_facts.push(pub_fn("current_api"));
        (tmp, ctx)
    }

    #[test]
    fn flags_when_model_says_gap() {
        let (_tmp, ctx) =
            setup_example("Demonstrates the legacy connect() flow (3-step handshake).");
        let client = Arc::new(FakeClient(LlmVerdict {
            match_spec: false,
            reason: "connect() no longer exists; API now uses current_api()".into(),
        }));
        let divs = LogicGapAnalyzer::new(client).analyze(&ctx);
        assert_eq!(divs.len(), 1);
        assert_eq!(divs[0].rule, RuleId::LogicGap);
        assert_eq!(divs[0].severity, Severity::Notice);
    }

    #[test]
    fn silent_when_model_says_match() {
        let (_tmp, ctx) = setup_example("Demonstrates the current_api() flow.");
        let client = Arc::new(FakeClient(LlmVerdict {
            match_spec: true,
            reason: "ok".into(),
        }));
        assert!(LogicGapAnalyzer::new(client).analyze(&ctx).is_empty());
    }

    #[test]
    fn silent_when_client_is_null() {
        let (_tmp, ctx) = setup_example("Demonstrates anything at all.");
        assert!(
            LogicGapAnalyzer::new(Arc::new(NullClient))
                .analyze(&ctx)
                .is_empty()
        );
    }

    #[test]
    fn skips_examples_with_no_narrative() {
        let tmp = tempfile::tempdir().unwrap();
        let examples = tmp.path().join("examples");
        std::fs::create_dir(&examples).unwrap();
        let demo = examples.join("bare.rs");
        std::fs::write(&demo, "fn main() {}\n").unwrap();

        let mut ctx = ProjectContext::new(tmp.path());
        ctx.rust_files.push(demo);
        ctx.code_facts.push(pub_fn("x"));

        let client = Arc::new(FakeClient(LlmVerdict {
            match_spec: false,
            reason: "would fire".into(),
        }));
        assert!(LogicGapAnalyzer::new(client).analyze(&ctx).is_empty());
    }

    #[test]
    fn silent_when_library_has_no_public_functions() {
        let tmp = tempfile::tempdir().unwrap();
        let examples = tmp.path().join("examples");
        std::fs::create_dir(&examples).unwrap();
        let demo = examples.join("demo.rs");
        std::fs::write(
            &demo,
            "//! narrative narrative narrative narrative\nfn main(){}\n",
        )
        .unwrap();

        let mut ctx = ProjectContext::new(tmp.path());
        ctx.rust_files.push(demo);
        // No code facts at all → no public surface → nothing to compare.

        let client = Arc::new(FakeClient(LlmVerdict {
            match_spec: false,
            reason: "would fire".into(),
        }));
        assert!(LogicGapAnalyzer::new(client).analyze(&ctx).is_empty());
    }
}
