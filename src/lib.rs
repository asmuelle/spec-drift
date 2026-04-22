//! `spec-drift` — semantic coherence analysis between a project's
//! specification surfaces (README, AGENTS.md, examples, CI) and its Rust code.
//!
//! The library exposes the domain model, analyzers, and reporters so they can
//! be embedded in editors or other tools. The `spec-drift` binary is a thin
//! CLI wrapper over [`run`].

pub mod analyzers;
pub mod context;
pub mod domain;
pub mod error;
pub mod parsers;
pub mod reporters;
pub mod sources;

pub use context::ProjectContext;
pub use domain::{
    ClaimKind, CodeFact, Divergence, FactKind, Location, RuleId, Severity, SpecClaim,
};
pub use error::SpecDriftError;

use analyzers::DriftAnalyzer;

/// Execute every analyzer and return divergences sorted deterministically by
/// `(file, line, rule)` so output can be diffed between runs.
pub fn run(ctx: &ProjectContext, analyzers: &[Box<dyn DriftAnalyzer>]) -> Vec<Divergence> {
    let mut all: Vec<Divergence> = analyzers.iter().flat_map(|a| a.analyze(ctx)).collect();

    all.sort_by(|a, b| {
        a.location
            .file
            .cmp(&b.location.file)
            .then_with(|| a.location.line.cmp(&b.location.line))
            .then_with(|| a.rule.as_str().cmp(b.rule.as_str()))
    });
    all
}
