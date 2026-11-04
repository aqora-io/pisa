//! A thin client for Qdrant's REST API: what loading, indexing and searching
//! use, with request bodies as JSON.

use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// The id of the point that holds a collection's bundle manifest.
pub const MANIFEST_ID: &str = "00000000-0000-0000-0000-000000000000";

#[derive(Clone)]
pub struct Qdrant {
    http: reqwest::Client,
    url: String,
    key: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScoredPoint {
    pub id: Value,
    #[serde(default)]
    pub score: f32,
    #[serde(default)]
    pub payload: Option<Value>,
}

impl ScoredPoint {
    /// The id as a string, as UUID ids come back.
    pub fn id(&self) -> String {
        match &self.id {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        }
    }
}

#[derive(Deserialize)]
struct Points {
    points: Vec<ScoredPoint>,
}

#[derive(Deserialize)]
struct Scrolled {
    points: Vec<ScoredPoint>,
    next_page_offset: Option<Value>,
}

#[derive(Deserialize)]
struct Facets {
    hits: Vec<FacetHit>,
}

#[derive(Deserialize)]
struct FacetHit {
    value: Value,
}

/// A point id from bytes, as a UUID.
pub fn uuid(bytes: &[u8; 16]) -> String {
    let h = hex::encode(bytes);
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}

/// `field = value`, as a filter condition.
pub fn is(field: &str, value: impl Into<Value>) -> Value {
    json!({"key": field, "match": {"value": value.into()}})
}

/// `field` among `values`.
pub fn any(field: &str, values: &[String]) -> Value {
    json!({"key": field, "match": {"any": values}})
}

impl Qdrant {
    /// The Qdrant at `url`, with an API key if it needs one.
    /// A client of the Qdrant at `url`, with the API key in environment
    /// variable `key_env`, if it names one.
    pub fn with_key_env(url: &str, key_env: Option<&str>) -> anyhow::Result<Qdrant> {
        let key = match key_env {
            Some(var) => Some(std::env::var(var).map_err(|_| anyhow::anyhow!("{var} is not set"))?),
            None => None,
        };
        Qdrant::new(url, key)
    }

