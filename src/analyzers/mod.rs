pub mod docs;

use crate::context::ProjectContext;
use crate::domain::Divergence;

/// A `DriftAnalyzer` owns one coherence pillar (docs / examples / tests / CI).
///
/// Analyzers are independent by construction — they must not depend on each
/// other's output. This keeps the architecture composable and makes it safe
/// to parallelize analyzer execution in the future.
pub trait DriftAnalyzer {
    fn id(&self) -> &'static str;
    fn analyze(&self, ctx: &ProjectContext) -> Vec<Divergence>;
}

pub use docs::DocsAnalyzer;
