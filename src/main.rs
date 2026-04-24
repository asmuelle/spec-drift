use clap::Parser;
use spec_drift::{Pillar, RunConfig, Severity};
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

    /// In a cargo workspace, restrict analysis to the named member.
    #[arg(long)]
    package: Option<String>,

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

    /// Promote every non-deterministic rule by one severity level.
    #[arg(long)]
    strict: bool,

    /// Disable every LLM-backed rule, regardless of `[llm]` config.
    #[arg(long)]
    no_llm: bool,

    /// Attribute each divergence to the commit/author that wrote the source line.
    /// Spawns one `git blame` per divergence; off by default.
    #[arg(long)]
    blame: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match spec_drift::run_cli(&into_run_config(cli)) {
        Ok(exit) => exit,
        Err(e) => {
            eprintln!("spec-drift: {e}");
            ExitCode::from(2)
        }
    }
}

fn into_run_config(cli: Cli) -> RunConfig {
    let pillar = if cli.docs {
        Pillar::Docs
    } else if cli.examples {
        Pillar::Examples
    } else if cli.tests {
        Pillar::Tests
    } else if cli.ci {
        Pillar::Ci
    } else {
        Pillar::All
    };

    let deny = match cli.deny.as_str() {
        "critical" => Severity::Critical,
        "warning" => Severity::Warning,
        _ => Severity::Notice,
    };

    RunConfig {
        root: cli.root,
        pillar,
        format: cli.format,
        fix_prompt: cli.fix_prompt,
        config: cli.config,
        baseline: cli.baseline,
        diff: cli.diff,
        package: cli.package,
        deny,
        strict: cli.strict,
        no_llm: cli.no_llm,
        blame: cli.blame,
    }
}
