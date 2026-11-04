//! Takes jobs from the database's job queue ([`pisa_db::queue`]): claims
//! what this worker should run, runs it on the pool, and writes the result.
//!
//! Every free worker of a version looks at the same oldest jobs, and each
//! leaves a job alone for a moment ([`PREFER`]) when another live worker with
//! a free slot is a better home for it: the one that stored the proof state
//! it continues, or, when this worker hasn't imported its header, one that
//! has. Otherwise the first to claim a job runs it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pisa_db::db::Notice;
use pisa_db::queue::{Candidate, Claimed, LIVE, Peer, Queue};
use pisa_protocol::wire::{Job, JobResult};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{Mutex, Notify, watch};

use crate::pool::Pool;
use crate::sessions::Sessions;
use crate::{prewarm, run_job};

/// How long another worker gets first refusal of a job it is a better home
/// for. Long enough for it to hear of the job and claim it; short enough
/// that a stale registry costs little.
pub const PREFER: Duration = Duration::from_millis(500);

/// How many of the oldest open jobs a worker picks from.
const CANDIDATES: usize = 64;

/// How many of the headers most in demand a new worker pre-warms.
const PREWARM_POPULAR: usize = 2;

/// What a worker leaves to others and what it claims, from [`pick`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Picked {
    pub ids: Vec<i64>,
    /// When the first job left to another worker becomes this one's to take
    /// (microseconds), if any was.
    pub release_at: Option<i64>,
}

/// Picks up to `free` of `candidates`, which come interactive first and then
/// oldest first, for worker `me` with header keys `warm` to claim.
pub fn pick(
    me: &str,
    free: usize,
    warm: &HashSet<String>,
    candidates: &[Candidate],
    peers: &[Peer],
    now: i64,
) -> Picked {
    let mut spare: HashMap<&str, u32> = peers
        .iter()
        .filter(|p| p.name != me && !p.draining && p.busy < p.slots)
        .map(|p| (p.name.as_str(), p.slots - p.busy))
        .collect();
    let take = |name: &str, spare: &mut HashMap<&str, u32>| match spare.get_mut(name) {
        Some(n) if *n > 0 => {
            *n -= 1;
            true
        }
        _ => false,
    };
    let mut picked = Picked::default();
    for c in candidates {
        if picked.ids.len() >= free {
            break;
        }
        let released = c.enqueued_micros + PREFER.as_micros() as i64;
        let elsewhere = now < released && {
            let holder = c.affinity.as_deref();
            match holder {
                Some(h) if h == me => false,
                Some(h) if take(h, &mut spare) => true,
                _ => c.header_key.as_ref().is_some_and(|key| {
                    !warm.contains(key)
                        && peers
                            .iter()
                            .filter(|p| p.warm.contains(key))
                            .any(|p| take(&p.name, &mut spare))
                }),
            }
        };
        if elsewhere {
            picked.release_at = Some(picked.release_at.map_or(released, |r| r.min(released)));
        } else {
            picked.ids.push(c.id);
        }
    }
    picked
}

fn now_micros() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as i64
}

pub struct Claims {
    pub queue: Queue,
    pub me: String,
    pub version: String,
    pub lease: Duration,
    pub pool: Arc<Pool>,
    pub sessions: Arc<Sessions>,
    pub warm: watch::Receiver<BTreeMap<usize, String>>,
}

/// What the running claim loop shares with its jobs.
struct Shared {
    queue: Queue,
    me: String,
    version: String,
    lease: Duration,
    pool: Arc<Pool>,
    sessions: Arc<Sessions>,
    running: AtomicUsize,
    finished: Notify,
    warm: watch::Receiver<BTreeMap<usize, String>>,
    /// The warm header keys last written to the registry.
    published: Mutex<Vec<String>>,
}

