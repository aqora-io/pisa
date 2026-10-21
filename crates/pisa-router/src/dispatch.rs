//! The global queue and per-message routing to connected workers.
//!
//! Every job waits in one queue per priority class. Whenever capacity frees up
//! or a job arrives, `pump` places as many queued jobs as it can, preferring in
//! order: the worker that holds the job's state, an idle worker with its header
//! warm, the header's rendezvous-hash owner under a load bound, then any idle
//! worker of the right version.

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use pisa_protocol::wire::{Job, JobResult, RouterMessage};
use tokio::sync::{mpsc, oneshot};

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
    /// Header text, remembered so popular headers can be pre-warmed.
    pub header: Option<String>,
    /// Prefer this worker (by name), e.g. the one holding a proof state.
    pub affinity: Option<String>,
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum DispatchError {
    #[error("No available REPLs")]
    NoCapacity,
    #[error("job failed on {0} workers that disconnected")]
    WorkerLost(u32),
}

#[derive(Debug, Clone)]
pub struct DispatchConfig {
    /// How long a job may wait in the queue before it is rejected.
    pub max_wait: Duration,
    /// How many times a job is retried after its worker disconnects.
    pub max_retries: u32,
    /// Bounded-load factor for header affinity: a worker may take at most this
    /// multiple of the average load before the next one is tried.
    pub load_factor: f64,
    /// How many popular headers to ask new workers to pre-warm.
    pub prewarm: usize,
}

impl Default for DispatchConfig {
    fn default() -> Self {
        DispatchConfig {
            max_wait: Duration::from_secs(60),
            max_retries: 2,
            load_factor: 1.25,
            prewarm: 2,
        }
    }
}

pub struct Dispatcher {
    state: Mutex<State>,
    cfg: DispatchConfig,
}

/// Where a finished job ran, for the caller's bookkeeping.
#[derive(Debug, Clone)]
pub struct Completion {
    pub result: JobResult,
    pub worker: String,
}

struct Pending {
    job: Job,
    route: Route,
    priority: Priority,
    enqueued: Instant,
    retries: u32,
    reply: oneshot::Sender<Result<Completion, DispatchError>>,
}

struct WorkerEntry {
    name: String,
    version: String,
    slots: u32,
    warm: HashSet<String>,
    in_flight: HashMap<u64, Pending>,
    tx: mpsc::UnboundedSender<RouterMessage>,
}

impl WorkerEntry {
    fn free(&self) -> bool {
        (self.in_flight.len() as u32) < self.slots
    }
}

#[derive(Default)]
struct State {
    workers: HashMap<u64, WorkerEntry>,
    queues: [VecDeque<Pending>; 2],
    next_request: u64,
    /// (version, header key) → (header text, requests seen).
    headers: HashMap<(Option<String>, String), (String, u64)>,
}

impl Dispatcher {
    pub fn new(cfg: DispatchConfig) -> Self {
        Dispatcher {
            state: Mutex::new(State::default()),
            cfg,
        }
    }

    pub async fn submit(
        &self,
        job: Job,
        route: Route,
        priority: Priority,
    ) -> Result<Completion, DispatchError> {
        let (reply, rx) = oneshot::channel();
        {
            let mut s = self.state.lock().unwrap();
            if let (Some(key), Some(text)) = (&route.header_key, &route.header) {
                let entry = s
                    .headers
                    .entry((route.version.clone(), key.clone()))
                    .or_insert_with(|| (text.clone(), 0));
                entry.1 += 1;
            }
            s.queues[priority as usize].push_back(Pending {
                job,
                route,
                priority,
                enqueued: Instant::now(),
                retries: 0,
                reply,
            });
            self.pump(&mut s);
        }
        rx.await.unwrap_or(Err(DispatchError::NoCapacity))
    }

    /// Registers a worker connection and returns headers it should pre-warm.
    pub fn connect(
        &self,
        conn: u64,
        name: String,
        version: String,
        slots: u32,
        tx: mpsc::UnboundedSender<RouterMessage>,
    ) {
        let mut s = self.state.lock().unwrap();
        let mut popular: Vec<_> = s
            .headers
            .iter()
            .filter(|((v, _), _)| v.as_deref().is_none_or(|v| v == version))
            .map(|(_, (text, count))| (*count, text.clone()))
            .collect();
        popular.sort_by_key(|p| std::cmp::Reverse(p.0));
        let headers: Vec<String> = popular
            .into_iter()
            .take(self.cfg.prewarm)
            .map(|(_, t)| t)
            .collect();
        if !headers.is_empty() {
            let _ = tx.send(RouterMessage::Prewarm { headers });
        }
        s.workers.insert(
            conn,
            WorkerEntry {
                name,
                version,
                slots,
                warm: HashSet::new(),
                in_flight: HashMap::new(),
                tx,
            },
        );
        self.pump(&mut s);
    }

