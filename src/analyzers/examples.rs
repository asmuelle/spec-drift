use super::DriftAnalyzer;
use crate::context::ProjectContext;
use crate::domain::{Divergence, Location, RuleId, Severity};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;

/// ExamplesAnalyzer — enforces `compile_failure`.
///
/// Strategy: shell out to `cargo check --examples --message-format=json`, parse
/// the JSON message stream, and surface every `compiler-message` with level
/// `error` that points at a file under `examples/`.
///
/// This is deterministic by definition: if cargo says it doesn't compile, the
/// example is drifting from the library's current API.
pub struct ExamplesAnalyzer {
    /// Overrides the project root used to launch `cargo`. Defaults to
    /// [`ProjectContext::root`] at analyze-time.
    manifest_override: Option<PathBuf>,
    /// Injectable executor, so unit tests can avoid spawning cargo.
    runner: Box<dyn CargoRunner>,
}

impl Default for ExamplesAnalyzer {
    fn default() -> Self {
        Self {
            manifest_override: None,
            runner: Box::new(RealCargoRunner),
        }
    }
}

impl ExamplesAnalyzer {
    /// Construct an analyzer with a custom runner (used by tests).
    pub fn with_runner(runner: Box<dyn CargoRunner>) -> Self {
        Self {
            manifest_override: None,
            runner,
        }
    }

    fn manifest(&self, ctx: &ProjectContext) -> PathBuf {
        self.manifest_override
            .clone()
            .unwrap_or_else(|| ctx.analysis_root.clone())
    }
}

impl DriftAnalyzer for ExamplesAnalyzer {
    fn analyze(&self, ctx: &ProjectContext) -> Vec<Divergence> {
        let manifest = self.manifest(ctx);
        // Skip the check entirely if the selected package has no examples dir.
        let examples_dir = manifest.join("examples");
        if !examples_dir.exists() {
            return Vec::new();
        }

        let stdout = match self.runner.check_examples(&manifest) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("spec-drift: cargo check --examples failed to launch: {e}");
                return Vec::new();
            }
        };

        parse_cargo_messages(&stdout, &manifest, &ctx.root)
    }
}

/// Abstraction over invoking cargo so tests can inject canned JSON streams.
pub trait CargoRunner: Send + Sync {
    fn check_examples(&self, manifest_dir: &std::path::Path) -> std::io::Result<String>;
    /// Clippy output for examples. Used by DeprecatedUsageAnalyzer.
    /// Default implementation returns empty output so runners that only care
    /// about `cargo check` don't have to stub a second method.
    fn clippy_examples(&self, _manifest_dir: &std::path::Path) -> std::io::Result<String> {
        Ok(String::new())
    }
}

pub struct RealCargoRunner;

