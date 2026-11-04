//! The job queue between the servers and the workers, and the registry of
//! the workers taking jobs from it.
//!
//! A server [enqueues](Queue::enqueue) a job and waits for its
//! [`Notice::Done`](crate::db::Notice), or polls, to [take](Queue::poll) the
//! result. A worker reads the open [candidates](Queue::candidates) and the
//! [registry](Queue::registry), [claims](Queue::claim) what it picks, renews
//! its claims' leases with each [heartbeat](Queue::heartbeat), and
//! [completes](Queue::complete) each job. A claim whose lease lapses, because
//! its worker died, is claimed again while the job has attempts left.
//!
//! Every statement is a single conditional write, which either backend runs
//! atomically: two workers claiming one job can't both win. Times are
//! microseconds since the Unix epoch on each process's clock, as for the
//! catalog's leases.

use std::time::Duration;

use serde_json::Value;

use crate::backend::dispatch;
#[cfg(test)]
use crate::db::Notice;
use crate::db::{Db, DbError};

/// Results nobody takes, e.g. because their server died, are swept after
/// this long.
pub const RESULT_RETENTION: Duration = Duration::from_secs(600);

/// Jobs nobody can claim any more, and workers that stopped heartbeating,
/// are swept after this long.
pub const STALE_RETENTION: Duration = Duration::from_secs(3600);

/// A job queue: the jobs and workers under one name, so that deployments
/// sharing a database keep theirs apart.
#[derive(Clone)]
pub struct Queue {
    db: Db,
    name: String,
}

#[derive(Debug, Clone)]
pub struct NewJob {
    /// The pool version that may run it; `None` for any.
    pub version: Option<String>,
    /// 0 is interactive, 1 batch: lower goes first.
    pub priority: i16,
    pub header_key: Option<String>,
    /// The header's text, which new workers may prewarm.
    pub header: Option<String>,
    /// The proof state the job continues, by version and hash. The worker
    /// that stored it is preferred.
    pub state: Option<(String, String)>,
    pub job: Value,
    /// How long it may wait for a worker, from now or from a lapsed claim.
    pub max_wait: Duration,
    /// How many workers may claim it before it is lost.
    pub max_attempts: u32,
}

/// A finished job, taken out of the queue.
#[derive(Debug, Clone)]
pub struct Done {
    pub result: Value,
    pub worker: String,
    pub attempts: u32,
    /// From being enqueued to its last claim.
    pub waited: Duration,
    /// Causes of Lean processes its worker replaced, since its last result.
    pub restarts: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum Poll {
    /// Taken out of the queue.
    Done(Done),
    /// Not done. If no worker claims it by `give_up_at` (microseconds), it
    /// is given up; `None` while a worker runs it.
    Waiting { give_up_at: Option<i64> },
    /// Its last claim lapsed with no attempts or time left: taken out.
    Lost { attempts: u32 },
    /// Not in the queue: given up or swept.
    Gone,
}

/// An open job, as a worker picks between them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub id: i64,
    pub priority: i16,
    pub header_key: Option<String>,
    /// The worker that stored the proof state the job continues.
    pub affinity: Option<String>,
    pub enqueued_micros: i64,
}

/// A live worker, as another picks jobs around it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    pub name: String,
    pub slots: u32,
    /// Jobs it holds live claims on.
    pub busy: u32,
    pub warm: Vec<String>,
    pub draining: bool,
}

#[derive(Debug, Clone)]
pub struct Claimed {
    pub id: i64,
    pub job: Value,
}

/// The queue's state, for metrics.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Counts {
    /// Unclaimed jobs by priority: interactive, then batch.
    pub queued: [u64; 2],
    /// Jobs with a live claim.
    pub running: u64,
    /// Live workers, and their slots.
    pub workers: u64,
    pub slots: u64,
}

