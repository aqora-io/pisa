//! The jobs that servers hand to workers, and their results, as they go
//! through the database's job queue (`pisa_db::queue`) as JSON.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::api;
use crate::repl::CommandResponse;
use crate::scope::Scope;
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
    /// A create with [`amends`](SnapshotCreateJob::amends) set, as a
    /// replacement has its own kind.
    SnapshotAmend(SnapshotCreateJob),
    /// A create with [`refutes`](SnapshotCreateJob::refutes) set, as a
    /// replacement has its own kind.
    SnapshotRefute(SnapshotCreateJob),
    SnapshotDelete(SnapshotDeleteJob),
    SnapshotVerify(SnapshotVerifyJob),
    SnapshotRebuild(SnapshotRebuildJob),
    /// The record of a settle, which a server runs.
    SnapshotSettle(api::SettleRunRequest),
    /// The record of a resubmit, which a server runs.
    SnapshotResubmit(api::ResubmitSnapshotsRequest),
    SnapshotExport(SnapshotExportJob),
    SnapshotAccept(SnapshotAcceptJob),
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
    /// What the router resolved the code's imports by run name to; the
    /// code imports those modules already.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<api::RunImport>,
    /// The code as sent, importing runs by name, when `runs` resolved them:
    /// where messages on its header lines are placed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStartJob {
    /// The session's document: 32 lowercase hex, its id within the job's
    /// version.
    #[serde(default)]
    pub document: String,
    pub code: String,
    pub timeout_secs: u64,
    /// What the router resolved the code's imports by run name to; the
    /// code imports those modules already.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<api::RunImport>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detail: Vec<api::Detail>,
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
    /// A spec's holes as the request names them: none with `replaces`, and
    /// with `amends`, when it names none, the amended spec's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holes: Option<Vec<String>>,
    /// The spec it amends; not with `replaces`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amends: Option<api::Amends>,
    /// The spec target it refutes; alone among these.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refutes: Option<api::Refutes>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detail: Vec<api::Detail>,
    /// The name for the run it starts; not with `replaces`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The code as the request sent it, when the router resolved imports by
    /// run name in `code`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// What the router resolved the code's imports by run name to; the
    /// code imports those modules already.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<api::RunImport>,
}

/// `POST /snapshots/{id}/verify`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotVerifyJob {
    /// The snapshot id, `<version>~<entry id>`.
    pub entry: String,
    /// Lean's time limit for each module, once their imports are loaded.
    #[serde(default = "default_verify_secs")]
    pub timeout_secs: u64,
    /// Re-check the modules that are verified already too, so that their
    /// records are described again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub again: bool,
}

fn default_verify_secs() -> u64 {
    60
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

/// `GET /snapshots/export`, its query checked by the router.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotExportJob {
    /// The spec's snapshot ids, of the job's version.
    pub against: Vec<String>,
    /// The definitions the leader allows the run's heads to change.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
}

/// `POST /snapshots/accept`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotAcceptJob {
    pub against: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
    pub paranoid: bool,
    /// For each pair.
    pub timeout_secs: u64,
    /// More for the first pair comparator runs, which builds the run's
    /// modules that later pairs replay.
    #[serde(default)]
    pub build_timeout_secs: u64,
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
    /// Their modules, `PisaSnap.s…`.
    #[serde(default)]
    pub spec_module: String,
    #[serde(default)]
    pub head_module: String,
    /// What the project names them, which comparator judges: a named run's
    /// as `PisaSpec.<name>` and `PisaRun.<name>`, others as their modules.
    #[serde(default)]
    pub challenge_module: String,
    #[serde(default)]
    pub solution_module: String,
    /// Its comparator configuration's path in the project.
    pub config: String,
    /// How many of the spec's theorems it names.
    pub theorems: u64,
    /// What it leaves out of what it would name, and why.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub left_out: Vec<api::LeftOut>,
}

/// `POST /snapshots/rebuild`, as a plan, and as the record of a run the
/// server holds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotRebuildJob {
    pub from: String,
    pub to: String,
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub allow_changes: bool,
}

