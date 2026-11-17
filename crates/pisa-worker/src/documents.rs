//! Open documents (`/documents`) on Lean Beam: the goals at positions in a
//! client's code, and candidate tactics probed there, on code the client
//! keeps open and edits.
//!
//! Each Beam slot runs one `lean-beam-mcp`, in its own slot directory,
//! sandbox and cgroup as a REPL runs, with a Lake workspace over the project.
//! A document is the file `PisaDocs/D<id>.lean` there: the header as the REPL
//! imports it ([`header_command`]), then the code's body, so that positions in
//! the code map to positions in the file ([`CodeMap`]). A slot keeps up to
//! `beam_documents` documents open, and closes the least recently used one no
//! job holds to open another.
//!
//! The code is stored in the database too, so that a worker without the
//! document open, because the worker that had it is gone or it closed it,
//! reopens it there. Jobs on a document wait for the worker holding it while
//! that one is live ([`crate::claims`]), and run one at a time.

use std::collections::{HashMap, VecDeque};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use pisa_beam::pos::BodyMap;
use pisa_beam::{
    Beam, BeamError, Doc, Goal, Handle, LspPos, Mode, Packages, Range, RunResult, Workspace,
};
use pisa_protocol::api::{
    self, Error, ErrorKind, GoalsMode, LinePos, Pos, ProbeAt, ProbeResult, Timing,
};
use pisa_protocol::header::{Split, split_header};
use pisa_protocol::wire::{
    DocumentEditJob, DocumentGoalsJob, DocumentJob, DocumentOpenJob, DocumentProbeJob, Job,
    JobResult,
};
use tokio::sync::OwnedMutexGuard;

use crate::check::{policy_error, round6};
use crate::config::WorkerConfig;
use crate::db::Db;
use crate::launcher::Launcher;
use crate::limits::lean_limit;
use crate::policy::header_command;
use crate::pool::Pool;
use crate::records::{DocumentRow, Records};

mod sessions;

/// The library the documents' files belong to.
const LIB: &str = pisa_protocol::snapshot::DOCUMENTS;

/// How long `lean-beam-mcp` gets to start.
const SPAWN_TIMEOUT: Duration = Duration::from_secs(120);

/// How long closing a document gets.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(10);

/// Timeouts in a row after which a slot's Lean Beam is restarted, as what
/// timed out may still be running in it.
const RESTART_AFTER_TIMEOUTS: u32 = 3;

/// The toolchain of a workspace without a project. Deployments have a
/// project, whose toolchain workspaces take; this is for tests, on a fake
/// Beam.
const BARE_TOOLCHAIN: &str = "leanprover/lean4:stable";

pub struct Documents {
    pool: Arc<Pool>,
    records: Records,
    me: String,
    version: String,
    beam_bin: PathBuf,
    per_slot: usize,
    slots: Vec<BeamSlot>,
    /// Each document a job has touched here, behind the lock its jobs hold,
    /// with its state if it is open here.
    docs: Mutex<HashMap<String, Arc<tokio::sync::Mutex<Option<Open>>>>>,
    /// The slot of each document open here.
    placed: Mutex<HashMap<String, usize>>,
}

struct BeamSlot {
    /// The launcher's slot: its directory, sandbox and cgroup.
    id: usize,
    workspace: Workspace,
    beam: tokio::sync::Mutex<Option<Running>>,
    /// Requests to it that timed out since one last answered.
    timeouts: AtomicU32,
    /// The process last stopped on purpose, so that requests in flight on
    /// it, on its other documents too, say so rather than that it crashed.
    stopped: Mutex<Option<Arc<Beam>>>,
}

#[derive(Clone)]
struct Running {
    beam: Arc<Beam>,
    /// OOM kills in the slot's cgroup when it started.
    oom_baseline: u64,
}

/// A document open in a slot's Beam.
struct Open {
    slot: usize,
    /// The process it is open in: a slot's next process doesn't have it.
    beam: Arc<Beam>,
    revision: u64,
    /// The time limit its open took, which replays take too.
    timeout: u64,
    /// Beam's version of the file.
    version: u64,
    map: CodeMap,
    messages: Vec<api::Message>,
    /// Each `sorry`, with where probes at it run.
    sorries: Vec<(api::Sorry, LspPos)>,
    used: Instant,
    /// Tactic sessions' states this process has a handle on, with their
    /// `sorry`'s position, oldest first ([`sessions`]).
    handles: HashMap<String, (Handle, LspPos)>,
    kept: VecDeque<String>,
}

/// A job's time limit.
#[derive(Clone, Copy)]
struct Budget {
    until: Instant,
    secs: u64,
}

impl Budget {
    fn new(secs: u64) -> Budget {
        Budget {
            until: Instant::now() + Duration::from_secs(secs),
            secs,
        }
    }

    fn left(&self) -> Duration {
        self.until.saturating_duration_since(Instant::now())
    }

    /// A limit of `secs`, cut to what is left of this one; `None` once
    /// nothing is.
    fn within(&self, secs: u64) -> Option<Budget> {
        let left = self.left();
        if left.is_zero() {
            return None;
        }
        let own = Budget::new(secs);
        Some(match own.until <= self.until {
            true => own,
            false => Budget {
                until: self.until,
                secs: left.as_secs_f64().ceil() as u64,
            },
        })
    }
}

