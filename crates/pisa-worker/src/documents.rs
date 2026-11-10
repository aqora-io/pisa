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

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use pisa_beam::pos::BodyMap;
use pisa_beam::{Beam, BeamError, Doc, Goal, LspPos, Mode, Packages, Range, RunResult, Workspace};
use pisa_protocol::api::{self, Error, ErrorKind, GoalsMode, LinePos, Pos, ProbeAt, ProbeResult};
use pisa_protocol::header::{Split, split_header};
use pisa_protocol::wire::{
    DocumentEditJob, DocumentGoalsJob, DocumentJob, DocumentOpenJob, DocumentProbeJob, Job,
    JobResult,
};
use tokio::sync::OwnedMutexGuard;

use crate::check::policy_error;
use crate::config::WorkerConfig;
use crate::db::Db;
use crate::launcher::Launcher;
use crate::policy::header_command;
use crate::pool::Pool;
use crate::records::{DocumentRow, Records};

/// The library the documents' files belong to.
const LIB: &str = "PisaDocs";

/// How long `lean-beam-mcp` gets to start.
const SPAWN_TIMEOUT: Duration = Duration::from_secs(120);

/// How long closing a document gets.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(10);

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
    /// Beam's version of the file.
    version: u64,
    map: CodeMap,
    messages: Vec<api::Message>,
    /// Each `sorry`, with where probes at it run.
    sorries: Vec<(api::Sorry, LspPos)>,
    used: Instant,
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

    /// The error for `e` from slot `slot`'s process `beam`. A process that
    /// failed is stopped, so that its documents reopen in a new one.
    async fn failure(&self, slot: usize, beam: &Arc<Beam>, e: BeamError, budget: Budget) -> Error {
        match e {
            BeamError::Timeout => Error::timeout(budget.secs),
            BeamError::Busy(m) => {
                Error::new(ErrorKind::Unavailable, format!("Lean Beam is busy: {m}"))
            }
            BeamError::Tool { message, .. } => Error::new(ErrorKind::Lean, message),
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
    /// unless Beam still has it as it was.
    async fn fail(&self, doc: &mut Option<Open>, id: &str, e: BeamError, budget: Budget) -> Error {
        let Some(open) = doc.as_ref() else {
            return Error::new(ErrorKind::Internal, format!("Lean Beam: {e}"));
        };
        let (slot, beam) = (open.slot, open.beam.clone());
        let kept = matches!(
            e,
            BeamError::Timeout | BeamError::Busy(_) | BeamError::Tool { .. }
        );
        let error = self.failure(slot, &beam, e, budget).await;
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
    async fn ready(
        &self,
        doc: &mut Option<Open>,
        id: &str,
        revision: Option<u64>,
        budget: Budget,
    ) -> Result<(), Error> {
        let row = self.row(id, revision).await?;
        let current = match doc.as_ref() {
            Some(open) => open.revision == row.revision && self.is_current(open).await,
            None => false,
        };
        if !current {
            self.load(doc, id, &row.code, row.revision, budget).await?;
        }
        if !self.records.hold_document(id, &self.me, now_secs()).await? {
            self.unload(doc, id).await;
            return Err(not_found(&self.version, id));
        }
        if let Some(open) = doc.as_mut() {
            open.used = Instant::now();
        }
        Ok(())
    }

    /// Opens `code` as `doc` at `revision`: in the slot it is open in, or
    /// in the least full slot.
    async fn load(
        &self,
        doc: &mut Option<Open>,
        id: &str,
        code: &str,
        revision: u64,
        budget: Budget,
    ) -> Result<(), Error> {
        let split = split_header(code);
        self.pool.snapshots().ensure(&split.header, None).await?;
        let map = CodeMap::new(&split);
        let slot = match doc.as_ref() {
            Some(open) if self.is_current(open).await => open.slot,
            _ => {
                self.unload(doc, id).await;
                self.place().await
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
        match sync(&beam, ws.doc(&path), &map, budget).await {
            Ok((version, messages, sorries)) => {
                *doc = Some(Open {
                    slot,
                    beam,
                    revision,
                    version,
                    map,
                    messages,
                    sorries,
                    used: Instant::now(),
                });
                self.placed.lock().unwrap().insert(id.to_string(), slot);
                Ok(())
            }
            Err(e) => {
                let error = self.failure(slot, &beam, e, budget).await;
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

    /// The slot with the fewest documents open, after closing its least
    /// recently used idle one if it is full.
    async fn place(&self) -> usize {
        let mut counts = vec![0; self.slots.len()];
        let in_slot = |slot: usize| -> Vec<String> {
            let placed = self.placed.lock().unwrap();
            placed
                .iter()
                .filter(|(_, s)| **s == slot)
                .map(|(id, _)| id.clone())
                .collect()
        };
        for slot in self.placed.lock().unwrap().values() {
            counts[*slot] += 1;
        }
        let slot = (0..counts.len()).min_by_key(|s| counts[*s]).unwrap_or(0);
        if counts[slot] < self.per_slot {
            return slot;
        }
        let mut oldest: Option<(Instant, String, OwnedMutexGuard<Option<Open>>)> = None;
        for id in in_slot(slot) {
            let Some(entry) = self.docs.lock().unwrap().get(&id).cloned() else {
                continue;
            };
            let Ok(guard) = entry.try_lock_owned() else {
                continue;
            };
            let Some(used) = guard.as_ref().map(|open| open.used) else {
                continue;
            };
            if oldest.as_ref().is_none_or(|(u, ..)| used < *u) {
                oldest = Some((used, id, guard));
            }
        }
        if let Some((_, id, mut guard)) = oldest {
            tracing::debug!(
                document = id,
                slot,
                "closing the least recently used document"
            );
            self.unload(&mut guard, &id).await;
            // No job waits on it: each document has at most one job here.
            self.docs.lock().unwrap().remove(&id);
        }
        slot
    }

    fn describe(&self, id: &str, open: &Open) -> api::Document {
        api::Document {
            id: format!("{}~{id}", self.version),
            revision: open.revision,
            messages: open.messages.clone(),
            sorries: open.sorries.iter().map(|(s, _)| s.clone()).collect(),
        }
    }

    fn scan(&self, text: &str) -> Result<(), Error> {
        let rejected = self.pool.policy().scan(text, false);
        if rejected.is_empty() {
            Ok(())
        } else {
            Err(policy_error(&rejected))
        }
    }

    async fn open(&self, j: DocumentOpenJob) -> Result<api::Document, Error> {
        self.scan(&split_header(&j.code).body)?;
        let budget = Budget::new(j.timeout_secs);
        let mut doc = self.lock(&j.document).await;
        self.load(&mut doc, &j.document, &j.code, 1, budget).await?;
        let stored = self
            .records
            .put_document(&j.document, &j.code, &self.me, now_secs())
            .await;
        if let Err(e) = stored {
            self.unload(&mut doc, &j.document).await;
            return Err(e.into());
        }
        Ok(self.describe(&j.document, doc.as_ref().unwrap()))
    }

    /// Opens the new code, then stores it if the document is still at the
    /// revision the edit replaces.
    async fn edit(&self, j: DocumentEditJob) -> Result<api::Document, Error> {
        self.scan(&split_header(&j.code).body)?;
        let budget = Budget::new(j.timeout_secs);
        let id = &j.document;
        let mut doc = self.lock(id).await;
        self.row(id, Some(j.revision)).await?;
        self.load(&mut doc, id, &j.code, j.revision + 1, budget)
            .await?;
        let stored = self
            .records
            .edit_document(id, &j.code, j.revision, &self.me, now_secs())
            .await;
        match stored {
            Ok(Some(_)) => Ok(self.describe(id, doc.as_ref().unwrap())),
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
        let budget = Budget::new(j.timeout_secs);
        let id = &j.document;
        let mut doc = self.lock(id).await;
        self.ready(&mut doc, id, j.revision, budget).await?;
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
        let asked = async {
            let mut out = Vec::new();
            for (pos, lsp) in at {
                let goals = match lsp {
                    Some(lsp) => {
                        let doc = self.slots[open.slot].workspace.doc(&file);
                        let goals = open
                            .beam
                            .goals(doc, open.version, lsp, mode, budget.left())
                            .await?;
                        goals.iter().map(pisa_beam::goal::render).collect()
                    }
                    None => Vec::new(),
                };
                out.push(api::GoalsAt { pos, goals });
            }
            Ok::<_, BeamError>(out)
        }
        .await;
        let revision = open.revision;
        match asked {
            Ok(at) => Ok(api::DocumentGoals { revision, at }),
            Err(e) => Err(self.fail(&mut doc, id, e, budget).await),
        }
    }

    async fn probe(&self, j: DocumentProbeJob) -> Result<api::DocumentProbes, Error> {
        let budget = Budget::new(j.timeout_secs);
        let id = &j.document;
        let mut doc = self.lock(id).await;
        self.ready(&mut doc, id, j.revision, budget).await?;
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
        let file = file(id);
        let ran = async {
            let mut results = Vec::new();
            for candidate in &j.candidates {
                if let Err(e) = self.scan(candidate) {
                    results.push(refused(e));
                    continue;
                }
                if budget.left().is_zero() {
                    results.push(refused(Error::timeout(budget.secs)));
                    continue;
                }
                let doc = self.slots[open.slot].workspace.doc(&file);
                let run = open
                    .beam
                    .run_at(doc, open.version, lsp, candidate, budget.left())
                    .await;
                results.push(match run {
                    Ok(r) => open.map.probe_result(candidate, r),
                    Err(BeamError::Timeout) => refused(Error::timeout(budget.secs)),
                    Err(BeamError::Tool { message, .. }) => {
                        refused(Error::new(ErrorKind::Lean, message))
                    }
                    Err(e) => return Err(e),
                });
            }
            Ok(results)
        }
        .await;
        let (revision, pos) = (open.revision, open.map.to_code(lsp));
        match ran {
            Ok(results) => Ok(api::DocumentProbes {
                revision,
                pos,
                results,
            }),
            Err(e) => Err(self.fail(&mut doc, id, e, budget).await),
        }
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
            _ => JobResult::DocumentClosed(Err(none())),
        };
    };
    match job {
        Job::DocumentOpen(j) => JobResult::Document(d.open(j).await),
        Job::DocumentEdit(j) => JobResult::Document(d.edit(j).await),
        Job::DocumentGoals(j) => JobResult::DocumentGoals(d.goals(j).await),
        Job::DocumentProbe(j) => JobResult::DocumentProbes(d.probe(j).await),
        Job::DocumentClose(j) => JobResult::DocumentClosed(d.close(j).await),
        _ => JobResult::Failed {
            error: "not a job on a document".into(),
        },
    }
}

/// Syncs the file Beam reads for `map`: its version, messages and sorries.
async fn sync(
    beam: &Beam,
    doc: Doc<'_>,
    map: &CodeMap,
    budget: Budget,
) -> Result<(u64, Vec<api::Message>, Vec<(api::Sorry, LspPos)>), BeamError> {
    let synced = beam.sync(doc, true, budget.left()).await?;
    let version = synced.version;
    let todo = beam.todo(doc, version, &["sorry"], budget.left()).await?;
    let messages = synced
        .diagnostics
        .items
        .iter()
        .map(|d| map.message(&d.severity, &d.message, d.range))
        .collect();
    let mut sorries = Vec::new();
    for item in todo.items {
        let Some(range) = item.range else { continue };
        let at = item.run_at_position.unwrap_or(range.start);
        let goals = match item.proof_state {
            Some(state) => state.goals,
            None => {
                beam.goals(doc, version, at, Mode::Before, budget.left())
                    .await?
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

fn now_secs() -> u64 {
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
    /// Lines of the code before its body.
    header_lines: u64,
    /// Lines of the file before the body.
    offset: u32,
    /// Lines of the code.
    lines: u64,
}

impl CodeMap {
    pub fn new(split: &Split) -> CodeMap {
        let header = header_command(&split.header);
        let offset = header.split('\n').count() as u32;
        CodeMap {
            text: format!("{header}\n{}", split.body),
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

    fn message(&self, severity: &str, text: &str, range: Option<Range>) -> api::Message {
        let severity = match severity {
            "error" => api::Severity::Error,
            "warning" => api::Severity::Warning,
            "trace" => api::Severity::Trace,
            _ => api::Severity::Info,
        };
        api::Message {
            severity,
            pos: range.map_or(Pos { line: 1, column: 0 }, |r| self.to_code(r.start)),
            end_pos: range.map(|r| self.to_code(r.end)),
            text: text.to_string(),
        }
    }

    fn probe_result(&self, candidate: &str, r: RunResult) -> ProbeResult {
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
                .map(|m| self.message(&m.severity, &m.text, m.range))
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

    fn map(code: &str) -> CodeMap {
        CodeMap::new(&split_header(code))
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
