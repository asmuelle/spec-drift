//! OpenAI and Local OpenAI-compatible API adapter.
//!
//! Fails closed: any real connection error or parsing error yields `None`
//! rather than crashing. Local providers do not require an API key by default.

use super::{LlmClient, LlmVerdict};
use crate::config::LlmProvider;
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub struct OpenAiLlmClient {
    api_key: Option<String>,
    url: String,
    model: String,
    timeout: Duration,
}

impl OpenAiLlmClient {
    /// Build from environment variables. For `OpenAi`, `OPENAI_API_KEY` is required.
    /// For `Local`, keys are optional, allowing developer-friendly local setups.
    pub fn new(provider: LlmProvider, model: String, timeout_s: u32) -> Option<Self> {
        let api_key = match provider {
            LlmProvider::OpenAi => {
                let key = std::env::var("OPENAI_API_KEY").ok()?;
                if key.trim().is_empty() {
                    return None;
                }
                Some(key)
            }
            LlmProvider::Local => std::env::var("LOCAL_API_KEY")
                .or_else(|_| std::env::var("OPENAI_API_KEY"))
                .ok()
                .filter(|k| !k.trim().is_empty()),
            _ => return None,
        };

        let url = resolve_url(provider);

        Some(Self {
            api_key,
            url,
            model,
            timeout: Duration::from_secs(timeout_s as u64),
        })
    }

    fn call(&self, system_prompt: &str, user_prompt: &str) -> Option<Response> {
        let mut messages = Vec::new();
        if !system_prompt.is_empty() {
            messages.push(Message {
                role: "system",
                content: system_prompt,
            });
        }
        messages.push(Message {
            role: "user",
            content: user_prompt,
        });

        let body = Request {
            model: &self.model,
            messages,
            temperature: 0.2,
        };

        let agent = ureq::AgentBuilder::new().timeout(self.timeout).build();
        let mut req = agent
            .post(&self.url)
            .set("content-type", "application/json");

        if let Some(ref key) = self.api_key {
            req = req.set("Authorization", &format!("Bearer {key}"));
        }

        let resp = req.send_json(serde_json::to_value(&body).ok()?).ok()?;
        resp.into_json::<Response>().ok()
    }
}

impl LlmClient for OpenAiLlmClient {
    fn evaluate(&self, system_prompt: &str, user_prompt: &str) -> Option<LlmVerdict> {
        let resp = self.call(system_prompt, user_prompt)?;
        let text = resp.choices.first()?.message.content.clone();
        parse_verdict(&text)
    }

    fn complete(&self, system_prompt: &str, user_prompt: &str) -> Option<String> {
        let resp = self.call(system_prompt, user_prompt)?;
        Some(resp.choices.first()?.message.content.clone())
    }
}

fn resolve_url(provider: LlmProvider) -> String {
    let env_var = match provider {
        LlmProvider::OpenAi => std::env::var("OPENAI_API_BASE").ok(),
        LlmProvider::Local => std::env::var("LOCAL_API_BASE")
            .or_else(|_| std::env::var("OPENAI_API_BASE"))
            .ok(),
        _ => None,
    };
    let base = env_var.unwrap_or_else(|| match provider {
        LlmProvider::OpenAi => "https://api.openai.com/v1".to_string(),
        LlmProvider::Local => "http://localhost:8080/v1".to_string(),
        _ => unreachable!(),
    });
    if base.ends_with("/chat/completions") {
        base
    } else if base.ends_with('/') {
        format!("{}chat/completions", base)
    } else {
        format!("{}/chat/completions", base)
    }
}

/// Parse a JSON verdict out of the model's text response. The prompt asks the
/// model to emit `{"match_spec": bool, "reason": "..."}`. We extract the first
/// balanced `{...}` region — so preambles don't break the parse.
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
    messages: Vec<Message<'a>>,
    temperature: f32,
}

#[derive(Serialize, Deserialize, Clone)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct Response {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: MessageResponse,
}

#[derive(Deserialize)]
struct MessageResponse {
    content: String,
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
    fn parses_openai_verdict() {
        let v = parse_verdict(r#"{"match_spec": true, "reason": "ok"}"#).unwrap();
        assert!(v.match_spec);
        assert_eq!(v.reason, "ok");
    }

    #[test]
    fn resolve_url_defaults() {
        // OpenAI default
        let url_openai = resolve_url(LlmProvider::OpenAi);
        assert_eq!(url_openai, "https://api.openai.com/v1/chat/completions");

        // Local default
        let url_local = resolve_url(LlmProvider::Local);
        assert_eq!(url_local, "http://localhost:8080/v1/chat/completions");
    }
}
