//! A fixed set of slots, each holding at most one REPL process bound to one
//! import header. Idle slots are reused by header (a warm hit) and otherwise
//! evicted least-recently-used first.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pisa_protocol::api::{Error, ErrorKind};
use pisa_protocol::repl::{CommandResponse, Message, Pos, ReplCommand, ReplResponse, Severity};
use pisa_repl::{Repl, ReplFailure};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};

use crate::launcher::{Compiled, Launcher, Verified};
use crate::policy::{Policy, header_command};
use crate::snapshots::Snapshots;

/// Succeeds only in an environment where the header's imports loaded.
const IMPORT_PROBE: &str = "#check @_root_.Pisa.reportNew";

pub struct PoolConfig {
    pub slots: usize,
    pub max_uses: u64,
    pub max_rss_bytes: u64,
    pub import_timeout: Duration,
    pub policy: Policy,
}

pub struct Pool {
    idle: Mutex<Vec<Slot>>,
    permits: Arc<Semaphore>,
    /// Header key per slot that has one imported.
    warm: watch::Sender<BTreeMap<usize, String>>,
    /// Header keys a pre-warm is importing, so that none is imported twice.
    prewarming: Arc<Mutex<HashSet<String>>>,
    launcher: Launcher,
    snapshots: Arc<Snapshots>,
    cfg: PoolConfig,
}

pub struct Slot {
    pub id: usize,
    repl: Option<Repl>,
    header: Option<Warm>,
    jobs: u64,
    last_used: Instant,
    /// OOM kills in the slot's cgroup when the current process started.
    oom_baseline: u64,
    /// Proof states loaded in the current process, by state hash.
    states: HashMap<String, Resident>,
}

/// A proof state held in a live process.
#[derive(Debug, Clone)]
pub struct Resident {
    pub proof_state: u64,
    pub header: String,
    pub session: String,
}

struct Warm {
    key: String,
    env: u64,
}

#[derive(Debug)]
pub enum HeaderFailure {
    /// The header elaborated with errors (e.g. an unknown module).
    Lean(Box<CommandResponse>),
    Timeout,
    /// The process failed to start or died.
    Failed(Error),
}

impl Pool {
    pub fn new(
        launcher: Launcher,
        snapshots: Arc<Snapshots>,
        cfg: PoolConfig,
    ) -> (Arc<Self>, watch::Receiver<BTreeMap<usize, String>>) {
        let now = Instant::now();
        let slots = (0..cfg.slots)
            .map(|id| Slot {
                id,
                repl: None,
                header: None,
                jobs: 0,
                last_used: now,
                oom_baseline: 0,
                states: HashMap::new(),
            })
            .collect();
        let (warm, warm_rx) = watch::channel(BTreeMap::new());
        let pool = Pool {
            idle: Mutex::new(slots),
            permits: Arc::new(Semaphore::new(cfg.slots)),
            warm,
            prewarming: Arc::default(),
            launcher,
            snapshots,
            cfg,
        };
        (Arc::new(pool), warm_rx)
    }

    pub fn slots(&self) -> usize {
        self.cfg.slots
    }

    pub fn policy(&self) -> &Policy {
        &self.cfg.policy
    }

    pub fn snapshots(&self) -> &Arc<Snapshots> {
        &self.snapshots
    }

    /// The launcher, for processes besides the REPLs: slots from
    /// [`slots`](Self::slots) on are free for them.
    pub fn launcher(&self) -> &Launcher {
        &self.launcher
    }

    /// Waits for a free slot, preferring one that holds proof state `state`,
    /// then one that already has `header_key` warm.
    pub async fn acquire(self: &Arc<Self>, header_key: &str, state: Option<&str>) -> Lease {
        let permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .expect("pool semaphore closed");
        let mut idle = self.idle.lock().unwrap();
        let pick = state
            .and_then(|st| idle.iter().position(|s| s.states.contains_key(st)))
            .or_else(|| {
                idle.iter()
                    .position(|s| s.header.as_ref().is_some_and(|w| w.key == header_key))
            })
            .or_else(|| idle.iter().position(|s| s.header.is_none()))
            .or_else(|| {
                idle.iter()
                    .enumerate()
                    .min_by_key(|(_, s)| s.last_used)
                    .map(|(i, _)| i)
            })
            .expect("a permit guarantees an idle slot");
        let slot = idle.swap_remove(pick);
        Lease {
            pool: self.clone(),
            slot: Some(slot),
            _permit: permit,
        }
    }

    /// A slot with nothing imported, if one is idle right now. Used for pre-warming
    /// so that it never evicts a warm header or delays real work.
    pub fn try_acquire_cold(self: &Arc<Self>) -> Option<Lease> {
        let permit = self.permits.clone().try_acquire_owned().ok()?;
        let mut idle = self.idle.lock().unwrap();
        let pick = idle.iter().position(|s| s.header.is_none())?;
        let slot = idle.swap_remove(pick);
        Some(Lease {
            pool: self.clone(),
            slot: Some(slot),
            _permit: permit,
        })
    }

