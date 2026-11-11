//! Takes jobs from the database's job queue ([`pisa_db::queue`]): claims
//! what this worker should run, runs it on the pool, and writes the result.
//!
//! Every free worker of a version looks at the same oldest jobs, and each
//! leaves a job alone for a moment ([`PREFER`]) when another live worker with
//! a free slot is a better home for it: when this worker hasn't imported its
//! header, one that has. Otherwise the first to claim a job runs it.
//!
//! Jobs on open documents go to workers with Beam slots ([`pick_documents`]),
//! one at a time per document, and wait for the worker holding the document
//! while it is live. One that no live worker holds, as an open, waits a
//! moment ([`PLACE`]) for a worker with fewer documents open per Beam slot.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pisa_db::db::Notice;
use pisa_db::queue::{Candidate, Claimed, LIVE, Peer, Queue};
use pisa_protocol::wire::{Job, JobResult};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{Mutex, Notify, watch};

use crate::documents::Documents;
use crate::pool::Pool;
use crate::sessions::Sessions;
use crate::{prewarm, run_job};

/// How long another worker gets first refusal of a job it is a better home
/// for. Long enough for it to hear of the job and claim it; short enough
/// that a stale registry costs little.
pub const PREFER: Duration = Duration::from_millis(500);

/// How long a job on a document that no live worker holds is left to a Beam
/// worker with fewer documents open per Beam slot, which opens it there.
pub const PLACE: Duration = Duration::from_secs(2);

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
        let elsewhere = now < released
            && c.header_key.as_ref().is_some_and(|key| {
                !warm.contains(key)
                    && peers
                        .iter()
                        .filter(|p| p.warm.contains(key))
                        .any(|p| take(&p.name, &mut spare))
            });
        if elsewhere {
            picked.release_at = Some(picked.release_at.map_or(released, |r| r.min(released)));
        } else {
            picked.ids.push(c.id);
        }
    }
    picked
}

