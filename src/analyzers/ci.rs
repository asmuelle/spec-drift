use super::DriftAnalyzer;
use crate::context::ProjectContext;
use crate::domain::{Divergence, Location, RuleId, Severity};
use regex::Regex;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

/// CiAnalyzer — enforces the `ghost_command` rule (deterministic).
///
/// Strategy: scan Makefile / justfile / GitHub workflow YAMLs for `cargo`
/// invocations. For each `--package <name>` / `-p <name>` / `--bin <name>`
/// argument, confirm the target exists in `cargo metadata`. Anything else
/// (unknown packages, unknown bins) is a ghost command.
#[derive(Default)]
pub struct CiAnalyzer {
    metadata_override: Option<CargoMetadata>,
}

impl CiAnalyzer {
    /// Construct with pre-loaded metadata (used by tests so they don't shell out).
    pub fn with_metadata(md: CargoMetadata) -> Self {
        Self {
            metadata_override: Some(md),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CargoMetadata {
    pub packages: Vec<String>,
    pub bins: Vec<String>,
}

impl CargoMetadata {
    fn knows_package(&self, name: &str) -> bool {
        self.packages.iter().any(|p| p == name)
    }
    fn knows_bin(&self, name: &str) -> bool {
        self.bins.iter().any(|b| b == name)
    }

    fn load(manifest_dir: &Path) -> Self {
        let out = Command::new("cargo")
            .current_dir(manifest_dir)
            .args(["metadata", "--format-version=1", "--no-deps"])
            .output();
        let Ok(out) = out else {
            return Self::default();
        };
        if !out.status.success() {
            return Self::default();
        }
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
            return Self::default();
        };

        let mut packages = Vec::new();
        let mut bins = Vec::new();
        if let Some(arr) = v.get("packages").and_then(|p| p.as_array()) {
            for pkg in arr {
                if let Some(name) = pkg.get("name").and_then(|n| n.as_str()) {
                    packages.push(name.to_string());
                }
                if let Some(targets) = pkg.get("targets").and_then(|t| t.as_array()) {
                    for t in targets {
                        let kinds = t
                            .get("kind")
                            .and_then(|k| k.as_array())
                            .cloned()
                            .unwrap_or_default();
                        let is_bin = kinds
                            .iter()
                            .any(|k| k.as_str().is_some_and(|s| s == "bin"));
                        if is_bin
                            && let Some(name) = t.get("name").and_then(|n| n.as_str())
                        {
                            bins.push(name.to_string());
                        }
                    }
                }
            }
        }
        Self { packages, bins }
    }
}

impl DriftAnalyzer for CiAnalyzer {
    fn id(&self) -> &'static str {
        "ci"
    }

    fn analyze(&self, ctx: &ProjectContext) -> Vec<Divergence> {
        let metadata = self
            .metadata_override
            .clone()
            .unwrap_or_else(|| CargoMetadata::load(&ctx.root));

        let mut out = Vec::new();

        // Makefiles and justfiles — line-oriented.
        for mk in &ctx.makefile_files {
            let Ok(src) = std::fs::read_to_string(mk) else {
                continue;
            };
            for (idx, line) in src.lines().enumerate() {
                let line_no = (idx + 1) as u32;
                let trimmed = line.trim_start_matches(['\t', ' ']);
                // Strip `@` echo prefix common in make recipes.
                let trimmed = trimmed.strip_prefix('@').unwrap_or(trimmed);
                inspect_cargo_line(trimmed, mk, line_no, &metadata, &mut out);
            }
        }

        // YAML workflows — treat every line containing a cargo invocation.
        for yaml in &ctx.yaml_files {
            let Ok(src) = std::fs::read_to_string(yaml) else {
                continue;
            };
            if !is_workflow_path(yaml) {
                continue;
            }
            for (idx, line) in src.lines().enumerate() {
                let line_no = (idx + 1) as u32;
                inspect_cargo_line(line, yaml, line_no, &metadata, &mut out);
            }
        }

        out
    }
}

fn is_workflow_path(path: &Path) -> bool {
    // Only scan files inside a `.github/workflows/` directory to avoid noisy
    // matches in unrelated YAML (k8s manifests, docker-compose, etc.).
    path.components()
        .any(|c| c.as_os_str() == ".github")
        && path.components().any(|c| c.as_os_str() == "workflows")
}

fn cargo_command_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?:^|\s)cargo\s+([a-zA-Z][a-zA-Z0-9\-]*)\b").unwrap())
}

fn package_arg_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?:--package|-p)[ =]([A-Za-z_][A-Za-z0-9_\-]*)").unwrap()
    })
}

fn bin_arg_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"--bin[ =]([A-Za-z_][A-Za-z0-9_\-]*)").unwrap())
}

