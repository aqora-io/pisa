//! Tactic sessions on the REPL's tactic mode, with proof states pickled into
//! shared storage so that any worker of the same version can continue them.

use std::sync::Arc;
use std::time::Duration;

use pisa_protocol::header::split_header;
use pisa_protocol::kimina::{header_timeout_error, timeout_error};
use pisa_protocol::repl::{ReplCommand, ReplResponse};
use pisa_protocol::session::{Key, StartResponse, StateInfo, TacticResponse};
use pisa_protocol::verify::Declaration;
use pisa_protocol::wire::{
    CheckJob, CheckOutcome, SessionCheckJob, SessionChecked, SessionStartJob, SessionTacticJob,
};

use crate::check::{Failure, axiom_report, run, run_check};
use crate::policy::parse_report;
use crate::pool::{HeaderFailure, Lease, Pool, Resident};
use crate::store::{SessionMeta, StateMeta, Store};

/// Pickling and unpickling only touch the state's own constants.
const PICKLE_TIMEOUT: Duration = Duration::from_secs(60);

pub struct Sessions {
    pub store: Store,
    pub version: String,
}

impl Sessions {
    fn key(&self, header_key: &str, id: &str) -> String {
        Key {
            version: self.version.clone(),
            header_key: header_key.to_string(),
            id: id.to_string(),
        }
        .to_string()
    }

    fn parse(&self, key: &str) -> Result<Key, String> {
        let key = Key::parse(key).ok_or_else(|| format!("invalid key: {key}"))?;
        if key.version != self.version {
            return Err(format!("key belongs to version {}", key.version));
        }
        Ok(key)
    }

    /// Runs code containing `sorry` and stores one proof state per `sorry`.
    pub async fn start(&self, pool: &Arc<Pool>, job: SessionStartJob) -> StartResponse {
        let mut resp = StartResponse::default();
        let split = split_header(&job.code);
        let mut slot = pool.acquire(&split.header_key, None).await;
        let rejected = slot.policy().scan(&split.body);
        if !rejected.is_empty() {
            resp.error = Some(format!("Rejected by policy: {}", rejected.join(", ")));
            return resp;
        }
        let env = match warm_up(&mut slot, &split.header, &split.header_key).await {
            Ok(env) => env,
            Err((messages, error)) => {
                resp.messages = messages;
                resp.error = Some(error);
                return resp;
            }
        };
        let cmd = ReplCommand::cmd(split.body.clone(), Some(env));
        let mut r = match run(&mut slot, &cmd, Duration::from_secs(job.timeout_secs)).await {
            Ok(ReplResponse::Command(r)) => r,
            Ok(ReplResponse::Error(e)) => {
                resp.error = Some(e.message);
                return resp;
            }
            Ok(other) => {
                resp.error = Some(format!("unexpected REPL response: {other:?}"));
                return resp;
            }
            Err(f) => {
                resp.error = Some(failure_message(f, job.timeout_secs));
                return resp;
            }
        };
        let declarations = axiom_report(&mut slot, &r)
            .await
            .as_ref()
            .and_then(parse_report)
            .unwrap_or_default();

        let session_id = uuid::Uuid::new_v4().simple().to_string();
        let session = self.key(&split.header_key, &session_id);
        r.shift_lines(split.header_lines);
        for sorry in &r.sorries {
            let Some(proof_state) = sorry.proof_state else {
                continue;
            };
            let meta = StateMeta {
                header: split.header.clone(),
                session: session.clone(),
                parent: None,
                tactic: None,
                goals: vec![sorry.goal.clone()],
            };
            match self.save(&mut slot, proof_state, &meta).await {
                Ok(hash) => {
                    resp.states.push(StateInfo {
                        state: self.key(&split.header_key, &hash),
                        goal: sorry.goal.clone(),
                        pos: sorry.pos,
                        end_pos: sorry.end_pos,
                    });
                }
                Err(e) => {
                    resp.error = Some(format!("saving proof state failed: {e}"));
                    slot.finish_job(true);
                    return resp;
                }
            }
        }
        let meta = SessionMeta {
            code: job.code,
            declarations,
        };
        if let Err(e) = self.store.put_session(&session_id, &meta).await {
            resp.error = Some(format!("saving session failed: {e}"));
        } else {
            resp.session = Some(session);
        }
        resp.messages = r.messages;
        slot.finish_job(true);
        resp
    }