impl Documents {
    /// Sets up `cfg.beam_slots` Beam slots after `pool`'s REPL slots, and
    /// starts their processes, so that a broken setup fails at startup.
    pub async fn new(
        pool: Arc<Pool>,
        db: Db,
        version: String,
        me: String,
        cfg: &WorkerConfig,
    ) -> anyhow::Result<Documents> {
        let beam_bin = match &cfg.beam_bin {
            Some(bin) => bin.clone(),
            None => beam_in_toolchain(&cfg.lean, cfg.project.as_deref()).await?,
        };
        let mut slots = Vec::new();
        for id in pool.slots()..pool.slots() + cfg.beam_slots {
            let dir = pool.launcher().slot_dir(id).join("docs");
            // Documents left by an earlier process reopen from the database.
            let _ = std::fs::remove_dir_all(&dir);
            let workspace = match &cfg.project {
                Some(project) => Workspace::over(&dir, project, LIB, Packages::Symlink),
                None => Workspace::bare(&dir, BARE_TOOLCHAIN, LIB),
            }
            .with_context(|| format!("creating the documents' workspace in {}", dir.display()))?;
            slots.push(BeamSlot {
                id,
                workspace,
                beam: tokio::sync::Mutex::new(None),
                timeouts: AtomicU32::new(0),
                stopped: Mutex::default(),
            });
        }
        let documents = Documents {
            pool,
            records: Records::new(db, version.clone()),
            me,
            version,
            beam_bin,
            per_slot: cfg.beam_documents.max(1),
            slots,
            docs: Mutex::default(),
            placed: Mutex::default(),
        };
        for slot in 0..documents.slots.len() {
            documents
                .beam(slot)
                .await
                .map_err(|e| anyhow::anyhow!("{}", e.message))?;
        }
        Ok(documents)
    }

    pub fn beam_slots(&self) -> usize {
        self.slots.len()
    }

    /// How many documents are open here, or being opened.
    pub fn open_count(&self) -> usize {
        self.placed.lock().unwrap().len()
    }

    /// How many jobs on documents may run at once: one per document a slot
    /// keeps open.
    pub fn capacity(&self) -> usize {
        self.slots.len() * self.per_slot
    }

    /// The slot's live process, started if it has none.
    async fn beam(&self, slot: usize) -> Result<Running, Error> {
        let s = &self.slots[slot];
        let mut running = s.beam.lock().await;
        if let Some(r) = running.as_ref().filter(|r| r.beam.is_alive()) {
            return Ok(r.clone());
        }
        let launcher = self.pool.launcher();
        let oom_baseline = launcher.oom_kills(s.id);
        let beam = spawn(launcher, s.id, &self.beam_bin)
            .await
            .map_err(|e| Error::new(ErrorKind::Crash, format!("starting Lean Beam failed: {e}")))?;
        tracing::info!(slot = s.id, "Lean Beam started");
        let r = Running {
            beam: Arc::new(beam),
            oom_baseline,
        };
        *running = Some(r.clone());
        Ok(r)
    }

    /// Whether `open` is in its slot's live process.
    async fn is_current(&self, open: &Open) -> bool {
        let running = self.slots[open.slot].beam.lock().await;
        running
            .as_ref()
            .is_some_and(|r| Arc::ptr_eq(&r.beam, &open.beam) && r.beam.is_alive())
    }

    /// Slot `slot`'s process answered.
    fn answered(&self, slot: usize) {
        self.slots[slot].timeouts.store(0, Ordering::Release);
    }

    /// Slot `slot`'s process `beam` didn't answer in time. After
    /// [`RESTART_AFTER_TIMEOUTS`] in a row it is stopped, and the slot starts
    /// a new one when next asked, which its documents reopen in.
    async fn timed_out(&self, slot: usize, beam: &Arc<Beam>) {
        let s = &self.slots[slot];
        if s.timeouts.fetch_add(1, Ordering::AcqRel) + 1 < RESTART_AFTER_TIMEOUTS {
            return;
        }
        s.timeouts.store(0, Ordering::Release);
        if let Some(r) = s.beam.lock().await.take_if(|r| Arc::ptr_eq(&r.beam, beam)) {
            tracing::warn!(
                slot = s.id,
                "restarting Lean Beam after {RESTART_AFTER_TIMEOUTS} timeouts in a row"
            );
            *s.stopped.lock().unwrap() = Some(r.beam.clone());
            r.beam.kill().await;
        }
    }

    /// The error for `e` from slot `slot`'s process `beam`, which ran
    /// `code_text`. A process that failed is stopped, so that its documents
    /// reopen in a new one.
    async fn failure(
        &self,
        slot: usize,
        beam: &Arc<Beam>,
        e: BeamError,
        budget: Budget,
        code_text: &str,
    ) -> Error {
        match e {
            BeamError::Timeout => {
                self.timed_out(slot, beam).await;
                Error::timeout(budget.secs)
            }
            BeamError::Busy(m) => {
                Error::new(ErrorKind::Unavailable, format!("Lean Beam is busy: {m}"))
            }
            // Lean's own answer comes back as a result; an error is Beam's.
            BeamError::Tool { code, message, .. } => lean_limit(&code, &message, code_text)
                .unwrap_or_else(|| {
                    Error::new(
                        ErrorKind::Unavailable,
                        format!("Lean Beam failed: {message}"),
                    )
                }),
            BeamError::Exited { .. } | BeamError::Dead | BeamError::Io(_)
                if (self.slots[slot].stopped.lock().unwrap().as_ref())
                    .is_some_and(|stopped| Arc::ptr_eq(stopped, beam)) =>
            {
                Error::new(
                    ErrorKind::Unavailable,
                    format!(
                        "Lean Beam was restarted after {RESTART_AFTER_TIMEOUTS} timeouts in a row \
                         on its slot; the document opens again on the next request"
                    ),
                )
            }
            BeamError::Exited { .. } | BeamError::Dead | BeamError::Io(_) => {
                let s = &self.slots[slot];
                let mut oom = false;
                if let Some(r) = s.beam.lock().await.take_if(|r| Arc::ptr_eq(&r.beam, beam)) {
                    oom = self.pool.launcher().oom_kills(s.id) > r.oom_baseline;
                    r.beam.kill().await;
                }
                tracing::warn!(slot = s.id, oom, "Lean Beam failed: {e}");
                if oom {
                    Error::new(
                        ErrorKind::OutOfMemory,
                        "Lean Beam exceeded its memory limit",
                    )
                } else {
                    Error::new(ErrorKind::Crash, format!("Lean Beam failed: {e}"))
                }
            }
            e => Error::new(ErrorKind::Internal, format!("Lean Beam: {e}")),
        }
    }

