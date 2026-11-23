//! Tactic sessions on open documents (`/sessions`, `/states/{id}/tactic`). A
//! session is a document of code with `sorry`s, and a state is one of those
//! `sorry`s or a tactic applied to another state, which the records keep
//! (`states`). Beam chains steps from a `sorry` with handles, which live in
//! one process: a worker without a state's handle, because another worker
//! opened the document or its Beam restarted, replays the chain from the
//! nearest state it has one for. A tactic runs in the file's own context, so
//! it sees what the session's code declares before the `sorry`.

use pisa_protocol::api::{ApplyTacticResponse, Key, Session, State, Timing};
use pisa_protocol::wire::{SessionStartJob, SessionTacticJob};
use sha2::{Digest, Sha256};

use super::*;
use crate::db::DbError;
use crate::limits::{lean_limit, limit_in};
use crate::records::StateRow;
use crate::sessions::SessionMeta;

/// The most handles an open document keeps. The oldest goes first, and its
/// state replays when it is asked for again.
const HANDLES: usize = 256;

/// Where a step runs from: a `sorry`, or a handle on an earlier step.
enum From {
    Sorry(LspPos),
    Handle(Handle),
}

/// Why a step couldn't be run: Beam's failure, or another error.
enum Stop {
    Beam(BeamError),
    Error(Error),
}

impl std::convert::From<DbError> for Stop {
    fn from(e: DbError) -> Self {
        Stop::Error(e.into())
    }
}

/// 32 hex characters of the SHA-256 of `parts`, each ended by a NUL.
fn digest(parts: &[&str]) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update(part.as_bytes());
        hash.update([0]);
    }
    hex::encode(&hash.finalize()[..16])
}

/// The state of session `session`'s `n`th `sorry`.
fn root_id(session: &str, n: usize) -> String {
    digest(&["sorry", session, &n.to_string()])
}

/// The state `tactic` leads to from state `parent`: the same tactic on the
/// same state is the same state.
fn child_id(parent: &str, tactic: &str) -> String {
    digest(&["tactic", parent, tactic])
}

/// The errors of a failed step, as one message.
fn errors(r: &RunResult) -> String {
    let texts: Vec<&str> = r
        .messages
        .iter()
        .filter(|m| m.severity == "error")
        .map(|m| m.text.as_str())
        .collect();
    match texts.is_empty() {
        true => "the tactic failed".to_string(),
        false => texts.join("\n"),
    }
}

impl Documents {
    fn state_key(&self, session: &str, state: &str) -> String {
        Key {
            version: self.version.clone(),
            session: session.to_string(),
            id: state.to_string(),
        }
        .to_string()
    }

    /// Opens `j.code` as document `j.document` and records it as a session,
    /// with a state for each of its `sorry`s.
    pub(super) async fn start_session(&self, j: SessionStartJob) -> Result<Session, Error> {
        self.scan_code(&j.code)?;
        let budget = Budget::new(j.timeout_secs);
        let id = &j.document;
        let mut doc = self.lock(id).await;
        let opening = Instant::now();
        self.load(&mut doc, id, &j.code, 1, budget, opening).await?;
        let execution = round6(opening.elapsed().as_secs_f64());
        let now = now_secs();
        let meta = SessionMeta {
            code: j.code.clone(),
            declarations: None,
            created: now,
            runs: j.runs.clone(),
        };
        let open = doc.as_ref().unwrap();
        let stored = async {
            self.records
                .put_document(id, &j.code, j.timeout_secs, &self.me, now, &j.runs)
                .await?;
            let meta = serde_json::to_value(&meta).unwrap_or_default();
            self.records.put_session(id, now, &meta).await?;
            let mut states = Vec::new();
            for (n, (sorry, _)) in open.sorries.iter().enumerate() {
                let state = root_id(id, n);
                let row = StateRow {
                    session: id.clone(),
                    sorry: Some(n as u32),
                    parent: None,
                    tactic: None,
                };
                self.records.put_state(&state, &row, now).await?;
                states.push(State {
                    id: self.state_key(id, &state),
                    goals: (!sorry.goal.is_empty())
                        .then(|| sorry.goal.clone())
                        .into_iter()
                        .collect(),
                    pos: sorry.pos,
                    end_pos: sorry.end_pos,
                });
            }
            Ok::<_, DbError>(states)
        }
        .await;
        match stored {
            Ok(states) => Ok(Session {
                id: format!("{}~{id}", self.version),
                states,
                messages: open.messages.clone(),
                runs: j.runs,
                timing: Timing {
                    execution,
                    ..Timing::default()
                },
            }),
            Err(e) => {
                self.unload(&mut doc, id).await;
                Err(e.into())
            }
        }
    }

