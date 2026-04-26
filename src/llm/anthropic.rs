//! Anthropic Messages API adapter.
//!
//! Fails closed: any error (missing key, network timeout, malformed JSON,
//! rate limit) yields `None` rather than crashing. LLM rules are experimental
//! and must never be load-bearing for CI.

use super::{LlmClient, LlmVerdict};
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub struct AnthropicLlmClient {
    api_key: String,
    model: String,
    timeout: Duration,
}

impl AnthropicLlmClient {
    /// Build from `ANTHROPIC_API_KEY`. Returns `None` when the env var is
    /// unset or empty — the caller should fall back to [`NullLlmClient`].
    pub fn from_env(model: String, timeout_s: u32) -> Option<Self> {
        let api_key = std::env::var("ANTHROPIC_API_KEY").ok()?;
        if api_key.trim().is_empty() {
            return None;
        }
        Some(Self {
            api_key,
            model,
            timeout: Duration::from_secs(timeout_s as u64),
        })
    }

    fn call(&self, system_prompt: &str, user_prompt: &str) -> Option<Response> {
        let body = Request {
            model: &self.model,
            max_tokens: 512,
            system: system_prompt,
            messages: vec![Message {
                role: "user",
                content: user_prompt,
            }],
        };

        let agent = ureq::AgentBuilder::new().timeout(self.timeout).build();

        let resp = agent
            .post("https://api.anthropic.com/v1/messages")
            .set("x-api-key", &self.api_key)
            .set("anthropic-version", "2023-06-01")
            .set("content-type", "application/json")
            .send_json(serde_json::to_value(&body).ok()?)
            .ok()?;

        resp.into_json::<Response>().ok()
    }
}

impl LlmClient for AnthropicLlmClient {
    fn evaluate(&self, system_prompt: &str, user_prompt: &str) -> Option<LlmVerdict> {
        let resp = self.call(system_prompt, user_prompt)?;
        // Concatenate every `text` block the model emits — modern Claude
        // usually returns one block, but extended thinking can produce more.
        let text: String = resp
            .content
            .into_iter()
            .filter_map(|c| match c {
                ContentBlock::Text { text } => Some(text),
                ContentBlock::Other => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        parse_verdict(&text)
    }
}

/// Parse a JSON verdict out of the model's text response. The prompt asks the
/// model to emit `{"match_spec": bool, "reason": "..."}`. We extract the first
/// balanced `{...}` region — so a preamble like "Here is my analysis:" doesn't
/// break the parse.
fn parse_verdict(text: &str) -> Option<LlmVerdict> {
    let start = text.find('{')?;
    // Find the matching close brace, respecting nested braces.
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    let mut end: Option<usize> = None;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(i + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let slice = &text[start..end?];
    let raw: VerdictJson = serde_json::from_str(slice).ok()?;
    Some(LlmVerdict {
        match_spec: raw.match_spec,
        reason: raw.reason,
    })
}

#[derive(Serialize)]
struct Request<'a> {
    model: &'a str,
    max_tokens: u32,
    system: &'a str,
    messages: Vec<Message<'a>>,
}

#[derive(Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct Response {
    content: Vec<ContentBlock>,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct VerdictJson {
    match_spec: bool,
    reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_json_verdict() {
        let v = parse_verdict(r#"{"match_spec": true, "reason": "all good"}"#).unwrap();
        assert!(v.match_spec);
        assert_eq!(v.reason, "all good");
    }

    #[test]
    fn parses_verdict_after_preamble() {
        let text = r#"Here is my analysis:
        {"match_spec": false, "reason": "function was renamed"}
        — end of analysis."#;
        let v = parse_verdict(text).unwrap();
        assert!(!v.match_spec);
        assert!(v.reason.contains("renamed"));
    }

    #[test]
    fn returns_none_on_unparseable_response() {
        assert!(parse_verdict("I'm sorry, I can't help with that.").is_none());
    }

    #[test]
    fn from_env_returns_none_without_key() {
        // SAFETY: the test-binary process is single-threaded per test in cargo
        // test by default, but environment manipulation is global — we restore
        // any prior value to keep other tests in the suite unaffected.
        let prior = std::env::var("ANTHROPIC_API_KEY").ok();
        // SAFETY: single-threaded test isolation — see comment above.
        unsafe {
            std::env::remove_var("ANTHROPIC_API_KEY");
        }
        assert!(AnthropicLlmClient::from_env("m".into(), 5).is_none());
        if let Some(v) = prior {
            // SAFETY: single-threaded test isolation — see comment above.
            unsafe {
                std::env::set_var("ANTHROPIC_API_KEY", v);
            }
        }
    }
}