    /// The error for `e` on the open document `doc`, which is closed here
    /// unless Beam still has it as it was. `code_text` is what ran besides
    /// the document.
    async fn fail(
        &self,
        doc: &mut Option<Open>,
        id: &str,
        e: BeamError,
        budget: Budget,
        code_text: &str,
    ) -> Error {
        let Some(open) = doc.as_ref() else {
            return Error::new(ErrorKind::Internal, format!("Lean Beam: {e}"));
        };
        let (slot, beam) = (open.slot, open.beam.clone());
        let kept = matches!(
            e,
            BeamError::Timeout | BeamError::Busy(_) | BeamError::Tool { .. }
        );
        let ran = format!("{}\n{code_text}", open.map.text());
        let error = self.failure(slot, &beam, e, budget, &ran).await;
        if !kept {
            self.unload(doc, id).await;
        }
        error
    }

    async fn lock(&self, id: &str) -> OwnedMutexGuard<Option<Open>> {
        let entry = self
            .docs
            .lock()
            .unwrap()
            .entry(id.to_string())
            .or_default()
            .clone();
        entry.lock_owned().await
    }

    /// Document `id`'s record, which must be at `revision` if that is given.
    async fn row(&self, id: &str, revision: Option<u64>) -> Result<DocumentRow, Error> {
        let row = self
            .records
            .document(id)
            .await?
            .ok_or_else(|| not_found(&self.version, id))?;
        match revision {
            Some(r) if r != row.revision => Err(conflict(row.revision, r)),
            _ => Ok(row),
        }
    }

