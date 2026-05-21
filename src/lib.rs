//! `spec-drift` — semantic coherence analysis between a project's
//! specification surfaces (README, AGENTS.md, examples, CI) and its Rust code.
//!
//! The library exposes the domain model, analyzers, and reporters so they can
//! be embedded in editors or other tools. The `spec-drift` binary is a thin
//! CLI wrapper over [`run_cli`].

pub mod analyzers;
pub mod auto_fix;
pub mod baseline;
pub mod blame;
pub mod config;
pub mod context;
pub mod domain;
pub mod error;
pub mod llm;
pub mod parsers;
pub mod reporters;
pub mod sources;
pub mod suppress;
pub mod workspace;

pub use config::{Config, ConfigSource};
pub use context::ProjectContext;
pub use domain::{
    Attribution, ClaimKind, CodeFact, Confidence, Divergence, FactKind, Location, RuleId, Severity,
    SpecClaim,
};
pub use error::SpecDriftError;

use analyzers::DriftAnalyzer;
use rayon::prelude::*;
use reporters::Reporter;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;

// ---------------------------------------------------------------------------
// Pipeline
// ---------------------------------------------------------------------------

/// Execute every analyzer in parallel and return divergences sorted
/// deterministically by `(file, line, rule)` so output can be diffed between
/// runs.
///
/// Analyzers are independent by construction — see the `DriftAnalyzer` trait
/// docstring — so `par_iter` is safe and parallelizes nicely on multi-pillar
/// runs where one analyzer (e.g. `ExamplesAnalyzer`) spends most of its time
/// blocked on `cargo`.
pub fn run(ctx: &ProjectContext, analyzers: &[Box<dyn DriftAnalyzer>]) -> Vec<Divergence> {
    let mut all: Vec<Divergence> = analyzers
        .par_iter()
        .flat_map_iter(|a| a.analyze(ctx))
        .collect();

    all.sort_by(|a, b| {
        a.location
            .file
            .cmp(&b.location.file)
            .then_with(|| a.location.line.cmp(&b.location.line))
            .then_with(|| a.rule.as_str().cmp(b.rule.as_str()))
    });
    all
}

/// Apply a [`Config`] to a divergence set: drop suppressed items and apply
/// severity overrides.
pub fn apply_config(
    mut divs: Vec<Divergence>,
    cfg: &Config,
    root: &std::path::Path,
) -> Vec<Divergence> {
    divs.retain(|d| !cfg.is_suppressed(d, root));
    cfg.apply_severity_overrides(&mut divs);
    divs
}

/// Promote every non-deterministic divergence one severity level. Mirrors the
/// `--strict` CLI flag — deterministic rules stay untouched because they
/// already carry unambiguous verdicts.
pub fn apply_strict(mut divs: Vec<Divergence>) -> Vec<Divergence> {
    for d in &mut divs {
        if d.rule.confidence() != Confidence::Deterministic {
            d.severity = d.severity.promoted();
        }
    }
    divs
}

/// Render and baseline locations relative to the workspace root when possible.
/// Analyzers work with absolute paths so suppression and blame can read files
/// reliably; reporters should not leak machine-local temp or checkout paths.
pub fn normalize_locations(mut divs: Vec<Divergence>, root: &Path) -> Vec<Divergence> {
    for d in &mut divs {
        if let Ok(rel) = d.location.file.strip_prefix(root) {
            d.location.file = rel.to_path_buf();
        }
    }
    divs
}

// ---------------------------------------------------------------------------
// CLI orchestration
// ---------------------------------------------------------------------------

/// Pillar selection mirrors the CLI `--docs` / `--examples` / `--tests` / `--ci` flags.
#[derive(Debug, Clone, Copy)]
pub enum Pillar {
    All,
    Docs,
    Examples,
    Tests,
    Ci,
}

