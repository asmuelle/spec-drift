pub mod human;
pub mod json;

pub use human::HumanReporter;
pub use json::JsonReporter;

use crate::domain::Divergence;

pub trait Reporter {
    fn render(&self, divergences: &[Divergence]) -> String;
}