    pub fn status(&self, conn: u64, warm: Vec<String>) {
        let mut s = self.state.lock().unwrap();
        if let Some(w) = s.workers.get_mut(&conn) {
            w.warm = warm.into_iter().collect();
        }
    }

    pub fn complete(&self, conn: u64, request_id: u64, result: JobResult) {
        let mut s = self.state.lock().unwrap();
        let Some(w) = s.workers.get_mut(&conn) else {
            return;
        };
        let Some(p) = w.in_flight.remove(&request_id) else {
            tracing::warn!(conn, request_id, "result for unknown request");
            return;
        };
        let worker = w.name.clone();
        let _ = p.reply.send(Ok(Completion { result, worker }));
        self.pump(&mut s);
    }

    /// Drops a worker and requeues its in-flight jobs at the front.
    pub fn disconnect(&self, conn: u64) {
        let mut s = self.state.lock().unwrap();
        let Some(w) = s.workers.remove(&conn) else {
            return;
        };
        tracing::info!(worker = %w.name, in_flight = w.in_flight.len(), "worker disconnected");
        for (_, mut p) in w.in_flight {
            p.retries += 1;
            if p.retries > self.cfg.max_retries {
                let _ = p.reply.send(Err(DispatchError::WorkerLost(p.retries)));
            } else {
                metrics::counter!(m::RETRIES).increment(1);
                s.queues[p.priority as usize].push_front(p);
            }
        }
        self.pump(&mut s);
    }

    /// Rejects jobs that have waited longer than `max_wait`, and drops jobs
    /// whose caller went away.
    pub fn expire(&self) {
        let mut s = self.state.lock().unwrap();
        let max_wait = self.cfg.max_wait;
        for q in &mut s.queues {
            let mut kept = VecDeque::with_capacity(q.len());
            for p in q.drain(..) {
                if p.reply.is_closed() {
                    continue;
                }
                if p.enqueued.elapsed() > max_wait {
                    metrics::counter!(m::EXPIRED).increment(1);
                    let _ = p.reply.send(Err(DispatchError::NoCapacity));
                } else {
                    kept.push_back(p);
                }
            }
            *q = kept;
        }
        self.update_gauges(&s);
    }

    fn pump(&self, s: &mut State) {
        for q in 0..s.queues.len() {
            let mut i = 0;
            while i < s.queues[q].len() {
                if !s.workers.values().any(WorkerEntry::free) {
                    self.update_gauges(s);
                    return;
                }
                if s.queues[q][i].reply.is_closed() {
                    s.queues[q].remove(i);
                    continue;
                }
                match choose(&s.workers, &s.queues[q][i].route, self.cfg.load_factor) {
                    Some(conn) => {
                        let p = s.queues[q].remove(i).unwrap();
                        let id = s.next_request;
                        s.next_request += 1;
                        dispatch(s.workers.get_mut(&conn).unwrap(), id, p);
                    }
                    None => i += 1,
                }
            }
        }
        self.update_gauges(s);
    }

    fn update_gauges(&self, s: &State) {
        for p in [Priority::Interactive, Priority::Batch] {
            metrics::gauge!(m::QUEUE_DEPTH, "priority" => p.label())
                .set(s.queues[p as usize].len() as f64);
        }
        let total: u32 = s.workers.values().map(|w| w.slots).sum();
        let busy: usize = s.workers.values().map(|w| w.in_flight.len()).sum();
        metrics::gauge!(m::SLOTS, "state" => "total").set(total as f64);
        metrics::gauge!(m::SLOTS, "state" => "busy").set(busy as f64);
        metrics::gauge!(m::WORKERS).set(s.workers.len() as f64);
    }

    pub fn worker_count(&self) -> usize {
        self.state.lock().unwrap().workers.len()
    }

    #[cfg(test)]
    fn in_flight(&self) -> HashMap<String, usize> {
        let s = self.state.lock().unwrap();
        s.workers
            .values()
            .map(|w| (w.name.clone(), w.in_flight.len()))
            .collect()
    }
}

fn dispatch(w: &mut WorkerEntry, request_id: u64, p: Pending) {
    if p.retries == 0 {
        metrics::histogram!(m::QUEUE_WAIT, "priority" => p.priority.label())
            .record(p.enqueued.elapsed().as_secs_f64());
    }
    // A closed channel means the connection is going away; `disconnect` will
    // requeue everything in flight, including this job.
    let _ = w.tx.send(RouterMessage::Job {
        request_id,
        job: p.job.clone(),
    });
    w.in_flight.insert(request_id, p);
}

