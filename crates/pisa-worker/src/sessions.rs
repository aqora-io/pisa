//! Tactic sessions on the REPL's tactic mode, with proof states pickled into
//! shared storage so that any worker of the same version can continue them.

use std::sync::Arc;
use std::time::Duration;

use pisa_protocol::api::{self, Error, ErrorKind, Key, StartResponse, StateInfo, TacticResponse};
use pisa_protocol::header::split_header;
use pisa_protocol::repl::{ReplCommand, ReplResponse};
use pisa_protocol::verify::Declaration;
use pisa_protocol::wire::{
    CheckJob, CheckOutcome, SessionCheckJob, SessionChecked, SessionStartJob, SessionTacticJob,
};

use crate::check::{axiom_report, header_error, policy_error, run, run_check, unexpected};
use crate::policy::parse_report;
use crate::pool::{Lease, Pool, Resident};
use crate::store::{SessionMeta, StateMeta, Store};

/// Pickling and unpickling only touch the state's own constants.
const PICKLE_TIMEOUT: Duration = Duration::from_secs(60);

pub struct Sessions {
    pub store: Store,
    pub version: String,
}

fn internal(e: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::Internal, e.to_string())
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

    fn parse(&self, key: &str) -> Result<Key, Error> {
        let key = Key::parse(key)
            .ok_or_else(|| Error::new(ErrorKind::BadRequest, format!("invalid key: {key}")))?;
        if key.version != self.version {
            return Err(Error::new(
                ErrorKind::BadRequest,
                format!("key belongs to version {}", key.version),
            ));
        }
        Ok(key)
    }

    /// Runs code containing `sorry` and stores one proof state per `sorry`.
    pub async fn start(
        &self,
        pool: &Arc<Pool>,
        job: SessionStartJob,
    ) -> Result<StartResponse, Error> {
        let split = split_header(&job.code);
        let mut slot = pool.acquire(&split.header_key, None).await;
        let rejected = slot.policy().scan(&split.body, false);
        if !rejected.is_empty() {
            return Err(policy_error(&rejected));
        }
        let result = self.start_in(&mut slot, job, split).await;
        slot.finish_job();
        result
    }

    async fn start_in(
        &self,
        slot: &mut Lease,
        job: SessionStartJob,
        split: pisa_protocol::header::Split,
    ) -> Result<StartResponse, Error> {
        let (env, _) = slot
            .warm_up(&split.header, &split.header_key)
            .await
            .map_err(|f| header_error(f, slot.import_timeout()))?;
        let cmd = ReplCommand::cmd(split.body.clone(), Some(env));
        let mut r = match run(slot, &cmd, Duration::from_secs(job.timeout_secs)).await? {
            ReplResponse::Command(r) => r,
            ReplResponse::Error(e) => return Err(Error::new(ErrorKind::Lean, e.message)),
            other => return Err(unexpected(other)),
        };
        let declarations = axiom_report(slot, &r)
            .await
            .as_ref()
            .and_then(parse_report)
            .unwrap_or_default();

        let session_id = uuid::Uuid::new_v4().simple().to_string();
        let session = self.key(&split.header_key, &session_id);
        r.shift_lines(split.header_lines);
        let mut states = Vec::new();
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
            let hash = self.save(slot, proof_state, &meta).await?;
            states.push(StateInfo {
                state: self.key(&split.header_key, &hash),
                goal: sorry.goal.clone(),
                pos: sorry.pos.map(Into::into),
                end_pos: sorry.end_pos.map(Into::into),
            });
        }
        let meta = SessionMeta {
            code: job.code,
            declarations,
        };
        self.store
            .put_session(&session_id, &meta)
            .await
            .map_err(internal)?;
        Ok(StartResponse {
            session,
            states,
            messages: api::messages(r.messages),
        })
    }

    /// Applies one tactic to a stored proof state and stores the result.
    pub async fn tactic(
        &self,
        pool: &Arc<Pool>,
        job: SessionTacticJob,
    ) -> Result<TacticResponse, Error> {
        let key = self.parse(&job.state)?;
        let mut slot = pool.acquire(&key.header_key, Some(&key.id)).await;
        let rejected = slot.policy().scan(&job.tactic, false);
        if !rejected.is_empty() {
            return Err(policy_error(&rejected));
        }
        let result = self.tactic_in(&mut slot, &key, job).await;
        slot.finish_job();
        result
    }

    async fn tactic_in(
        &self,
        slot: &mut Lease,
        key: &Key,
        job: SessionTacticJob,
    ) -> Result<TacticResponse, Error> {
        let resident = match slot.resident(&key.id) {
            Some(r) => r,
            None => self.load(slot, key).await?,
        };
        let cmd = ReplCommand::Tactic {
            tactic: job.tactic.clone(),
            proof_state: resident.proof_state,
        };
        let step = match run(slot, &cmd, Duration::from_secs(job.timeout_secs)).await? {
            ReplResponse::ProofStep(step) => step,
            ReplResponse::Error(e) => return Err(Error::new(ErrorKind::Lean, e.message)),
            other => return Err(unexpected(other)),
        };
        let meta = StateMeta {
            header: resident.header.clone(),
            session: resident.session.clone(),
            parent: Some(job.state),
            tactic: Some(job.tactic),
            goals: step.goals.clone(),
        };
        let hash = self.save(slot, step.proof_state, &meta).await?;
        Ok(TacticResponse {
            state: self.key(&key.header_key, &hash),
            complete: step.proof_status == "Completed",
            goals: step.goals,
            messages: api::messages(step.messages),
        })
    }

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
            Err(e) => {
                return failed(Error::new(
                    ErrorKind::NotFound,
                    format!("unknown session: {e}"),
                ));
            }
        };
        let same_imports = split_header(&job.code).header_key == key.header_key;
        let outcome = run_check(
            pool,
            CheckJob {
                code: job.code,
                timeout_secs: job.timeout_secs,
            },
        )
        .await;
        let now = outcome
            .verification
            .as_ref()
            .map(|v| v.declarations.as_slice())
            .unwrap_or_default();
        let mut statement_reasons = statement_changes(&session.declarations, now);
        if !same_imports {
            statement_reasons.push("statement: the imports differ from the session's".into());
        }
        SessionChecked {
            statement_reasons,
            outcome,
        }
    }

    /// Pickles `proof_state` from the slot's process into the store.
    async fn save(
        &self,
        slot: &mut Lease,
        proof_state: u64,
        meta: &StateMeta,
    ) -> Result<String, Error> {
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
        match result? {
            ReplResponse::ProofStep(_) => {}
            ReplResponse::Error(e) => return Err(Error::new(ErrorKind::Lean, e.message)),
            other => return Err(unexpected(other)),
        }
        let hash = self
            .store
            .put_state(&bytes.map_err(internal)?, meta)
            .await
            .map_err(internal)?;
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
    async fn load(&self, slot: &mut Lease, key: &Key) -> Result<Resident, Error> {
        let meta = self
            .store
            .state_meta(&key.id)
            .await
            .map_err(|e| Error::new(ErrorKind::NotFound, format!("unknown state: {e}")))?;
        let (env, _) = slot
            .warm_up(&meta.header, &key.header_key)
            .await
            .map_err(|f| header_error(f, slot.import_timeout()))?;
        let pickle = self.store.state_pickle(&key.id).await.map_err(internal)?;
        let path = slot
            .scratch_dir()
            .join(format!("{}.olean", uuid::Uuid::new_v4().simple()));
        tokio::fs::write(&path, pickle).await.map_err(internal)?;
        let cmd = ReplCommand::UnpickleProofState {
            unpickle_proof_state_from: path.to_string_lossy().into_owned(),
            env: Some(env),
        };
        let result = run(slot, &cmd, PICKLE_TIMEOUT).await;
        let _ = tokio::fs::remove_file(&path).await;
        let proof_state = match result? {
            ReplResponse::ProofStep(step) => step.proof_state,
            ReplResponse::Error(e) => return Err(Error::new(ErrorKind::Lean, e.message)),
            other => return Err(unexpected(other)),
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
