//! What informalization needs from a language model, over either the
//! Anthropic Messages API or the OpenAI Responses API: one structured-output
//! completion, with tokens and cost.

use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{anthropic, openai};

const MAX_ATTEMPTS: u32 = 6;

/// Tokens of one call. Input is split into uncached, cache writes and cache
/// reads, which are priced differently.
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

impl Usage {
    /// Adds `other`'s tokens to these.
    pub fn add(&mut self, other: &Usage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_creation_input_tokens += other.cache_creation_input_tokens;
        self.cache_read_input_tokens += other.cache_read_input_tokens;
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

/// One structured-output request.
pub struct Request<'a> {
    pub model: &'a str,
    pub effort: &'a str,
    pub max_tokens: u32,
    /// Instructions, cached as a prefix where the API allows it.
    pub system: &'a [String],
    pub prompt: &'a str,
    pub schema: &'a Value,
}

pub struct Completion {
    /// The model that answered, as the API reports it.
    pub model: String,
    pub text: String,
    /// Why the answer is unusable (`refusal`, `max_tokens`, ...), if it is.
    pub stopped: Option<String>,
    pub usage: Usage,
    pub cost: Option<f64>,
}

pub enum Llm {
    Anthropic(anthropic::Client),
    OpenAi(openai::Client),
}

impl Llm {
    /// The provider for `model`, with its key from the environment.
    pub fn for_model(model: &str) -> anyhow::Result<Llm> {
        if model.starts_with("claude-") {
            Ok(Llm::Anthropic(anthropic::Client::from_env()?))
        } else {
            Ok(Llm::OpenAi(openai::Client::from_env()?))
        }
    }

    pub async fn complete(&self, req: &Request<'_>) -> Result<Completion, Error> {
        match self {
            Llm::Anthropic(c) => c.complete(req).await,
            Llm::OpenAi(c) => c.complete(req).await,
        }
    }
}

/// Sends a request, retrying rate limits, overload, server errors and dropped
/// connections with backoff (honouring `retry-after`).
pub(crate) async fn send<T: DeserializeOwned>(
    build: impl Fn() -> reqwest::RequestBuilder,
) -> Result<T, Error> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        let backoff =
            Duration::from_millis(1000 * 2u64.pow(attempt - 1)).min(Duration::from_secs(60));
        let r = match build().send().await {
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
        if (status == 429 || status >= 500) && attempt < MAX_ATTEMPTS {
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

fn error_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| body.chars().take(300).collect())
}

/// Dollars per million tokens: input, output, cache reads and cache writes.
fn prices(model: &str) -> Option<(f64, f64, f64, f64)> {
    Some(match model {
        // Prompts here stay under Haiku 5.5's 100K-token rate-card boundary.
        m if m.starts_with("claude-haiku-5-5") => (0.10, 0.50, 0.01, 0.125),
        m if m.starts_with("claude-sonnet-5-5") => (2.0, 10.0, 0.10, 2.50),
        m if m.starts_with("claude-opus-5-5") => (4.0, 20.0, 0.20, 5.0),
        // OpenAI caches automatically and doesn't charge extra for writes.
        m if m.starts_with("gpt-6.1-sol") => (2.0, 10.0, 0.10, 2.0),
        m if m.starts_with("gpt-6-luna") => (0.10, 0.50, 0.01, 0.10),
        _ => return None,
    })
}

/// What a call cost in dollars, if the model's prices are known.
pub fn cost(model: &str, u: &Usage) -> Option<f64> {
    let (input, output, cache_read, cache_write) = prices(model)?;
    Some(
        (u.input_tokens as f64 * input
            + u.cache_creation_input_tokens as f64 * cache_write
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
        let c = cost("gpt-6-luna", &u).unwrap();
        assert!((c - (0.10 + 0.50 + 0.10 + 0.01)).abs() < 1e-9, "{c}");
        assert_eq!(cost("some-other-model", &u), None);
    }
}
