use super::DriftAnalyzer;
use super::examples::{CargoRunner, RealCargoRunner};
use crate::context::ProjectContext;
use crate::domain::{Divergence, Location, RuleId, Severity};
use serde::Deserialize;
use std::path::PathBuf;

/// DeprecatedUsageAnalyzer — enforces `deprecated_usage`.
///
/// Strategy: run `cargo clippy --examples --message-format=json` and surface
/// every warning whose diagnostic code is `deprecated`. This re-frames the
/// built-in lint as drift: an example that still calls deprecated API is
/// teaching users a pattern the codebase is moving away from.
pub struct DeprecatedUsageAnalyzer {
    runner: Box<dyn CargoRunner>,
}

impl Default for DeprecatedUsageAnalyzer {
    fn default() -> Self {
        Self {
            runner: Box::new(RealCargoRunner),
        }
    }
}

impl DeprecatedUsageAnalyzer {
    pub fn with_runner(runner: Box<dyn CargoRunner>) -> Self {
        Self { runner }
    }
}

impl DriftAnalyzer for DeprecatedUsageAnalyzer {
    fn id(&self) -> &'static str {
        "deprecated_usage"
    }

    fn analyze(&self, ctx: &ProjectContext) -> Vec<Divergence> {
        let examples_dir = ctx.root.join("examples");
        if !examples_dir.exists() {
            return Vec::new();
        }

        let stdout = match self.runner.clippy_examples(&ctx.root) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("spec-drift: cargo clippy --examples failed to launch: {e}");
                return Vec::new();
            }
        };

        parse_clippy_messages(&stdout, &ctx.root)
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "reason")]
enum ClippyMessage {
    #[serde(rename = "compiler-message")]
    CompilerMessage { message: CompilerMessage },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct CompilerMessage {
    message: String,
    level: String,
    code: Option<DiagCode>,
    spans: Vec<DiagSpan>,
}

#[derive(Debug, Deserialize)]
struct DiagCode {
    code: String,
}

#[derive(Debug, Deserialize)]
struct DiagSpan {
    file_name: String,
    line_start: u32,
    is_primary: bool,
}

fn parse_clippy_messages(stdout: &str, root: &std::path::Path) -> Vec<Divergence> {
    let mut out = Vec::new();

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<ClippyMessage>(line) else {
            continue;
        };
        let ClippyMessage::CompilerMessage { message } = msg else {
            continue;
        };
        if message.level != "warning" {
            continue;
        }
        let Some(code) = message.code.as_ref() else {
            continue;
        };
        if !is_deprecated_code(&code.code) {
            continue;
        }

        let span = message
            .spans
            .iter()
            .find(|s| s.is_primary)
            .or_else(|| message.spans.first());
        let Some(span) = span else { continue };

        let path = std::path::Path::new(&span.file_name);
        let abs: PathBuf = if path.is_absolute() {
            path.to_path_buf()
        } else {
            root.join(path)
        };
        let rel = abs.strip_prefix(root).unwrap_or(&abs);
        let Some(first) = rel.components().next() else {
            continue;
        };
        if first.as_os_str() != "examples" {
            continue;
        }

        out.push(Divergence {
            rule: RuleId::DeprecatedUsage,
            severity: Severity::Warning,
            location: Location::new(rel.to_path_buf(), span.line_start),
            stated: format!("`{}` demonstrates supported API", rel.display()),
            reality: format!("example uses deprecated API: {}", message.message),
            risk: "Examples teach users a pattern the codebase is retiring.".to_string(),
            attribution: None,

        });
    }

    out
}

fn is_deprecated_code(code: &str) -> bool {
    // rustc: `deprecated`; clippy: `clippy::deprecated_*`, etc.
    code == "deprecated" || code.starts_with("deprecated") || code.contains("::deprecated")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    struct FakeRunner(String);
    impl CargoRunner for FakeRunner {
        fn check_examples(&self, _: &Path) -> std::io::Result<String> {
            Ok(String::new())
        }
        fn clippy_examples(&self, _: &Path) -> std::io::Result<String> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn flags_deprecated_warning_in_example() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("examples")).unwrap();

        let stdout = r#"{"reason":"compiler-message","message":{"message":"use of deprecated function","level":"warning","code":{"code":"deprecated"},"spans":[{"file_name":"examples/demo.rs","line_start":12,"is_primary":true}]}}"#;

        let ctx = ProjectContext::new(tmp.path());
        let analyzer =
            DeprecatedUsageAnalyzer::with_runner(Box::new(FakeRunner(stdout.to_string())));
        let divs = analyzer.analyze(&ctx);

        assert_eq!(divs.len(), 1);
        assert_eq!(divs[0].rule, RuleId::DeprecatedUsage);
        assert_eq!(divs[0].severity, Severity::Warning);
        assert_eq!(divs[0].location.line, 12);
    }

    #[test]
    fn ignores_non_deprecated_warnings() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("examples")).unwrap();
        let stdout = r#"{"reason":"compiler-message","message":{"message":"unused import","level":"warning","code":{"code":"unused_imports"},"spans":[{"file_name":"examples/demo.rs","line_start":3,"is_primary":true}]}}"#;
        let ctx = ProjectContext::new(tmp.path());
        let analyzer =
            DeprecatedUsageAnalyzer::with_runner(Box::new(FakeRunner(stdout.to_string())));
        assert!(analyzer.analyze(&ctx).is_empty());
    }

    #[test]
    fn ignores_deprecated_outside_examples_dir() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("examples")).unwrap();
        let stdout = r#"{"reason":"compiler-message","message":{"message":"deprecated","level":"warning","code":{"code":"deprecated"},"spans":[{"file_name":"src/lib.rs","line_start":1,"is_primary":true}]}}"#;
        let ctx = ProjectContext::new(tmp.path());
        let analyzer =
            DeprecatedUsageAnalyzer::with_runner(Box::new(FakeRunner(stdout.to_string())));
        assert!(analyzer.analyze(&ctx).is_empty());
    }

    #[test]
    fn noop_when_no_examples_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let analyzer = DeprecatedUsageAnalyzer::with_runner(Box::new(FakeRunner(
            "SHOULD NOT PARSE".to_string(),
        )));
        let ctx = ProjectContext::new(tmp.path());
        assert!(analyzer.analyze(&ctx).is_empty());
    }
}