fn choose(workers: &HashMap<u64, WorkerEntry>, route: &Route, load_factor: f64) -> Option<u64> {
    let fits = |w: &WorkerEntry| route.version.as_ref().is_none_or(|v| *v == w.version);
    let candidates: Vec<(u64, &WorkerEntry)> = workers
        .iter()
        .filter(|(_, w)| fits(w))
        .map(|(c, w)| (*c, w))
        .collect();
    let free: Vec<(u64, &WorkerEntry)> = candidates
        .iter()
        .copied()
        .filter(|(_, w)| w.free())
        .collect();
    if free.is_empty() {
        return None;
    }
    let least_loaded = |ws: &[(u64, &WorkerEntry)]| {
        ws.iter()
            .min_by(|a, b| {
                let la = a.1.in_flight.len() as f64 / a.1.slots as f64;
                let lb = b.1.in_flight.len() as f64 / b.1.slots as f64;
                la.total_cmp(&lb).then(a.0.cmp(&b.0))
            })
            .map(|(c, _)| *c)
    };

    if let Some(name) = &route.affinity
        && let Some((c, _)) = free.iter().find(|(_, w)| w.name == *name)
    {
        return Some(*c);
    }
    let Some(key) = &route.header_key else {
        return least_loaded(&free);
    };
    let warm: Vec<_> = free
        .iter()
        .copied()
        .filter(|(_, w)| w.warm.contains(key))
        .collect();
    if !warm.is_empty() {
        return least_loaded(&warm);
    }
    // Rendezvous hashing with a load bound: the header's preferred workers in a
    // stable order, skipping any already above `load_factor` × average load.
    let in_flight: usize = candidates.iter().map(|(_, w)| w.in_flight.len()).sum();
    let bound = (load_factor * (in_flight + 1) as f64 / candidates.len() as f64).ceil() as usize;
    let mut ranked = free.clone();
    ranked.sort_by_key(|(_, w)| std::cmp::Reverse(rendezvous(key, &w.name)));
    ranked
        .iter()
        .find(|(_, w)| w.in_flight.len() < bound.max(1))
        .map(|(c, _)| *c)
        .or_else(|| least_loaded(&free))
}