    pub fn is_warm(&self, header_key: &str) -> bool {
        self.warm.borrow().values().any(|k| k == header_key)
    }

    /// Marks `header_key` as being pre-warmed until the guard drops, unless
    /// it already is.
    pub fn prewarming(&self, header_key: &str) -> Option<Prewarming> {
        let key = header_key.to_string();
        self.prewarming
            .lock()
            .unwrap()
            .insert(key.clone())
            .then(|| Prewarming {
                keys: self.prewarming.clone(),
                key,
            })
    }

    fn set_warm(&self, slot: usize, key: Option<&str>) {
        self.warm.send_if_modified(|w| match key {
            Some(k) => w.insert(slot, k.to_string()).as_deref() != Some(k),
            None => w.remove(&slot).is_some(),
        });
    }
}

/// A header being pre-warmed; see [`Pool::prewarming`].
pub struct Prewarming {
    keys: Arc<Mutex<HashSet<String>>>,
    key: String,
}

impl Drop for Prewarming {
    fn drop(&mut self) {
        self.keys.lock().unwrap().remove(&self.key);
    }
}

/// Exclusive use of one slot; returned to the pool on drop.
pub struct Lease {
    pool: Arc<Pool>,
    slot: Option<Slot>,
    _permit: OwnedSemaphorePermit,
}

impl Deref for Lease {
    type Target = Slot;
    fn deref(&self) -> &Slot {
        self.slot.as_ref().unwrap()
    }
}

impl DerefMut for Lease {
    fn deref_mut(&mut self) -> &mut Slot {
        self.slot.as_mut().unwrap()
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut slot = self.slot.take().unwrap();
        slot.last_used = Instant::now();
        self.pool.idle.lock().unwrap().push(slot);
    }
}

impl Lease {
    /// Makes sure this slot's process has `header` imported and returns its
    /// environment ID and whether it was already warm.
    pub async fn warm_up(&mut self, header: &str, key: &str) -> Result<(u64, bool), HeaderFailure> {
        self.warm_up_with(header, key, None).await
    }

    /// Like [`warm_up`](Self::warm_up), but the header may import snapshot
    /// `pending`, which this job built and has not stored yet.
    pub async fn warm_up_with(
        &mut self,
        header: &str,
        key: &str,
        pending: Option<&str>,
    ) -> Result<(u64, bool), HeaderFailure> {
        // Even on a warm hit, so that a deleted snapshot stops working
        // everywhere; and before evicting anything, so that a bad import
        // doesn't cost a warm process.
        if let Err(e) = self.pool.snapshots.ensure(header, pending).await {
            return Err(HeaderFailure::Failed(e));
        }
        let alive = self.repl.as_ref().is_some_and(Repl::is_alive);
        if alive && let Some(w) = self.header.as_ref().filter(|w| w.key == key) {
            return Ok((w.env, true));
        }
        if alive {
            // A process keeps everything it ever imported, so start clean.
            self.retire("evict");
        }
        let id = self.id;
        let pool = self.pool.clone();
        let slot = self.slot.as_mut().unwrap();
        slot.jobs = 0;
        slot.oom_baseline = pool.launcher.oom_kills(id);
        let repl = match pool.launcher.spawn(id) {
            Ok(r) => slot.repl.insert(r),
            Err(e) => {
                return Err(HeaderFailure::Failed(Error::new(
                    ErrorKind::Crash,
                    format!("failed to start Lean: {e}"),
                )));
            }
        };
        let header = header_command(header);
        let timeout = pool.cfg.import_timeout;
        let r = match repl
            .send(&ReplCommand::cmd(header.clone(), None), timeout)
            .await
        {
            Ok(ReplResponse::Command(r)) if !r.has_errors() => r,
            Ok(ReplResponse::Command(r)) => {
                self.retire("header_error");
                return Err(HeaderFailure::Lean(Box::new(r)));
            }
            Ok(other) => {
                self.retire("header_error");
                return Err(HeaderFailure::Failed(Error::new(
                    ErrorKind::Internal,
                    format!("unexpected response to header: {other:?}"),
                )));
            }
            Err(e) => return Err(self.header_failed(e)),
        };
        // When imports can't be loaded together (two modules declaring the same
        // name), the REPL reports no error but leaves the environment empty.
        // Every header imports the report's library, so probe for it.
        let probe = ReplCommand::cmd(IMPORT_PROBE, Some(r.env));
        match repl.send(&probe, timeout).await {
            Ok(ReplResponse::Command(p)) if !p.has_errors() => {}
            Ok(_) => {
                let mut messages = pool.launcher.diagnose_header(id, &header, timeout).await;
                if !messages.iter().any(|m| m.severity == Severity::Error) {
                    messages.push(Message {
                        severity: Severity::Error,
                        pos: Pos { line: 1, column: 0 },
                        end_pos: None,
                        data: "the imports could not be loaded together".into(),
                    });
                }
                self.retire("header_error");
                return Err(HeaderFailure::Lean(Box::new(CommandResponse {
                    env: r.env,
                    messages,
                    sorries: Vec::new(),
                    tactics: Vec::new(),
                    at: Vec::new(),
                    infotree: None,
                })));
            }
            Err(e) => return Err(self.header_failed(e)),
        }
        self.header = Some(Warm {
            key: key.to_string(),
            env: r.env,
        });
        self.pool.set_warm(id, Some(key));
        Ok((r.env, false))
    }