    pub fn new(url: &str, key: Option<String>) -> anyhow::Result<Qdrant> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(120))
            .build()?;
        Ok(Qdrant {
            http,
            url: url.trim_end_matches('/').to_string(),
            key,
        })
    }

    async fn call<T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> anyhow::Result<T> {
        let mut req = self.http.request(method, format!("{}{path}", self.url));
        if let Some(key) = &self.key {
            req = req.header("api-key", key);
        }
        if let Some(body) = body {
            req = req.json(body);
        }
        let r = req.send().await?;
        let status = r.status();
        let text = r.text().await?;
        anyhow::ensure!(
            status.is_success(),
            "qdrant {path}: {status}: {}",
            text.chars().take(500).collect::<String>()
        );
        #[derive(Deserialize)]
        struct Envelope<T> {
            result: T,
        }
        let v: Envelope<T> = serde_json::from_str(&text)
            .map_err(|e| anyhow::anyhow!("qdrant {path}: unreadable response: {e}"))?;
        Ok(v.result)
    }

    pub async fn exists(&self, collection: &str) -> anyhow::Result<bool> {
        #[derive(Deserialize)]
        struct Exists {
            exists: bool,
        }
        let path = format!("/collections/{collection}/exists");
        let r: Exists = self.call(reqwest::Method::GET, &path, None).await?;
        Ok(r.exists)
    }

    pub async fn create(&self, collection: &str, config: &Value) -> anyhow::Result<()> {
        let path = format!("/collections/{collection}");
        let _: Value = self.call(reqwest::Method::PUT, &path, Some(config)).await?;
        Ok(())
    }

    /// Deletes a collection and its points.
    pub async fn drop(&self, collection: &str) -> anyhow::Result<()> {
        let path = format!("/collections/{collection}");
        let _: Value = self.call(reqwest::Method::DELETE, &path, None).await?;
        Ok(())
    }

    /// Indexes payload field `field` with `schema`, e.g. `"keyword"`.
    pub async fn index(&self, collection: &str, field: &str, schema: &str) -> anyhow::Result<()> {
        let path = format!("/collections/{collection}/index?wait=true");
        let body = json!({"field_name": field, "field_schema": schema});
        let _: Value = self.call(reqwest::Method::PUT, &path, Some(&body)).await?;
        Ok(())
    }

    /// Writes points, each `{id, vector, payload}`, waiting until they apply.
    pub async fn upsert(&self, collection: &str, points: Vec<Value>) -> anyhow::Result<()> {
        if points.is_empty() {
            return Ok(());
        }
        let path = format!("/collections/{collection}/points?wait=true");
        let body = json!({"points": points});
        let _: Value = self.call(reqwest::Method::PUT, &path, Some(&body)).await?;
        Ok(())
    }

    /// Deletes the points that match `filter`.
    pub async fn delete_where(&self, collection: &str, filter: Value) -> anyhow::Result<()> {
        let path = format!("/collections/{collection}/points/delete?wait=true");
        let body = json!({"filter": filter});
        let _: Value = self.call(reqwest::Method::POST, &path, Some(&body)).await?;
        Ok(())
    }

    pub async fn delete_ids(&self, collection: &str, ids: &[String]) -> anyhow::Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let path = format!("/collections/{collection}/points/delete?wait=true");
        let body = json!({"points": ids});
        let _: Value = self.call(reqwest::Method::POST, &path, Some(&body)).await?;
        Ok(())
    }

    /// Sets payload keys on the points that match `filter`.
    pub async fn set_payload(
        &self,
        collection: &str,
        payload: Value,
        filter: Value,
    ) -> anyhow::Result<()> {
        let path = format!("/collections/{collection}/points/payload?wait=true");
        let body = json!({"payload": payload, "filter": filter});
        let _: Value = self.call(reqwest::Method::POST, &path, Some(&body)).await?;
        Ok(())
    }

    /// Runs Query API requests in one batch; results in order.
    pub async fn query_batch(
        &self,
        collection: &str,
        searches: Vec<Value>,
    ) -> anyhow::Result<Vec<Vec<ScoredPoint>>> {
        if searches.is_empty() {
            return Ok(Vec::new());
        }
        let path = format!("/collections/{collection}/points/query/batch");
        let body = json!({"searches": searches});
        let r: Vec<Points> = self.call(reqwest::Method::POST, &path, Some(&body)).await?;
        Ok(r.into_iter().map(|p| p.points).collect())
    }

    /// A page of the points that match `filter`, and the offset of the next.
    pub async fn scroll(
        &self,
        collection: &str,
        filter: Value,
        with_payload: Value,
        limit: usize,
        offset: Option<Value>,
    ) -> anyhow::Result<(Vec<ScoredPoint>, Option<Value>)> {
        let path = format!("/collections/{collection}/points/scroll");
        let mut body = json!({
            "filter": filter, "with_payload": with_payload, "with_vector": false, "limit": limit,
        });
        if let Some(offset) = offset {
            body["offset"] = offset;
        }
        let r: Scrolled = self.call(reqwest::Method::POST, &path, Some(&body)).await?;
        Ok((r.points, r.next_page_offset))
    }

    /// The distinct values of keyword field `key` among the points that match
    /// `filter`, at most `limit`.
    pub async fn facet(
        &self,
        collection: &str,
        key: &str,
        filter: Value,
        limit: usize,
    ) -> anyhow::Result<Vec<String>> {
        let path = format!("/collections/{collection}/facet");
        let body = json!({"key": key, "filter": filter, "limit": limit, "exact": true});
        let r: Facets = self.call(reqwest::Method::POST, &path, Some(&body)).await?;
        Ok(r.hits
            .into_iter()
            .filter_map(|h| h.value.as_str().map(str::to_string))
            .collect())
    }

    /// Whether Qdrant answers.
    pub async fn healthy(&self) -> bool {
        let r = self.http.get(format!("{}/readyz", self.url)).send().await;
        r.is_ok_and(|r| r.status().is_success())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuids_are_formatted() {
        assert_eq!(uuid(&[0; 16]), MANIFEST_ID);
        let mut b = [0u8; 16];
        b[15] = 0xab;
        assert_eq!(uuid(&b), "00000000-0000-0000-0000-0000000000ab");
    }
}