    /// Applies one tactic to a stored proof state and stores the result.
    pub async fn tactic(&self, pool: &Arc<Pool>, job: SessionTacticJob) -> TacticResponse {
        let mut resp = TacticResponse::default();
        let key = match self.parse(&job.state) {
            Ok(k) => k,
            Err(e) => {
                resp.error = Some(e);
                return resp;
            }
        };
        let mut slot = pool.acquire(&key.header_key, Some(&key.id)).await;
        let rejected = slot.policy().scan(&job.tactic);
        if !rejected.is_empty() {
            resp.error = Some(format!("Rejected by policy: {}", rejected.join(", ")));
            return resp;
        }
        let resident = match slot.resident(&key.id) {
            Some(r) => r,
            None => match self.load(&mut slot, &key).await {
                Ok(r) => r,
                Err(e) => {
                    resp.error = Some(e);
                    slot.finish_job(true);
                    return resp;
                }
            },
        };

        let cmd = ReplCommand::Tactic {
            tactic: job.tactic.clone(),
            proof_state: resident.proof_state,
        };
        match run(&mut slot, &cmd, Duration::from_secs(job.timeout_secs)).await {
            Ok(ReplResponse::ProofStep(step)) => {
                let meta = StateMeta {
                    header: resident.header.clone(),
                    session: resident.session.clone(),
                    parent: Some(job.state),
                    tactic: Some(job.tactic),
                    goals: step.goals.clone(),
                };
                match self.save(&mut slot, step.proof_state, &meta).await {
                    Ok(hash) => resp.state = Some(self.key(&key.header_key, &hash)),
                    Err(e) => resp.error = Some(format!("saving proof state failed: {e}")),
                }
                resp.goals = step.goals;
                resp.proof_status = Some(step.proof_status);
                resp.messages = step.messages;
            }
            Ok(ReplResponse::Error(e)) => resp.error = Some(e.message),
            Ok(other) => resp.error = Some(format!("unexpected REPL response: {other:?}")),
            Err(f) => resp.error = Some(failure_message(f, job.timeout_secs)),
        }
        slot.finish_job(true);
        resp
    }

    /// Re-checks completed code from scratch and confirms it proves the same
    /// statements the session started from.
    pub async fn check(&self, pool: &Arc<Pool>, job: SessionCheckJob) -> SessionChecked {
        let failed = |error: String| SessionChecked {
            outcome: CheckOutcome {
                time: 0.0,
                error: Some(error),
                response: None,
                verification: None,
                warm: false,
            },
            statement_reasons: Vec::new(),
        };
        let key = match self.parse(&job.session) {
            Ok(k) => k,
            Err(e) => return failed(e),
        };
        let session = match self.store.session(&key.id).await {
            Ok(s) => s,
            Err(e) => return failed(format!("unknown session: {e}")),
        };
        let outcome = run_check(
            pool,
            CheckJob {
                code: job.code,
                timeout_secs: job.timeout_secs,
                infotree: None,
                reuse: true,
            },
        )
        .await;
        let now = outcome
            .verification
            .as_ref()
            .map(|v| v.declarations.as_slice())
            .unwrap_or_default();
        SessionChecked {
            statement_reasons: statement_changes(&session.declarations, now),
            outcome,
        }
    }

    /// Pickles `proof_state` from the slot's process into the store.
    async fn save(
        &self,
        slot: &mut Lease,
        proof_state: u64,
        meta: &StateMeta,
    ) -> anyhow::Result<String> {
        let path = slot
            .scratch_dir()
            .join(format!("{}.olean", uuid::Uuid::new_v4().simple()));
        let cmd = ReplCommand::PickleProofState {
            pickle_to: path.to_string_lossy().into_owned(),
            proof_state,
        };
        let result = run(slot, &cmd, PICKLE_TIMEOUT).await;
        let bytes = tokio::fs::read(&path).await;
        let _ = tokio::fs::remove_file(&path).await;
        match result {
            Ok(ReplResponse::ProofStep(_)) => {}
            Ok(ReplResponse::Error(e)) => anyhow::bail!(e.message),
            Ok(other) => anyhow::bail!("unexpected REPL response: {other:?}"),
            Err(f) => anyhow::bail!(failure_message(f, PICKLE_TIMEOUT.as_secs())),
        }
        let hash = self.store.put_state(&bytes?, meta).await?;
        slot.remember(
            hash.clone(),
            Resident {
                proof_state,
                header: meta.header.clone(),
                session: meta.session.clone(),
            },
        );
        Ok(hash)
    }