    /// Retires the process after the REPL failed while importing a header.
    fn header_failed(&mut self, e: ReplFailure) -> HeaderFailure {
        if matches!(e, ReplFailure::Timeout) {
            self.retire("timeout");
            return HeaderFailure::Timeout;
        }
        let cause = self.failure_cause();
        self.retire(cause);
        HeaderFailure::Failed(if cause == "oom" {
            Error::new(ErrorKind::OutOfMemory, "Lean exceeded its memory limit")
        } else {
            Error::new(ErrorKind::Crash, format!("Lean process failed: {e}"))
        })
    }

    pub fn resident(&self, state: &str) -> Option<Resident> {
        self.states.get(state).cloned()
    }

    pub fn remember(&mut self, state: String, resident: Resident) {
        self.slot.as_mut().unwrap().states.insert(state, resident);
    }

    /// The slot's private directory, shared with its sandboxed process.
    pub fn scratch_dir(&self) -> std::path::PathBuf {
        self.pool.launcher.slot_dir(self.id)
    }

    pub fn repl(&mut self) -> Option<&mut Repl> {
        self.repl.as_mut().filter(|r| r.is_alive())
    }

    pub fn import_timeout(&self) -> Duration {
        self.pool.cfg.import_timeout
    }

    /// Compiles a module in this slot's sandbox and cgroup; see
    /// [`Launcher::compile`].
    pub async fn compile(
        &self,
        root: &std::path::Path,
        file: &std::path::Path,
        olean: &std::path::Path,
        timeout: Duration,
    ) -> std::io::Result<Compiled> {
        self.pool
            .launcher
            .compile(self.id, root, file, olean, timeout)
            .await
    }

    /// The `.olean` files of the modules `file` imports, as Lean reads its
    /// header; see [`Launcher::deps`].
    pub async fn deps(
        &self,
        file: &std::path::Path,
        timeout: Duration,
    ) -> Result<Vec<std::path::PathBuf>, String> {
        self.pool.launcher.deps(self.id, file, timeout).await
    }

    /// Re-verifies a snapshot module through the kernel; see
    /// [`Launcher::verify`].
    pub async fn verify(&self, module: &str, timeout: Duration) -> Verified {
        self.pool.launcher.verify(self.id, module, timeout).await
    }

    pub fn policy(&self) -> &Policy {
        &self.pool.cfg.policy
    }

    /// Why the process died: "oom" if its cgroup recorded an OOM kill since it
    /// started, otherwise "crash".
    pub fn failure_cause(&self) -> &'static str {
        if self.pool.launcher.oom_kills(self.id) > self.oom_baseline {
            "oom"
        } else {
            "crash"
        }
    }

    /// Kills the process (if any) and forgets its header, logging `cause`.
    pub fn retire(&mut self, cause: &str) {
        if let Some(mut repl) = self.repl.take() {
            tokio::spawn(async move { repl.kill().await });
            tracing::debug!(slot = self.id, cause, "replacing the Lean process");
        }
        self.forget();
    }

    /// Like [`retire`](Self::retire), but returns only once the process, and
    /// anything it started, has exited, so the slot's memory is free.
    pub async fn retire_and_wait(&mut self, cause: &str) {
        if let Some(mut repl) = self.repl.take() {
            repl.kill().await;
            tracing::debug!(slot = self.id, cause, "replacing the Lean process");
        }
        self.forget();
        self.pool.launcher.wait_idle(self.id).await;
    }

    fn forget(&mut self) {
        self.states.clear();
        if self.header.take().is_some() {
            self.pool.set_warm(self.id, None);
        }
    }

    /// Records a finished job and retires the process if it died, has run too
    /// many jobs, or holds too much memory.
    pub fn finish_job(&mut self) {
        self.jobs += 1;
        let Some(repl) = self.repl.as_ref() else {
            return;
        };
        if !repl.is_alive() {
            // The caller already saw the failure; just clear the slot.
            self.retire("crash");
        } else if self.jobs >= self.pool.cfg.max_uses {
            self.retire("uses");
        } else if self.pool.cfg.max_rss_bytes > 0
            && self.pool.launcher.memory(self.id, repl) > self.pool.cfg.max_rss_bytes
        {
            self.retire("memory");
        }
    }
}
