//! Tactic sessions on the REPL's tactic mode, with proof states pickled into
//! shared storage so that any worker of the same version can continue them.

use std::sync::Arc;
use std::time::Duration;

use pisa_protocol::api::{self, ApplyTacticResponse, Error, ErrorKind, Key, Session, State};
use pisa_protocol::header::split_header;
use pisa_protocol::repl::{self, ReplCommand, ReplResponse};
use pisa_protocol::verify::{grouped, parent_in, statement_changes};
use pisa_protocol::wire::{
    CheckJob, CheckOutcome, SessionCheckJob, SessionChecked, SessionStartJob, SessionTacticJob,
};

use crate::check::{axiom_report, header_error, policy_error, run, run_check, unexpected};
use crate::db::DbError;
use crate::policy::read_report;
use crate::pool::{Lease, Pool, Resident};
use crate::store::{SessionMeta, StateMeta, Store, now_secs};

/// Pickling and unpickling only touch the state's own constants.
const PICKLE_TIMEOUT: Duration = Duration::from_secs(60);

pub struct Sessions {
    pub store: Store,
    pub version: String,
}

/// What a session's messages say when its code declares something its
/// states can't see (see [`hidden_from_states`]).
const HIDDEN: &str = "tactics on this session's states can't use what its code declares before a \
`sorry`, from here on: each state starts from the imports alone. Put those declarations in a \
snapshot, and start the session on its header";

/// What starts a command that declares something, after its attributes and
/// modifiers.
const DECLARING: &[&str] = &[
    "def",
    "theorem",
    "lemma",
    "abbrev",
    "instance",
    "structure",
    "class",
    "inductive",
    "axiom",
    "opaque",
    "notation",
    "infix",
    "infixl",
    "infixr",
    "prefix",
    "postfix",
    "macro",
    "macro_rules",
    "syntax",
    "declare_syntax_cat",
    "attribute",
    "deriving",
    "export",
    // Mathlib's and Batteries'.
    "alias",
    "irreducible_def",
    "notation3",
];

/// What starts a command that declares nothing; a word starting with `#`
/// does too.
const COMMANDS: &[&str] = &[
    "example",
    "open",
    "variable",
    "universe",
    "section",
    "namespace",
    "end",
    "set_option",
    "mutual",
    "omit",
    "include",
];

const MODIFIERS: &[&str] = &[
    "private",
    "protected",
    "noncomputable",
    "partial",
    "nonrec",
    "public",
    "meta",
    "scoped",
    "local",
];

/// The first line of `body`, from 1, of a declaration that the states of the
/// `sorry`s on `sorry_lines` can't see, if any. The REPL makes each state on
/// the environment the body started from, so what the body declares before
/// the command that holds a `sorry` isn't in its state. A command starts on a
/// line at column 0 outside a comment whose first word, after attributes and
/// modifiers, is a command's keyword, from the attributes above it, and
/// declares something by that keyword. Any other line at column 0, such as a
/// match alternative or `termination_by`, continues the command above.
fn hidden_from_states(body: &str, sorry_lines: impl IntoIterator<Item = u64>) -> Option<u64> {
    let last = sorry_lines.into_iter().max()?;
    // Each command's first line, and whether it declares something.
    let mut commands: Vec<(u64, bool)> = Vec::new();
    let mut attributes: Option<u64> = None;
    let mut comments = 0usize;
    for (n, line) in (1..=last).zip(body.lines()) {
        let starts = comments == 0
            && line.starts_with(|c: char| !c.is_whitespace())
            && !line.starts_with("--")
            && !line.starts_with("/-");
        comments = comment_depth(line, comments);
        if !starts {
            continue;
        }
        let mut rest = line;
        while let Some(inner) = rest
            .strip_prefix("@[")
            .or_else(|| rest.strip_prefix("scoped["))
        {
            rest = skip_brackets(inner).trim_start();
        }
        if rest.is_empty() {
            attributes.get_or_insert(n);
            continue;
        }
        let mut words = rest.split(|c: char| c.is_whitespace() || c == '(' || c == ':');
        let Some(keyword) = words.find(|w| !w.is_empty() && !MODIFIERS.contains(w)) else {
            continue;
        };
        let declares = DECLARING.contains(&keyword);
        if !declares && !COMMANDS.contains(&keyword) && !keyword.starts_with('#') {
            continue;
        }
        commands.push((attributes.take().unwrap_or(n), declares));
    }
    let (holder, _) = *commands.last()?;
    commands
        .iter()
        .find(|(start, declares)| *declares && *start < holder)
        .map(|(start, _)| *start)
}

