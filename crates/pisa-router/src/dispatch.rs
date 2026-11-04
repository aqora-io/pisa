//! Hands jobs to workers through the database's job queue
//! ([`pisa_db::queue`]) and waits for their results. Workers pick what they
//! run, so priority classes and waiting limits hold across every server on
//! the queue.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pisa_db::db::{DbError, Notice};
use pisa_db::queue::{Done, LIVE, NewJob, Poll, Queue};
use pisa_protocol::wire::{Job, JobResult};
use tokio::sync::Notify;
use tokio::sync::broadcast::error::RecvError;

use crate::metrics as m;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    Interactive = 0,
    Batch = 1,
}

impl Priority {
    fn label(self) -> &'static str {
        match self {
            Priority::Interactive => "interactive",
            Priority::Batch => "batch",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Route {
    /// Only workers with this pool label may run the job; `None` means any.
    pub version: Option<String>,
    pub header_key: Option<String>,
    /// Header text, which new workers may pre-warm.
    pub header: Option<String>,
    /// The hash of the proof state the job continues: the worker that stored
    /// it, which may still hold it, gets first refusal. Needs `version`.
    pub state: Option<String>,
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum DispatchError {
    #[error("No available REPLs")]
    NoCapacity,
    #[error("job failed on {0} workers that disconnected")]
    WorkerLost(u32),
    #[error("the job queue is unavailable, retry later: {0}")]
    Unavailable(String),
}

impl From<DbError> for DispatchError {
    fn from(e: DbError) -> Self {
        DispatchError::Unavailable(e.to_string())
    }
}

#[derive(Debug, Clone)]
pub struct DispatchConfig {
    /// How long a job may wait for a worker before it is given up.
    pub max_wait: Duration,
    /// How many more workers may run a job after its worker is lost.
    pub max_retries: u32,
}

impl Default for DispatchConfig {
    fn default() -> Self {
        DispatchConfig {
            max_wait: Duration::from_secs(60),
            max_retries: 2,
        }
    }
}

pub struct Dispatcher {
    queue: Queue,
    cfg: DispatchConfig,
    waiters: Arc<Waiters>,
}

/// Where a finished job ran, for the caller's bookkeeping.
#[derive(Debug, Clone)]
pub struct Completion {
    pub result: JobResult,
    pub worker: String,
}

/// This process's requests waiting for their jobs, woken by the queue's
/// [`Notice::Done`]s.
#[derive(Default)]
struct Waiters(Mutex<HashMap<i64, Arc<Notify>>>);

impl Waiters {
    fn add(&self, id: i64) -> Arc<Notify> {
        let notify = Arc::new(Notify::new());
        self.0.lock().unwrap().insert(id, notify.clone());
        notify
    }

    fn remove(&self, id: i64) {
        self.0.lock().unwrap().remove(&id);
    }

    fn wake(&self, id: i64) {
        if let Some(n) = self.0.lock().unwrap().get(&id) {
            n.notify_one();
        }
    }

    fn wake_all(&self) {
        self.0.lock().unwrap().values().for_each(|n| n.notify_one());
    }
}

/// Gives the job up, unless it is running, when its request goes away.
struct Waiting<'a> {
    dispatcher: &'a Dispatcher,
    id: Option<i64>,
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        let Some(id) = self.id else { return };
        self.dispatcher.waiters.remove(id);
        let queue = self.dispatcher.queue.clone();
        tokio::spawn(async move {
            if let Err(e) = queue.give_up(id).await {
                tracing::warn!(id, "giving up an abandoned job failed: {e}");
            }
        });
    }
}

fn now_micros() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as i64
}

impl Dispatcher {
    /// A dispatcher on `queue`, listening for its notifications.
    pub async fn start(queue: Queue, cfg: DispatchConfig) -> Dispatcher {
        let waiters = Arc::new(Waiters::default());
        let mut notices = queue.db().notices().await;
        let listening = Arc::downgrade(&waiters);
        tokio::spawn(async move {
            loop {
                let notice = notices.recv().await;
                let Some(waiters) = listening.upgrade() else {
                    return;
                };
                match notice {
                    Ok(Notice::Done(id)) => waiters.wake(id),
                    Ok(Notice::Queued(_)) => {}
                    // Ours may be among the missed: everyone looks again.
                    Err(RecvError::Lagged(_)) => waiters.wake_all(),
                    Err(RecvError::Closed) => return,
                }
            }
        });
        Dispatcher {
            queue,
            cfg,
            waiters,
        }
    }

