//! The jobs that servers hand to workers, and their results, as they go
//! through the database's job queue (`pisa_db::queue`) as JSON.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::api;
use crate::repl::CommandResponse;
use crate::verify::{DeclarationChange, Verification};

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
    SnapshotDelete(SnapshotDeleteJob),
    SnapshotVerify(SnapshotJob),
    SnapshotList(SnapshotListJob),
    SnapshotLabels(SnapshotLabelsJob),
    SnapshotDependents(SnapshotDependentsJob),
    SnapshotRebuild(SnapshotRebuildJob),
    SnapshotAudit(SnapshotAuditJob),
    SnapshotExport(SnapshotExportJob),
    SnapshotComparator(SnapshotComparatorJob),
    DocumentOpen(DocumentOpenJob),
    DocumentEdit(DocumentEditJob),
    DocumentGoals(DocumentGoalsJob),
    DocumentProbe(DocumentProbeJob),
    DocumentClose(DocumentJob),
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
    /// The session's document: 32 lowercase hex, its id within the job's
    /// version.
    #[serde(default)]
    pub document: String,
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
    /// Whether `replaces` may name a snapshot that another replaces.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fork: bool,
    /// A spec's holes; empty with `replaces`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub holes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotJob {
    /// The snapshot id, `<version>~<entry id>`.
    pub entry: String,
}

/// `DELETE /snapshots/{id}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotDeleteJob {
    /// The snapshot id, `<version>~<entry id>`.
    pub entry: String,
    /// Whether to delete the snapshots that replace it too, directly or
    /// through others, rather than splice them.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub with_replacers: bool,
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
    /// Whether to list only the snapshots that no other snapshot replaces.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub heads: bool,
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
    /// The definitions the leader allows the run's heads to change; only
    /// with `against`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
    /// With `against`, also the review list.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub review: bool,
}

/// `GET /snapshots/export`, its query checked by the router.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotExportJob {
    /// The spec's snapshot ids, of the job's version.
    pub against: Vec<String>,
    /// The definitions the leader allows the run's heads to change.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
}

/// `POST /snapshots/comparator`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotComparatorJob {
    pub against: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
    pub paranoid: bool,
    /// For each pair.
    pub timeout_secs: u64,
}

/// A run as a Lake project, for `lake comparator`: its files, and the pairs
/// of a spec snapshot and a head it reaches that comparator judges.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportedRun {
    pub files: Vec<ExportedFile>,
    pub pairs: Vec<ExportedPair>,
    /// As in the audit: a run with one isn't accepted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forks: Vec<api::AuditFork>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportedFile {
    /// Relative to the project's root.
    pub path: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportedPair {
    /// The spec snapshot's id.
    pub spec: String,
    /// The head's id.
    pub head: String,
    /// Its comparator configuration's path in the project.
    pub config: String,
    /// Targets, holes and assumptions it leaves out because they are
    /// private, which comparator can't name across two modules.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<String>,
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

/// `POST /documents`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentOpenJob {
    /// The new document's id within the job's version: 32 lowercase hex.
    pub document: String,
    pub code: String,
    pub timeout_secs: u64,
}

/// `PATCH /documents/{id}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentEditJob {
    pub document: String,
    pub code: String,
    /// The revision the edit replaces.
    pub revision: u64,
    pub timeout_secs: u64,
}

/// `POST /documents/{id}/goals`, its positions checked by the router.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentGoalsJob {
    pub document: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    pub at: Vec<api::LinePos>,
    #[serde(default)]
    pub mode: api::GoalsMode,
    pub timeout_secs: u64,
}

/// `POST /documents/{id}/probes`, its candidates counted by the router.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentProbeJob {
    pub document: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    pub at: api::ProbeAt,
    pub candidates: Vec<String>,
    pub timeout_secs: u64,
}

/// `DELETE /documents/{id}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentJob {
    pub document: String,
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
    SnapshotExport(Result<ExportedRun, api::Error>),
    SnapshotComparator(Result<api::ComparatorResponse, api::Error>),
    /// An opened or edited document.
    Document(Result<api::Document, api::Error>),
    DocumentGoals(Result<api::DocumentGoals, api::Error>),
    DocumentProbes(Result<api::DocumentProbes, api::Error>),
    DocumentClosed(Result<(), api::Error>),
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
        let (verified, reasons, declarations, changes) = o
            .verification
            .map_or((false, Vec::new(), Vec::new(), Vec::new()), |v| {
                (v.ok, v.reasons, v.declarations, v.changes)
            });
        api::CheckResult {
            ok: verified && o.error.is_none(),
            time: o.time,
            error: o.error,
            messages,
            sorries,
            reasons,
            changes,
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
    /// The declarations among them that changed.
    #[serde(default)]
    pub statement_changes: Vec<DeclarationChange>,
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
            fork: true,
            holes: Vec::new(),
        };
        let job = serde_json::to_value(Job::SnapshotReplace(create)).unwrap();
        assert_eq!(job["kind"], "snapshot_replace");
        assert_eq!(job["replaces"], serde_json::json!({"snapshot_id": "e"}));
        assert_eq!(job["fork"], true);
        let Ok(Job::SnapshotReplace(back)) = serde_json::from_value(job) else {
            panic!("not a replace")
        };
        assert_eq!(back.replaces.unwrap().snapshot_id, "e");
        assert!(back.fork);
    }

    #[test]
    fn document_jobs_and_results_round_trip() {
        let job = Job::DocumentProbe(DocumentProbeJob {
            document: "d".into(),
            revision: None,
            at: api::ProbeAt::Sorry { sorry: 0 },
            candidates: vec!["rfl".into()],
            timeout_secs: 5,
        });
        let value = serde_json::to_value(&job).unwrap();
        assert_eq!(value["kind"], "document_probe");
        assert_eq!(value["at"], serde_json::json!({"sorry": 0}));
        let Ok(Job::DocumentProbe(back)) = serde_json::from_value(value) else {
            panic!("not a probe")
        };
        assert_eq!(back.candidates, ["rfl"]);

        for result in [
            JobResult::DocumentClosed(Ok(())),
            JobResult::DocumentClosed(Err(api::Error::new(api::ErrorKind::NotFound, "gone"))),
        ] {
            let value = serde_json::to_value(&result).unwrap();
            assert_eq!(value["kind"], "document_closed");
            let back: JobResult = serde_json::from_value(value).unwrap();
            assert_eq!(format!("{back:?}"), format!("{result:?}"));
        }
    }

    #[test]
    fn an_export_round_trips() {
        let run = ExportedRun {
            files: vec![ExportedFile {
                path: "lakefile.toml".into(),
                text: "name = \"pisa_run\"\n".into(),
            }],
            pairs: vec![ExportedPair {
                spec: "v~s".into(),
                head: "v~h".into(),
                config: "comparator/1.json".into(),
                skipped: Vec::new(),
            }],
            forks: Vec::new(),
        };
        let value = serde_json::to_value(JobResult::SnapshotExport(Ok(run.clone()))).unwrap();
        assert_eq!(value["kind"], "snapshot_export");
        let Ok(JobResult::SnapshotExport(Ok(back))) = serde_json::from_value(value) else {
            panic!("not an export")
        };
        assert_eq!(back, run);
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