/// The block comment depth after `line`, from `depth` before it: `/-` opens
/// one and `-/` closes one, outside strings and `--` comments.
fn comment_depth(line: &str, mut depth: usize) -> usize {
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, chars.peek()) {
            ('/', Some('-')) => {
                chars.next();
                depth += 1;
            }
            ('-', Some('/')) if depth > 0 => {
                chars.next();
                depth -= 1;
            }
            ('-', Some('-')) if depth == 0 => break,
            ('"', _) if depth == 0 => {
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => {
                            chars.next();
                        }
                        '"' => break,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    depth
}

/// What follows the `]` that closes an attribute list whose `@[` came just
/// before `s`, or nothing if it doesn't close on the line.
fn skip_brackets(s: &str) -> &str {
    let mut depth = 1;
    for (i, c) in s.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return &s[i + 1..];
                }
            }
            _ => {}
        }
    }
    ""
}

fn internal(e: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::Internal, e.to_string())
}

/// The error for a [`Store`] call that failed: `unavailable` if the database
/// did, else `internal`.
fn store_error(e: anyhow::Error) -> Error {
    match e.downcast::<DbError>() {
        Ok(e) => e.into(),
        Err(e) => internal(e),
    }
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
    pub async fn start(&self, pool: &Arc<Pool>, job: SessionStartJob) -> Result<Session, Error> {
        let split = split_header(&job.code);
        let found = pool.policy().findings(&split.body, false);
        if !found.is_empty() {
            return Err(policy_error(&found, split.header_lines));
        }
        let mut slot = pool.acquire(&split.header_key, None).await;
        let result = self.start_in(&mut slot, job, split).await;
        slot.finish_job();
        result
    }

    async fn start_in(
        &self,
        slot: &mut Lease,
        job: SessionStartJob,
        split: pisa_protocol::header::Split,
    ) -> Result<Session, Error> {
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
        let report = axiom_report(slot, &r).await;
        let reported = report.and_then(|r| read_report(&r).ok().map(|r| r.declarations));
        let declarations = reported
            .map(|decls| slot.policy().judge_decls(false, Some(decls)).declarations)
            .unwrap_or_default();

        let stated = r.sorries.iter().filter(|s| s.proof_state.is_some());
        if let Some(line) =
            hidden_from_states(&split.body, stated.filter_map(|s| s.pos.map(|p| p.line)))
        {
            r.messages.push(repl::Message {
                severity: repl::Severity::Warning,
                pos: repl::Pos { line, column: 0 },
                end_pos: None,
                data: HIDDEN.into(),
            });
        }

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
                created: now_secs(),
            };
            let hash = self.save(slot, proof_state, &meta).await?;
            states.push(State {
                id: self.key(&split.header_key, &hash),
                goals: vec![sorry.goal.clone()],
                pos: sorry.pos.map(Into::into),
                end_pos: sorry.end_pos.map(Into::into),
            });
        }
        let meta = SessionMeta {
            code: job.code,
            declarations,
            created: now_secs(),
        };
        self.store
            .put_session(&session_id, &meta)
            .await
            .map_err(store_error)?;
        Ok(Session {
            id: session,
            states,
            messages: api::messages(r.messages),
        })
    }

    /// Applies one tactic to a stored proof state and stores the result.
    pub async fn tactic(
        &self,
        pool: &Arc<Pool>,
        job: SessionTacticJob,
    ) -> Result<ApplyTacticResponse, Error> {
        let key = self.parse(&job.state)?;
        let found = pool.policy().findings(&job.tactic, false);
        if !found.is_empty() {
            return Err(policy_error(&found, 0));
        }
        let mut slot = pool.acquire(&key.header_key, Some(&key.id)).await;
        let result = self.tactic_in(&mut slot, &key, job).await;
        slot.finish_job();
        result
    }

    async fn tactic_in(
        &self,
        slot: &mut Lease,
        key: &Key,
        job: SessionTacticJob,
    ) -> Result<ApplyTacticResponse, Error> {
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
            created: now_secs(),
        };
        let hash = self.save(slot, step.proof_state, &meta).await?;
        Ok(ApplyTacticResponse {
            state: State {
                id: self.key(&key.header_key, &hash),
                goals: step.goals,
                pos: None,
                end_pos: None,
            },
            complete: step.proof_status == "Completed",
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
            },
            statement_reasons: Vec::new(),
            statement_changes: Vec::new(),
        };
        let key = match self.parse(&job.session) {
            Ok(k) => k,
            Err(e) => return failed(e),
        };
        let session = match self.store.session(&key.id).await {
            Ok(Some(s)) => s,
            Ok(None) => {
                return failed(Error::new(
                    ErrorKind::NotFound,
                    format!("unknown session: {}", key.id),
                ));
            }
            Err(e) => return failed(store_error(e)),
        };
        let same_imports = split_header(&job.code).header_key == key.header_key;
        let outcome = run_check(
            pool,
            CheckJob {
                code: job.code,
                timeout_secs: job.timeout_secs,
                detail: Vec::new(),
                at: Vec::new(),
            },
        )
        .await;
        let now = outcome
            .verification
            .as_ref()
            .map(|v| v.declarations.as_slice())
            .unwrap_or_default();
        let changes = grouped(statement_changes(&session.declarations, now), |c| {
            parent_in(&session.declarations, &c.name)
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
            .map_err(store_error)?;
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
            .map_err(store_error)?
            .ok_or_else(|| Error::new(ErrorKind::NotFound, format!("unknown state: {}", key.id)))?;
        let (env, _) = slot
            .warm_up(&meta.header, &key.header_key)
            .await
            .map_err(|f| header_error(f, slot.import_timeout()))?;
        let pickle = self
            .store
            .state_pickle(&key.id)
            .await
            .map_err(internal)?
            .ok_or_else(|| Error::new(ErrorKind::NotFound, format!("unknown state: {}", key.id)))?;
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

#[cfg(test)]
mod tests {
    use super::hidden_from_states;

    /// [`hidden_from_states`] with a `sorry` on each line that names one.
    fn hidden(body: &str) -> Option<u64> {
        let sorries = (1..).zip(body.lines()).filter(|(_, l)| l.contains("sorry"));
        hidden_from_states(body, sorries.map(|(n, _)| n))
    }

    #[test]
    fn nothing_declared_before_the_sorry_is_hidden() {
        assert_eq!(hidden("theorem t : 1 = 1 := by sorry\n"), None);
        let opened = "open Nat\n\n-- t\ntheorem t : 1 = 1 := by\n  sorry\n";
        assert_eq!(hidden(opened), None);
        assert_eq!(hidden("def foo : Nat := 1\n"), None);
    }

    #[test]
    fn a_declaration_before_the_command_of_a_sorry_is_hidden() {
        let code = "def foo : Nat := 1\n\ntheorem t : foo = 1 := by sorry\n";
        assert_eq!(hidden(code), Some(1));
        // From its first line, attributes included, past modifiers.
        let code = "open Nat\n@[simp]\nprivate theorem a : 1 = 1 := rfl\n\
                    noncomputable def b : Nat := 1\nexample : b = 1 := by\n  sorry\n";
        assert_eq!(hidden(code), Some(2));
        let code = "@[simp] lemma a : 1 = 1 := rfl\ntheorem t : 1 = 1 := sorry\n";
        assert_eq!(hidden(code), Some(1));
        // An earlier theorem with a sorry of its own, notation, a namespace's.
        let code = "theorem a : 1 = 1 := sorry\ntheorem b : 2 = 2 := sorry\n";
        assert_eq!(hidden(code), Some(1));
        let code = "local notation \"d\" => 2\ntheorem t : d = 2 := sorry\n";
        assert_eq!(hidden(code), Some(1));
        let code = "namespace N\ndef foo : Nat := 1\nend N\ntheorem t : N.foo = 1 := sorry\n";
        assert_eq!(hidden(code), Some(2));
    }

    #[test]
    fn what_comes_after_the_last_sorry_or_in_a_comment_isnt() {
        let code = "theorem t : 1 = 1 := sorry\ndef foo : Nat := 1\n";
        assert_eq!(hidden(code), None);
        let code = "theorem t : f 1 = 1 := by\n  sorry\nwhere\n  f (n : Nat) : Nat := n\n";
        assert_eq!(hidden(code), None);
        for comment in [
            "-- def foo : Nat := 1\n",
            "/- def foo : Nat := 1 -/\n",
            "/-\ndef foo : Nat := 1\n-/\n",
            "/- a /- b -/\ndef foo : Nat := 1\n-/\n",
            "/-- A doc.\ndef foo : Nat := 1\n-/\n",
        ] {
            let code = format!("{comment}theorem t : 1 = 1 := sorry\n");
            assert_eq!(hidden(&code), None, "{code}");
        }
        // Indented, it is part of the command above.
        let code = "theorem t : 1 = 1 := by\n  have : True := by\n    sorry\n  rfl\n";
        assert_eq!(hidden(code), None);
    }

    #[test]
    fn a_line_at_column_0_without_a_command_continues_the_one_above() {
        for code in [
            "theorem t : ∀ n : Nat, n + 0 = n\n| 0 => by sorry\n| n + 1 => by sorry\n",
            "theorem t (a b : Nat)\n(h : a = b) : b = a := by\n  sorry\n",
            "def g : Nat := f 1\nwhere\n  f (n : Nat) : Nat := sorry\n",
            "def f (n : Nat) : Nat := n\ntermination_by n\ndecreasing_by sorry\n",
            "theorem t : 1 = 1 :=\nby sorry\n",
        ] {
            assert_eq!(hidden(code), None, "{code}");
        }
        // Commands of Mathlib's, and a scoped notation.
        for code in [
            "irreducible_def foo : Nat := 1\ntheorem t : foo = 1 := sorry\n",
            "alias foo := Nat.add_comm\ntheorem t : 1 = 1 := sorry\n",
            "scoped[N] notation \"d\" => 2\ntheorem t : 1 = 1 := sorry\n",
        ] {
            assert_eq!(hidden(code), Some(1), "{code}");
        }
    }
}
