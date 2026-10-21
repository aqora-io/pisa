//! A fixed set of slots, each holding at most one REPL process bound to one
//! import header. Idle slots are reused by header (a warm hit) and otherwise
//! evicted least-recently-used first.

use std::collections::BTreeMap;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pisa_protocol::repl::{CommandResponse, ReplCommand, ReplResponse};
use pisa_repl::{Repl, ReplFailure};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch};

use crate::launcher::Launcher;
use crate::policy::{Policy, header_with_report_import};

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
    restarts: mpsc::UnboundedSender<String>,
    launcher: Launcher,
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
}

struct Warm {
    key: String,
    env: u64,
}

#[derive(Debug)]
pub enum HeaderFailure {
    /// The header elaborated with errors (e.g. an unknown module).
    Lean(CommandResponse),
    Timeout,
    Repl(String),
}

impl Pool {
    pub fn new(
        launcher: Launcher,
        cfg: PoolConfig,
    ) -> (
        Arc<Self>,
        watch::Receiver<BTreeMap<usize, String>>,
        mpsc::UnboundedReceiver<String>,
    ) {
        let now = Instant::now();
        let slots = (0..cfg.slots)
            .map(|id| Slot {
                id,
                repl: None,
                header: None,
                jobs: 0,
                last_used: now,
                oom_baseline: 0,
            })
            .collect();
        let (warm, warm_rx) = watch::channel(BTreeMap::new());
        let (restarts, restarts_rx) = mpsc::unbounded_channel();
        let pool = Pool {
            idle: Mutex::new(slots),
            permits: Arc::new(Semaphore::new(cfg.slots)),
            warm,
            restarts,
            launcher,
            cfg,
        };
        (Arc::new(pool), warm_rx, restarts_rx)
    }

    pub fn slots(&self) -> usize {
        self.cfg.slots
    }

    /// Waits for a free slot, preferring one that already has `header_key` warm.
    pub async fn acquire(self: &Arc<Self>, header_key: &str) -> Lease {
        let permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .expect("pool semaphore closed");
        let mut idle = self.idle.lock().unwrap();
        let pick = idle
            .iter()
            .position(|s| s.header.as_ref().is_some_and(|w| w.key == header_key))
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

    fn set_warm(&self, slot: usize, key: Option<&str>) {
        self.warm.send_if_modified(|w| match key {
            Some(k) => w.insert(slot, k.to_string()).as_deref() != Some(k),
            None => w.remove(&slot).is_some(),
        });
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
            Err(e) => return Err(HeaderFailure::Repl(format!("failed to start REPL: {e}"))),
        };
        match repl
            .send(
                &ReplCommand::cmd(header_with_report_import(header), None),
                pool.cfg.import_timeout,
            )
            .await
        {
            Ok(ReplResponse::Command(r)) if !r.has_errors() => {
                self.header = Some(Warm {
                    key: key.to_string(),
                    env: r.env,
                });
                self.pool.set_warm(id, Some(key));
                Ok((r.env, false))
            }
            Ok(ReplResponse::Command(r)) => {
                self.retire("header_error");
                Err(HeaderFailure::Lean(r))
            }
            Ok(other) => {
                self.retire("header_error");
                Err(HeaderFailure::Repl(format!(
                    "unexpected response to header: {other:?}"
                )))
            }
            Err(ReplFailure::Timeout) => {
                self.retire("timeout");
                Err(HeaderFailure::Timeout)
            }
            Err(e) => {
                let cause = self.failure_cause();
                self.retire(cause);
                Err(HeaderFailure::Repl(if cause == "oom" {
                    "ran out of memory".into()
                } else {
                    e.to_string()
                }))
            }
        }
    }

    pub fn repl(&mut self) -> Option<&mut Repl> {
        self.repl.as_mut().filter(|r| r.is_alive())
    }

    pub fn import_timeout(&self) -> Duration {
        self.pool.cfg.import_timeout
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

    /// Kills the process (if any) and forgets its header, reporting `cause`.
    pub fn retire(&mut self, cause: &str) {
        if let Some(mut repl) = self.repl.take() {
            tokio::spawn(async move { repl.kill().await });
            let _ = self.pool.restarts.send(cause.to_string());
        }
        if self.header.take().is_some() {
            self.pool.set_warm(self.id, None);
        }
    }

    /// Records a finished job and retires the process if it died, has run too
    /// many jobs, or holds too much memory.
    pub fn finish_job(&mut self, reuse: bool) {
        self.jobs += 1;
        let Some(repl) = self.repl.as_ref() else {
            return;
        };
        if !repl.is_alive() {
            // The caller already saw the failure; just clear the slot.
            self.retire("crash");
        } else if !reuse {
            self.retire("no_reuse");
        } else if self.jobs >= self.pool.cfg.max_uses {
            self.retire("uses");
        } else if self.pool.cfg.max_rss_bytes > 0
            && self.pool.launcher.memory(self.id, repl) > self.pool.cfg.max_rss_bytes
        {
            self.retire("memory");
        }
    }
}
