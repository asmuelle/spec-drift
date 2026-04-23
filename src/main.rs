use clap::Parser;
use spec_drift::analyzers::{
    CiAnalyzer, ConstraintAnalyzer, DeprecatedUsageAnalyzer, DocsAnalyzer, DriftAnalyzer,
    ExamplesAnalyzer, MissingCoverageAnalyzer, TestsAnalyzer,
};
use spec_drift::baseline;
use spec_drift::config::Config;
use spec_drift::context::ProjectContext;
use spec_drift::domain::Severity;
use spec_drift::parsers::RustParser;
use spec_drift::reporters::{
    FixPromptReporter, HumanReporter, JsonReporter, Reporter, SarifReporter,
};
use spec_drift::sources::{FsWalker, GitHistory};
use spec_drift::suppress;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser, Debug)]
#[command(
    name = "spec-drift",
    version,
    about = "Semantic coherence analysis between specification and implementation."
)]
struct Cli {
    /// Project root (defaults to the current directory).
    #[arg(long, default_value = ".")]
    root: PathBuf,

    /// Output format.
    #[arg(long, value_parser = ["human", "json", "sarif"], default_value = "human")]
    format: String,

    /// Emit a structured correction prompt instead of a report. Overrides --format.
    #[arg(long)]
    fix_prompt: bool,

    /// Path to `spec-drift.toml`. Defaults to walking up from `--root`.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Baseline JSON file. Divergences present in the baseline are not re-reported.
    #[arg(long)]
    baseline: Option<PathBuf>,

    /// Only analyze files changed since this git ref (e.g. `HEAD`, `origin/main`).
    #[arg(long)]
    diff: Option<String>,

    /// Run only the docs pillar.
    #[arg(long, conflicts_with_all = ["examples", "tests", "ci"])]
    docs: bool,

    /// Run only the examples pillar.
    #[arg(long, conflicts_with_all = ["docs", "tests", "ci"])]
    examples: bool,

    /// Run only the tests pillar.
    #[arg(long, conflicts_with_all = ["docs", "examples", "ci"])]
    tests: bool,

    /// Run only the CI pillar.
    #[arg(long, conflicts_with_all = ["docs", "examples", "tests"])]
    ci: bool,

    /// Exit non-zero only when a divergence at or above this severity exists.
    #[arg(long, value_parser = ["notice", "warning", "critical"], default_value = "notice")]
    deny: String,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(exit) => exit,
        Err(e) => {
            eprintln!("spec-drift: {e}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    let root = cli.root.canonicalize()?;

    let config_path = cli
        .config
        .clone()
        .or_else(|| Config::discover(&root))
        .unwrap_or_else(|| root.join("spec-drift.toml"));
    let config = Config::load(&config_path)?;

    let mut files = FsWalker::walk(&root)?;
    if let Some(reference) = cli.diff.as_deref() {
        match GitHistory::changed_files(&root, reference) {
            Some(changed) => {
                files = GitHistory::narrow(files, &changed);
            }
            None => {
                eprintln!(
                    "spec-drift: --diff {reference}: git unavailable or ref unknown; \
                     scanning full tree."
                );
            }
        }
    }

    let mut ctx = ProjectContext::new(&root);
    ctx.rust_files = files.rust;
    ctx.markdown_files = files.markdown;
    ctx.yaml_files = files.yaml;
    ctx.makefile_files = files.makefiles;

    for rs in &ctx.rust_files {
        match RustParser::parse(rs) {
            Ok(facts) => ctx.code_facts.extend(facts),
            Err(e) => eprintln!("spec-drift: skipping {}: {e}", rs.display()),
        }
    }

    let analyzers = select_analyzers(&cli, &config);
    let mut divergences = spec_drift::run(&ctx, &analyzers);
    divergences = spec_drift::apply_config(divergences, &config, &root);
    divergences = suppress::apply_inline_ignores(divergences);

    if let Some(baseline_path) = cli.baseline.as_ref() {
        let baseline = baseline::load(baseline_path)?;
        divergences = baseline::subtract(divergences, &baseline);
    }

    let rendered = if cli.fix_prompt {
        FixPromptReporter.render(&divergences)
    } else {
        match cli.format.as_str() {
            "json" => JsonReporter.render(&divergences),
            "sarif" => SarifReporter.render(&divergences),
            _ => HumanReporter.render(&divergences),
        }
    };
    print!("{rendered}");

    let threshold = parse_severity(&cli.deny);
    let has_blocking = divergences.iter().any(|d| d.severity >= threshold);
    if has_blocking {
        Ok(ExitCode::from(1))
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

fn parse_severity(s: &str) -> Severity {
    match s {
        "critical" => Severity::Critical,
        "warning" => Severity::Warning,
        _ => Severity::Notice,
    }
}

fn select_analyzers(cli: &Cli, config: &Config) -> Vec<Box<dyn DriftAnalyzer>> {
    let any_specific = cli.docs || cli.examples || cli.tests || cli.ci;

    let mut v: Vec<Box<dyn DriftAnalyzer>> = Vec::new();
    if !any_specific || cli.docs {
        v.push(Box::new(DocsAnalyzer::default()));
        v.push(Box::new(MissingCoverageAnalyzer));
        if !config.constraint_rules.is_empty() {
            v.push(Box::new(ConstraintAnalyzer::new(
                config.constraint_rules.clone(),
            )));
        }
    }
    if !any_specific || cli.examples {
        v.push(Box::new(ExamplesAnalyzer::default()));
        v.push(Box::new(DeprecatedUsageAnalyzer::default()));
    }
    if !any_specific || cli.tests {
        v.push(Box::new(TestsAnalyzer));
    }
    if !any_specific || cli.ci {
        v.push(Box::new(CiAnalyzer::default()));
    }
    v
}
