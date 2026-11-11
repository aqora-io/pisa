//! Hands jobs to workers through the database's job queue
//! ([`pisa_db::queue`]) and waits for their results. Workers pick what they
//! run, so priority classes and waiting limits hold across every server on
//! the queue.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pisa_db::db::{DbError, Notice};
use pisa_db::queue::{LIVE, NewJob, Poll, Queue, Stage};
use pisa_protocol::api::{JobKind, JobStatus};
use pisa_protocol::wire::{self, Job, JobResult};
use tokio::sync::Notify;
use tokio::sync::broadcast::error::RecvError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    Interactive = 0,
    Batch = 1,
}

#[derive(Debug, Clone, Default)]
pub struct Route {
    /// Only workers with this pool label may run the job; `None` means any.
    pub version: Option<String>,
    pub header_key: Option<String>,
    /// Header text, which new workers may pre-warm.
    pub header: Option<String>,
    /// The open document the job works on, by its id within `version`: only
    /// workers with Beam slots run it, and the one holding it open first.
    pub document: Option<String>,
}

/// A job, where it may run, and how soon, as an endpoint makes it.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub job: Job,
    pub route: Route,
    pub priority: Priority,
}

/// An async job, as its client looks it up.
#[derive(Debug, Clone)]
pub struct Looked {
    pub kind: JobKind,
    pub state: Looking,
}

#[derive(Debug, Clone)]
pub enum Looking {
    /// `Queued` or `Running`.
    Pending(JobStatus),
    /// What its request would have got.
    Done(Box<Result<JobResult, DispatchError>>),
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
    /// How long the claim on a job this server runs itself lasts, renewed
    /// three times as often; see [`Held`].
    pub held_lease: Duration,
}

impl Default for DispatchConfig {
    fn default() -> Self {
        DispatchConfig {
            max_wait: Duration::from_secs(60),
            max_retries: 2,
            held_lease: Duration::from_secs(30),
        }
    }
}

pub struct Dispatcher {
    queue: Queue,
    cfg: DispatchConfig,
    waiters: Arc<Waiters>,
    /// The name this process holds the jobs it runs itself under.
    holder: String,
}

/// The version of the jobs a server runs itself: no worker has it, so none
/// is offered them.
const HELD: &str = "(run by a server)";

/// An async job this server runs itself, such as a rebuild, which sends
/// workers jobs of its own. Its row holds the server's claim, renewed while
/// it runs, and then its answer, so `/jobs/{id}` reads it from any server.
/// If the server stops first, the claim lapses and the job reads as lost.
pub struct Held {
    pub token: String,
    id: i64,
    queue: Queue,
    holder: String,
    renewing: tokio::task::JoinHandle<()>,
}

impl Held {
    /// Stores the job's answer.
    pub async fn finish(self, result: &JobResult) -> Result<(), DispatchError> {
        self.renewing.abort();
        let result = serde_json::to_value(result)
            .map_err(|e| DispatchError::Unavailable(format!("encoding the answer: {e}")))?;
        self.queue.complete(&self.holder, self.id, &result).await?;
        Ok(())
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.renewing.abort();
    }
}

/// This process's requests waiting for their jobs, woken by the queue's
/// [`Notice::Done`]s. Several can wait for one job, e.g. long polls of an
/// async one.
#[derive(Default)]
struct Waiters(Mutex<HashMap<i64, Vec<Arc<Notify>>>>);

impl Waiters {
    fn add(self: &Arc<Self>, id: i64) -> Woken {
        let notify = Arc::new(Notify::new());
        let mut waiting = self.0.lock().unwrap();
        waiting.entry(id).or_default().push(notify.clone());
        Woken {
            waiters: self.clone(),
            id,
            notify,
        }
    }

    fn wake(&self, id: i64) {
        if let Some(waiting) = self.0.lock().unwrap().get(&id) {
            waiting.iter().for_each(|n| n.notify_one());
        }
    }

    fn wake_all(&self) {
        let waiting = self.0.lock().unwrap();
        waiting.values().flatten().for_each(|n| n.notify_one());
    }
}

/// A wake-up when job `id` is done, until it is dropped.
struct Woken {
    waiters: Arc<Waiters>,
    id: i64,
    notify: Arc<Notify>,
}

impl Woken {
    async fn notified(&self) {
        self.notify.notified().await
    }
}