    /// Makes `doc` open here at its stored revision (which must be
    /// `revision` if that is given), and records this worker as its holder.
    /// Opening it again takes the time limit its open took, not the
    /// request's, so that a short request doesn't time out on every try;
    /// returns why it was opened again, if it was. An open that fails says
    /// it was opening the document again.
    async fn ready(
        &self,
        doc: &mut Option<Open>,
        id: &str,
        revision: Option<u64>,
    ) -> Result<Option<&'static str>, Error> {
        let row = self.row(id, revision).await?;
        let why = match doc.as_ref() {
            None => Some("it isn't open here"),
            Some(open) if open.revision != row.revision => Some("it was edited elsewhere"),
            Some(open) if !self.is_current(open).await => Some("its Lean Beam restarted"),
            Some(_) => None,
        };
        if let Some(why) = why {
            tracing::info!(document = id, why, "opening a document again");
            let budget = Budget::new(row.timeout);
            let loaded = self
                .load(doc, id, &row.code, row.revision, budget, Instant::now())
                .await;
            loaded.map_err(|e| reopen_failed(e, why))?;
        }
        if !self.records.hold_document(id, &self.me, now_secs()).await? {
            self.unload(doc, id).await;
            return Err(not_found(&self.version, id));
        }
        if let Some(open) = doc.as_mut() {
            open.used = Instant::now();
        }
        Ok(why)
    }

    /// Opens `code` as `doc` at `revision` within `budget`, which replays
    /// take too: in the slot it is open in, or in the least full slot. The
    /// startup counts from `since`. Says where the time went, also in the
    /// error of a load that fails.
    async fn load(
        &self,
        doc: &mut Option<Open>,
        id: &str,
        code: &str,
        revision: u64,
        budget: Budget,
        since: Instant,
    ) -> Result<Timing, Error> {
        let mut phases = Phases::default();
        match self
            .load_phases(doc, id, code, revision, budget, since, &mut phases)
            .await
        {
            Ok(()) => Ok(phases.timing(since)),
            Err(mut e) => {
                e.timing = Some(Box::new(phases.timing(since)));
                Err(e)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn load_phases(
        &self,
        doc: &mut Option<Open>,
        id: &str,
        code: &str,
        revision: u64,
        budget: Budget,
        since: Instant,
        phases: &mut Phases,
    ) -> Result<(), Error> {
        let split = split_header(code);
        self.pool.snapshots().ensure(&split.header, None).await?;
        if let Some(e) = self.pool.snapshots().clash(&split.header).await? {
            return Err(e);
        }
        let map = CodeMap::new(code, &split);
        let slot = match doc.as_ref() {
            Some(open) if self.is_current(open).await => open.slot,
            _ => {
                self.unload(doc, id).await;
                self.place(id).await
            }
        };
        let beam = self.beam(slot).await?.beam;
        let path = file(id);
        let ws = &self.slots[slot].workspace;
        ws.write(&path, map.text()).map_err(|e| {
            Error::new(
                ErrorKind::Internal,
                format!("writing the document's file: {e}"),
            )
        })?;
        phases.load = Some(since.elapsed());
        match sync(&beam, ws.doc(&path), &map, budget, phases).await {
            Ok((version, messages, sorries)) => {
                self.answered(slot);
                *doc = Some(Open {
                    slot,
                    beam,
                    revision,
                    timeout: budget.secs,
                    version,
                    map,
                    messages,
                    sorries,
                    used: Instant::now(),
                    handles: HashMap::new(),
                    kept: VecDeque::new(),
                });
                self.placed.lock().unwrap().insert(id.to_string(), slot);
                Ok(())
            }
            Err(e) => {
                let error = self.failure(slot, &beam, e, budget, code).await;
                *doc = None;
                self.placed.lock().unwrap().remove(id);
                let _ = beam.close(ws.doc(&path), CLOSE_TIMEOUT).await;
                let _ = std::fs::remove_file(ws.root().join(&path));
                Err(error)
            }
        }
    }

    /// Closes `doc` here, if it is open.
    async fn unload(&self, doc: &mut Option<Open>, id: &str) {
        if let Some(open) = doc.take() {
            let ws = &self.slots[open.slot].workspace;
            let path = file(id);
            if let Err(e) = open.beam.close(ws.doc(&path), CLOSE_TIMEOUT).await {
                tracing::debug!(document = id, "closing the document failed: {e}");
            }
            let _ = std::fs::remove_file(ws.root().join(&path));
        }
        self.placed.lock().unwrap().remove(id);
    }

    /// The slot with the fewest documents open, which document `id` takes a
    /// place in before it opens, so that opens at once count each other,
    /// after closing the slot's least recently used idle one if it is full.
    /// An open that fails gives the place back ([`load`](Self::load)).
    async fn place(&self, id: &str) -> usize {
        let (slot, in_slot) = {
            let mut placed = self.placed.lock().unwrap();
            let mut counts = vec![0; self.slots.len()];
            for (doc, slot) in placed.iter() {
                if doc != id {
                    counts[*slot] += 1;
                }
            }
            let slot = (0..counts.len()).min_by_key(|s| counts[*s]).unwrap_or(0);
            placed.insert(id.to_string(), slot);
            let in_slot: Vec<String> = (placed.iter())
                .filter(|(doc, s)| **s == slot && *doc != id)
                .map(|(doc, _)| doc.clone())
                .collect();
            (slot, in_slot)
        };
        if in_slot.len() < self.per_slot {
            return slot;
        }
        let mut oldest: Option<(Instant, String, OwnedMutexGuard<Option<Open>>)> = None;
        for other in in_slot {
            let Some(entry) = self.docs.lock().unwrap().get(&other).cloned() else {
                continue;
            };
            let Ok(guard) = entry.try_lock_owned() else {
                continue;
            };
            let Some(used) = guard.as_ref().map(|open| open.used) else {
                continue;
            };
            if oldest.as_ref().is_none_or(|(u, ..)| used < *u) {
                oldest = Some((used, other, guard));
            }
        }
        if let Some((_, old, mut guard)) = oldest {
            tracing::info!(
                document = old,
                slot,
                "closing the least recently used document to open another"
            );
            self.unload(&mut guard, &old).await;
            // No job waits on it: each document has at most one job here.
            self.docs.lock().unwrap().remove(&old);
            // Its next job opens it on whichever worker has room.
            if let Err(e) = self.records.release_document(&old, &self.me).await {
                tracing::warn!(document = old, "releasing a closed document failed: {e}");
            }
        }
        slot
    }

    fn describe(&self, id: &str, open: &Open) -> api::Document {
        api::Document {
            id: format!("{}~{id}", self.version),
            revision: open.revision,
            messages: open.messages.clone(),
            sorries: open.sorries.iter().map(|(s, _)| s.clone()).collect(),
            runs: Vec::new(),
            timing: Timing::default(),
        }
    }

    /// Refuses what the policy refuses in `text`, which starts `header_lines`
    /// lines into its code.
    fn scan(&self, text: &str, header_lines: u64) -> Result<(), Error> {
        let found = self.pool.policy().findings(text, false);
        if found.is_empty() {
            Ok(())
        } else {
            Err(policy_error(&found, header_lines))
        }
    }

    /// [`scan`](Self::scan) on `code`'s body, at its lines in `code`.
    fn scan_code(&self, code: &str) -> Result<(), Error> {
        let split = split_header(code);
        self.scan(&split.body, split.header_lines)
    }

    async fn open(&self, j: DocumentOpenJob) -> Result<api::Document, Error> {
        self.scan_code(&j.code)?;
        let started = Instant::now();
        let budget = Budget::new(j.timeout_secs);
        let mut doc = self.lock(&j.document).await;
        let timing = self
            .load(&mut doc, &j.document, &j.code, 1, budget, started)
            .await?;
        let stored = self
            .records
            .put_document(
                &j.document,
                &j.code,
                j.timeout_secs,
                &self.me,
                now_secs(),
                &j.runs,
            )
            .await;
        if let Err(e) = stored {
            self.unload(&mut doc, &j.document).await;
            return Err(e.into());
        }
        Ok(api::Document {
            timing,
            ..self.describe(&j.document, doc.as_ref().unwrap())
        })
    }

    /// Opens the new code, then stores it if the document is still at the
    /// revision the edit replaces.
    async fn edit(&self, j: DocumentEditJob) -> Result<api::Document, Error> {
        if self.records.session(&j.document).await?.is_some() {
            return Err(bad_request(
                "a session's code doesn't change, as its states are tied to it: start another \
                 session",
            ));
        }
        self.scan_code(&j.code)?;
        let started = Instant::now();
        let budget = Budget::new(j.timeout_secs);
        let id = &j.document;
        let mut doc = self.lock(id).await;
        self.row(id, Some(j.revision)).await?;
        let timing = self
            .load(&mut doc, id, &j.code, j.revision + 1, budget, started)
            .await?;
        let stored = self
            .records
            .edit_document(
                id,
                &j.code,
                j.timeout_secs,
                j.revision,
                &self.me,
                now_secs(),
                &j.runs,
            )
            .await;
        match stored {
            Ok(Some(_)) => Ok(api::Document {
                timing,
                ..self.describe(id, doc.as_ref().unwrap())
            }),
            Ok(None) => {
                self.unload(&mut doc, id).await;
                Err(match self.row(id, None).await {
                    Ok(row) => conflict(row.revision, j.revision),
                    Err(e) => e,
                })
            }
            Err(e) => {
                self.unload(&mut doc, id).await;
                Err(e.into())
            }
        }
    }

    async fn goals(&self, j: DocumentGoalsJob) -> Result<api::DocumentGoals, Error> {
        let id = &j.document;
        let mut doc = self.lock(id).await;
        let loading = Instant::now();
        let reopened = self.ready(&mut doc, id, j.revision).await?;
        let load = round6(loading.elapsed().as_secs_f64());
        let budget = Budget::new(j.timeout_secs);
        let open = doc.as_ref().unwrap();
        let mut at = Vec::new();
        for p in &j.at {
            let pos = open.map.resolve(*p);
            at.push((pos, open.map.to_lsp(pos)?));
        }
        let mode = match j.mode {
            GoalsMode::Before => Mode::Before,
            GoalsMode::After => Mode::After,
        };
        let file = file(id);
        let traced = Instant::now();
        let asked = async {
            let mut out = Vec::new();
            for (pos, lsp) in at {
                let (in_proof, goals) = match lsp {
                    Some(lsp) => {
                        let doc = self.slots[open.slot].workspace.doc(&file);
                        let asked = open
                            .beam
                            .goals(doc, open.version, lsp, mode, budget.left())
                            .await;
                        match asked {
                            // None, after a proof's last tactic.
                            Ok(goals) => {
                                self.answered(open.slot);
                                (true, goals.iter().map(pisa_beam::goal::render).collect())
                            }
                            // Beam's answer where Lean has no proof.
                            Err(BeamError::Tool { code, .. }) if code == "invalidParams" => {
                                (false, Vec::new())
                            }
                            Err(e) => return Err(e),
                        }
                    }
                    None => (false, Vec::new()),
                };
                let line = open.map.line(pos.line).to_string();
                out.push(api::GoalsAt {
                    pos,
                    line,
                    in_proof,
                    goals,
                });
            }
            Ok::<_, BeamError>(out)
        }
        .await;
        let revision = open.revision;
        let tracing = round6(traced.elapsed().as_secs_f64());
        match asked {
            Ok(at) => Ok(api::DocumentGoals {
                revision,
                at,
                reopened: reopened.is_some(),
                timing: Timing {
                    load,
                    tracing,
                    ..Timing::default()
                },
            }),
            Err(e) => Err(after_reopen(
                self.fail(&mut doc, id, e, budget, "").await,
                reopened,
            )),
        }
    }

    async fn probe(&self, j: DocumentProbeJob) -> Result<api::DocumentProbes, Error> {
        let id = &j.document;
        let mut doc = self.lock(id).await;
        let loading = Instant::now();
        let mut reopened = self.ready(&mut doc, id, j.revision).await?.is_some();
        let mut load = loading.elapsed();
        let mut execution = Duration::ZERO;
        let budget = Budget::new(j.budget_secs);
        let open = doc.as_ref().unwrap();
        let lsp = match j.at {
            ProbeAt::Sorry { sorry } => match open.sorries.get(sorry) {
                Some((_, at)) => *at,
                None => {
                    return Err(bad_request(format!(
                        "there is no sorry {sorry}: the document has {}",
                        open.sorries.len()
                    )));
                }
            },
            ProbeAt::Pos(p) => match open.map.to_lsp(open.map.resolve(p))? {
                Some(lsp) => lsp,
                None => {
                    return Err(bad_request(
                        "probes run in the code's body, not among its imports",
                    ));
                }
            },
        };
        let (revision, pos) = (open.revision, open.map.to_code(lsp));
        let file = file(id);
        // Each candidate has the whole limit, within what is left of the
        // request's budget, so that one that loops doesn't hide whether the
        // next works.
        let mut results = Vec::new();
        for (i, candidate) in j.candidates.iter().enumerate() {
            if let Err(e) = self.scan(candidate, 0) {
                results.push(refused(e));
                continue;
            }
            let ran_out = || {
                let ran_out = "the request's budget ran out before it ran";
                refused(Error::new(ErrorKind::Timeout, ran_out))
            };
            if budget.left().is_zero() {
                results.push(ran_out());
                continue;
            }
            // Beam restarted, after timeouts in a row or a failure: the rest
            // run in the new process, or answer why they can't.
            let current = match doc.as_ref() {
                Some(open) => self.is_current(open).await,
                None => false,
            };
            if !current {
                let loading = Instant::now();
                let ready = self.ready(&mut doc, id, Some(revision)).await;
                load += loading.elapsed();
                match ready {
                    Ok(_) => reopened = true,
                    Err(e) => {
                        let rest = j.candidates.len() - i;
                        results.extend(std::iter::repeat_n(refused(e), rest));
                        break;
                    }
                }
            }
            // The candidate's limit starts once the document is open.
            let Some(limit) = budget.within(j.timeout_secs) else {
                results.push(ran_out());
                continue;
            };
            let open = doc.as_ref().unwrap();
            let in_slot = self.slots[open.slot].workspace.doc(&file);
            let running = Instant::now();
            let run = open
                .beam
                .run_at(in_slot, open.version, lsp, candidate, limit.left())
                .await;
            execution += running.elapsed();
            results.push(match run {
                Ok(r) => {
                    self.answered(open.slot);
                    open.map.probe_result(candidate, r, lsp)
                }
                Err(BeamError::Timeout) => {
                    self.timed_out(open.slot, &open.beam).await;
                    refused(Error::timeout(limit.secs))
                }
                Err(BeamError::Tool { code, message, .. }) => refused(
                    lean_limit(
                        &code,
                        &message,
                        &format!("{}\n{candidate}", open.map.text()),
                    )
                    .unwrap_or_else(|| {
                        Error::new(
                            ErrorKind::Unavailable,
                            format!("Lean Beam failed to run it: {message}"),
                        )
                    }),
                ),
                // Busy, or the process died on it.
                Err(e) => refused(self.fail(&mut doc, id, e, limit, candidate).await),
            });
        }
        Ok(api::DocumentProbes {
            revision,
            pos,
            results,
            reopened,
            timing: Timing {
                load: round6(load.as_secs_f64()),
                execution: round6(execution.as_secs_f64()),
                ..Timing::default()
            },
        })
    }

    async fn close(&self, j: DocumentJob) -> Result<(), Error> {
        let id = &j.document;
        let mut doc = self.lock(id).await;
        self.unload(&mut doc, id).await;
        self.docs.lock().unwrap().remove(id);
        if self.records.delete_document(id).await? {
            Ok(())
        } else {
            Err(not_found(&self.version, id))
        }
    }
}

/// Runs a job on a document, on `documents` if this worker has Beam slots.
pub async fn run(documents: Option<&Documents>, job: Job) -> JobResult {
    let Some(d) = documents else {
        let none = || Error::new(ErrorKind::Unavailable, "this worker runs no Lean Beam");
        return match job {
            Job::DocumentOpen(_) | Job::DocumentEdit(_) => JobResult::Document(Err(none())),
            Job::DocumentGoals(_) => JobResult::DocumentGoals(Err(none())),
            Job::DocumentProbe(_) => JobResult::DocumentProbes(Err(none())),
            Job::SessionStart(_) => JobResult::SessionStarted(Err(none())),
            Job::SessionTactic(_) => JobResult::TacticApplied(Err(none())),
            _ => JobResult::DocumentClosed(Err(none())),
        };
    };
    match job {
        Job::DocumentOpen(j) => {
            let runs = j.runs.clone();
            JobResult::Document(d.open(j).await.map(|doc| api::Document { runs, ..doc }))
        }
        Job::DocumentEdit(j) => {
            let runs = j.runs.clone();
            JobResult::Document(d.edit(j).await.map(|doc| api::Document { runs, ..doc }))
        }
        Job::DocumentGoals(j) => JobResult::DocumentGoals(d.goals(j).await),
        Job::DocumentProbe(j) => JobResult::DocumentProbes(d.probe(j).await),
        Job::DocumentClose(j) => JobResult::DocumentClosed(d.close(j).await),
        Job::SessionStart(j) => JobResult::SessionStarted(d.start_session(j).await),
        Job::SessionTactic(j) => JobResult::TacticApplied(d.tactic(j).await),
        _ => JobResult::Failed {
            error: "not a job on a document".into(),
        },
    }
}

/// How long the steps of a load took, as far as it got.
#[derive(Default)]
struct Phases {
    /// Until the file was written: `None` while that is still running.
    load: Option<Duration>,
    execution: Duration,
    tracing: Duration,
}

impl Phases {
    /// The timing of a load that began at `since`, a failed one's included.
    fn timing(&self, since: Instant) -> Timing {
        Timing {
            load: round6(self.load.unwrap_or_else(|| since.elapsed()).as_secs_f64()),
            execution: round6(self.execution.as_secs_f64()),
            tracing: round6(self.tracing.as_secs_f64()),
            ..Timing::default()
        }
    }
}

/// Syncs the file Beam reads for `map`: its version, messages and sorries.
async fn sync(
    beam: &Beam,
    doc: Doc<'_>,
    map: &CodeMap,
    budget: Budget,
    phases: &mut Phases,
) -> Result<(u64, Vec<api::Message>, Vec<(api::Sorry, LspPos)>), BeamError> {
    let running = Instant::now();
    let synced = beam.sync(doc, true, budget.left()).await;
    phases.execution = running.elapsed();
    let synced = synced?;
    let version = synced.version;
    let tracing = Instant::now();
    let todo = beam.todo(doc, version, &["sorry"], budget.left()).await;
    phases.tracing = tracing.elapsed();
    let todo = todo?;
    let messages = synced
        .diagnostics
        .items
        .iter()
        .map(|d| map.message(&d.severity, &d.message, d.range, None))
        .collect();
    let mut sorries = Vec::new();
    for item in todo.items {
        let Some(range) = item.range else { continue };
        let at = item.run_at_position.unwrap_or(range.start);
        let goals = match item.proof_state {
            Some(state) => state.goals,
            None => {
                let goals = beam
                    .goals(doc, version, at, Mode::Before, budget.left())
                    .await;
                phases.tracing = tracing.elapsed();
                goals?
            }
        };
        let sorry = api::Sorry {
            goal: render(&goals),
            pos: Some(map.to_code(range.start)),
            end_pos: Some(map.to_code(range.end)),
        };
        sorries.push((sorry, at));
    }
    Ok((version, messages, sorries))
}

/// `lean-beam-mcp` set up for `slot` as the slot's REPL is, with a writable
/// home and session directory in the slot.
async fn spawn(launcher: &Launcher, slot: usize, bin: &Path) -> Result<Beam, BeamError> {
    let dir = launcher.slot_dir(slot);
    let home = dir.join("home");
    let session = dir.join("beam");
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(&session)?;
    // Beam refuses a session directory others may read.
    std::fs::set_permissions(&session, std::fs::Permissions::from_mode(0o700))?;
    let mut cmd = launcher.command(slot, bin, &[])?;
    cmd.env("HOME", &home).env("BEAM_SESSION_ROOT", &session);
    Beam::spawn(cmd, SPAWN_TIMEOUT).await
}

/// `lean-beam-mcp` in the toolchain of `lean`, run in `project`.
async fn beam_in_toolchain(lean: &Path, project: Option<&Path>) -> anyhow::Result<PathBuf> {
    let mut cmd = tokio::process::Command::new(lean);
    cmd.arg("--print-prefix");
    if let Some(project) = project {
        cmd.current_dir(project);
    }
    let out = cmd.output().await.context("running lean --print-prefix")?;
    anyhow::ensure!(
        out.status.success(),
        "lean --print-prefix failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let prefix = String::from_utf8_lossy(&out.stdout);
    Ok(Path::new(prefix.trim()).join("bin/lean-beam-mcp"))
}

/// The file of document `id`, relative to the workspace.
fn file(id: &str) -> String {
    format!("{LIB}/D{id}.lean")
}

fn render(goals: &[Goal]) -> String {
    let goals: Vec<String> = goals.iter().map(pisa_beam::goal::render).collect();
    goals.join("\n\n")
}

/// `e`, saying the document was opened again first, as `why`, if it was.
fn after_reopen(e: Error, why: Option<&str>) -> Error {
    match why {
        Some(why) => Error {
            message: format!(
                "{} (the document was opened again first, as {why})",
                e.message
            ),
            ..e
        },
        None => e,
    }
}

/// `e`, from opening a document again as `why`.
fn reopen_failed(e: Error, why: &str) -> Error {
    Error {
        message: format!(
            "opening the document again, as {why}, failed: {}",
            e.message
        ),
        ..e
    }
}

fn refused(error: Error) -> ProbeResult {
    ProbeResult {
        ok: false,
        closed: false,
        goals: Vec::new(),
        messages: Vec::new(),
        error: Some(error),
    }
}

/// Whether `text` has the word `sorry` or `admit`.
fn says_sorry(text: &str) -> bool {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|w| w == "sorry" || w == "admit")
}

fn not_found(version: &str, id: &str) -> Error {
    Error::new(
        ErrorKind::NotFound,
        format!("no document {version}~{id}: it was closed, or it expired"),
    )
}

fn conflict(at: u64, asked: u64) -> Error {
    Error::new(
        ErrorKind::Conflict,
        format!("the document is at revision {at}, not {asked}"),
    )
}

fn bad_request(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::BadRequest, message)
}

pub(crate) fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Where positions in submitted code are in its document's file, and back.
/// Code positions are as for `/check`: lines from 1, columns in code points.
/// The file's are LSP's: lines from 0, columns in UTF-16 units. The file is
/// the header as the REPL imports it, then the code's body.
#[derive(Debug, Clone)]
pub struct CodeMap {
    text: String,
    /// The code's lines before its body, as they are in the code.
    header: String,
    /// Lines of the code before its body.
    header_lines: u64,
    /// Lines of the file before the body.
    offset: u32,
    /// Lines of the code.
    lines: u64,
}

impl CodeMap {
    /// The map of `code`, split as `split`.
    pub fn new(code: &str, split: &Split) -> CodeMap {
        let header = header_command(&split.header);
        let offset = header.split('\n').count() as u32;
        let lines: Vec<&str> = code.split('\n').collect();
        CodeMap {
            text: format!("{header}\n{}", split.body),
            header: lines[..(split.header_lines as usize).min(lines.len())].join("\n"),
            header_lines: split.header_lines,
            offset,
            lines: split.header_lines + split.body.split('\n').count() as u64,
        }
    }

    /// The file's text.
    pub fn text(&self) -> &str {
        &self.text
    }

    fn body(&self) -> BodyMap<'_> {
        BodyMap::new(&self.text, self.offset)
    }

    /// Code line `line`, if it is in the body.
    fn body_line(&self, line: u64) -> Option<&str> {
        let n = line.checked_sub(self.header_lines).filter(|n| *n >= 1)?;
        self.text
            .split('\n')
            .nth(self.offset as usize + n as usize - 1)
    }

    /// Code line `line`'s text: empty past the code's end.
    pub fn line(&self, line: u64) -> &str {
        let Some(n) = line.checked_sub(1) else {
            return "";
        };
        match line <= self.header_lines {
            true => self.header.split('\n').nth(n as usize),
            false => self.body_line(line),
        }
        .unwrap_or_default()
    }

    /// `at`, its column the one given or the line's first tactic.
    pub fn resolve(&self, at: LinePos) -> Pos {
        let column = at
            .column
            .unwrap_or_else(|| self.body_line(at.line).map_or(0, first_tactic));
        Pos {
            line: at.line,
            column,
        }
    }

    /// Where `pos` is in the file: `None` in the header, and an error past
    /// the code's end.
    pub fn to_lsp(&self, pos: Pos) -> Result<Option<LspPos>, Error> {
        if pos.line == 0 || pos.line > self.lines {
            return Err(bad_request(format!(
                "line {} is not in the code, which has {} lines",
                pos.line, self.lines
            )));
        }
        if pos.line <= self.header_lines {
            return Ok(None);
        }
        let line = (pos.line - self.header_lines) as u32;
        Ok(Some(self.body().to_lsp(line, pos.column as u32)))
    }

    /// Where file position `pos` is in the code: the code's start for one in
    /// the header.
    pub fn to_code(&self, pos: LspPos) -> Pos {
        match self.body().to_body(pos) {
            Some((line, column)) => Pos {
                line: line as u64 + self.header_lines,
                column: column as u64,
            },
            None => Pos { line: 1, column: 0 },
        }
    }

    /// A message of Beam's, at the code's start, or at `at`, if it has no
    /// range.
    fn message(
        &self,
        severity: &str,
        text: &str,
        range: Option<Range>,
        at: Option<LspPos>,
    ) -> api::Message {
        let severity = match severity {
            "error" => api::Severity::Error,
            "warning" => api::Severity::Warning,
            "trace" => api::Severity::Trace,
            _ => api::Severity::Info,
        };
        api::Message {
            severity,
            pos: match (range, at) {
                (Some(r), _) => self.to_code(r.start),
                (None, Some(at)) => self.to_code(at),
                (None, None) => Pos { line: 1, column: 0 },
            },
            end_pos: range.map(|r| self.to_code(r.end)),
            text: text.to_string(),
        }
    }

    /// The result of `candidate`, run at `at`, where its messages without a
    /// range are.
    fn probe_result(&self, candidate: &str, r: RunResult, at: LspPos) -> ProbeResult {
        let goals: Vec<String> = match (r.success, r.proof_state) {
            (true, Some(state)) => state.goals.iter().map(pisa_beam::goal::render).collect(),
            _ => Vec::new(),
        };
        ProbeResult {
            ok: r.success,
            closed: r.success && goals.is_empty() && !says_sorry(candidate),
            goals,
            messages: r
                .messages
                .iter()
                .map(|m| self.message(&m.severity, &m.text, m.range, Some(at)))
                .collect(),
            error: None,
        }
    }
}

