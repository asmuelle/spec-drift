pub mod ci;
pub mod constraint;
pub mod coverage;
pub mod deprecated;
pub mod docs;
pub mod env;
pub mod examples;
pub mod logic_gap;
pub mod outdated_logic;
pub mod tests;

use crate::context::ProjectContext;
use crate::domain::Divergence;

/// A `DriftAnalyzer` owns one coherence pillar (docs / examples / tests / CI).
///
/// Analyzers are independent by construction — they must not depend on each
/// other's output. `Send + Sync` lets the harness run analyzers in parallel
/// under `rayon::par_iter`.
pub trait DriftAnalyzer: Send + Sync {
    fn id(&self) -> &'static str;
    fn analyze(&self, ctx: &ProjectContext) -> Vec<Divergence>;
}

pub use ci::{CargoMetadata, CiAnalyzer};
pub use constraint::ConstraintAnalyzer;
pub use coverage::MissingCoverageAnalyzer;
pub use deprecated::DeprecatedUsageAnalyzer;
pub use docs::DocsAnalyzer;
pub use env::EnvMismatchAnalyzer;
pub use examples::{CargoRunner, ExamplesAnalyzer};
pub use logic_gap::LogicGapAnalyzer;
pub use outdated_logic::OutdatedLogicAnalyzer;
pub use tests::TestsAnalyzer;