impl Drop for Woken {
    fn drop(&mut self) {
        let mut waiting = self.waiters.0.lock().unwrap();
        if let Some(these) = waiting.get_mut(&self.id) {
            these.retain(|n| !Arc::ptr_eq(n, &self.notify));
            if these.is_empty() {
                waiting.remove(&self.id);
            }
        }
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
            holder: format!("server-{}", uuid::Uuid::new_v4().simple()),
        }
    }

    /// Queues `job` as an async job of `kind` that this server runs itself,
    /// and claims it; see [`Held`].
    pub async fn hold(&self, kind: JobKind, job: &Job) -> Result<Held, DispatchError> {
        let new = NewJob {
            version: Some(HELD.into()),
            priority: Priority::Batch as i16,
            header_key: None,
            header: None,
            document: None,
            job: wire::scoped(job, &pisa_db::scope::current())
                .map_err(|e| DispatchError::Unavailable(format!("encoding the job: {e}")))?,
            max_wait: self.cfg.max_wait,
            max_attempts: 1,
        };
        let (id, token) = self.queue.enqueue_async(&new, &kind_name(kind)).await?;
        let lease = self.cfg.held_lease;
        if self
            .queue
            .claim(&self.holder, &[id], lease)
            .await?
            .is_empty()
        {
            return Err(DispatchError::Unavailable(
                "the job couldn't be claimed as it was queued".into(),
            ));
        }
        let (queue, holder) = (self.queue.clone(), self.holder.clone());
        let renewing = tokio::spawn(async move {
            loop {
                tokio::time::sleep(lease / 3).await;
                if let Err(e) = queue.heartbeat(&holder, 0, lease).await {
                    tracing::warn!("renewing held jobs failed: {e}");
                }
            }
        });
        Ok(Held {
            token,
            id,
            queue: self.queue.clone(),
            holder: self.holder.clone(),
            renewing,
        })
    }

    pub fn queue(&self) -> &Queue {
        &self.queue
    }

    /// Queues `job` and waits for a worker to run it. It fails with
    /// [`DispatchError::NoCapacity`] if no worker claims it within
    /// `max_wait`, and [`DispatchError::WorkerLost`] if its worker was lost
    /// more times than `max_retries` allows, or no other one claimed it
    /// within `max_wait` after.
    pub async fn submit(&self, p: Prepared) -> Result<JobResult, DispatchError> {
        self.submit_new(self.new_job(p)?).await
    }

    async fn submit_new(&self, new: NewJob) -> Result<JobResult, DispatchError> {
        let id = self.queue.enqueue(&new).await?;
        let mut waiting = Waiting {
            dispatcher: self,
            id: Some(id),
        };
        let woken = self.waiters.add(id);
        let poll_every = self.queue.db().poll_interval();
        let outcome = loop {
            match self.queue.poll(id).await? {
                Poll::Done(result) => break Ok(decode(result)),
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
                                Some(0) => break Err(DispatchError::NoCapacity),
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
        waiting.id = None;
        outcome
    }

    /// Queues `p` for its client to look up later, as `kind`, by the token
    /// this returns; nothing gives it up if the client goes away.
    pub async fn submit_async(&self, kind: JobKind, p: Prepared) -> Result<String, DispatchError> {
        let new = self.new_job(p)?;
        Ok(self.queue.enqueue_async(&new, &kind_name(kind)).await?.1)
    }

    /// Where the async job `token` stands, after waiting up to `wait` for it
    /// to finish; `None` if there's no such job, or it has expired.
    pub async fn look(&self, token: &str, wait: Duration) -> Result<Option<Looked>, DispatchError> {
        let deadline = tokio::time::Instant::now() + wait;
        let poll_every = self.queue.db().poll_interval();
        let mut woken = None;
        loop {
            let Some(status) = self.queue.status(token).await? else {
                return Ok(None);
            };
            let kind = kind_of(&status.kind)?;
            let mut sleep = poll_every;
            let state = match status.stage {
                Stage::Done(result) => Looking::Done(Box::new(Ok(decode(result)))),
                Stage::GaveUp { attempts: 0 } => {
                    Looking::Done(Box::new(Err(DispatchError::NoCapacity)))
                }
                Stage::GaveUp { attempts } => {
                    Looking::Done(Box::new(Err(DispatchError::WorkerLost(attempts))))
                }
                Stage::Running => Looking::Pending(JobStatus::Running),
                Stage::Queued { give_up_at } => {
                    // Then the next look closes it.
                    let left = (give_up_at - now_micros()).max(0) as u64;
                    sleep = sleep.min(Duration::from_micros(left));
                    Looking::Pending(JobStatus::Queued)
                }
            };
            let now = tokio::time::Instant::now();
            if matches!(state, Looking::Done(_)) || now >= deadline {
                return Ok(Some(Looked { kind, state }));
            }
            let Some(woken) = &woken else {
                // Look again once subscribed, so a notice in between isn't
                // missed.
                woken = Some(self.waiters.add(status.id));
                continue;
            };
            let sleep = sleep.min(deadline - now);
            let _ = tokio::time::timeout(sleep, woken.notified()).await;
        }
    }

    /// Withdraws the async job `token` if no worker holds it. Returns its
    /// kind and what became of it, or `None` if there's no such job.
    pub async fn cancel(&self, token: &str) -> Result<Option<(JobKind, JobStatus)>, DispatchError> {
        let Some(status) = self.queue.status(token).await? else {
            return Ok(None);
        };
        let kind = kind_of(&status.kind)?;
        if self.queue.cancel(token).await?.is_some() {
            return Ok(Some((kind, JobStatus::Cancelled)));
        }
        let state = match self.queue.status(token).await?.map(|s| s.stage) {
            None => JobStatus::Cancelled,
            Some(Stage::Queued { .. }) => JobStatus::Queued,
            Some(Stage::Running) => JobStatus::Running,
            Some(Stage::Done(_) | Stage::GaveUp { .. }) => JobStatus::Done,
        };
        Ok(Some((kind, state)))
    }

    fn new_job(&self, p: Prepared) -> Result<NewJob, DispatchError> {
        // The worker runs it in the request's scope.
        let job = wire::scoped(&p.job, &pisa_db::scope::current())
            .map_err(|e| DispatchError::Unavailable(format!("encoding the job: {e}")))?;
        let route = p.route;
        Ok(NewJob {
            version: route.version,
            priority: p.priority as i16,
            header_key: route.header_key,
            header: route.header,
            document: route.document,
            job,
            max_wait: self.cfg.max_wait,
            max_attempts: self.cfg.max_retries + 1,
        })
    }

    /// The live workers on the queue, if it can be read.
    pub async fn workers(&self) -> Option<u64> {
        match self.queue.counts(LIVE).await {
            Ok(counts) => Some(counts.workers),
            Err(e) => {
                tracing::warn!("reading the queue's state failed: {e}");
                None
            }
        }
    }
}

/// How the queue records `kind`: as the API names it.
fn kind_name(kind: JobKind) -> String {
    match serde_json::to_value(kind) {
        Ok(serde_json::Value::String(name)) => name,
        _ => unreachable!("a JobKind serializes as a string"),
    }
}

/// The kind the queue recorded, which a server of another release may not
/// know.
fn kind_of(name: &str) -> Result<JobKind, DispatchError> {
    serde_json::from_value(serde_json::Value::String(name.into())).map_err(|_| {
        DispatchError::Unavailable(format!("this server doesn't know jobs of kind {name}"))
    })
}

/// A worker's result, or why it isn't one.
fn decode(result: serde_json::Value) -> JobResult {
    serde_json::from_value(result).unwrap_or_else(|e| JobResult::Failed {
        error: format!(
            "the worker's result doesn't decode, which may come from a worker of another \
             release: {e}"
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pisa_db::db::Db;
    use pisa_protocol::wire::SnapshotRebuildJob;

    #[tokio::test]
    async fn a_held_job_is_no_workers_and_reads_lost_once_its_server_stops() {
        let queue = Queue::new(Db::memory().await, "q");
        let cfg = DispatchConfig {
            held_lease: Duration::from_millis(300),
            ..Default::default()
        };
        let d = Dispatcher::start(queue.clone(), cfg).await;
        let job = Job::SnapshotRebuild(SnapshotRebuildJob {
            from: "f".into(),
            to: "t".into(),
            labels: Default::default(),
            allow_changes: false,
        });
        let looked = |token: String| {
            let d = &d;
            async move { d.look(&token, Duration::ZERO).await.unwrap().unwrap().state }
        };

        let held = d.hold(JobKind::RebuildSnapshots, &job).await.unwrap();
        let token = held.token.clone();
        assert!(
            queue.candidates("v", 10).await.unwrap().is_empty(),
            "no worker is offered it"
        );
        // Renewed, it stays running past its lease.
        tokio::time::sleep(Duration::from_millis(700)).await;
        assert!(matches!(
            looked(token.clone()).await,
            Looking::Pending(JobStatus::Running)
        ));
        held.finish(&JobResult::Failed {
            error: "done".into(),
        })
        .await
        .unwrap();
        let Looking::Done(done) = looked(token).await else {
            panic!("not done")
        };
        assert!(matches!(*done, Ok(JobResult::Failed { .. })));

        // Its server stops renewing it: it is lost.
        let held = d.hold(JobKind::RebuildSnapshots, &job).await.unwrap();
        let token = held.token.clone();
        drop(held);
        tokio::time::sleep(Duration::from_millis(700)).await;
        let Looking::Done(done) = looked(token).await else {
            panic!("not done")
        };
        assert!(
            matches!(*done, Err(DispatchError::WorkerLost(1))),
            "{done:?}"
        );
    }

    #[tokio::test]
    async fn every_waiter_on_a_job_is_woken_and_leaves_alone() {
        let waiters = Arc::new(Waiters::default());
        let a = waiters.add(1);
        let b = waiters.add(1);
        waiters.wake(1);
        let woken = |w: &Woken| {
            let n = w.notify.clone();
            async move { tokio::time::timeout(Duration::from_millis(50), n.notified()).await }
        };
        assert!(woken(&a).await.is_ok());
        assert!(woken(&b).await.is_ok());
        // `a` going leaves `b` waiting.
        drop(a);
        waiters.wake(1);
        assert!(woken(&b).await.is_ok());
        drop(b);
        assert!(waiters.0.lock().unwrap().is_empty());
    }
}