/// Everything the orchestration layer needs to run a spec-drift analysis.
#[derive(Debug, Clone)]
pub struct RunConfig {
    pub root: PathBuf,
    pub pillar: Pillar,
    pub format: String,
    pub fix_prompt: bool,
    pub config: Option<PathBuf>,
    pub baseline: Option<PathBuf>,
    pub diff: Option<String>,
    pub package: Option<String>,
    pub deny: Severity,
    pub strict: bool,
    pub no_llm: bool,
    pub blame: bool,
    pub fix: bool,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            root: PathBuf::from("."),
            pillar: Pillar::All,
            format: "human".into(),
            fix_prompt: false,
            config: None,
            baseline: None,
            diff: None,
            package: None,
            deny: Severity::Notice,
            strict: false,
            no_llm: false,
            blame: false,
            fix: false,
        }
    }
}

/// Full end-to-end run: discover config, walk files, parse sources, select and
/// run analyzers, apply suppression / baselines / strict, enrich with blame,
/// and render through the chosen reporter.
///
/// Returns `ExitCode::SUCCESS` when no blocking divergences remain, and
/// `ExitCode::from(1)` when the `deny` threshold is exceeded.
pub fn run_cli(cfg: &RunConfig) -> anyhow::Result<ExitCode> {
    let root = cfg.root.canonicalize()?;

    let (config_path, source) = if let Some(ref p) = cfg.config {
        (p.clone(), ConfigSource::Explicit)
    } else {
        let p = Config::discover(&root).unwrap_or_else(|| root.join("spec-drift.toml"));
        (p, ConfigSource::Discovered)
    };
    let config = Config::load(&config_path, source)?;

    let mut files = sources::FsWalker::walk(&root)?;
    let changed_files = if let Some(ref reference) = cfg.diff {
        match sources::GitHistory::changed_files(&root, reference) {
            Some(changed) => Some(changed),
            None => {
                eprintln!(
                    "spec-drift: --diff {reference}: git unavailable or ref unknown; \
                     scanning full tree."
                );
                None
            }
        }
    } else {
        None
    };

    let mut analysis_root = root.clone();
    if let Some(ref name) = cfg.package {
        let packages = workspace::load(&root);
        let pkg = workspace::find(&packages, name).map_err(anyhow::Error::msg)?;
        analysis_root = pkg.root.clone();
        files.rust = workspace::narrow_paths(files.rust, pkg);
        files.markdown = workspace::narrow_paths(files.markdown, pkg);
        files.yaml = workspace::narrow_paths(files.yaml, pkg);
        files.makefiles = workspace::narrow_paths(files.makefiles, pkg);
    }

    let diff_scope = changed_files.map(|changed| {
        let changed = if cfg.package.is_some() {
            changed
                .into_iter()
                .filter(|p| p.starts_with(&analysis_root))
                .collect()
        } else {
            changed
        };
        DiffScope::new(&root, changed)
    });

    let mut ctx = ProjectContext::new(&root);
    ctx.analysis_root = analysis_root.clone();
    ctx.rust_files = files.rust.clone();
    ctx.markdown_files = files.markdown.clone();
    ctx.yaml_files = files.yaml.clone();
    ctx.makefile_files = files.makefiles.clone();

    for rs in &ctx.rust_files {
        match parsers::RustParser::parse(rs) {
            Ok(facts) => ctx.code_facts.extend(facts),
            Err(e) => eprintln!("spec-drift: skipping {}: {e}", rs.display()),
        }
    }

    let analyzers = build_analyzers(cfg.pillar, &config, cfg.no_llm);
    let mut divergences = run(&ctx, &analyzers);
    if let Some(scope) = diff_scope.as_ref() {
        divergences = filter_to_diff_scope(divergences, &root, scope);
    }
    divergences = apply_config(divergences, &config, &root);
    divergences = suppress::apply_inline_ignores(divergences);
    divergences = normalize_locations(divergences, &root);

    if let Some(ref baseline_path) = cfg.baseline {
        let baseline = baseline::load(baseline_path)?;
        divergences = baseline::subtract(divergences, &baseline);
    }

    if cfg.strict {
        divergences = apply_strict(divergences);
    }

    if cfg.blame {
        divergences = blame::apply(divergences, &root, &blame::GitBlameEngine);
    }

    if cfg.fix {
        let applied = auto_fix::apply_fixes(&divergences, &root);
        eprintln!("spec-drift: applied {applied} auto-fix(es)");

        let llm_client = llm::build_client(&config.llm, cfg.no_llm);
        let nd_applied = apply_nondeterministic_fixes(
            &root,
            &analysis_root,
            &files,
            cfg.pillar,
            &config,
            cfg.no_llm,
            &divergences,
            &llm_client,
            &ctx,
        )?;
        if nd_applied > 0 {
            eprintln!("spec-drift: applied {nd_applied} coherency auto-correction(s)");
        }
    }

    let rendered = if cfg.fix_prompt {
        reporters::FixPromptReporter.render(&divergences)
    } else {
        match cfg.format.as_str() {
            "json" => reporters::JsonReporter.render(&divergences),
            "sarif" => reporters::SarifReporter.render(&divergences),
            _ => reporters::HumanReporter.render(&divergences),
        }
    };
    print!("{rendered}");

    let has_blocking = divergences.iter().any(|d| d.severity >= cfg.deny);
    if has_blocking {
        Ok(ExitCode::from(1))
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

struct DiffScope {
    changed: HashSet<PathBuf>,
    implementation_changed: bool,
    cargo_metadata_changed: bool,
}

impl DiffScope {
    fn new(root: &Path, changed: HashSet<PathBuf>) -> Self {
        let implementation_changed = changed.iter().any(|p| is_rust_implementation_path(root, p));
        let cargo_metadata_changed = changed.iter().any(|p| is_cargo_manifest_path(p));

        Self {
            changed,
            implementation_changed,
            cargo_metadata_changed,
        }
    }
}

fn filter_to_diff_scope(divs: Vec<Divergence>, root: &Path, scope: &DiffScope) -> Vec<Divergence> {
    divs.into_iter()
        .filter(|d| {
            divergence_is_in_changed_file(d, root, &scope.changed)
                || could_be_diff_induced(d, scope)
        })
        .collect()
}

fn divergence_is_in_changed_file(d: &Divergence, root: &Path, changed: &HashSet<PathBuf>) -> bool {
    if changed.contains(&d.location.file) {
        return true;
    }
    if d.location.file.is_relative() {
        return changed.contains(&root.join(&d.location.file));
    }
    if let Ok(rel) = d.location.file.strip_prefix(root) {
        return changed.contains(rel);
    }
    false
}

fn could_be_diff_induced(d: &Divergence, scope: &DiffScope) -> bool {
    match d.rule {
        RuleId::SymbolAbsence
        | RuleId::CompileFailure
        | RuleId::DeprecatedUsage
        | RuleId::OutdatedLogic
        | RuleId::LogicGap => scope.implementation_changed || scope.cargo_metadata_changed,
        RuleId::ConstraintViolation => scope.implementation_changed,
        RuleId::GhostCommand => scope.cargo_metadata_changed,
        RuleId::MissingCoverage | RuleId::LyingTest | RuleId::EnvMismatch => false,
    }
}

fn is_rust_implementation_path(root: &Path, path: &Path) -> bool {
    if path.extension().and_then(|e| e.to_str()) != Some("rs") {
        return false;
    }

    let rel = path.strip_prefix(root).unwrap_or(path);
    !rel.components()
        .any(|c| c.as_os_str() == "tests" || c.as_os_str() == "examples")
}

fn is_cargo_manifest_path(path: &Path) -> bool {
    path.file_name().and_then(|n| n.to_str()) == Some("Cargo.toml")
}

/// Build the analyzer list for the selected pillar(s), wiring the LLM client
/// once so it is shared across all LLM-backed analyzers.
fn build_analyzers(pillar: Pillar, config: &Config, no_llm: bool) -> Vec<Box<dyn DriftAnalyzer>> {
    let docs = matches!(pillar, Pillar::All | Pillar::Docs);
    let examples = matches!(pillar, Pillar::All | Pillar::Examples);
    let tests = matches!(pillar, Pillar::All | Pillar::Tests);
    let ci = matches!(pillar, Pillar::All | Pillar::Ci);

    let llm_client = llm::build_client(&config.llm, no_llm);

    let mut v: Vec<Box<dyn DriftAnalyzer>> = Vec::new();
    if docs {
        v.push(Box::new(analyzers::DocsAnalyzer::default()));
        v.push(Box::new(analyzers::MissingCoverageAnalyzer));
        if !config.constraint_rules.is_empty() {
            v.push(Box::new(analyzers::ConstraintAnalyzer::new(
                config.constraint_rules.clone(),
            )));
        }
        v.push(Box::new(analyzers::OutdatedLogicAnalyzer::new(
            llm_client.clone(),
        )));
    }
    if examples {
        v.push(Box::new(analyzers::ExamplesAnalyzer::default()));
        v.push(Box::new(analyzers::DeprecatedUsageAnalyzer::default()));
        v.push(Box::new(analyzers::LogicGapAnalyzer::new(
            llm_client.clone(),
        )));
    }
    if tests {
        v.push(Box::new(analyzers::TestsAnalyzer));
    }
    if ci {
        v.push(Box::new(analyzers::CiAnalyzer::default()));
        v.push(Box::new(analyzers::EnvMismatchAnalyzer));
    }
    v
}

#[allow(clippy::too_many_arguments)]
fn apply_nondeterministic_fixes(
    root: &Path,
    analysis_root: &Path,
    files: &sources::DiscoveredFiles,
    pillar: Pillar,
    config: &Config,
    no_llm: bool,
    divergences: &[Divergence],
    llm_client: &std::sync::Arc<dyn llm::LlmClient>,
    ctx: &ProjectContext,
) -> anyhow::Result<u32> {
    let mut applied = 0;

    for d in divergences {
        if d.rule == RuleId::OutdatedLogic {
            let abs_path = root.join(&d.location.file);
            if let Some((original_section, range)) =
                auto_fix::slice_markdown_section(&abs_path, d.location.line)
            {
                let code_context = auto_fix::build_outdated_logic_context(ctx, &original_section);
                let (system, user) = auto_fix::build_markdown_correction_prompt(
                    &original_section,
                    &code_context,
                    &d.reality,
                );

                eprintln!(
                    "  Coherency Auto-Correction (OutdatedLogic): {}",
                    d.location.file.display()
                );
                if let Some(corrected) = llm_client.complete(&system, &user) {
                    let corrected = corrected.trim().to_string();
                    if !corrected.is_empty() {
                        let original_content = std::fs::read_to_string(&abs_path)?;
                        let mut new_content = original_content.clone();
                        new_content.replace_range(range.clone(), &corrected);

                        // Transaction: write new content
                        std::fs::write(&abs_path, &new_content)?;

                        // Validation sweep
                        if run_validation_sweep(root, analysis_root, files, pillar, config, no_llm)
                        {
                            eprintln!("    [OK] Passed validation!");
                            applied += 1;
                        } else {
                            eprintln!(
                                "    [ROLLBACK] Validation failed (compile error or critical regression). Rolling back..."
                            );
                            std::fs::write(&abs_path, &original_content)?;
                        }
                    }
                }
            }
        } else if d.rule == RuleId::LogicGap {
            let abs_path = root.join(&d.location.file);
            if let Some((original_narrative, range)) = auto_fix::slice_example_narrative(&abs_path)
            {
                let public_signatures = auto_fix::collect_public_signatures(ctx);
                let (system, user) = auto_fix::build_example_narrative_prompt(
                    &original_narrative,
                    &public_signatures,
                    &d.reality,
                );

                eprintln!(
                    "  Coherency Auto-Correction (LogicGap): {}",
                    d.location.file.display()
                );
                if let Some(corrected_narrative) = llm_client.complete(&system, &user) {
                    let original_content = std::fs::read_to_string(&abs_path)?;
                    let first_line = original_content[range.clone()].lines().next().unwrap_or("");
                    let prefix = if first_line.trim_start().starts_with("//!") {
                        "//!"
                    } else {
                        "//"
                    };

                    let formatted_comments =
                        auto_fix::format_as_comments(&corrected_narrative, prefix);
                    let mut new_content = original_content.clone();
                    new_content.replace_range(range.clone(), &formatted_comments);

                    // Transaction: write new content
                    std::fs::write(&abs_path, &new_content)?;

                    // Validation sweep
                    if run_validation_sweep(root, analysis_root, files, pillar, config, no_llm) {
                        eprintln!("    [OK] Passed validation!");
                        applied += 1;
                    } else {
                        eprintln!(
                            "    [ROLLBACK] Validation failed (compile error or critical regression). Rolling back..."
                        );
                        std::fs::write(&abs_path, &original_content)?;
                    }
                }
            }
        }
    }

    Ok(applied)
}

fn run_validation_sweep(
    root: &Path,
    analysis_root: &Path,
    files: &sources::DiscoveredFiles,
    pillar: Pillar,
    config: &Config,
    no_llm: bool,
) -> bool {
    let mut val_ctx = ProjectContext::new(root);
    val_ctx.analysis_root = analysis_root.to_path_buf();
    val_ctx.rust_files = files.rust.clone();
    val_ctx.markdown_files = files.markdown.clone();
    val_ctx.yaml_files = files.yaml.clone();
    val_ctx.makefile_files = files.makefiles.clone();

    for rs in &val_ctx.rust_files {
        if let Ok(facts) = parsers::RustParser::parse(rs) {
            val_ctx.code_facts.extend(facts);
        }
    }

    let analyzers = build_analyzers(pillar, config, no_llm);
    let divergences = run(&val_ctx, &analyzers);
    let divergences = apply_config(divergences, config, root);
    let divergences = suppress::apply_inline_ignores(divergences);

    for d in &divergences {
        if d.rule == RuleId::CompileFailure || d.severity == Severity::Critical {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Location, RuleId};

    fn div(path: &str) -> Divergence {
        Divergence {
            rule: RuleId::SymbolAbsence,
            severity: Severity::Critical,
            location: Location::new(path, 1),
            stated: "x".into(),
            reality: "y".into(),
            risk: "z".into(),
            attribution: None,
        }
    }

    #[test]
    fn diff_filter_matches_relative_locations_against_changed_absolute_paths() {
        let root = Path::new("/repo");
        let scope = DiffScope::new(root, HashSet::from([PathBuf::from("/repo/README.md")]));

        let out = filter_to_diff_scope(vec![div("README.md"), div("src/lib.rs")], root, &scope);

        assert_eq!(out.len(), 1);
        assert_eq!(out[0].location.file, PathBuf::from("README.md"));
    }

    #[test]
    fn diff_filter_keeps_doc_drift_when_implementation_changed() {
        let root = Path::new("/repo");
        let scope = DiffScope::new(root, HashSet::from([PathBuf::from("/repo/src/lib.rs")]));

        let out = filter_to_diff_scope(vec![div("README.md")], root, &scope);

        assert_eq!(out.len(), 1);
        assert_eq!(out[0].rule, RuleId::SymbolAbsence);
    }

    #[test]
    fn diff_filter_keeps_ci_drift_when_cargo_manifest_changed() {
        let root = Path::new("/repo");
        let scope = DiffScope::new(root, HashSet::from([PathBuf::from("/repo/Cargo.toml")]));
        let ghost = Divergence {
            rule: RuleId::GhostCommand,
            location: Location::new(".github/workflows/ci.yml", 10),
            ..div(".github/workflows/ci.yml")
        };

        let out = filter_to_diff_scope(vec![ghost], root, &scope);

        assert_eq!(out.len(), 1);
        assert_eq!(out[0].rule, RuleId::GhostCommand);
    }

    #[test]
    fn normalize_locations_strips_workspace_root() {
        let root = Path::new("/repo");
        let out = normalize_locations(
            vec![div("/repo/README.md"), div("/elsewhere/file.md")],
            root,
        );

        assert_eq!(out[0].location.file, PathBuf::from("README.md"));
        assert_eq!(out[1].location.file, PathBuf::from("/elsewhere/file.md"));
    }

    struct MockLlmClient {
        complete_val: String,
    }
    impl llm::LlmClient for MockLlmClient {
        fn evaluate(&self, _: &str, _: &str) -> Option<crate::llm::LlmVerdict> {
            None
        }
        fn complete(&self, _: &str, _: &str) -> Option<String> {
            Some(self.complete_val.clone())
        }
    }

    #[test]
    fn test_nondeterministic_fixes_transaction_success_and_rollback() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        let readme_path = root.join("README.md");
        std::fs::write(
            &readme_path,
            "## Section 1\nUse `old_function()` to start.\n",
        )
        .unwrap();

        let lib_path = root.join("src/lib.rs");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(&lib_path, "pub fn new_function() {}\n").unwrap();

        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();

        let files = sources::DiscoveredFiles {
            rust: vec![lib_path.clone()],
            markdown: vec![readme_path.clone()],
            yaml: vec![],
            makefiles: vec![],
        };

        let mut ctx = ProjectContext::new(root);
        ctx.analysis_root = root.to_path_buf();
        ctx.rust_files = vec![lib_path.clone()];
        ctx.markdown_files = vec![readme_path.clone()];
        ctx.code_facts = crate::parsers::RustParser::parse(&lib_path).unwrap();

        let divergences = vec![Divergence {
            rule: RuleId::OutdatedLogic,
            severity: Severity::Notice,
            location: Location::new("README.md", 1),
            stated: "section describes current behavior".into(),
            reality: "Outdated!".into(),
            risk: "Docs teach behavior the code no longer implements.".into(),
            attribution: None,
        }];

        let mock_client_success: std::sync::Arc<dyn llm::LlmClient> =
            std::sync::Arc::new(MockLlmClient {
                complete_val: "## Section 1\nUse `new_function()` to start.\n".to_string(),
            });

        let config = Config::default();

        let applied_success = apply_nondeterministic_fixes(
            root,
            root,
            &files,
            Pillar::Docs,
            &config,
            false,
            &divergences,
            &mock_client_success,
            &ctx,
        )
        .unwrap();

        assert_eq!(applied_success, 1);
        let content_success = std::fs::read_to_string(&readme_path).unwrap();
        assert!(content_success.contains("new_function"));

        std::fs::write(
            &readme_path,
            "## Section 1\nUse `old_function()` to start.\n",
        )
        .unwrap();

        let mock_client_fail: std::sync::Arc<dyn llm::LlmClient> =
            std::sync::Arc::new(MockLlmClient {
                complete_val: "## Section 1\nUse `missing_function()` to start.\n".to_string(),
            });

        let applied_fail = apply_nondeterministic_fixes(
            root,
            root,
            &files,
            Pillar::Docs,
            &config,
            false,
            &divergences,
            &mock_client_fail,
            &ctx,
        )
        .unwrap();

        assert_eq!(applied_fail, 0);
        let content_fail = std::fs::read_to_string(&readme_path).unwrap();
        assert!(content_fail.contains("old_function"));
        assert!(!content_fail.contains("missing_function"));
    }
}