impl Queue {
    pub fn new(db: Db, name: impl Into<String>) -> Queue {
        Queue {
            db,
            name: name.into(),
        }
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Adds `job` and tells the workers; returns its id.
    pub async fn enqueue(&self, job: &NewJob) -> Result<i64, DbError> {
        dispatch!(&self.db, enqueue(&self.name, job))
    }

    /// Where job `id` stands, taking it out of the queue if it is done or
    /// lost.
    pub async fn poll(&self, id: i64) -> Result<Poll, DbError> {
        dispatch!(&self.db, poll(id))
    }

    /// Takes job `id` out of the queue unless a worker holds a live claim on
    /// it or it is done. Returns how many workers had claimed it, or `None`
    /// if it wasn't taken out.
    pub async fn give_up(&self, id: i64) -> Result<Option<u32>, DbError> {
        dispatch!(&self.db, give_up(id))
    }

    /// Workers count as live if seen within `stale`.
    pub async fn counts(&self, stale: Duration) -> Result<Counts, DbError> {
        dispatch!(&self.db, counts(&self.name, stale))
    }

    /// Deletes results older than [`RESULT_RETENTION`], and jobs and workers
    /// nobody has touched for [`STALE_RETENTION`]; returns how many rows.
    pub async fn sweep(&self) -> Result<usize, DbError> {
        dispatch!(&self.db, sweep_jobs(&self.name))
    }

    /// Adds or resets `worker`'s row, and lets its claims from before, of a
    /// process under the same name, lapse at once. Returns when, which
    /// [`Queue::heartbeat`] takes.
    pub async fn register(&self, worker: &str, version: &str, slots: u32) -> Result<i64, DbError> {
        dispatch!(&self.db, register(&self.name, worker, version, slots))
    }

    /// Up to `limit` jobs that a worker of `version` could claim, the
    /// interactive ones first, then oldest first.
    pub async fn candidates(&self, version: &str, limit: usize) -> Result<Vec<Candidate>, DbError> {
        dispatch!(&self.db, candidates(&self.name, version, limit))
    }

    /// The workers of `version` seen within `stale`.
    pub async fn registry(&self, version: &str, stale: Duration) -> Result<Vec<Peer>, DbError> {
        dispatch!(&self.db, registry(&self.name, version, stale))
    }

    /// Claims those of `ids` still claimable for `worker`, for `lease`;
    /// returns the ones it got.
    pub async fn claim(
        &self,
        worker: &str,
        ids: &[i64],
        lease: Duration,
    ) -> Result<Vec<Claimed>, DbError> {
        dispatch!(&self.db, claim(&self.name, worker, ids, lease))
    }

    /// Records job `id`'s result if `worker` still holds its claim, and tells
    /// its server; returns whether it did.
    pub async fn complete(
        &self,
        worker: &str,
        id: i64,
        result: &Value,
        restarts: &[String],
    ) -> Result<bool, DbError> {
        dispatch!(&self.db, complete(worker, id, result, restarts))
    }

    /// Renews the leases of the claims `worker` made since it registered at
    /// `since`, and marks it seen.
    pub async fn heartbeat(
        &self,
        worker: &str,
        since: i64,
        lease: Duration,
    ) -> Result<(), DbError> {
        dispatch!(&self.db, heartbeat(&self.name, worker, since, lease))
    }

    pub async fn set_warm(&self, worker: &str, warm: &[String]) -> Result<(), DbError> {
        dispatch!(&self.db, set_warm(&self.name, worker, warm))
    }

    /// Tells the other workers that `worker` takes no more jobs.
    pub async fn set_draining(&self, worker: &str) -> Result<(), DbError> {
        dispatch!(&self.db, set_draining(&self.name, worker))
    }

    pub async fn unregister(&self, worker: &str) -> Result<(), DbError> {
        dispatch!(&self.db, unregister(&self.name, worker))
    }

    /// The headers most open jobs of `version` have, most first.
    pub async fn popular_headers(&self, version: &str, n: usize) -> Result<Vec<String>, DbError> {
        dispatch!(&self.db, popular_headers(&self.name, version, n))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::json;

    use super::*;
    use crate::records::Records;

    /// A queue of its own on in-memory SQLite, and one on the PostgreSQL at
    /// `PISA_TEST_DATABASE_URL` if that is set.
    async fn queues() -> Vec<Queue> {
        let mut dbs = vec![Db::memory().await];
        match std::env::var("PISA_TEST_DATABASE_URL") {
            Ok(url) => dbs.push(Db::connect(Some(&url)).await.unwrap()),
            Err(_) => eprintln!("PISA_TEST_DATABASE_URL unset: SQLite only"),
        }
        dbs.into_iter()
            .map(|db| Queue::new(db, uuid::Uuid::new_v4().simple().to_string()))
            .collect()
    }

    fn job(version: Option<&str>, priority: i16) -> NewJob {
        NewJob {
            version: version.map(String::from),
            priority,
            header_key: Some("k".into()),
            header: Some("import Mathlib".into()),
            state: None,
            job: json!({"kind": "check"}),
            max_wait: Duration::from_secs(60),
            max_attempts: 3,
        }
    }

    const LEASE: Duration = Duration::from_secs(60);

    async fn ids(q: &Queue, version: &str) -> Vec<i64> {
        let found = q.candidates(version, 100).await.unwrap();
        found.into_iter().map(|c| c.id).collect()
    }

    #[tokio::test]
    async fn a_job_goes_from_enqueued_to_its_result() {
        for q in queues().await {
            let id = q.enqueue(&job(Some("v"), 1)).await.unwrap();
            assert!(matches!(
                q.poll(id).await.unwrap(),
                Poll::Waiting {
                    give_up_at: Some(_)
                }
            ));
            assert_eq!(ids(&q, "v").await, [id]);
            let claimed = q.claim("w", &[id], LEASE).await.unwrap();
            assert_eq!(claimed.len(), 1);
            assert_eq!(claimed[0].job, json!({"kind": "check"}));
            assert!(ids(&q, "v").await.is_empty());
            assert!(matches!(
                q.poll(id).await.unwrap(),
                Poll::Waiting { give_up_at: None }
            ));

            let restarts = vec!["timeout".to_string()];
            assert!(
                q.complete("w", id, &json!({"ok": 1}), &restarts)
                    .await
                    .unwrap()
            );
            let Poll::Done(done) = q.poll(id).await.unwrap() else {
                panic!("not done");
            };
            assert_eq!(done.result, json!({"ok": 1}));
            assert_eq!((done.worker.as_str(), done.attempts), ("w", 1));
            assert_eq!(done.restarts, restarts);
            assert!(matches!(q.poll(id).await.unwrap(), Poll::Gone));
        }
    }

    #[tokio::test]
    async fn candidates_follow_the_queue_the_version_and_the_priority() {
        for q in queues().await {
            let batch = q.enqueue(&job(Some("v"), 1)).await.unwrap();
            let other = q.enqueue(&job(Some("w"), 0)).await.unwrap();
            let any = q.enqueue(&job(None, 1)).await.unwrap();
            let interactive = q.enqueue(&job(Some("v"), 0)).await.unwrap();
            assert_eq!(ids(&q, "v").await, [interactive, batch, any]);
            assert_eq!(ids(&q, "w").await, [other, any]);
            let elsewhere = Queue::new(q.db().clone(), format!("{}-2", q.name()));
            assert!(ids(&elsewhere, "v").await.is_empty());
            assert!(
                elsewhere
                    .claim("x", &[batch], LEASE)
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn racing_claims_give_each_job_one_worker() {
        for q in queues().await {
            let mut all = Vec::new();
            for _ in 0..4 {
                all.push(q.enqueue(&job(Some("v"), 1)).await.unwrap());
            }
            let tasks: Vec<_> = (0..8)
                .map(|i| {
                    let (q, all) = (q.clone(), all.clone());
                    tokio::spawn(async move { q.claim(&format!("w{i}"), &all, LEASE).await })
                })
                .collect();
            let mut won = Vec::new();
            for task in tasks {
                won.extend(task.await.unwrap().unwrap().into_iter().map(|c| c.id));
            }
            won.sort();
            assert_eq!(won, all);
        }
    }

    #[tokio::test]
    async fn a_lapsed_claim_is_claimed_again_until_it_is_lost() {
        for q in queues().await {
            let mut new = job(Some("v"), 1);
            new.max_attempts = 2;
            let id = q.enqueue(&new).await.unwrap();
            let short = Duration::from_millis(1);
            assert_eq!(q.claim("a", &[id], short).await.unwrap().len(), 1);
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert!(matches!(
                q.poll(id).await.unwrap(),
                Poll::Waiting {
                    give_up_at: Some(_)
                }
            ));
            assert_eq!(ids(&q, "v").await, [id]);
            assert_eq!(q.claim("b", &[id], short).await.unwrap().len(), 1);
            // `a` was too slow: its result goes nowhere.
            assert!(!q.complete("a", id, &json!(1), &[]).await.unwrap());
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert!(ids(&q, "v").await.is_empty());
            assert!(q.claim("c", &[id], LEASE).await.unwrap().is_empty());
            assert!(matches!(
                q.poll(id).await.unwrap(),
                Poll::Lost { attempts: 2 }
            ));
            assert!(matches!(q.poll(id).await.unwrap(), Poll::Gone));
        }
    }

    #[tokio::test]
    async fn an_unclaimed_job_waits_at_most_its_max_wait() {
        for q in queues().await {
            let mut new = job(Some("v"), 1);
            new.max_wait = Duration::from_millis(10);
            let id = q.enqueue(&new).await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert!(ids(&q, "v").await.is_empty());
            assert!(q.claim("w", &[id], LEASE).await.unwrap().is_empty());
            assert_eq!(q.give_up(id).await.unwrap(), Some(0));
            assert!(matches!(q.poll(id).await.unwrap(), Poll::Gone));

            // A running job isn't given up.
            let id = q.enqueue(&job(Some("v"), 1)).await.unwrap();
            q.claim("w", &[id], LEASE).await.unwrap();
            assert_eq!(q.give_up(id).await.unwrap(), None);
            assert!(q.complete("w", id, &json!(1), &[]).await.unwrap());
        }
    }

    #[tokio::test]
    async fn the_worker_that_stored_a_state_is_its_jobs_affinity() {
        for q in queues().await {
            let records = Records::new(q.db().clone(), "v".into());
            let hash = uuid::Uuid::new_v4().simple().to_string();
            records.put_state(&hash, 0, &json!({}), "w1").await.unwrap();
            let mut new = job(Some("v"), 0);
            new.state = Some(("v".into(), hash));
            q.enqueue(&new).await.unwrap();
            let mut unknown = job(Some("v"), 0);
            unknown.state = Some(("v".into(), "nope".into()));
            q.enqueue(&unknown).await.unwrap();
            let affinity: Vec<_> = q
                .candidates("v", 10)
                .await
                .unwrap()
                .into_iter()
                .map(|c| c.affinity)
                .collect();
            assert_eq!(affinity, [Some("w1".to_string()), None]);
        }
    }

    #[tokio::test]
    async fn the_registry_has_live_workers_and_their_claims() {
        for q in queues().await {
            q.register("w1", "v", 2).await.unwrap();
            q.register("w2", "v", 1).await.unwrap();
            q.register("x", "other", 4).await.unwrap();
            let id = q.enqueue(&job(Some("v"), 0)).await.unwrap();
            q.enqueue(&job(Some("v"), 1)).await.unwrap();
            q.claim("w1", &[id], LEASE).await.unwrap();
            q.set_warm("w2", &["k".into()]).await.unwrap();
            q.set_draining("w2").await.unwrap();
            let mut peers = q.registry("v", LEASE).await.unwrap();
            peers.sort_by(|a, b| a.name.cmp(&b.name));
            assert_eq!(
                peers,
                [
                    Peer {
                        name: "w1".into(),
                        slots: 2,
                        busy: 1,
                        warm: Vec::new(),
                        draining: false
                    },
                    Peer {
                        name: "w2".into(),
                        slots: 1,
                        busy: 0,
                        warm: vec!["k".into()],
                        draining: true
                    },
                ]
            );
            assert_eq!(
                q.counts(LEASE).await.unwrap(),
                Counts {
                    queued: [0, 1],
                    running: 1,
                    workers: 3,
                    slots: 7
                }
            );
            q.unregister("w2").await.unwrap();
            assert_eq!(q.registry("v", LEASE).await.unwrap().len(), 1);
            assert!(q.registry("v", Duration::ZERO).await.unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn heartbeats_renew_only_this_registrations_claims() {
        for q in queues().await {
            let since = q.register("w", "v", 1).await.unwrap();
            let id = q.enqueue(&job(Some("v"), 1)).await.unwrap();
            q.claim("w", &[id], Duration::from_millis(1)).await.unwrap();
            q.heartbeat("w", since, LEASE).await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert!(ids(&q, "v").await.is_empty(), "renewed");

            // A new process under the same name doesn't run the old one's
            // jobs: they lapse, and its heartbeats don't renew them.
            let since = q.register("w", "v", 1).await.unwrap();
            q.heartbeat("w", since, LEASE).await.unwrap();
            assert_eq!(ids(&q, "v").await, [id]);
        }
    }

    #[tokio::test]
    async fn popular_headers_come_from_open_jobs() {
        for q in queues().await {
            let mut lean = job(Some("v"), 1);
            lean.header = Some("import Lean".into());
            q.enqueue(&lean).await.unwrap();
            for _ in 0..2 {
                q.enqueue(&job(Some("v"), 1)).await.unwrap();
            }
            let mut none = job(Some("v"), 1);
            none.header = None;
            q.enqueue(&none).await.unwrap();
            q.enqueue(&job(Some("other"), 1)).await.unwrap();
            assert_eq!(
                q.popular_headers("v", 5).await.unwrap(),
                ["import Mathlib", "import Lean"]
            );
            assert_eq!(q.popular_headers("v", 1).await.unwrap(), ["import Mathlib"]);
        }
    }

    #[tokio::test]
    async fn notices_tell_of_new_and_finished_jobs() {
        for q in queues().await {
            let mut notices = q.db().notices().await;
            let id = q.enqueue(&job(Some("v"), 1)).await.unwrap();
            let mut heard = BTreeSet::new();
            let mut next = async || {
                tokio::time::timeout(Duration::from_secs(5), notices.recv())
                    .await
                    .expect("a notice")
                    .unwrap()
            };
            heard.insert(format!("{:?}", next().await));
            q.claim("w", &[id], LEASE).await.unwrap();
            q.complete("w", id, &json!(1), &[]).await.unwrap();
            // Other tests' queues on a shared PostgreSQL notify too.
            while !heard.contains(&format!("{:?}", Notice::Done(id))) {
                heard.insert(format!("{:?}", next().await));
            }
            assert!(heard.contains(&format!("{:?}", Notice::Queued(q.name().into()))));
        }
    }
}