/// The column of a line's first tactic: its first character that isn't
/// blank, after a `·` or `.` bullet.
fn first_tactic(line: &str) -> u64 {
    let chars: Vec<char> = line.chars().collect();
    let blank = |from: usize| {
        chars[from..]
            .iter()
            .take_while(|c| c.is_whitespace())
            .count()
    };
    let start = blank(0);
    match chars.get(start) {
        Some('·' | '.') if chars.get(start + 1).is_none_or(|c| c.is_whitespace()) => {
            (start + 1 + blank(start + 1)) as u64
        }
        _ => start as u64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recursion_limit() -> Error {
        Error {
            limit: Some(pisa_protocol::api::LeanLimit {
                option: "maxRecDepth".into(),
                value: 512,
                phase: None,
            }),
            timing: Some(Box::new(Timing {
                load: 1.5,
                ..Timing::default()
            })),
            ..Error::new(ErrorKind::RecursionLimit, "maximum recursion depth")
        }
    }

    #[test]
    fn an_error_after_a_reopen_keeps_its_limit_and_timing() {
        let e = after_reopen(recursion_limit(), Some("it was edited elsewhere"));
        assert_eq!(e.kind, ErrorKind::RecursionLimit);
        assert!(e.message.contains("opened again first"), "{}", e.message);
        assert_eq!(e.limit, recursion_limit().limit);
        assert_eq!(e.timing, recursion_limit().timing);

        let e = reopen_failed(recursion_limit(), "its Lean Beam restarted");
        assert_eq!(e.kind, ErrorKind::RecursionLimit);
        assert!(e.message.contains("failed: maximum recursion depth"));
        assert_eq!(e.limit, recursion_limit().limit);
        assert_eq!(e.timing, recursion_limit().timing);
    }

    fn map(code: &str) -> CodeMap {
        CodeMap::new(code, &split_header(code))
    }

    fn pos(line: u64, column: u64) -> Pos {
        Pos { line, column }
    }

    fn lsp(line: u32, character: u32) -> LspPos {
        LspPos { line, character }
    }

    #[test]
    fn code_positions_land_in_the_file_below_the_header() {
        // Three header lines, one a comment, and a body from line 4.
        let m = map("import Mathlib\n-- c\nimport Foo\n\ntheorem t : 𝓝 = 𝓝 := by\n  sorry");
        assert_eq!(
            m.text(),
            "import Mathlib\nimport Foo\nimport Pisa\nset_option autoImplicit false\n\n\
             theorem t : 𝓝 = 𝓝 := by\n  sorry"
        );
        // The second `𝓝` is code point 16 and UTF-16 unit 17.
        assert_eq!(m.to_lsp(pos(5, 16)).unwrap(), Some(lsp(5, 17)));
        assert_eq!(m.to_code(lsp(5, 17)), pos(5, 16));
        assert_eq!(m.to_lsp(pos(6, 2)).unwrap(), Some(lsp(6, 2)));
        // The header, and what is past the end.
        assert_eq!(m.to_lsp(pos(2, 0)).unwrap(), None);
        assert_eq!(m.to_code(lsp(2, 0)), pos(1, 0));
        let past = m.to_lsp(pos(7, 0)).unwrap_err();
        assert_eq!(past.kind, ErrorKind::BadRequest);
        assert!(m.to_lsp(pos(0, 0)).is_err());
        // Each line's text, the header's as the code has it.
        let lines: Vec<&str> = (0..=7).map(|n| m.line(n)).collect();
        assert_eq!(
            lines,
            [
                "",
                "import Mathlib",
                "-- c",
                "import Foo",
                "",
                "theorem t : 𝓝 = 𝓝 := by",
                "  sorry",
                ""
            ]
        );
    }

    #[test]
    fn code_without_imports_or_in_a_module_keeps_its_lines() {
        let m = map("theorem t : True := by\n  trivial\n");
        assert_eq!(m.to_lsp(pos(2, 2)).unwrap(), Some(lsp(3, 2)));
        assert_eq!(m.to_code(lsp(3, 2)), pos(2, 2));
        // The trailing newline leaves an empty last line.
        assert_eq!(m.to_lsp(pos(3, 0)).unwrap(), Some(lsp(4, 0)));

        let m = map("module\n\npublic import Foo\n\ntheorem t : True := by\n  trivial");
        assert!(
            m.text()
                .starts_with("module\npublic import Foo\nmeta import Pisa\n")
        );
        assert_eq!(m.to_lsp(pos(6, 2)).unwrap(), Some(lsp(6, 2)));
        assert_eq!(m.to_lsp(pos(3, 0)).unwrap(), None);
    }

    #[test]
    fn a_line_alone_means_its_first_tactic() {
        let m =
            map("example : True ∧ True := by\n  constructor\n  · trivial\n  . trivial\n\n  .inl");
        let line = |line| m.resolve(LinePos { line, column: None });
        assert_eq!(line(2), pos(2, 2));
        assert_eq!(line(3), pos(3, 4));
        assert_eq!(line(4), pos(4, 4));
        assert_eq!(line(5), pos(5, 0));
        assert_eq!(line(6), pos(6, 2));
        let given = m.resolve(LinePos {
            line: 3,
            column: Some(2),
        });
        assert_eq!(given, pos(3, 2));
        // A header line is its start.
        let m = map("import Foo\n  theorem t : True := trivial");
        assert_eq!(
            m.resolve(LinePos {
                line: 1,
                column: None
            }),
            pos(1, 0)
        );
    }

    #[test]
    fn sorry_or_admit_closes_nothing() {
        assert!(says_sorry("sorry"));
        assert!(says_sorry("constructor <;> admit"));
        assert!(!says_sorry("exact sorryLemma"));
        assert!(!says_sorry("simp [h_sorry]"));
    }
}
