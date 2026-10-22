//! The Anthropic Messages API over raw HTTP (there is no official Rust SDK).
//! The API key comes from `ANTHROPIC_API_KEY` and is only ever placed in the
//! request header.

use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::llm::{self, Completion, Error, Request, Usage};

pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base_url: String,
    key: String,
}

#[derive(Debug, Clone, Deserialize)]
struct Response {
    model: String,
    stop_reason: Option<String>,
    #[serde(default)]
    content: Vec<Value>,
    #[serde(default)]
    usage: Usage,
}

impl Response {
    /// The concatenated text blocks (thinking blocks come first and are skipped).
    fn text(&self) -> String {
        self.content
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect()
    }
}

impl Client {
    pub fn from_env() -> anyhow::Result<Client> {
        let key = std::env::var("ANTHROPIC_API_KEY")
            .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY is not set"))?;
        let base_url =
            std::env::var("ANTHROPIC_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.into());
        Ok(Client::new(&base_url, key))
    }

    pub fn new(base_url: &str, key: String) -> Client {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(600))
            .build()
            .expect("HTTP client");
        Client {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            key,
        }
    }

    /// `POST /v1/messages` with the system blocks prompt-cached (the cache
    /// marker goes on the last one) and the output fixed by a JSON schema.
    pub async fn complete(&self, req: &Request<'_>) -> Result<Completion, Error> {
        let n = req.system.len();
        let system: Vec<Value> = req
            .system
            .iter()
            .enumerate()
            .map(|(i, text)| {
                let mut block = json!({"type": "text", "text": text});
                if i + 1 == n {
                    block["cache_control"] = json!({"type": "ephemeral"});
                }
                block
            })
            .collect();
        let body = json!({
            "model": req.model,
            "max_tokens": req.max_tokens,
            "system": system,
            "messages": [{"role": "user", "content": req.prompt}],
            "output_config": {
                "effort": req.effort,
                "format": {"type": "json_schema", "schema": req.schema},
            },
        });
        let r: Response = llm::send(|| {
            self.http
                .post(format!("{}/v1/messages", self.base_url))
                .header("x-api-key", &self.key)
                .header("anthropic-version", API_VERSION)
                .json(&body)
        })
        .await?;
        let stopped = match r.stop_reason.as_deref() {
            Some("end_turn") => None,
            other => Some(other.unwrap_or("unknown").to_string()),
        };
        Ok(Completion {
            cost: llm::cost(&r.model, &r.usage),
            text: r.text(),
            stopped,
            usage: r.usage,
            model: r.model,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_skips_thinking_blocks() {
        let r: Response = serde_json::from_value(json!({
            "model": "m",
            "stop_reason": "end_turn",
            "content": [{"type": "thinking", "thinking": "", "signature": "s"}, {"type": "text", "text": "{}"}],
            "usage": {"input_tokens": 1, "output_tokens": 2}
        }))
        .unwrap();
        assert_eq!(r.text(), "{}");
    }
}