impl Shared {
    fn warm_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.warm.borrow().values().cloned().collect();
        keys.sort();
        keys.dedup();
        keys
    }

    /// Writes the warm header keys to the registry if they changed.
    async fn publish_warm(&self) {
        let mut published = self.published.lock().await;
        let keys = self.warm_keys();
        if *published == keys {
            return;
        }
        match self.queue.set_warm(&self.me, &keys).await {
            Ok(()) => *published = keys,
            Err(e) => tracing::warn!("publishing warm headers failed: {e}"),
        }
    }

    /// Runs one claimed job and writes its result.
    async fn run_one(self: Arc<Self>, claimed: Claimed) {
        let result = match serde_json::from_value::<Job>(claimed.job) {
            Ok(job) => run_job(&self.pool, &self.sessions, job).await,
            Err(e) => {
                tracing::warn!(id = claimed.id, "a job did not decode: {e}");
                JobResult::Failed {
                    error: format!(
                        "the worker could not read the job, which may come from a server of \
                         another release: {e}"
                    ),
                }
            }
        };
        // A header the job warmed is published first, so the client's next
        // request can find this worker.
        self.publish_warm().await;
        let result = serde_json::to_value(&result).unwrap_or_else(
            |e| serde_json::json!({"kind": "failed", "error": format!("encoding the result: {e}")}),
        );
        let mut backoff = Duration::from_millis(100);
        for attempt in 1.. {
            match self.queue.complete(&self.me, claimed.id, &result).await {
                Ok(true) => break,
                Ok(false) => {
                    tracing::warn!(id = claimed.id, "the job's claim lapsed; result dropped");
                    break;
                }
                Err(e) if attempt < 6 => {
                    tracing::warn!(id = claimed.id, "writing the result failed: {e}");
                    tokio::time::sleep(backoff).await;
                    backoff *= 2;
                }
                Err(e) => {
                    tracing::error!(id = claimed.id, "the result is lost: {e}");
                    break;
                }
            }
        }
        self.running.fetch_sub(1, Ordering::AcqRel);
        self.finished.notify_one();
    }
}

impl Claims {
    /// Registers this worker, then claims and runs jobs until `drain` turns
    /// true. Then it takes no more, waits up to `drain_timeout` for those it
    /// runs, and unregisters.
    pub async fn run(
        self,
        drain: watch::Receiver<bool>,
        drain_timeout: Duration,
    ) -> anyhow::Result<()> {
        let shared = Arc::new(Shared {
            queue: self.queue,
            me: self.me,
            version: self.version,
            lease: self.lease,
            pool: self.pool,
            sessions: self.sessions,
            running: AtomicUsize::new(0),
            finished: Notify::new(),
            warm: self.warm,
            published: Mutex::new(Vec::new()),
        });
        shared.serve(drain, drain_timeout).await
    }
}