    /// Loads a stored state into the slot's process, importing its header first.
    async fn load(&self, slot: &mut Lease, key: &Key) -> Result<Resident, String> {
        let meta = self
            .store
            .state_meta(&key.id)
            .await
            .map_err(|e| format!("unknown state: {e}"))?;
        let env = warm_up(slot, &meta.header, &key.header_key)
            .await
            .map_err(|(_, e)| e)?;
        let pickle = self
            .store
            .state_pickle(&key.id)
            .await
            .map_err(|e| format!("loading state failed: {e}"))?;
        let path = slot
            .scratch_dir()
            .join(format!("{}.olean", uuid::Uuid::new_v4().simple()));
        tokio::fs::write(&path, pickle)
            .await
            .map_err(|e| format!("writing state failed: {e}"))?;
        let cmd = ReplCommand::UnpickleProofState {
            unpickle_proof_state_from: path.to_string_lossy().into_owned(),
            env: Some(env),
        };
        let result = run(slot, &cmd, PICKLE_TIMEOUT).await;
        let _ = tokio::fs::remove_file(&path).await;
        let proof_state = match result {
            Ok(ReplResponse::ProofStep(step)) => step.proof_state,
            Ok(ReplResponse::Error(e)) => return Err(e.message),
            Ok(other) => return Err(format!("unexpected REPL response: {other:?}")),
            Err(f) => return Err(failure_message(f, PICKLE_TIMEOUT.as_secs())),
        };
        let resident = Resident {
            proof_state,
            header: meta.header,
            session: meta.session,
        };
        slot.remember(key.id.clone(), resident.clone());
        Ok(resident)
    }
}

async fn warm_up(
    slot: &mut Lease,
    header: &str,
    header_key: &str,
) -> Result<u64, (Vec<pisa_protocol::repl::Message>, String)> {
    match slot.warm_up(header, header_key).await {
        Ok((env, _)) => Ok(env),
        Err(HeaderFailure::Lean(r)) => Err((r.messages, "header failed to elaborate".into())),
        Err(HeaderFailure::Timeout) => Err((
            Vec::new(),
            header_timeout_error(slot.import_timeout().as_secs()),
        )),
        Err(HeaderFailure::Repl(e)) => {
            Err((Vec::new(), format!("Lean REPL failed on header: {e}")))
        }
    }
}

fn failure_message(f: Failure, timeout_secs: u64) -> String {
    match f {
        Failure::Timeout => timeout_error(timeout_secs),
        Failure::OutOfMemory => "Lean REPL ran out of memory".into(),
        Failure::Crashed(e) => format!("Lean REPL failed: {e}"),
    }
}

/// Every declaration of the original code must still exist with the same
/// type, and definitions with the same value.
pub fn statement_changes(original: &[Declaration], now: &[Declaration]) -> Vec<String> {
    original
        .iter()
        .filter_map(|d| match now.iter().find(|n| n.name == d.name) {
            None => Some(format!("statement: {} is missing", d.name)),
            Some(n) if n.type_hash != d.type_hash => {
                Some(format!("statement: {} has a different type", d.name))
            }
            Some(n) if n.value_hash != d.value_hash => {
                Some(format!("statement: {} has a different value", d.name))
            }
            Some(_) => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decl(name: &str, ty: &str, value: Option<&str>) -> Declaration {
        Declaration {
            name: name.into(),
            kind: if value.is_some() { "def" } else { "theorem" }.into(),
            axioms: vec![],
            type_hash: ty.into(),
            value_hash: value.map(Into::into),
        }
    }

    #[test]
    fn statement_changes_catch_missing_retyped_and_redefined() {
        let original = [decl("t", "1", None), decl("f", "2", Some("3"))];
        assert!(statement_changes(&original, &original).is_empty());
        let mut extra = original.to_vec();
        extra.push(decl("helper", "9", None));
        assert!(statement_changes(&original, &extra).is_empty());
        assert_eq!(
            statement_changes(
                &original,
                &[decl("t", "X", None), decl("f", "2", Some("4"))]
            ),
            [
                "statement: t has a different type",
                "statement: f has a different value"
            ]
        );
        assert_eq!(
            statement_changes(&original, &[decl("f", "2", Some("3"))]),
            ["statement: t is missing"]
        );
    }
}