    /// Applies `j.tactic` to state `j.state`, on its session's document.
    pub(super) async fn tactic(&self, j: SessionTacticJob) -> Result<ApplyTacticResponse, Error> {
        let key = Key::parse(&j.state)
            .filter(|k| k.version == self.version)
            .ok_or_else(|| bad_request(format!("invalid state id: {}", j.state)))?;
        self.scan(&j.tactic, 0)?;
        let (session, state) = (&key.session, &key.id);
        let unknown = || {
            Error::new(
                ErrorKind::NotFound,
                format!(
                    "no state {}: it expired, or its session was closed",
                    j.state
                ),
            )
        };
        let row = self
            .records
            .state(state)
            .await?
            .filter(|r| &r.session == session)
            .ok_or_else(unknown)?;
        let mut doc = self.lock(session).await;
        let mut load = Duration::ZERO;
        let loading = Instant::now();
        let reopened = match self.ready(&mut doc, session, None).await {
            Ok(reopened) => reopened,
            // The session's document is gone; a reopen's own error stands.
            Err(e) if e == not_found(&self.version, session) => return Err(unknown()),
            Err(e) => return Err(e),
        };
        load += loading.elapsed();
        // Replays take the time limit the session's start took, as its
        // reopen does.
        let replay = Budget::new(doc.as_ref().map_or(j.timeout_secs, |o| o.timeout));
        let mut retried = false;
        let (r, at, replayed, execution) = loop {
            let replaying = Instant::now();
            let started = self.start_of(&mut doc, session, state, row.clone()).await;
            load += replaying.elapsed();
            let (from, at, replayed) = match started {
                Ok(from) => from,
                Err(Stop::Beam(e)) => {
                    let e = self.fail(&mut doc, session, e, replay, "").await;
                    return Err(after_reopen(e, reopened));
                }
                Err(Stop::Error(e)) => return Err(after_reopen(e, reopened)),
            };
            let budget = Budget::new(j.timeout_secs);
            let stepping = Instant::now();
            let stepped = self.step(&doc, session, &from, &j.tactic, budget).await;
            let execution = stepping.elapsed();
            match stepped {
                Ok(r) => {
                    self.answered(doc.as_ref().unwrap().slot);
                    break (r, at, replayed, execution);
                }
                // Lean's answer is a result, so this is Beam's own failure,
                // perhaps of the handle: replay the state once without it. A
                // limit Lean reached comes again on a replay.
                Err(BeamError::Tool { code, message, .. })
                    if !retried && lean_limit(&code, &message, &j.tactic).is_none() =>
                {
                    tracing::info!(session, state, "Lean Beam failed on a step: {message}");
                    self.forget(&mut doc, session, state).await;
                    retried = true;
                }
                Err(e) => {
                    let e = self.fail(&mut doc, session, e, budget, &j.tactic).await;
                    return Err(after_reopen(e, reopened));
                }
            }
        };
        if !r.success {
            return Err(
                limit_in(&r, &j.tactic).unwrap_or_else(|| Error::new(ErrorKind::Lean, errors(&r)))
            );
        }
        let child = child_id(state, &j.tactic);
        let next = StateRow {
            session: session.clone(),
            sorry: None,
            parent: Some(state.clone()),
            tactic: Some(j.tactic.clone()),
        };
        self.records.put_state(&child, &next, now_secs()).await?;
        if let Some(handle) = r.next_handle.clone() {
            self.keep(&mut doc, session, &child, handle, at).await;
        }
        let goals: Vec<String> = r
            .proof_state
            .iter()
            .flat_map(|s| &s.goals)
            .map(pisa_beam::goal::render)
            .collect();
        // A `sorry` anywhere on the way leaves the proof unfinished, as
        // `have h : P := sorry` and then `exact h` does.
        let complete =
            goals.is_empty() && !says_sorry(&j.tactic) && !self.sorry_on_the_way(state).await?;
        let open = doc.as_ref().unwrap();
        let messages = r
            .messages
            .iter()
            .map(|m| open.map.message(&m.severity, &m.text, m.range, Some(at)))
            .collect();
        Ok(ApplyTacticResponse {
            state: State {
                id: self.state_key(session, &child),
                goals: goals.clone(),
                pos: None,
                end_pos: None,
            },
            complete,
            messages,
            reopened: reopened.is_some(),
            replayed,
            timing: Timing {
                load: round6(load.as_secs_f64()),
                execution: round6(execution.as_secs_f64()),
                ..Timing::default()
            },
        })
    }

