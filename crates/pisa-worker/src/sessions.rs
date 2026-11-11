//! A tactic session's final check (`/sessions/{id}/check`), on the REPL: the
//! completed code is checked again from scratch and must prove the
//! statements the session started from, with the same imports. The session
//! itself runs on Lean Beam, as an open document ([`crate::documents`]). Its
//! record keeps the code it started from, whose statements the first check
//! works out and keeps.

use std::sync::Arc;

use pisa_protocol::api::{DocumentId, Error, ErrorKind, RunImport};
use pisa_protocol::header::split_header;
use pisa_protocol::verify::{Declaration, grouped, parent_in, statement_changes};
use pisa_protocol::wire::{CheckJob, CheckOutcome, SessionCheckJob, SessionChecked};
use serde::{Deserialize, Serialize};

use crate::check::run_check;
use crate::pool::Pool;
use crate::records::Records;

/// A session's record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    /// The code it started from.
    pub code: String,
    /// That code's declarations, by the REPL's report: the statements a final
    /// check keeps. The first check works them out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declarations: Option<Vec<Declaration>>,
    /// What the code's imports by run name resolved to when it started:
    /// a final check's resolve the same.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<RunImport>,
    /// Seconds since the Unix epoch.
    pub created: u64,
}

pub struct Sessions {
    pub records: Records,
    pub version: String,
}

impl Sessions {
    /// Re-checks completed code from scratch and confirms it proves the same
    /// statements the session started from, with the same imports: other
    /// imports could redefine what the statements mention.
    pub async fn check(&self, pool: &Arc<Pool>, job: SessionCheckJob) -> SessionChecked {
        let failed = |error: Error| SessionChecked {
            outcome: CheckOutcome {
                time: 0.0,
                error: Some(error),
                response: None,
                verification: None,
                runs: Vec::new(),
            },
            statement_reasons: Vec::new(),
            statement_changes: Vec::new(),
        };
        let id = match DocumentId::parse(&job.session).filter(|id| id.version == self.version) {
            Some(id) => id.id,
            None => {
                return failed(Error::new(
                    ErrorKind::BadRequest,
                    format!("invalid session id: {}", job.session),
                ));
            }
        };
        let mut session: SessionMeta = match self.records.session(&id).await {
            Ok(Some(meta)) => match serde_json::from_value(meta) {
                Ok(meta) => meta,
                Err(e) => return failed(Error::new(ErrorKind::Internal, e.to_string())),
            },
            Ok(None) => {
                return failed(Error::new(
                    ErrorKind::NotFound,
                    format!("unknown session: {}", job.session),
                ));
            }
            Err(e) => return failed(e.into()),
        };
        let check = |code: String, runs: Vec<RunImport>| {
            run_check(
                pool,
                CheckJob {
                    code,
                    timeout_secs: job.timeout_secs,
                    detail: Vec::new(),
                    at: Vec::new(),
                    runs,
                },
            )
        };
        let original = match session.declarations.clone() {
            Some(declarations) => declarations,
            None => {
                let started = check(session.code.clone(), Vec::new()).await;
                if let Some(e) = started.error {
                    return failed(e);
                }
                // Checks are held to the statements the report describes, so
                // one it couldn't describe would go unguarded.
                let unguarded = started
                    .verification
                    .iter()
                    .flat_map(|v| &v.reasons)
                    .find(|r| r.starts_with("error: axiom report"));
                if let Some(reason) = unguarded {
                    let reason = reason.trim_start_matches("error: ");
                    return failed(Error::new(
                        ErrorKind::Lean,
                        format!("the session can't guard its statements: {reason}"),
                    ));
                }
                let declarations = started.verification.map(|v| v.declarations);
                session.declarations = Some(declarations.unwrap_or_default());
                let meta = serde_json::to_value(&session).unwrap_or_default();
                if let Err(e) = self.records.put_session(&id, session.created, &meta).await {
                    tracing::warn!(session = %job.session, "keeping its statements failed: {e}");
                }
                session.declarations.clone().unwrap_or_default()
            }
        };
        // Imports by run name resolve as they did when the session started.
        let reader = pool.snapshots().reader();
        let resolved = match reader.resolve_runs(&job.code, &session.runs).await {
            Ok(resolved) => resolved,
            Err(e) => return failed(e),
        };
        if let Err(e) = reader.pins_kept(&resolved.runs).await {
            return failed(e);
        }
        let same_imports =
            split_header(&resolved.code).header_key == split_header(&session.code).header_key;
        let mut outcome = check(resolved.code, resolved.runs).await;
        outcome.runs = reader.moved(std::mem::take(&mut outcome.runs)).await;
        let now = outcome
            .verification
            .as_ref()
            .map(|v| v.declarations.as_slice())
            .unwrap_or_default();
        let changes = grouped(statement_changes(&original, now), |c| {
            parent_in(&original, &c.name)
        });
        let mut statement_reasons: Vec<String> = changes.iter().map(ToString::to_string).collect();
        if !same_imports {
            statement_reasons.push("statement: the imports differ from the session's".into());
        }
        SessionChecked {
            statement_reasons,
            statement_changes: changes,
            outcome,
        }
    }
}
