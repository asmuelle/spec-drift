pub mod fix_prompt;
pub mod human;
pub mod json;
pub mod sarif;

pub use fix_prompt::FixPromptReporter;
pub use human::HumanReporter;
pub use json::JsonReporter;
pub use sarif::SarifReporter;

use crate::domain::Divergence;

pub trait Reporter {
    fn render(&self, divergences: &[Divergence]) -> String;
}