    /// Where state `id` (`row`) continues from in `doc`, its `sorry`'s
    /// position and the steps replayed: the `sorry` itself, or a handle on
    /// the state, which the chain is replayed for from the nearest state with
    /// one if need be, each step within the time limit the document's open
    /// took.
    async fn start_of(
        &self,
        doc: &mut Option<Open>,
        session: &str,
        id: &str,
        row: StateRow,
    ) -> Result<(From, LspPos, u32), Stop> {
        let gone = || {
            Stop::Error(Error::new(
                ErrorKind::NotFound,
                format!("a state on the way to {id} expired"),
            ))
        };
        let mut chain = vec![(id.to_string(), row)];
        loop {
            let (state, row) = chain.last().unwrap();
            let held = doc.as_ref().is_some_and(|o| o.handles.contains_key(state));
            if row.sorry.is_some() || held {
                break;
            }
            let parent = row.parent.clone().ok_or_else(gone)?;
            let above = self.records.state(&parent).await?.ok_or_else(gone)?;
            chain.push((parent, above));
        }
        let (top, top_row) = chain.pop().unwrap();
        let open = doc.as_ref().unwrap();
        let (mut from, at) = match (open.handles.get(&top), top_row.sorry) {
            (Some((handle, at)), _) => (From::Handle(handle.clone()), *at),
            (None, Some(n)) => {
                let (_, at) = open.sorries.get(n as usize).ok_or_else(gone)?;
                (From::Sorry(*at), *at)
            }
            (None, None) => return Err(gone()),
        };
        // Replay each step below it, from the top down.
        let (timeout, replayed) = (open.timeout, chain.len() as u32);
        if replayed > 0 {
            tracing::info!(session, id, replayed, "replaying a state's steps");
        }
        // A replay that fails lets go of the handles it made on the way.
        let mut made = Vec::new();
        for (state, row) in chain.into_iter().rev() {
            let tactic = row.tactic.unwrap_or_default();
            let stop = match self
                .step(doc, session, &from, &tactic, Budget::new(timeout))
                .await
            {
                Ok(r) => match r.next_handle.clone().filter(|_| r.success) {
                    Some(handle) => {
                        self.keep(doc, session, &state, handle.clone(), at).await;
                        made.push(state);
                        from = From::Handle(handle);
                        continue;
                    }
                    None => Stop::Error(Error::new(
                        ErrorKind::Internal,
                        format!("replaying `{tactic}` failed: {}", errors(&r)),
                    )),
                },
                Err(e) => Stop::Beam(e),
            };
            for state in &made {
                self.forget(doc, session, state).await;
            }
            return Err(stop);
        }
        Ok((from, at, replayed))
    }

    /// Whether a tactic on the way from its `sorry` to state `id` says
    /// `sorry` or `admit`. One that expired on the way can't be read, so the
    /// way counts as unfinished.
    async fn sorry_on_the_way(&self, id: &str) -> Result<bool, Error> {
        let mut next = Some(id.to_string());
        while let Some(id) = next {
            let Some(row) = self.records.state(&id).await? else {
                return Ok(true);
            };
            if row.tactic.as_deref().is_some_and(says_sorry) {
                return Ok(true);
            }
            next = row.parent;
        }
        Ok(false)
    }

    /// Runs `text` from `from` on `session`'s open document, keeping its
    /// result.
    async fn step(
        &self,
        doc: &Option<Open>,
        session: &str,
        from: &From,
        text: &str,
        budget: Budget,
    ) -> Result<RunResult, BeamError> {
        let open = doc.as_ref().unwrap();
        let path = file(session);
        let d = self.slots[open.slot].workspace.doc(&path);
        match from {
            From::Sorry(at) => {
                open.beam
                    .run_at_handle(d, open.version, *at, text, budget.left())
                    .await
            }
            From::Handle(handle) => open.beam.run_with(d, handle, text, budget.left()).await,
        }
    }

    /// Lets go of the handle on state `state` of `session`'s open document,
    /// so that the state replays when it is next asked for.
    async fn forget(&self, doc: &mut Option<Open>, session: &str, state: &str) {
        let Some(open) = doc.as_mut() else { return };
        if let Some((handle, _)) = open.handles.remove(state) {
            open.kept.retain(|s| s != state);
            let path = file(session);
            let d = self.slots[open.slot].workspace.doc(&path);
            let _ = open.beam.release(d, &handle, CLOSE_TIMEOUT).await;
        }
    }

    /// Keeps `handle` on state `state` of `session`'s open document, letting
    /// the oldest handle go past [`HANDLES`].
    async fn keep(
        &self,
        doc: &mut Option<Open>,
        session: &str,
        state: &str,
        handle: Handle,
        at: LspPos,
    ) {
        let Some(open) = doc.as_mut() else { return };
        if open
            .handles
            .insert(state.to_string(), (handle, at))
            .is_none()
        {
            open.kept.push_back(state.to_string());
        }
        while open.kept.len() > HANDLES {
            let Some(old) = open.kept.pop_front() else {
                break;
            };
            if let Some((handle, _)) = open.handles.remove(&old) {
                let path = file(session);
                let d = self.slots[open.slot].workspace.doc(&path);
                let _ = open.beam.release(d, &handle, CLOSE_TIMEOUT).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_state_is_named_by_what_it_is() {
        assert_eq!(root_id("s", 0), root_id("s", 0));
        assert_ne!(root_id("s", 0), root_id("s", 1));
        assert_ne!(root_id("s", 0), root_id("t", 0));
        let root = root_id("s", 0);
        assert_eq!(child_id(&root, "simp"), child_id(&root, "simp"));
        assert_ne!(child_id(&root, "simp"), child_id(&root, "rfl"));
        // NULs part the pieces, so no two splits of one text meet.
        assert_ne!(digest(&["ab", "c"]), digest(&["a", "bc"]));
        assert_eq!(root.len(), 32);
    }
}