/// `POST /documents`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentOpenJob {
    /// The new document's id within the job's version: 32 lowercase hex.
    pub document: String,
    pub code: String,
    pub timeout_secs: u64,
    /// What the router resolved the code's imports by run name to; the
    /// code imports those modules already.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<api::RunImport>,
}

/// `PATCH /documents/{id}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentEditJob {
    pub document: String,
    pub code: String,
    /// The revision the edit replaces.
    pub revision: u64,
    pub timeout_secs: u64,
    /// What the router resolved the code's imports by run name to; the
    /// code imports those modules already.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<api::RunImport>,
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
    /// For all the candidates; from a server before budgets, the default.
    #[serde(default = "api::default_probe_budget")]
    pub budget_secs: u64,
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
    SnapshotDeleted(Result<api::DeleteSnapshotResponse, api::Error>),
    SnapshotVerified(Result<api::VerifySnapshotResponse, api::Error>),
    SnapshotRebuild(Result<api::RebuildSnapshotsResponse, api::Error>),
    SnapshotResubmit(Result<Box<api::ResubmitSnapshotsResponse>, api::Error>),
    SnapshotExport(Result<ExportedRun, api::Error>),
    SnapshotAccepted(Result<Box<api::AcceptResponse>, api::Error>),
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
    /// What the code's imports by run name resolved to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<api::RunImport>,
}

/// The JSON `job` is queued as, with the scope it runs in beside its `kind`
/// when there is one.
pub fn scoped(job: &Job, scope: &Scope) -> serde_json::Result<serde_json::Value> {
    let mut value = serde_json::to_value(job)?;
    if !scope.is_empty()
        && let Some(fields) = value.as_object_mut()
    {
        fields.insert("scope".into(), serde_json::to_value(scope)?);
    }
    Ok(value)
}

/// A queued job, and the scope it runs in: none for a job queued without one.
pub fn unscoped(mut value: serde_json::Value) -> serde_json::Result<(Job, Scope)> {
    let scope = match value
        .as_object_mut()
        .and_then(|fields| fields.remove("scope"))
    {
        Some(scope) => serde_json::from_value(scope)?,
        None => Scope::default(),
    };
    Ok((serde_json::from_value(value)?, scope))
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
            runs: o.runs,
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
    fn a_job_carries_its_scope_beside_its_kind() {
        let job = Job::DocumentClose(DocumentJob {
            document: "d".into(),
        });
        let scope = Scope::parse("user=42").unwrap();
        let queued = scoped(&job, &scope).unwrap();
        assert_eq!(queued["scope"], serde_json::json!({"user": "42"}));
        let (back, found) = unscoped(queued).unwrap();
        assert!(matches!(back, Job::DocumentClose(d) if d.document == "d"));
        assert_eq!(found, scope);
        let plain = scoped(&job, &Scope::default()).unwrap();
        assert!(plain.get("scope").is_none());
        assert!(unscoped(plain).unwrap().1.is_empty());
    }

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
            holes: None,
            amends: None,
            refutes: None,
            detail: Vec::new(),
            name: None,
            source: None,
            runs: Vec::new(),
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
            budget_secs: 60,
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
                spec_module: "PisaSnap.ss".into(),
                head_module: "PisaSnap.sh".into(),
                challenge_module: "PisaSpec.p".into(),
                solution_module: "PisaRun.p".into(),
                config: "comparator/1.json".into(),
                theorems: 1,
                left_out: vec![api::LeftOut {
                    name: "Gate".into(),
                    reason: api::LeftOutReason::NotADefinitionInSpec,
                }],
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
        // A malformed job of a known kind fails; a kind this worker doesn't
        // know, such as a read the servers now answer, is unsupported.
        let old = serde_json::json!({"kind": "snapshot_delete", "snapshot": "PisaSnap.s0"});
        assert!(decode(old).is_err());
        let gone = serde_json::json!({"kind": "snapshot_get", "entry": "v~0"});
        assert!(matches!(decode(gone), Ok(Job::Unsupported)));
    }
}
