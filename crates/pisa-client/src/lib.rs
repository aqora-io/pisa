//! Async client for the pisa Lean server.
//!
//! ```no_run
//! # async fn demo() -> Result<(), pisa_client::Error> {
//! let client = pisa_client::Client::new("http://localhost:8000")?;
//!
//! let result = client.check("import Mathlib\ntheorem t : 2 + 2 = 4 := by norm_num").await?;
//! assert!(result.ok);
//!
//! let session = client.start_session("theorem t (n : Nat) : n + 0 = n := by sorry").await?;
//! let step = client.tactic(&session.states[0].state, "simp").await?;
//! assert!(step.complete);
//! let verdict = client
//!     .check_session(&session.session, "theorem t (n : Nat) : n + 0 = n := by simp")
//!     .await?;
//! assert!(verdict.ok);
//!
//! let search = pisa_client::SearchClient::new("http://localhost:8100")?;
//! let hits = search
//!     .search(&pisa_client::Query { nl: Some("trace of a density matrix is one".into()), ..Default::default() })
//!     .await?
//!     .hits;
//! # Ok(()) }
//! ```

use pisa_protocol::api::{
    BatchItem, BatchRequest, BatchResponse, CheckRequest, ErrorResponse, SessionCheckRequest,
    StartRequest, TacticRequest,
};
use pisa_protocol::search::{
    DeclResponse, ResolveRequest, ResolveResponse, SearchRequest, SearchResponse,
};
use reqwest::Url;
use serde::Serialize;
use serde::de::DeserializeOwned;

pub use pisa_protocol::api::{
    CheckResult, Declaration, ErrorKind, Message, Pos, Severity, Sorry, StartResponse as Session,
    StateInfo, TacticResponse as Step,
};
pub use pisa_protocol::search::{
    DeclDetail, Flags, Hit, Informal, Query, QueryResult, Resolution, Suggestion,
};

const DEFAULT_TIMEOUT: u64 = 60;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid server URL: {0}")]
    Url(String),
    /// The server could not be reached or sent something unreadable.
    #[error("request failed: {0}")]
    Http(#[from] reqwest::Error),
    /// The server refused the request; `kind` says why. Kinds `lean`,
    /// `policy`, `timeout`, `out_of_memory` and `crash` mean the input itself
    /// failed, which during proof search is an ordinary dead end.
    #[error("{kind:?} ({status}): {message}")]
    Server {
        status: u16,
        kind: ErrorKind,
        message: String,
    },
}

impl Error {
    pub fn kind(&self) -> Option<ErrorKind> {
        match self {
            Error::Server { kind, .. } => Some(*kind),
            _ => None,
        }
    }
}

/// A server's base URL and the HTTP client that talks to it.
#[derive(Debug, Clone)]
struct Endpoint {
    http: reqwest::Client,
    base: Url,
}

impl Endpoint {
    fn new(base_url: &str) -> Result<Self, Error> {
        let base = Url::parse(base_url).map_err(|e| Error::Url(e.to_string()))?;
        Ok(Endpoint {
            http: reqwest::Client::new(),
            base,
        })
    }

    fn url(&self, path: &str) -> Result<Url, Error> {
        // Path segments are pushed individually so keys are percent-encoded.
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|_| Error::Url(self.base.to_string()))?
            .pop_if_empty()
            .extend(path.split('/'));
        Ok(url)
    }

    async fn health(&self) -> Result<(), Error> {
        let r = self.http.get(self.url("health")?).send().await?;
        server_error(r).await.map(drop)
    }

    async fn get<T: DeserializeOwned>(&self, url: Url) -> Result<T, Error> {
        let r = self.http.get(url).send().await?;
        Ok(server_error(r).await?.json().await?)
    }

    async fn post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, Error> {
        let r = self.http.post(self.url(path)?).json(body).send().await?;
        Ok(server_error(r).await?.json().await?)
    }
}

#[derive(Debug, Clone)]
pub struct Client {
    endpoint: Endpoint,
    version: Option<String>,
    timeout: u64,
}

impl Client {
    pub fn new(base_url: &str) -> Result<Self, Error> {
        Ok(Client {
            endpoint: Endpoint::new(base_url)?,
            version: None,
            timeout: DEFAULT_TIMEOUT,
        })
    }

    /// Checks against this worker pool version or alias instead of the
    /// server's default.
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    /// Lean's time limit per request, in seconds (default 60).
    pub fn with_timeout(mut self, seconds: u64) -> Self {
        self.timeout = seconds;
        self
    }

    pub async fn health(&self) -> Result<(), Error> {
        self.endpoint.health().await
    }

    /// Checks one piece of code. `ok` is the verdict; a check that could not
    /// complete has `error` set rather than failing.
    pub async fn check(&self, code: &str) -> Result<CheckResult, Error> {
        let req = CheckRequest {
            code: code.to_string(),
            version: self.version.clone(),
            timeout: self.timeout,
        };
        self.post("check", &req).await
    }