impl Shared {
    async fn serve(
        self: Arc<Self>,
        mut drain: watch::Receiver<bool>,
        drain_timeout: Duration,
    ) -> anyhow::Result<()> {
        let slots = self.pool.slots();
        let mut backoff = Duration::from_millis(100);
        let since = loop {
            match self
                .queue
                .register(&self.me, &self.version, slots as u32)
                .await
            {
                Ok(since) => break since,
                Err(e) => tracing::warn!("registering with the job queue failed: {e}"),
            }
            tokio::select! {
                _ = tokio::time::sleep(backoff) => {}
                _ = drain.wait_for(|d| *d) => return Ok(()),
            }
            backoff = (backoff * 2).min(Duration::from_secs(5));
        };
        match self
            .queue
            .popular_headers(&self.version, PREWARM_POPULAR)
            .await
        {
            Ok(headers) => prewarm(&self.pool, &headers),
            Err(e) => tracing::warn!("reading the headers in demand failed: {e}"),
        }
        let heartbeat = AbortOnDrop({
            let shared = self.clone();
            let mut warm = self.warm.clone();
            let lease = self.lease;
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(lease / 3);
                loop {
                    tokio::select! {
                        _ = tick.tick() => {
                            if let Err(e) = shared.queue.heartbeat(&shared.me, since, lease).await {
                                tracing::warn!("heartbeat failed: {e}");
                            }
                        }
                        Ok(()) = warm.changed() => {}
                    }
                    shared.publish_warm().await;
                }
            })
        });
        tracing::info!(queue = %self.queue.name(), "taking jobs");

        let mut notices = self.queue.db().notices().await;
        let poll_every = self.queue.db().poll_interval();
        let mut backoff = Duration::from_millis(100);
        while !*drain.borrow() {
            let free = slots.saturating_sub(self.running.load(Ordering::Acquire));
            let mut wait = poll_every;
            if free > 0 {
                match self.claim(free).await {
                    Ok(Round::Contended) => continue,
                    Ok(Round::Settled { release_at }) => {
                        backoff = Duration::from_millis(100);
                        if let Some(at) = release_at {
                            let until = (at - now_micros()).max(0) as u64;
                            wait = wait.min(Duration::from_micros(until));
                        }
                    }
                    Err(e) => {
                        tracing::warn!("claiming jobs failed: {e}");
                        wait = backoff;
                        backoff = (backoff * 2).min(Duration::from_secs(5));
                    }
                }
            }
            tokio::select! {
                () = more_work(&mut notices, self.queue.name()) => {}
                () = self.finished.notified() => {}
                () = tokio::time::sleep(wait) => {}
                Ok(()) = drain.changed() => {}
            }
        }

        let running = self.running.load(Ordering::Acquire);
        tracing::info!(running, "draining");
        if let Err(e) = self.queue.set_draining(&self.me).await {
            tracing::warn!("marking this worker draining failed: {e}");
        }
        let drained = tokio::time::timeout(drain_timeout, async {
            while self.running.load(Ordering::Acquire) > 0 {
                self.finished.notified().await;
            }
        })
        .await;
        if drained.is_err() {
            let running = self.running.load(Ordering::Acquire);
            tracing::warn!(running, "drain timed out with jobs unanswered");
        }
        drop(heartbeat);
        if let Err(e) = self.queue.unregister(&self.me).await {
            tracing::warn!("unregistering failed: {e}");
        }
        tracing::info!("drained");
        Ok(())
    }

    /// One round of reading the open jobs and claiming what [`pick`] picks.
    /// The registry is read afresh: a worker that just warmed a header has
    /// published it, and the job that follows should find it.
    async fn claim(self: &Arc<Self>, free: usize) -> anyhow::Result<Round> {
        let candidates = self.queue.candidates(&self.version, CANDIDATES).await?;
        if candidates.is_empty() {
            return Ok(Round::Settled { release_at: None });
        }
        let peers = self.queue.registry(&self.version, LIVE).await?;
        let warm: HashSet<String> = self.warm_keys().into_iter().collect();
        let picked = pick(&self.me, free, &warm, &candidates, &peers, now_micros());
        let claimed = self.queue.claim(&self.me, &picked.ids, self.lease).await?;
        let lost = claimed.len() < picked.ids.len();
        for c in claimed {
            self.running.fetch_add(1, Ordering::AcqRel);
            tokio::spawn(self.clone().run_one(c));
        }
        Ok(if lost {
            Round::Contended
        } else {
            Round::Settled {
                release_at: picked.release_at,
            }
        })
    }
}

/// Stops a task when its owner goes away.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

enum Round {
    /// Other workers claimed some of what this one picked: look again.
    Contended,
    Settled {
        release_at: Option<i64>,
    },
}