impl CargoRunner for RealCargoRunner {
    fn check_examples(&self, manifest_dir: &std::path::Path) -> std::io::Result<String> {
        let out = Command::new("cargo")
            .current_dir(manifest_dir)
            .args(["check", "--examples", "--message-format=json", "--quiet"])
            .output()?;
        // cargo returns non-zero on compile error — we still want the stdout stream.
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn clippy_examples(&self, manifest_dir: &std::path::Path) -> std::io::Result<String> {
        let out = Command::new("cargo")
            .current_dir(manifest_dir)
            .args(["clippy", "--examples", "--message-format=json", "--quiet"])
            .output()?;
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "reason")]
enum CargoMessage {
    #[serde(rename = "compiler-message")]
    CompilerMessage { message: CompilerMessage },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct CompilerMessage {
    message: String,
    level: String,
    spans: Vec<DiagSpan>,
}

#[derive(Debug, Deserialize)]
struct DiagSpan {
    file_name: String,
    line_start: u32,
    is_primary: bool,
}

fn parse_cargo_messages(stdout: &str, manifest_root: &Path, report_root: &Path) -> Vec<Divergence> {
    let mut out = Vec::new();

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<CargoMessage>(line) else {
            continue;
        };
        let CargoMessage::CompilerMessage { message } = msg else {
            continue;
        };
        if message.level != "error" {
            continue;
        }

        // Prefer the primary span; fall back to the first span if none flagged primary.
        let span = message
            .spans
            .iter()
            .find(|s| s.is_primary)
            .or_else(|| message.spans.first());

        let Some(span) = span else { continue };

        let path = Path::new(&span.file_name);
        // Only report diagnostics rooted in the selected package's `examples/`
        // directory, while rendering locations relative to the workspace root.
        let abs = if path.is_absolute() {
            path.to_path_buf()
        } else {
            manifest_root.join(path)
        };
        let package_rel = abs.strip_prefix(manifest_root).unwrap_or(&abs);
        let Some(first) = package_rel.components().next() else {
            continue;
        };
        if first.as_os_str() != "examples" {
            continue;
        }
        let report_path = abs.strip_prefix(report_root).unwrap_or(&abs);

        out.push(Divergence {
            rule: RuleId::CompileFailure,
            severity: Severity::Critical,
            location: Location::new(report_path.to_path_buf(), span.line_start),
            stated: format!("`{}` demonstrates the current API", report_path.display()),
            reality: format!("`cargo check --examples` fails: {}", message.message),
            risk: "Users copy from broken examples and ship broken code.".to_string(),
            attribution: None,
        });
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    struct FakeRunner(String);
    impl CargoRunner for FakeRunner {
        fn check_examples(&self, _: &Path) -> std::io::Result<String> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn flags_compile_error_in_examples_dir() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("examples")).unwrap();
        std::fs::write(tmp.path().join("examples/demo.rs"), "// placeholder").unwrap();

        let stdout = r#"{"reason":"compiler-message","message":{"message":"mismatched types","level":"error","spans":[{"file_name":"examples/demo.rs","line_start":7,"is_primary":true}]}}"#;

        let mut ctx = ProjectContext::new(tmp.path());
        // Analyzer reads from ctx.root so no additional setup needed.
        let analyzer = ExamplesAnalyzer::with_runner(Box::new(FakeRunner(stdout.to_string())));
        ctx.root = tmp.path().to_path_buf();
        let divergences = analyzer.analyze(&ctx);

        assert_eq!(divergences.len(), 1);
        let d = &divergences[0];
        assert_eq!(d.rule, RuleId::CompileFailure);
        assert_eq!(d.severity, Severity::Critical);
        assert_eq!(d.location.line, 7);
        assert_eq!(
            d.location.file,
            std::path::PathBuf::from("examples/demo.rs")
        );
    }

    #[test]
    fn package_examples_are_reported_relative_to_workspace_root() {
        let tmp = tempfile::tempdir().unwrap();
        let member = tmp.path().join("crates").join("api");
        std::fs::create_dir_all(member.join("examples")).unwrap();
        std::fs::write(member.join("examples/demo.rs"), "// placeholder").unwrap();

        let stdout = r#"{"reason":"compiler-message","message":{"message":"mismatched types","level":"error","spans":[{"file_name":"examples/demo.rs","line_start":4,"is_primary":true}]}}"#;

        let mut ctx = ProjectContext::new(tmp.path());
        ctx.analysis_root = member;
        let analyzer = ExamplesAnalyzer::with_runner(Box::new(FakeRunner(stdout.to_string())));
        let divergences = analyzer.analyze(&ctx);

        assert_eq!(divergences.len(), 1);
        assert_eq!(
            divergences[0].location.file,
            std::path::PathBuf::from("crates/api/examples/demo.rs")
        );
    }

    #[test]
    fn ignores_errors_outside_examples_dir() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("examples")).unwrap();
        let stdout = r#"{"reason":"compiler-message","message":{"message":"oops","level":"error","spans":[{"file_name":"src/lib.rs","line_start":1,"is_primary":true}]}}"#;
        let analyzer = ExamplesAnalyzer::with_runner(Box::new(FakeRunner(stdout.to_string())));
        let ctx = ProjectContext::new(tmp.path());
        assert!(analyzer.analyze(&ctx).is_empty());
    }

    #[test]
    fn skips_cleanly_when_no_examples_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let analyzer = ExamplesAnalyzer::with_runner(Box::new(FakeRunner("SHOULD NOT RUN".into())));
        let ctx = ProjectContext::new(tmp.path());
        assert!(analyzer.analyze(&ctx).is_empty());
    }

    #[test]
    fn ignores_warning_level_messages() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("examples")).unwrap();
        let stdout = r#"{"reason":"compiler-message","message":{"message":"unused import","level":"warning","spans":[{"file_name":"examples/demo.rs","line_start":3,"is_primary":true}]}}"#;
        let analyzer = ExamplesAnalyzer::with_runner(Box::new(FakeRunner(stdout.to_string())));
        let ctx = ProjectContext::new(tmp.path());
        assert!(analyzer.analyze(&ctx).is_empty());
    }
}
