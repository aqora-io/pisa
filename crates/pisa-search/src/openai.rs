//! The OpenAI Responses API over raw HTTP. The API key comes from
//! `OPENAI_API_KEY` and is only ever placed in the request header.

use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::llm::{self, Completion, Error, Request, Usage};

pub const DEFAULT_BASE_URL: &str = "https://api.openai.com";

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base_url: String,
    key: String,
}

#[derive(Debug, Deserialize)]
struct Response {
    model: String,
    status: String,
    #[serde(default)]
    incomplete_details: Option<Value>,
    #[serde(default)]
    output: Vec<Value>,
    #[serde(default)]
    usage: ResponseUsage,
}

#[derive(Debug, Default, Deserialize)]
struct ResponseUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    input_tokens_details: Option<Value>,
}

impl Client {
    pub fn from_env() -> anyhow::Result<Client> {
        let key = std::env::var("OPENAI_API_KEY")
            .map_err(|_| anyhow::anyhow!("OPENAI_API_KEY is not set"))?;
        let base_url = std::env::var("OPENAI_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.into());
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

    /// `POST /v1/responses` with the system blocks as instructions (OpenAI
    /// caches long prefixes by itself), strict JSON-schema output, and
    /// `store: false` so OpenAI keeps no copy of the response.
    pub async fn complete(&self, req: &Request<'_>) -> Result<Completion, Error> {
        let body = json!({
            "model": req.model,
            "instructions": req.system.join("\n\n"),
            "input": req.prompt,
            "reasoning": {"effort": req.effort},
            "max_output_tokens": req.max_tokens,
            "store": false,
            "text": {"format": {
                "type": "json_schema",
                "name": "output",
                "strict": true,
                "schema": req.schema,
            }},
        });
        let r: Response = llm::send(|| {
            self.http
                .post(format!("{}/v1/responses", self.base_url))
                .bearer_auth(&self.key)
                .json(&body)
        })
        .await?;
        let mut text = String::new();
        let mut refused = false;
        for item in r.output.iter().filter(|o| o["type"] == "message") {
            for c in item["content"].as_array().into_iter().flatten() {
                match c["type"].as_str() {
                    Some("output_text") => text.push_str(c["text"].as_str().unwrap_or_default()),
                    Some("refusal") => refused = true,
                    _ => {}
                }
            }
        }
        let stopped = if refused {
            Some("refusal".to_string())
        } else if r.status == "completed" {
            None
        } else {
            let reason = r
                .incomplete_details
                .as_ref()
                .and_then(|d| d["reason"].as_str())
                .unwrap_or(&r.status);
            Some(reason.to_string())
        };
        let cached = r
            .usage
            .input_tokens_details
            .as_ref()
            .and_then(|d| d["cached_tokens"].as_u64())
            .unwrap_or(0);
        let usage = Usage {
            input_tokens: r.usage.input_tokens.saturating_sub(cached),
            cache_read_input_tokens: cached,
            cache_creation_input_tokens: 0,
            output_tokens: r.usage.output_tokens,
        };
        Ok(Completion {
            cost: llm::cost(&r.model, &usage),
            text,
            stopped,
            usage,
            model: r.model,
        })
    }
}
