//! The jobs that servers hand to workers, and their results, as they go
//! through the database's job queue (`pisa_db::queue`) as JSON.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::api;
use crate::repl::CommandResponse;
use crate::verify::Verification;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Job {
    Check(CheckJob),
    SessionStart(SessionStartJob),
    SessionTactic(SessionTacticJob),
    SessionCheck(SessionCheckJob),
    SnapshotCreate(SnapshotCreateJob),
    SnapshotGet(SnapshotJob),
    SnapshotDelete(SnapshotJob),
    SnapshotVerify(SnapshotJob),
    SnapshotList(SnapshotListJob),
    SnapshotLabels(SnapshotLabelsJob),
    /// A job kind this worker doesn't know, from a newer server. It is
    /// answered with [`JobResult::Failed`].
    #[serde(other)]
    Unsupported,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckJob {
    pub code: String,
    pub timeout_secs: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detail: Vec<api::Detail>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStartJob {
    pub code: String,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTacticJob {
    pub state: String,
    pub tactic: String,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCheckJob {
    pub session: String,
    pub code: String,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotCreateJob {
    pub code: String,
    pub timeout_secs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify: Option<bool>,
    /// The new entry's labels.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    /// The request's idempotency key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotJob {
    /// The entry id. Routers of the release before entries called it
    /// `snapshot` and sent a module name, which is then a `bad_request`.
    #[serde(alias = "snapshot")]
    pub entry: String,
}

/// `GET /snapshots`, its query parsed and checked by the router.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotListJob {
    /// `(key, value)` pairs that an entry must all have, in request order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<(String, String)>,
    pub limit: usize,
    /// The cursor to start after.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotLabelsJob {
    /// The entry id.
    pub entry: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub set: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JobResult {
    Check(CheckOutcome),
    SessionStarted(Result<api::StartResponse, api::Error>),
    TacticApplied(Result<api::TacticResponse, api::Error>),
    SessionChecked(SessionChecked),
    /// Failures are in-band, in `result.error`, as for checks.
    SnapshotCreated(Box<api::SnapshotResponse>),
    Snapshot(Result<Box<api::SnapshotInfo>, api::Error>),
    SnapshotDeleted(Result<api::Deleted, api::Error>),
    SnapshotVerified(Result<api::SnapshotVerified, api::Error>),
    SnapshotList(Result<api::SnapshotList, api::Error>),
    SnapshotLabels(Result<api::SnapshotSummary, api::Error>),
    /// The worker could not run the job at all.
    Failed {
        error: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckOutcome {
    pub time: f64,
    #[serde(default)]
    pub error: Option<api::Error>,
    /// The REPL's response, positions already shifted to the submitted code.
    #[serde(default)]
    pub response: Option<CommandResponse>,
    #[serde(default)]
    pub verification: Option<Verification>,
    /// Whether the header was already imported in the process that ran the job.
    pub warm: bool,
}

impl From<CheckOutcome> for api::CheckResult {
    fn from(o: CheckOutcome) -> Self {
        let (messages, sorries, tactics) = o.response.map_or_else(Default::default, |r| {
            let sorries = r.sorries.into_iter().map(Into::into).collect();
            let tactics = r.tactics.into_iter().map(Into::into).collect();
            (api::messages(r.messages), sorries, tactics)
        });
        let (verified, reasons, declarations) =
            o.verification.map_or((false, Vec::new(), Vec::new()), |v| {
                (v.ok, v.reasons, v.declarations)
            });
        api::CheckResult {
            id: None,
            ok: verified && o.error.is_none(),
            time: o.time,
            error: o.error,
            messages,
            sorries,
            reasons,
            declarations,
            tactics,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionChecked {
    pub outcome: CheckOutcome,
    /// Reasons the completed code does not prove the session's statements.
    pub statement_reasons: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_job_kinds_parse_as_unsupported() {
        let job: Job = serde_json::from_value(serde_json::json!({
            "kind": "from_the_future", "code": "x", "timeout_secs": 1
        }))
        .unwrap();
        assert!(matches!(job, Job::Unsupported));
    }

    #[test]
    fn jobs_of_other_releases_decode_or_fail_alone() {
        let decode = serde_json::from_value::<Job>;
        assert!(decode(serde_json::json!({"kind": "check", "code": 5})).is_err());
        // A job from a server of the release before entries.
        let old = decode(serde_json::json!({"kind": "snapshot_get", "snapshot": "PisaSnap.s0"}));
        let Ok(Job::SnapshotGet(job)) = old else {
            panic!("{old:?}")
        };
        assert_eq!(job.entry, "PisaSnap.s0");
    }
}
