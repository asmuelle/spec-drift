use super::{LlmClient, LlmVerdict};

/// LLM client that always returns `None`. Used whenever the `[llm]` block is
/// disabled, `--no-llm` is set, or a real provider isn't configured. Making
/// the disabled-by-default path a first-class implementation means analyzers
/// never need a `if let Some(client) = ...` branch.
pub struct NullLlmClient;

impl LlmClient for NullLlmClient {
    fn evaluate(&self, _system_prompt: &str, _user_prompt: &str) -> Option<LlmVerdict> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn always_returns_none() {
        assert!(NullLlmClient.evaluate("x", "y").is_none());
    }
}
