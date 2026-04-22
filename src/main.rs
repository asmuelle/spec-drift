use clap::Parser;
use spec_drift::analyzers::{DocsAnalyzer, DriftAnalyzer};
use spec_drift::context::ProjectContext;
use spec_drift::parsers::RustParser;
use spec_drift::reporters::{HumanReporter, JsonReporter, Reporter};
use spec_drift::sources::FsWalker;
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
    #[arg(long, value_parser = ["human", "json"], default_value = "human")]
    format: String,
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
    let files = FsWalker::walk(&root)?;

    let mut ctx = ProjectContext::new(&root);
    ctx.rust_files = files.rust;
    ctx.markdown_files = files.markdown;
    ctx.yaml_files = files.yaml;

    for rs in &ctx.rust_files {
        match RustParser::parse(rs) {
            Ok(facts) => ctx.code_facts.extend(facts),
            Err(e) => eprintln!("spec-drift: skipping {}: {e}", rs.display()),
        }
    }

    let analyzers: Vec<Box<dyn DriftAnalyzer>> = vec![Box::new(DocsAnalyzer::default())];
    let divergences = spec_drift::run(&ctx, &analyzers);

    let rendered = match cli.format.as_str() {
        "json" => JsonReporter.render(&divergences),
        _ => HumanReporter.render(&divergences),
    };
    print!("{rendered}");

    if divergences.is_empty() {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(1))
    }
}