    /// Checks `(id, code)` pairs in one request. Results come back in order;
    /// each item's failures are in its own `error`.
    pub async fn check_batch<I, S, C>(&self, items: I) -> Result<Vec<CheckResult>, Error>
    where
        I: IntoIterator<Item = (S, C)>,
        S: Into<String>,
        C: Into<String>,
    {
        let req = BatchRequest {
            items: items
                .into_iter()
                .map(|(id, code)| BatchItem {
                    id: id.into(),
                    code: code.into(),
                })
                .collect(),
            version: self.version.clone(),
            timeout: self.timeout,
        };
        let resp: BatchResponse = self.post("check/batch", &req).await?;
        Ok(resp.results)
    }

    /// Runs code containing `sorry` and returns one proof state per `sorry`.
    pub async fn start_session(&self, code: &str) -> Result<Session, Error> {
        let req = StartRequest {
            code: code.to_string(),
            version: self.version.clone(),
            timeout: self.timeout,
        };
        self.post("sessions", &req).await
    }

    /// Applies one tactic to a proof state. A tactic Lean rejects is an
    /// [`Error::Server`] of kind [`ErrorKind::Lean`].
    pub async fn tactic(&self, state: &str, tactic: &str) -> Result<Step, Error> {
        let req = TacticRequest {
            tactic: tactic.to_string(),
            timeout: self.timeout,
        };
        self.post(&format!("states/{state}/tactic"), &req).await
    }

    /// Re-checks a completed proof from scratch and confirms it proves the
    /// statements the session started from.
    pub async fn check_session(&self, session: &str, code: &str) -> Result<CheckResult, Error> {
        let req = SessionCheckRequest {
            code: code.to_string(),
            timeout: self.timeout,
        };
        self.post(&format!("sessions/{session}/check"), &req).await
    }

    async fn post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, Error> {
        self.endpoint.post(path, body).await
    }
}

/// A client for a declaration search server (`pisa-search`). Unknown names
/// and empty results are answers, not errors.
#[derive(Debug, Clone)]
pub struct SearchClient {
    endpoint: Endpoint,
    env_id: Option<String>,
}

impl SearchClient {
    pub fn new(base_url: &str) -> Result<Self, Error> {
        Ok(SearchClient {
            endpoint: Endpoint::new(base_url)?,
            env_id: None,
        })
    }

    /// Searches only an index built from this environment (a worker pool
    /// version); a server with another answers [`ErrorKind::NotFound`].
    pub fn with_env(mut self, env_id: impl Into<String>) -> Self {
        self.env_id = Some(env_id.into());
        self
    }

    pub async fn health(&self) -> Result<(), Error> {
        self.endpoint.health().await
    }

    pub async fn search(&self, query: &Query) -> Result<QueryResult, Error> {
        let mut results = self.search_batch(vec![query.clone()]).await?;
        Ok(results.pop().unwrap_or_default())
    }

    /// Runs up to 64 queries in one request; results come back in order.
    pub async fn search_batch(&self, queries: Vec<Query>) -> Result<Vec<QueryResult>, Error> {
        let req = SearchRequest {
            queries,
            env_id: self.env_id.clone(),
        };
        let resp: SearchResponse = self.endpoint.post("v1/search", &req).await?;
        Ok(resp.results)
    }

    /// Says whether each name exists, following deprecations and suggesting
    /// close names for those that don't.
    pub async fn resolve<I, S>(&self, names: I) -> Result<Vec<Resolution>, Error>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let req = ResolveRequest {
            names: names.into_iter().map(Into::into).collect(),
            env_id: self.env_id.clone(),
        };
        let resp: ResolveResponse = self.endpoint.post("v1/resolve", &req).await?;
        Ok(resp.results)
    }

    /// One declaration with its description, dependencies and users. An
    /// unknown name is [`ErrorKind::NotFound`], with a suggestion if any.
    pub async fn decl(&self, name: &str) -> Result<DeclDetail, Error> {
        let mut url = self.endpoint.url("v1/decl")?;
        url.path_segments_mut()
            .map_err(|_| Error::Url(name.to_string()))?
            .push(name);
        let resp: DeclResponse = self.endpoint.get(url).await?;
        Ok(resp.decl)
    }
}

async fn server_error(r: reqwest::Response) -> Result<reqwest::Response, Error> {
    let status = r.status();
    if status.is_success() {
        return Ok(r);
    }
    let text = r.text().await.unwrap_or_default();
    let (kind, message) = match serde_json::from_str::<ErrorResponse>(&text) {
        Ok(e) => (e.error.kind, e.error.message),
        Err(_) => (ErrorKind::Internal, text),
    };
    Err(Error::Server {
        status: status.as_u16(),
        kind,
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_keep_base_path_and_encode_segments() {
        let c = Endpoint::new("http://h:8000/pisa/").unwrap();
        assert_eq!(
            c.url("check/batch").unwrap().as_str(),
            "http://h:8000/pisa/check/batch"
        );
        assert_eq!(
            c.url("states/v~h~a b/tactic").unwrap().as_str(),
            "http://h:8000/pisa/states/v~h~a%20b/tactic"
        );
        assert!(Client::new("not a url").is_err());
    }
}
