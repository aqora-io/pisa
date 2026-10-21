//! A small client for the Anthropic Messages API over raw HTTP (there is no
//! official Rust SDK). The API key comes from `ANTHROPIC_API_KEY` and is only
//! ever placed in the request header.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";
const MAX_ATTEMPTS: u32 = 6;

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base_url: String,
    key: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Response {
    pub model: String,
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub content: Vec<Value>,
    #[serde(default)]
    pub usage: Usage,
}

impl Response {
    /// The concatenated text blocks (thinking blocks come first and are skipped).
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The key is missing, invalid or lacks permission; no request will work.
    #[error("authentication failed ({status}): {message}")]
    Auth { status: u16, message: String },
    #[error("API error {status}: {message}")]
    Api { status: u16, message: String },
    #[error(transparent)]
    Http(#[from] reqwest::Error),
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

    /// `POST /v1/messages`, retrying rate limits, overload, server errors and
    /// dropped connections with backoff (honouring `retry-after`).
    pub async fn messages(&self, body: &Value) -> Result<Response, Error> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let sent = self
                .http
                .post(format!("{}/v1/messages", self.base_url))
                .header("x-api-key", &self.key)
                .header("anthropic-version", API_VERSION)
                .json(body)
                .send()
                .await;
            let backoff =
                Duration::from_millis(1000 * 2u64.pow(attempt - 1)).min(Duration::from_secs(60));
            let r = match sent {
                Ok(r) => r,
                Err(e)
                    if attempt < MAX_ATTEMPTS
                        && (e.is_connect() || e.is_timeout() || e.is_request()) =>
                {
                    tracing::warn!(attempt, "request failed, retrying: {e}");
                    tokio::time::sleep(backoff).await;
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            let status = r.status().as_u16();
            if r.status().is_success() {
                return Ok(r.json().await?);
            }
            let retry_after = r
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .map(Duration::from_secs);
            let message = error_message(&r.text().await.unwrap_or_default());
            let retryable = status == 429 || status >= 500;
            if retryable && attempt < MAX_ATTEMPTS {
                let wait = retry_after.unwrap_or(backoff);
                tracing::warn!(status, attempt, wait = ?wait, "{message}; retrying");
                tokio::time::sleep(wait).await;
                continue;
            }
            return Err(match status {
                401 | 403 => Error::Auth { status, message },
                _ => Error::Api { status, message },
            });
        }
    }
}

fn error_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| body.chars().take(300).collect())
}

/// Dollars per million tokens: input, output, and cache reads. Cache writes
/// (5-minute TTL) cost 1.25 × input.
fn prices(model: &str) -> Option<(f64, f64, f64)> {
    Some(match model {
        // Prompts here stay under Haiku 5.5's 100K-token rate-card boundary.
        m if m.starts_with("claude-haiku-5-5") => (0.10, 0.50, 0.01),
        m if m.starts_with("claude-sonnet-5-5") => (2.0, 10.0, 0.10),
        m if m.starts_with("claude-opus-5-5") => (4.0, 20.0, 0.20),
        _ => return None,
    })
}

/// What a call cost in dollars, if the model's prices are known.
pub fn cost(model: &str, u: &Usage) -> Option<f64> {
    let (input, output, cache_read) = prices(model)?;
    Some(
        (u.input_tokens as f64 * input
            + u.cache_creation_input_tokens as f64 * input * 1.25
            + u.cache_read_input_tokens as f64 * cache_read
            + u.output_tokens as f64 * output)
            / 1e6,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn costs_count_cache_reads_and_writes() {
        let u = Usage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            cache_creation_input_tokens: 1_000_000,
            cache_read_input_tokens: 1_000_000,
        };
        let c = cost("claude-haiku-5-5", &u).unwrap();
        assert!((c - (0.10 + 0.50 + 0.125 + 0.01)).abs() < 1e-9, "{c}");
        assert_eq!(cost("some-other-model", &u), None);
    }

    #[test]
    fn text_skips_thinking_blocks() {
        let r: Response = serde_json::from_value(serde_json::json!({
            "model": "m",
            "stop_reason": "end_turn",
            "content": [{"type": "thinking", "thinking": "", "signature": "s"}, {"type": "text", "text": "{}"}],
            "usage": {"input_tokens": 1, "output_tokens": 2}
        }))
        .unwrap();
        assert_eq!(r.text(), "{}");
    }
}