fn inspect_cargo_line(
    line: &str,
    path: &Path,
    line_no: u32,
    md: &CargoMetadata,
    out: &mut Vec<Divergence>,
) {
    if cargo_command_re().find(line).is_none() {
        return;
    }
    // Skip cargo-generated or unknown metadata — only flag when we have facts.
    if md.packages.is_empty() && md.bins.is_empty() {
        return;
    }

    if let Some(caps) = package_arg_re().captures(line) {
        let name = caps.get(1).unwrap().as_str();
        if !md.knows_package(name) {
            out.push(Divergence {
                rule: RuleId::GhostCommand,
                severity: Severity::Warning,
                location: Location::new(path.to_path_buf(), line_no),
                stated: format!("CI runs `cargo` against package `{name}`"),
                reality: format!("`{name}` is not a member of the workspace"),
                risk: "CI exercises a target that no longer exists; the step is a no-op at best."
                    .to_string(),
                attribution: None,

            });
        }
    }

    if let Some(caps) = bin_arg_re().captures(line) {
        let name = caps.get(1).unwrap().as_str();
        if !md.knows_bin(name) {
            out.push(Divergence {
                rule: RuleId::GhostCommand,
                severity: Severity::Warning,
                location: Location::new(path.to_path_buf(), line_no),
                stated: format!("CI builds/runs bin `{name}`"),
                reality: format!("no bin target named `{name}` in the workspace"),
                risk: "CI step refers to a bin that doesn't exist.".to_string(),
                attribution: None,

            });
        }
    }
}

#[cfg(test)]
mod tests_inner {
    use super::*;
    use std::path::PathBuf;

    fn ctx_with(path: PathBuf, mk: bool, yml: bool) -> ProjectContext {
        let tmp = path.parent().unwrap().to_path_buf();
        let mut ctx = ProjectContext::new(tmp);
        if mk {
            ctx.makefile_files.push(path);
        } else if yml {
            ctx.yaml_files.push(path);
        }
        ctx
    }

    fn md(packages: &[&str], bins: &[&str]) -> CargoMetadata {
        CargoMetadata {
            packages: packages.iter().map(|s| s.to_string()).collect(),
            bins: bins.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn flags_unknown_package_in_makefile() {
        let tmp = tempfile::tempdir().unwrap();
        let mk = tmp.path().join("Makefile");
        std::fs::write(&mk, "test:\n\tcargo test --package legacy_crate\n").unwrap();

        let ctx = ctx_with(mk, true, false);
        let analyzer = CiAnalyzer::with_metadata(md(&["spec-drift"], &["spec-drift"]));
        let divs = analyzer.analyze(&ctx);

        assert_eq!(divs.len(), 1);
        assert_eq!(divs[0].rule, RuleId::GhostCommand);
        assert_eq!(divs[0].severity, Severity::Warning);
    }

    #[test]
    fn accepts_known_package() {
        let tmp = tempfile::tempdir().unwrap();
        let mk = tmp.path().join("Makefile");
        std::fs::write(&mk, "test:\n\tcargo test --package spec-drift\n").unwrap();

        let ctx = ctx_with(mk, true, false);
        let analyzer = CiAnalyzer::with_metadata(md(&["spec-drift"], &[]));
        assert!(analyzer.analyze(&ctx).is_empty());
    }

    #[test]
    fn flags_unknown_bin_in_workflow() {
        let tmp = tempfile::tempdir().unwrap();
        let workflows = tmp.path().join(".github").join("workflows");
        std::fs::create_dir_all(&workflows).unwrap();
        let yml = workflows.join("ci.yml");
        std::fs::write(&yml, "jobs:\n  t:\n    steps:\n      - run: cargo run --bin legacy\n").unwrap();

        let mut ctx = ProjectContext::new(tmp.path());
        ctx.yaml_files.push(yml);
        let analyzer = CiAnalyzer::with_metadata(md(&["spec-drift"], &["spec-drift"]));
        let divs = analyzer.analyze(&ctx);

        assert_eq!(divs.len(), 1);
        assert_eq!(divs[0].rule, RuleId::GhostCommand);
    }

    #[test]
    fn ignores_yaml_outside_github_workflows() {
        let tmp = tempfile::tempdir().unwrap();
        let yml = tmp.path().join("deploy.yml");
        std::fs::write(&yml, "run: cargo run --bin legacy\n").unwrap();

        let mut ctx = ProjectContext::new(tmp.path());
        ctx.yaml_files.push(yml);
        let analyzer = CiAnalyzer::with_metadata(md(&["spec-drift"], &[]));
        assert!(analyzer.analyze(&ctx).is_empty());
    }

    #[test]
    fn noop_when_metadata_unavailable() {
        let tmp = tempfile::tempdir().unwrap();
        let mk = tmp.path().join("Makefile");
        std::fs::write(&mk, "cargo test --package ghost\n").unwrap();
        let ctx = ctx_with(mk, true, false);
        // Empty metadata — the analyzer must fail-open, never spurious-positive.
        let analyzer = CiAnalyzer::with_metadata(CargoMetadata::default());
        assert!(analyzer.analyze(&ctx).is_empty());
    }
}
