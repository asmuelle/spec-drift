use super::Reporter;
use crate::domain::Divergence;

pub struct JsonReporter;

impl Reporter for JsonReporter {
    fn render(&self, divergences: &[Divergence]) -> String {
        serde_json::to_string_pretty(divergences).unwrap_or_else(|_| "[]".to_string())
    }
}