fn rendezvous(key: &str, worker: &str) -> u64 {
    let mut h = DefaultHasher::new();
    (key, worker).hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pisa_protocol::wire::{CheckJob, CheckOutcome};
    use std::sync::Arc;

    fn job() -> Job {
        Job::Check(CheckJob {
            code: String::new(),
            timeout_secs: 1,
            infotree: None,
            reuse: true,
        })
    }

    fn route(header: &str) -> Route {
        Route {
            version: Some("v".into()),
            header_key: Some(header.into()),
            header: Some(header.into()),
            affinity: None,
        }
    }

    fn worker(
        d: &Dispatcher,
        conn: u64,
        name: &str,
        version: &str,
        slots: u32,
    ) -> mpsc::UnboundedReceiver<RouterMessage> {
        let (tx, rx) = mpsc::unbounded_channel();
        d.connect(conn, name.into(), version.into(), slots, tx);
        rx
    }

    fn done() -> JobResult {
        JobResult::Check(CheckOutcome {
            time: 0.0,
            error: None,
            response: None,
            verification: None,
            warm: true,
        })
    }

    async fn next_job(rx: &mut mpsc::UnboundedReceiver<RouterMessage>) -> u64 {
        loop {
            match rx.recv().await.unwrap() {
                RouterMessage::Job { request_id, .. } => return request_id,
                RouterMessage::Prewarm { .. } => continue,
            }
        }
    }

    #[tokio::test]
    async fn prefers_warm_worker() {
        let d = Arc::new(Dispatcher::new(DispatchConfig::default()));
        let _a = worker(&d, 1, "a", "v", 2);
        let mut b = worker(&d, 2, "b", "v", 2);
        d.status(2, vec!["h".into()]);
        let d2 = d.clone();
        let task = tokio::spawn(async move { d2.submit(job(), route("h"), Priority::Batch).await });
        let id = next_job(&mut b).await;
        d.complete(2, id, done());
        assert_eq!(task.await.unwrap().unwrap().worker, "b");
    }

    #[tokio::test]
    async fn respects_versions_and_queues_when_full() {
        let d = Arc::new(Dispatcher::new(DispatchConfig::default()));
        let _other = worker(&d, 1, "other", "w", 4);
        let mut a = worker(&d, 2, "a", "v", 1);
        let d2 = d.clone();
        let first =
            tokio::spawn(async move { d2.submit(job(), route("h"), Priority::Batch).await });
        let id1 = next_job(&mut a).await;
        let d3 = d.clone();
        let second =
            tokio::spawn(async move { d3.submit(job(), route("h"), Priority::Batch).await });
        tokio::task::yield_now().await;
        assert_eq!(d.in_flight()["other"], 0);
        assert_eq!(d.in_flight()["a"], 1);
        d.complete(2, id1, done());
        let id2 = next_job(&mut a).await;
        d.complete(2, id2, done());
        assert_eq!(first.await.unwrap().unwrap().worker, "a");
        assert_eq!(second.await.unwrap().unwrap().worker, "a");
    }

    #[tokio::test]
    async fn interactive_jumps_the_batch_queue() {
        let d = Arc::new(Dispatcher::new(DispatchConfig::default()));
        let mut a = worker(&d, 1, "a", "v", 1);
        let order = Arc::new(Mutex::new(Vec::new()));
        let spawn = |p: Priority, tag: &'static str| {
            let d = d.clone();
            let order = order.clone();
            tokio::spawn(async move {
                d.submit(job(), route("h"), p).await.unwrap();
                order.lock().unwrap().push(tag);
            })
        };
        let t0 = spawn(Priority::Batch, "batch0");
        let id0 = next_job(&mut a).await;
        let t1 = spawn(Priority::Batch, "batch1");
        tokio::task::yield_now().await;
        let t2 = spawn(Priority::Interactive, "interactive");
        tokio::task::yield_now().await;
        d.complete(1, id0, done());
        t0.await.unwrap();
        let id = next_job(&mut a).await;
        d.complete(1, id, done());
        t2.await.unwrap();
        let id = next_job(&mut a).await;
        d.complete(1, id, done());
        t1.await.unwrap();
        assert_eq!(*order.lock().unwrap(), ["batch0", "interactive", "batch1"]);
    }

    #[tokio::test]
    async fn disconnect_retries_elsewhere_then_gives_up() {
        let d = Arc::new(Dispatcher::new(DispatchConfig {
            max_retries: 1,
            ..Default::default()
        }));
        let mut a = worker(&d, 1, "a", "v", 1);
        let d2 = d.clone();
        let task = tokio::spawn(async move { d2.submit(job(), route("h"), Priority::Batch).await });
        next_job(&mut a).await;
        let mut b = worker(&d, 2, "b", "v", 1);
        d.disconnect(1);
        next_job(&mut b).await;
        d.disconnect(2);
        assert_eq!(
            task.await.unwrap().unwrap_err(),
            DispatchError::WorkerLost(2)
        );
    }

    #[tokio::test]
    async fn expires_jobs_that_wait_too_long() {
        let d = Arc::new(Dispatcher::new(DispatchConfig {
            max_wait: Duration::from_millis(10),
            ..Default::default()
        }));
        let d2 = d.clone();
        let task = tokio::spawn(async move { d2.submit(job(), route("h"), Priority::Batch).await });
        tokio::time::sleep(Duration::from_millis(30)).await;
        d.expire();
        assert_eq!(task.await.unwrap().unwrap_err(), DispatchError::NoCapacity);
    }

    #[test]
    fn header_affinity_is_stable_and_bounded() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mk = |name: &str, in_flight: usize| {
            let mut w = WorkerEntry {
                name: name.into(),
                version: "v".into(),
                slots: 8,
                warm: HashSet::new(),
                in_flight: HashMap::new(),
                tx: tx.clone(),
            };
            for i in 0..in_flight {
                let (reply, _) = oneshot::channel();
                w.in_flight.insert(
                    i as u64,
                    Pending {
                        job: job(),
                        route: Route::default(),
                        priority: Priority::Batch,
                        enqueued: Instant::now(),
                        retries: 0,
                        reply,
                    },
                );
            }
            w
        };
        let workers: HashMap<u64, WorkerEntry> =
            (0..4).map(|i| (i, mk(&format!("w{i}"), 0))).collect();
        let owner = choose(&workers, &route("h"), 1.25).unwrap();
        assert_eq!(choose(&workers, &route("h"), 1.25), Some(owner));

        // Overload the owner: the next job goes elsewhere.
        let mut workers = workers;
        let name = workers[&owner].name.clone();
        workers.insert(owner, mk(&name, 4));
        assert_ne!(choose(&workers, &route("h"), 1.25), Some(owner));
    }
}
