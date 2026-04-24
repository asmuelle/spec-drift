//! `spec-drift` — semantic coherence analysis between a project's
//! specification surfaces (README, AGENTS.md, examples, CI) and its Rust code.
//!
//! The library exposes the domain model, analyzers, and reporters so they can
//! be embedded in editors or other tools. The `spec-drift` binary is a thin
//! CLI wrapper over [`run_cli`].

pub mod analyzers;
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
    Attribution, ClaimKind, CodeFact, Confidence, Divergence, FactKind, Location, RuleId,
    Severity, SpecClaim,
};
pub use error::SpecDriftError;

use analyzers::DriftAnalyzer;
use rayon::prelude::*;
use reporters::Reporter;
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
    if let Some(ref reference) = cfg.diff {
        match sources::GitHistory::changed_files(&root, reference) {
            Some(changed) => {
                files = sources::GitHistory::narrow(files, &changed);
            }
            None => {
                eprintln!(
                    "spec-drift: --diff {reference}: git unavailable or ref unknown; \
                     scanning full tree."
                );
            }
        }
    }

    if let Some(ref name) = cfg.package {
        let packages = workspace::load(&root);
        let pkg = workspace::find(&packages, name).map_err(anyhow::Error::msg)?;
        files.rust = workspace::narrow_paths(files.rust, pkg);
        files.markdown = workspace::narrow_paths(files.markdown, pkg);
        files.yaml = workspace::narrow_paths(files.yaml, pkg);
        files.makefiles = workspace::narrow_paths(files.makefiles, pkg);
    }

    let mut ctx = ProjectContext::new(&root);
    ctx.rust_files = files.rust;
    ctx.markdown_files = files.markdown;
    ctx.yaml_files = files.yaml;
    ctx.makefile_files = files.makefiles;

    for rs in &ctx.rust_files {
        match parsers::RustParser::parse(rs) {
            Ok(facts) => ctx.code_facts.extend(facts),
            Err(e) => eprintln!("spec-drift: skipping {}: {e}", rs.display()),
        }
    }

    let analyzers = build_analyzers(cfg.pillar, &config, cfg.no_llm);
    let mut divergences = run(&ctx, &analyzers);
    divergences = apply_config(divergences, &config, &root);
    divergences = suppress::apply_inline_ignores(divergences);

    if let Some(ref baseline_path) = cfg.baseline {
        let baseline = baseline::load(baseline_path)?;
        divergences = baseline::subtract(divergences, &baseline);
    }

    if cfg.strict {
        divergences = apply_strict(divergences);
    }

    if cfg.blame {
        divergences =
            blame::apply(divergences, &root, &blame::GitBlameEngine);
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

/// Build the analyzer list for the selected pillar(s), wiring the LLM client
/// once so it is shared across all LLM-backed analyzers.
fn build_analyzers(
    pillar: Pillar,
    config: &Config,
    no_llm: bool,
) -> Vec<Box<dyn DriftAnalyzer>> {
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