/// Picks up to `free` of the jobs on documents `candidates` for worker `me` to
/// claim: one per document, none on a document a job of `running` here
/// works on, and none on a document that another live worker serving
/// documents holds, which is the only one to run its jobs. A job on a
/// document no live worker holds is left for [`PLACE`] to a worker with
/// fewer documents open per Beam slot than this one, as the registry has
/// them.
pub fn pick_documents(
    me: &str,
    free: usize,
    candidates: &[Candidate],
    peers: &[Peer],
    running: &HashSet<String>,
    now: i64,
) -> Picked {
    let serving: Vec<&Peer> = peers
        .iter()
        .filter(|p| p.name != me && !p.draining && p.beam_slots > 0)
        .collect();
    // Load as documents per Beam slot, compared without dividing.
    let (open, slots) = peers
        .iter()
        .find(|p| p.name == me)
        .map_or((0, 1), |p| (p.documents_open, p.beam_slots.max(1)));
    let lighter = serving.iter().any(|p| {
        u64::from(p.documents_open) * u64::from(slots) < u64::from(open) * u64::from(p.beam_slots)
    });
    let mut seen = HashSet::new();
    let mut picked = Picked::default();
    for c in candidates {
        if picked.ids.len() >= free {
            break;
        }
        let Some(document) = c.document.as_deref() else {
            continue;
        };
        if running.contains(document) || !seen.insert(document) {
            continue;
        }
        if c.affinity
            .as_deref()
            .is_some_and(|h| serving.iter().any(|p| p.name == h))
        {
            continue;
        }
        let released = c.enqueued_micros + PLACE.as_micros() as i64;
        if lighter && now < released {
            picked.release_at = Some(picked.release_at.map_or(released, |r| r.min(released)));
            continue;
        }
        picked.ids.push(c.id);
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
    pub documents: Option<Arc<Documents>>,
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
    documents: Option<Arc<Documents>>,
    /// Jobs running, on documents too.
    running: AtomicUsize,
    /// The documents jobs running here work on.
    on_documents: std::sync::Mutex<HashSet<String>>,
    finished: Notify,
    warm: watch::Receiver<BTreeMap<usize, String>>,
    /// The warm header keys last written to the registry.
    published: Mutex<Vec<String>>,
    /// The count of open documents last written to the registry.
    published_open: Mutex<Option<u32>>,
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

    /// Writes the documents open here to the registry if their count
    /// changed, for [`pick_documents`] elsewhere.
    async fn publish_open(&self) {
        let Some(documents) = &self.documents else {
            return;
        };
        let mut published = self.published_open.lock().await;
        let open = documents.open_count() as u32;
        if *published == Some(open) {
            return;
        }
        match self.queue.set_documents_open(&self.me, open).await {
            Ok(()) => *published = Some(open),
            Err(e) => tracing::warn!("publishing the documents open failed: {e}"),
        }
    }

    /// Free REPL slots, and room for jobs on documents.
    fn room(&self) -> (usize, usize) {
        let on_documents = self.on_documents.lock().unwrap().len();
        let running = self.running.load(Ordering::Acquire);
        let capacity = self.documents.as_ref().map_or(0, |d| d.capacity());
        (
            self.pool
                .slots()
                .saturating_sub(running.saturating_sub(on_documents)),
            capacity.saturating_sub(on_documents),
        )
    }

    /// Runs one claimed job, on `document` if it is a job on one, and writes
    /// its result.
    async fn run_one(self: Arc<Self>, claimed: Claimed, document: Option<String>) {
        let documents = self.documents.as_deref();
        let result = match serde_json::from_value::<Job>(claimed.job) {
            Ok(job) => run_job(&self.pool, &self.sessions, documents, job).await,
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
        // A header the job warmed, or a document it opened, is published
        // first, so that the client's next request can find this worker.
        self.publish_warm().await;
        if document.is_some() {
            self.publish_open().await;
        }
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
        if let Some(document) = &document {
            self.on_documents.lock().unwrap().remove(document);
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
            documents: self.documents,
            running: AtomicUsize::new(0),
            on_documents: std::sync::Mutex::default(),
            finished: Notify::new(),
            warm: self.warm,
            published: Mutex::new(Vec::new()),
            published_open: Mutex::new(None),
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
        let beam_slots = self.documents.as_ref().map_or(0, |d| d.beam_slots());
        let mut backoff = Duration::from_millis(100);
        let since = loop {
            match self
                .queue
                .register(&self.me, &self.version, slots as u32, beam_slots as u32)
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
                    shared.publish_open().await;
                }
            })
        });
        tracing::info!(queue = %self.queue.name(), "taking jobs");

        let mut notices = self.queue.db().notices().await;
        let poll_every = self.queue.db().poll_interval();
        let mut backoff = Duration::from_millis(100);
        while !*drain.borrow() {
            let (free, free_documents) = self.room();
            let mut wait = poll_every;
            if free > 0 || free_documents > 0 {
                match self.claim(free, free_documents).await {
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

    /// One round of reading the open jobs and claiming what [`pick`] and
    /// [`pick_documents`] pick, for `free` REPL slots and `free_documents`
    /// jobs on documents. The registry is read afresh: a worker that just
    /// warmed a header has published it, and the job that follows should
    /// find it.
    async fn claim(self: &Arc<Self>, free: usize, free_documents: usize) -> anyhow::Result<Round> {
        let candidates = match free {
            0 => Vec::new(),
            _ => self.queue.candidates(&self.version, CANDIDATES).await?,
        };
        let on_documents = match free_documents {
            0 => Vec::new(),
            _ => {
                self.queue
                    .document_candidates(&self.version, CANDIDATES)
                    .await?
            }
        };
        if candidates.is_empty() && on_documents.is_empty() {
            return Ok(Round::Settled { release_at: None });
        }
        let peers = self.queue.registry(&self.version, LIVE).await?;
        let warm: HashSet<String> = self.warm_keys().into_iter().collect();
        let now = now_micros();
        let picked = pick(&self.me, free, &warm, &candidates, &peers, now);
        let running = self.on_documents.lock().unwrap().clone();
        let placed = pick_documents(
            &self.me,
            free_documents,
            &on_documents,
            &peers,
            &running,
            now,
        );
        let mut ids = picked.ids.clone();
        ids.extend(&placed.ids);
        let release_at = match (picked.release_at, placed.release_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let claimed = self.queue.claim(&self.me, &ids, self.lease).await?;
        let lost = claimed.len() < ids.len();
        for c in claimed {
            let document = on_documents
                .iter()
                .find(|d| d.id == c.id)
                .and_then(|d| d.document.clone());
            if let Some(document) = &document {
                self.on_documents.lock().unwrap().insert(document.clone());
            }
            self.running.fetch_add(1, Ordering::AcqRel);
            tokio::spawn(self.clone().run_one(c, document));
        }
        Ok(if lost {
            Round::Contended
        } else {
            Round::Settled { release_at }
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
            document: None,
        }
    }

    fn peer(name: &str, slots: u32, busy: u32, warm: &[&str]) -> Peer {
        Peer {
            name: name.into(),
            slots,
            busy,
            warm: warm.iter().map(|k| k.to_string()).collect(),
            draining: false,
            beam_slots: 0,
            documents_open: 0,
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

    fn on(id: i64, document: &str, holder: Option<&str>) -> Candidate {
        Candidate {
            document: Some(document.into()),
            ..job(id, "h", holder, FRESH)
        }
    }

    fn beam(name: &str) -> Peer {
        Peer {
            beam_slots: 1,
            ..peer(name, 1, 0, &[])
        }
    }

    #[test]
    fn a_document_held_by_a_live_beam_worker_waits_for_it() {
        let none = HashSet::new();
        let jobs = [on(1, "d", Some("w")), on(2, "e", None)];
        let peers = [beam("w")];
        assert_eq!(pick_documents("me", 4, &jobs, &peers, &none, NOW).ids, [2]);
        assert_eq!(
            pick_documents("w", 4, &jobs, &peers, &none, NOW).ids,
            [1, 2]
        );
        // However long it has waited: only the holder has the document open.
        let old = [Candidate {
            enqueued_micros: NOW - 60_000_000,
            ..on(1, "d", Some("w"))
        }];
        assert!(
            pick_documents("me", 4, &old, &peers, &none, NOW)
                .ids
                .is_empty()
        );

        // A holder that is gone, draining or without Beam is no home.
        assert_eq!(pick_documents("me", 4, &jobs, &[], &none, NOW).ids, [1, 2]);
        let mut draining = beam("w");
        draining.draining = true;
        assert_eq!(
            pick_documents("me", 4, &jobs, &[draining], &none, NOW).ids,
            [1, 2]
        );
        let no_beam = [peer("w", 1, 0, &[])];
        assert_eq!(
            pick_documents("me", 4, &jobs, &no_beam, &none, NOW).ids,
            [1, 2]
        );
    }

    #[test]
    fn one_job_per_document_runs_at_a_time() {
        let jobs = [on(1, "d", None), on(2, "d", None), on(3, "e", None)];
        let none = HashSet::new();
        assert_eq!(pick_documents("me", 4, &jobs, &[], &none, NOW).ids, [1, 3]);
        let running = keys(&["d"]);
        assert_eq!(pick_documents("me", 4, &jobs, &[], &running, NOW).ids, [3]);
        assert_eq!(pick_documents("me", 1, &jobs, &[], &none, NOW).ids, [1]);
        assert!(
            pick_documents("me", 0, &jobs, &[], &none, NOW)
                .ids
                .is_empty()
        );
    }

    #[test]
    fn an_unheld_document_waits_a_moment_for_a_worker_with_fewer_open() {
        let none = HashSet::new();
        let with = |name: &str, beam_slots, documents_open| Peer {
            beam_slots,
            documents_open,
            ..beam(name)
        };
        let jobs = [on(1, "d", None), on(2, "e", Some("gone"))];
        // `me` has 2 open on 1 slot; `w` has 1 on 1.
        let peers = [with("me", 1, 2), with("w", 1, 1)];
        let picked = pick_documents("me", 4, &jobs, &peers, &none, NOW);
        assert!(picked.ids.is_empty());
        assert_eq!(picked.release_at, Some(NOW + PLACE.as_micros() as i64));
        // `w` takes them, as does `me` once they have waited.
        assert_eq!(
            pick_documents("w", 4, &jobs, &peers, &none, NOW).ids,
            [1, 2]
        );
        let later = NOW + PLACE.as_micros() as i64;
        assert_eq!(
            pick_documents("me", 4, &jobs, &peers, &none, later).ids,
            [1, 2]
        );
        // Per Beam slot: 2 open on 2 slots is lighter than 2 on 1, and as
        // light as 1 on 1, which doesn't wait.
        let peers = [with("me", 1, 2), with("w", 2, 2)];
        assert!(
            pick_documents("me", 4, &jobs, &peers, &none, NOW)
                .ids
                .is_empty()
        );
        let peers = [with("me", 1, 1), with("w", 2, 2)];
        assert_eq!(
            pick_documents("me", 4, &jobs, &peers, &none, NOW).ids,
            [1, 2]
        );
        // A draining worker is no home.
        let mut draining = with("w", 1, 0);
        draining.draining = true;
        let peers = [with("me", 1, 2), draining];
        assert_eq!(
            pick_documents("me", 4, &jobs, &peers, &none, NOW).ids,
            [1, 2]
        );
    }

    #[test]
    fn draining_workers_are_no_home() {
        let jobs = [job(1, "h", None, FRESH)];
        let mut w = peer("w", 1, 0, &["h"]);
        w.draining = true;
        assert_eq!(pick("me", 1, &keys(&[]), &jobs, &[w], NOW).ids, [1]);
    }
}
