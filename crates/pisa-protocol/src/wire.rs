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
    /// A create with [`replaces`](SnapshotCreateJob::replaces) set. It has a
    /// kind of its own so that a worker that doesn't know the field refuses
    /// it as [`Job::Unsupported`], rather than keeping the code unchecked.
    SnapshotReplace(SnapshotCreateJob),
    SnapshotGet(SnapshotJob),
    SnapshotDelete(SnapshotJob),
    SnapshotVerify(SnapshotJob),
    SnapshotList(SnapshotListJob),
    SnapshotLabels(SnapshotLabelsJob),
    SnapshotDependents(SnapshotDependentsJob),
    SnapshotRebuild(SnapshotRebuildJob),
    SnapshotAudit(SnapshotAuditJob),
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
    /// Positions in `code`, not in its body.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub at: Vec<api::Pos>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces: Option<api::Replaces>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotJob {
    /// The snapshot id, `<version>~<entry id>`.
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

/// `GET /snapshots/{id}/dependents`, its filters parsed and checked by the
/// router.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotDependentsJob {
    /// The snapshot id.
    pub entry: String,
    /// `(key, value)` pairs that an entry must all have.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<(String, String)>,
}

/// `GET /snapshots/audit`, its filters parsed and checked by the router.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotAuditJob {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<(String, String)>,
    /// The spec's snapshot ids, of the job's version; with them, `labels` is
    /// empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub against: Vec<String>,
}

/// `POST /snapshots/rebuild`, as a plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotRebuildJob {
    pub from: String,
    pub to: String,
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub allow_changes: bool,
}

/// `PATCH /snapshots/{id}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotLabelsJob {
    /// The snapshot id.
    pub entry: String,
    /// The request's merge patch: `None` removes a label, `Some` sets it.
    pub labels: BTreeMap<String, Option<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JobResult {
    Check(CheckOutcome),
    SessionStarted(Result<api::Session, api::Error>),
    TacticApplied(Result<api::ApplyTacticResponse, api::Error>),
    SessionChecked(SessionChecked),
    /// Failures are in-band, in `check.error`, as for checks.
    SnapshotCreated(Box<api::CreateSnapshotResponse>),
    Snapshot(Result<Box<api::SnapshotDetail>, api::Error>),
    SnapshotDeleted(Result<api::DeleteSnapshotResponse, api::Error>),
    SnapshotVerified(Result<api::VerifySnapshotResponse, api::Error>),
    SnapshotList(Result<api::ListSnapshotsResponse, api::Error>),
    SnapshotLabels(Result<api::Snapshot, api::Error>),
    SnapshotDependents(Result<api::ListDependentsResponse, api::Error>),
    SnapshotRebuild(Result<api::RebuildSnapshotsResponse, api::Error>),
    SnapshotAudit(Result<api::AuditSnapshotsResponse, api::Error>),
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
}

impl From<CheckOutcome> for api::CheckResult {
    fn from(o: CheckOutcome) -> Self {
        let (messages, sorries, tactics, at) = o.response.map_or_else(Default::default, |r| {
            let sorries = r.sorries.into_iter().map(Into::into).collect();
            let tactics = r.tactics.into_iter().map(Into::into).collect();
            let at = r.at.into_iter().map(Into::into).collect();
            (api::messages(r.messages), sorries, tactics, at)
        });
        let (verified, reasons, declarations) =
            o.verification.map_or((false, Vec::new(), Vec::new()), |v| {
                (v.ok, v.reasons, v.declarations)
            });
        api::CheckResult {
            ok: verified && o.error.is_none(),
            time: o.time,
            error: o.error,
            messages,
            sorries,
            reasons,
            declarations,
            tactics,
            at,
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
    fn a_create_that_replaces_has_a_kind_of_its_own() {
        let create = SnapshotCreateJob {
            code: "x".into(),
            timeout_secs: 1,
            verify: None,
            labels: BTreeMap::new(),
            key: None,
            replaces: Some(api::Replaces {
                snapshot_id: "e".into(),
                allow: None,
            }),
        };
        let job = serde_json::to_value(Job::SnapshotReplace(create)).unwrap();
        assert_eq!(job["kind"], "snapshot_replace");
        assert_eq!(job["replaces"], serde_json::json!({"snapshot_id": "e"}));
        let Ok(Job::SnapshotReplace(back)) = serde_json::from_value(job) else {
            panic!("not a replace")
        };
        assert_eq!(back.replaces.unwrap().snapshot_id, "e");
    }

    #[test]
    fn jobs_of_other_releases_decode_or_fail_alone() {
        let decode = serde_json::from_value::<Job>;
        assert!(decode(serde_json::json!({"kind": "check", "code": 5})).is_err());
        // A job from a server of the release before entries.
        let old = serde_json::json!({"kind": "snapshot_get", "snapshot": "PisaSnap.s0"});
        assert!(decode(old).is_err());
    }
}