    pub fn queue(&self) -> &Queue {
        &self.queue
    }

    /// Queues `job` and waits for a worker to run it. It fails with
    /// [`DispatchError::NoCapacity`] if no worker claims it within
    /// `max_wait`, and [`DispatchError::WorkerLost`] if its worker was lost
    /// more times than `max_retries` allows, or no other one claimed it
    /// within `max_wait` after.
    pub async fn submit(
        &self,
        job: Job,
        route: Route,
        priority: Priority,
    ) -> Result<Completion, DispatchError> {
        let job = serde_json::to_value(&job)
            .map_err(|e| DispatchError::Unavailable(format!("encoding the job: {e}")))?;
        let new = NewJob {
            state: route.state.zip(route.version.clone()).map(|(h, v)| (v, h)),
            version: route.version,
            priority: priority as i16,
            header_key: route.header_key,
            header: route.header,
            job,
            max_wait: self.cfg.max_wait,
            max_attempts: self.cfg.max_retries + 1,
        };
        let id = self.queue.enqueue(&new).await?;
        let mut waiting = Waiting {
            dispatcher: self,
            id: Some(id),
        };
        let woken = self.waiters.add(id);
        let poll_every = self.queue.db().poll_interval();
        let outcome = loop {
            match self.queue.poll(id).await? {
                Poll::Done(done) => break Ok(self.finish(done, priority)),
                Poll::Lost { attempts } => break Err(DispatchError::WorkerLost(attempts)),
                Poll::Gone => {
                    break Err(DispatchError::Unavailable(
                        "the job left the queue unfinished".into(),
                    ));
                }
                Poll::Waiting { give_up_at } => {
                    let now = now_micros();
                    let mut wait = poll_every;
                    if let Some(at) = give_up_at {
                        if at <= now {
                            match self.queue.give_up(id).await? {
                                Some(0) => {
                                    metrics::counter!(m::EXPIRED).increment(1);
                                    break Err(DispatchError::NoCapacity);
                                }
                                Some(attempts) => break Err(DispatchError::WorkerLost(attempts)),
                                // A worker claimed it meanwhile.
                                None => continue,
                            }
                        }
                        wait = wait.min(Duration::from_micros((at - now) as u64));
                    }
                    let _ = tokio::time::timeout(wait, woken.notified()).await;
                }
            }
        };
        self.waiters.remove(id);
        waiting.id = None;
        outcome
    }

    fn finish(&self, done: Done, priority: Priority) -> Completion {
        if done.attempts <= 1 {
            metrics::histogram!(m::QUEUE_WAIT, "priority" => priority.label())
                .record(done.waited.as_secs_f64());
        }
        if done.attempts > 1 {
            metrics::counter!(m::RETRIES).increment((done.attempts - 1) as u64);
        }
        for cause in done.restarts {
            metrics::counter!(m::RESTARTS, "cause" => cause).increment(1);
        }
        let result = serde_json::from_value(done.result).unwrap_or_else(|e| JobResult::Failed {
            error: format!(
                "the worker's result doesn't decode, which may come from a worker of another \
                 release: {e}"
            ),
        });
        Completion {
            result,
            worker: done.worker,
        }
    }

    /// Sets the gauges of the queue's state, shared by every server on it.
    pub async fn record_gauges(&self) {
        let counts = match self.queue.counts(LIVE).await {
            Ok(counts) => counts,
            Err(e) => {
                tracing::warn!("reading the queue's state failed: {e}");
                return;
            }
        };
        for p in [Priority::Interactive, Priority::Batch] {
            metrics::gauge!(m::QUEUE_DEPTH, "priority" => p.label())
                .set(counts.queued[p as usize] as f64);
        }
        metrics::gauge!(m::SLOTS, "state" => "total").set(counts.slots as f64);
        metrics::gauge!(m::SLOTS, "state" => "busy").set(counts.running as f64);
        metrics::gauge!(m::WORKERS).set(counts.workers as f64);
    }
}