/// Returns once jobs may have been queued on `queue`.
async fn more_work(notices: &mut tokio::sync::broadcast::Receiver<Notice>, queue: &str) {
    loop {
        match notices.recv().await {
            Ok(Notice::Queued(q)) if q == queue => return,
            Ok(_) => {}
            Err(RecvError::Lagged(_)) => return,
            Err(RecvError::Closed) => std::future::pending().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_000_000_000;

    fn job(id: i64, key: &str, affinity: Option<&str>, age: Duration) -> Candidate {
        Candidate {
            id,
            priority: 1,
            header_key: Some(key.into()),
            affinity: affinity.map(String::from),
            enqueued_micros: NOW - age.as_micros() as i64,
        }
    }

    fn peer(name: &str, slots: u32, busy: u32, warm: &[&str]) -> Peer {
        Peer {
            name: name.into(),
            slots,
            busy,
            warm: warm.iter().map(|k| k.to_string()).collect(),
            draining: false,
        }
    }

    fn keys(warm: &[&str]) -> HashSet<String> {
        warm.iter().map(|k| k.to_string()).collect()
    }

    const FRESH: Duration = Duration::ZERO;

    #[test]
    fn takes_the_oldest_jobs_up_to_its_free_slots() {
        let jobs = [
            job(1, "a", None, FRESH),
            job(2, "a", None, FRESH),
            job(3, "a", None, FRESH),
        ];
        let picked = pick("me", 2, &keys(&["a"]), &jobs, &[], NOW);
        assert_eq!(picked.ids, [1, 2]);
        assert_eq!(picked.release_at, None);
    }

    #[test]
    fn leaves_a_job_to_a_free_worker_with_its_header_warm() {
        let jobs = [job(1, "h", None, FRESH), job(2, "cold", None, FRESH)];
        let peers = [peer("w", 1, 0, &["h"])];
        let picked = pick("me", 2, &keys(&[]), &jobs, &peers, NOW);
        assert_eq!(picked.ids, [2]);
        assert_eq!(picked.release_at, Some(NOW + PREFER.as_micros() as i64));

        // Unless this worker has it warm too, the other one is busy, or the
        // job has waited long enough.
        assert_eq!(pick("me", 2, &keys(&["h"]), &jobs, &peers, NOW).ids, [1, 2]);
        let busy = [peer("w", 1, 1, &["h"])];
        assert_eq!(pick("me", 2, &keys(&[]), &jobs, &busy, NOW).ids, [1, 2]);
        let old = [job(1, "h", None, PREFER)];
        assert_eq!(pick("me", 2, &keys(&[]), &old, &peers, NOW).ids, [1]);
    }

    #[test]
    fn a_warm_worker_gets_no_more_jobs_than_free_slots() {
        let jobs = [job(1, "h", None, FRESH), job(2, "h", None, FRESH)];
        let peers = [peer("w", 2, 1, &["h"])];
        assert_eq!(pick("me", 2, &keys(&[]), &jobs, &peers, NOW).ids, [2]);
    }

    #[test]
    fn the_holder_of_a_state_gets_first_refusal() {
        let jobs = [job(1, "h", Some("w"), FRESH)];
        let peers = [peer("w", 1, 0, &[]), peer("me", 1, 0, &["h"])];
        // Even over a worker with the header warm.
        assert!(
            pick("me", 1, &keys(&["h"]), &jobs, &peers, NOW)
                .ids
                .is_empty()
        );
        assert_eq!(pick("w", 1, &keys(&[]), &jobs, &peers, NOW).ids, [1]);
        // A busy holder doesn't get it, and a lost one isn't in the registry.
        let busy = [peer("w", 1, 1, &[])];
        assert_eq!(pick("me", 1, &keys(&[]), &jobs, &busy, NOW).ids, [1]);
        assert_eq!(pick("me", 1, &keys(&[]), &jobs, &[], NOW).ids, [1]);
    }

    #[test]
    fn draining_workers_are_no_home() {
        let jobs = [job(1, "h", None, FRESH)];
        let mut w = peer("w", 1, 0, &["h"]);
        w.draining = true;
        assert_eq!(pick("me", 1, &keys(&[]), &jobs, &[w], NOW).ids, [1]);
    }
}
