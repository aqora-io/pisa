//! HTTP API compatible with Kimina Lean Server (project-numina/kimina-lean-server),
//! so `kimina-client` works unchanged. Null fields are omitted, as Kimina does.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::repl::{CommandResponse, ReplError};
use crate::verify::Verification;

fn default_true() -> bool {
    true
}

fn default_check_timeout() -> u64 {
    30
}

fn default_verify_timeout() -> u64 {
    300
}

/// `POST /api/check`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckRequest {
    pub snippets: Vec<Snippet>,
    #[serde(default = "default_check_timeout")]
    pub timeout: u64,
    #[serde(default)]
    pub debug: bool,
    #[serde(default = "default_true")]
    pub reuse: bool,
    #[serde(default)]
    pub infotree: Option<String>,
    /// Extension: the project version (worker pool label or alias) to check against.
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snippet {
    pub id: String,
    pub code: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CheckOutput {
    Command(CommandResponse),
    Error(ReplError),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResponse {
    pub results: Vec<CheckResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResult {
    pub id: String,
    pub time: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<CheckOutput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Value>,
    /// Extension: the outcome of the verification policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<Verification>,
}

/// `POST /verify`, Kimina's legacy endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyRequest {
    pub codes: Vec<Code>,
    #[serde(default = "default_verify_timeout")]
    pub timeout: u64,
    #[serde(default)]
    pub infotree_type: Option<String>,
    #[serde(default)]
    pub disable_cache: bool,
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Code {
    pub custom_id: Value,
    #[serde(default)]
    pub proof: Option<String>,
    #[serde(default)]
    pub code: Option<String>,
}

impl Code {
    /// Kimina gives `proof` precedence over `code`.
    pub fn text(&self) -> &str {
        self.proof.as_deref().or(self.code.as_deref()).unwrap_or("")
    }

    pub fn id(&self) -> String {
        match &self.custom_id {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyResponse {
    pub results: Vec<VerifyResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyResult {
    pub custom_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The REPL response or error with `time` added to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<Verification>,
}

impl VerifyResult {
    pub fn from_check(custom_id: String, r: CheckResult) -> Self {
        let response = r.response.map(|out| {
            let mut v = serde_json::to_value(out).expect("responses serialize");
            if let Value::Object(m) = &mut v {
                m.insert("time".into(), r.time.into());
            }
            v
        });
        VerifyResult {
            custom_id,
            error: r.error,
            response,
            verification: r.verification,
        }
    }
}

pub fn timeout_error(seconds: u64) -> String {
    format!("Lean REPL command timed out in {seconds} seconds")
}

pub fn header_timeout_error(seconds: u64) -> String {
    format!("Lean REPL header command timed out in {seconds} seconds")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn check_request_defaults_match_kimina() {
        let r: CheckRequest =
            serde_json::from_value(json!({"snippets": [{"id": "a", "code": "#check Nat"}]}))
                .unwrap();
        assert_eq!(r.timeout, 30);
        assert!(r.reuse);
        assert!(!r.debug);
        assert_eq!(r.infotree, None);
    }

    #[test]
    fn check_result_omits_nulls() {
        let r = CheckResult {
            id: "a".into(),
            time: 0.5,
            error: None,
            response: Some(CheckOutput::Command(
                serde_json::from_value(json!({
                    "env": 0,
                    "messages": [{"severity": "info", "pos": {"line": 1, "column": 0},
                                  "endPos": {"line": 1, "column": 6}, "data": "Nat : Type"}]
                }))
                .unwrap(),
            )),
            diagnostics: None,
            verification: None,
        };
        assert_eq!(
            serde_json::to_value(&r).unwrap(),
            json!({"id": "a", "time": 0.5, "response": {"env": 0, "messages": [
                {"severity": "info", "pos": {"line": 1, "column": 0},
                 "endPos": {"line": 1, "column": 6}, "data": "Nat : Type"}]}})
        );
    }

    #[test]
    fn verify_puts_time_inside_response_and_accepts_int_ids() {
        let req: VerifyRequest = serde_json::from_value(json!({
            "codes": [{"custom_id": 7, "proof": "p", "code": "c"}]
        }))
        .unwrap();
        assert_eq!(req.timeout, 300);
        assert_eq!(req.codes[0].text(), "p");
        assert_eq!(req.codes[0].id(), "7");

        let r = VerifyResult::from_check(
            "7".into(),
            CheckResult {
                id: "7".into(),
                time: 1.25,
                error: None,
                response: Some(CheckOutput::Error(ReplError {
                    message: "bad".into(),
                })),
                diagnostics: None,
                verification: None,
            },
        );
        assert_eq!(
            serde_json::to_value(&r).unwrap(),
            json!({"custom_id": "7", "response": {"message": "bad", "time": 1.25}})
        );
    }
}
