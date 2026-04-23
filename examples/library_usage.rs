//! Programmatic use of the `spec-drift` library.
//!
//! Demonstrates how to embed drift analysis in your own tooling: build a
//! `ProjectContext`, parse sources, run a custom selection of analyzers, and
//! render divergences through any `Reporter`.
//!
//! Run against the current directory:
//!
//! ```bash
//! cargo run --example library_usage
//! ```
//!
//! Or against an external project:
//!
//! ```bash
//! cargo run --example library_usage -- /path/to/project
//! ```

use spec_drift::analyzers::{DocsAnalyzer, DriftAnalyzer, MissingCoverageAnalyzer, TestsAnalyzer};
use spec_drift::parsers::RustParser;
use spec_drift::reporters::{HumanReporter, Reporter};
use spec_drift::sources::FsWalker;
use spec_drift::{Config, ProjectContext};
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let root: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .canonicalize()?;

    // `[severity]` / `[ignore]` overrides, if a config exists. A missing file
    // is fine — Config::load returns the default in that case.
    let config_path = Config::discover(&root).unwrap_or_else(|| root.join("spec-drift.toml"));
    let config = Config::load(&config_path)?;

    // Bucket every source file in the project by kind, respecting .gitignore.
    let files = FsWalker::walk(&root)?;
    let mut ctx = ProjectContext::new(&root);
    ctx.rust_files = files.rust;
    ctx.markdown_files = files.markdown;
    ctx.yaml_files = files.yaml;
    ctx.makefile_files = files.makefiles;

    // Parse every Rust file once into the shared fact index. Analyzers read
    // from `ctx.code_facts` rather than re-parsing.
    for rs in &ctx.rust_files {
        if let Ok(facts) = RustParser::parse(rs) {
            ctx.code_facts.extend(facts);
        }
    }

    // Pick the pure-Rust subset of analyzers — no cargo shell-outs, no LLM.
    // This is what you'd run from an editor plugin or a pre-commit hook.
    let analyzers: Vec<Box<dyn DriftAnalyzer>> = vec![
        Box::new(DocsAnalyzer::default()),
        Box::new(MissingCoverageAnalyzer),
        Box::new(TestsAnalyzer),
    ];

    let divergences = spec_drift::run(&ctx, &analyzers);
    let divergences = spec_drift::apply_config(divergences, &config, &root);
    print!("{}", HumanReporter.render(&divergences));

    Ok(())
}
