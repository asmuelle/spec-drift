//! LLM-backed rule support.
//!
//! Everything in this module is strictly opt-in: the default [`Config::llm`]
//! has `enabled = false`, and `--no-llm` on the CLI forces the null client
//! regardless of config. Analyzers that consume an [`LlmClient`] receive a
//! [`NullLlmClient`] in every default code path.
//!
//! # Design
//!
//! - The trait returns `Option<LlmVerdict>`. `None` is the universal
//!   "no answer available" signal — used for: LLM disabled, budget exhausted,
//!   network error, timeout, malformed response. Analyzers must treat `None`
//!   as "skip silently, don't emit drift" because the rule is experimental.
//! - The budget is enforced via an atomic counter on the wrapper. Analyzers
//!   can't accidentally overspend even under `rayon` parallelism.

pub mod anthropic;
pub mod null;

use crate::config::{LlmConfig, LlmProvider};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

pub use anthropic::AnthropicLlmClient;
pub use null::NullLlmClient;

/// Verdict an LLM returns when asked whether a spec claim still matches code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmVerdict {
    /// True when the spec text still accurately describes the code.
    pub match_spec: bool,
    /// Short human-readable explanation — shown in the divergence report.
    pub reason: String,
}

/// Minimum surface every LLM provider must implement.
pub trait LlmClient: Send + Sync {
    /// Ask the model whether `user_prompt` holds given the project context in
    /// `system_prompt`. Returns `None` on any failure mode — analyzers treat
    /// `None` as "skip, don't flag."
    fn evaluate(&self, system_prompt: &str, user_prompt: &str) -> Option<LlmVerdict>;
}

/// Budget + kill-switch wrapper. Every real LLM call flows through here.
pub struct BudgetedClient {
    inner: Arc<dyn LlmClient>,
    remaining: AtomicU32,
}

impl BudgetedClient {
    pub fn new(inner: Arc<dyn LlmClient>, budget: u32) -> Self {
        Self {
            inner,
            remaining: AtomicU32::new(budget),
        }
    }
}

impl LlmClient for BudgetedClient {
    fn evaluate(&self, system_prompt: &str, user_prompt: &str) -> Option<LlmVerdict> {
        // Atomic fetch-decrement with saturating underflow guard.
        loop {
            let current = self.remaining.load(Ordering::Acquire);
            if current == 0 {
                return None;
            }
            if self
                .remaining
                .compare_exchange(current, current - 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
        }
        self.inner.evaluate(system_prompt, user_prompt)
    }
}

/// Build the LLM client stack described by `cfg`. `force_off=true` mirrors
/// `--no-llm` and always yields a null client, regardless of config.
///
/// Providers other than `Anthropic` are not implemented yet — they fall back
/// to the null client with a warning so config authors can migrate once the
/// adapters ship, without breaking existing runs.
pub fn build_client(cfg: &LlmConfig, force_off: bool) -> Arc<dyn LlmClient> {
    if force_off || !cfg.enabled {
        return Arc::new(NullLlmClient);
    }

    let inner: Arc<dyn LlmClient> = match cfg.provider {
        LlmProvider::Anthropic => {
            match AnthropicLlmClient::from_env(cfg.model.clone(), cfg.timeout_s) {
                Some(c) => Arc::new(c),
                None => {
                    eprintln!(
                        "spec-drift: [llm] enabled but ANTHROPIC_API_KEY is not set; \
                     skipping LLM rules for this run."
                    );
                    Arc::new(NullLlmClient)
                }
            }
        }
        LlmProvider::OpenAi | LlmProvider::Local => {
            eprintln!(
                "spec-drift: [llm].provider {:?} is not implemented yet; \
                 skipping LLM rules for this run.",
                cfg.provider
            );
            Arc::new(NullLlmClient)
        }
    };

    Arc::new(BudgetedClient::new(inner, cfg.max_calls))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct CountingClient {
        calls: AtomicU32,
    }

    impl LlmClient for CountingClient {
        fn evaluate(&self, _: &str, _: &str) -> Option<LlmVerdict> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            Some(LlmVerdict {
                match_spec: true,
                reason: "ok".into(),
            })
        }
    }

    #[test]
    fn budget_exhausts_and_stops_calling_inner() {
        let inner = Arc::new(CountingClient {
            calls: AtomicU32::new(0),
        });
        let client = BudgetedClient::new(inner.clone(), 2);

        assert!(client.evaluate("s", "u").is_some());
        assert!(client.evaluate("s", "u").is_some());
        assert!(client.evaluate("s", "u").is_none());
        assert!(client.evaluate("s", "u").is_none());

        assert_eq!(inner.calls.load(Ordering::Acquire), 2);
    }

    #[test]
    fn force_off_always_yields_null_client() {
        let cfg = LlmConfig {
            enabled: true,
            ..LlmConfig::default()
        };
        let client = build_client(&cfg, true);
        assert!(client.evaluate("s", "u").is_none());
    }

    #[test]
    fn disabled_config_yields_null_client() {
        let cfg = LlmConfig::default(); // enabled=false
        let client = build_client(&cfg, false);
        assert!(client.evaluate("s", "u").is_none());
    }
}
