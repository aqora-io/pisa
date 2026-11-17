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

use pisa_protocol::api::{DependencyState, ErrorKind, JobKind};
use pisa_protocol::wire::JobResult;
use serde_json::Value;

use crate::backend::dispatch;
#[cfg(test)]
use crate::db::Notice;
use crate::db::{Db, DbError};

/// Results nobody takes, e.g. because their server died, are swept after
/// this long.
pub const RESULT_RETENTION: Duration = Duration::from_secs(600);

/// An async job's outcome, which its client reads back, is kept this long
/// after it is done, but for the kinds [`answer_retention`] keeps longer.
pub const ASYNC_RETENTION: Duration = Duration::from_secs(3600);

/// The kinds of async job whose answers are kept a day: the answer to a
/// create or an accept may be the only record of what it made.
pub(crate) const DAY_KINDS: [&str; 2] = ["create_snapshot", "accept_run"];

/// How long the answer of an async job of `kind` (as `status` names it) is
/// kept after it is done.
pub fn answer_retention(kind: &str) -> Duration {
    if DAY_KINDS.contains(&kind) {
        Duration::from_secs(24 * 3600)
    } else {
        ASYNC_RETENTION
    }
}

/// What an async job was and how it ended is kept this long after its row
/// went, so that a client who asks later is told it expired.
pub const TOMBSTONE_RETENTION: Duration = Duration::from_secs(30 * 24 * 3600);

/// Jobs nobody can claim any more, and workers that stopped heartbeating,
/// are swept after this long.
pub const STALE_RETENTION: Duration = Duration::from_secs(3600);

/// A worker seen within this long is live. Workers heartbeat well within it.
pub const LIVE: Duration = Duration::from_secs(30);

/// An async job is given up this long after it was queued at the latest,
/// even if no worker that can run it was ever live.
pub const LOST_CEILING: Duration = Duration::from_secs(24 * 3600);

/// A job queue: the jobs and workers under one name, so that deployments
/// sharing a database keep theirs apart.
#[derive(Clone)]
pub struct Queue {
    db: Db,
    name: String,
}

/// What a worker tells the queue about itself as it starts.
#[derive(Debug, Clone, Default)]
pub struct Registration {
    pub worker: String,
    pub version: String,
    /// Its REPL slots.
    pub slots: u32,
    /// Its Lean Beam slots.
    pub beam_slots: u32,
    /// Whether it runs Lean without memory or process limits.
    pub unlimited: bool,
    /// The commit it was built from.
    pub revision: String,
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
    /// The open document it works on, by id within `version`. Only workers
    /// with Beam slots claim it, and the worker holding the document open is
    /// preferred.
    pub document: Option<String>,
    pub job: Value,
    /// How long it may wait for a worker, from now or from a lapsed claim.
    /// An async job counts it only while a worker that can run it is live,
    /// up to [`LOST_CEILING`], or, if none has claimed it, while no worker
    /// that can run it has a row, live or stopped within
    /// [`STALE_RETENTION`]: it is of a version no worker runs.
    pub max_wait: Duration,
    /// How many workers may claim it before it is lost.
    pub max_attempts: u32,
}

#[derive(Debug, Clone)]
pub enum Poll {
    /// Its result, taken out of the queue.
    Done(Value),
    /// Not done. If no worker claims it by `give_up_at` (microseconds), it
    /// is given up; `None` while a worker runs it.
    Waiting { give_up_at: Option<i64> },
    /// Its last claim lapsed with no attempts or time left: taken out. It
    /// was claimed `attempts` times; `exhausted` if that was all it was
    /// allowed, else no worker that could run it came back in time.
    Lost { attempts: u32, exhausted: bool },
    /// Not in the queue: given up or swept.
    Gone,
}

/// An async job, as its client reads it back by token.
#[derive(Debug, Clone)]
pub struct Status {
    pub id: i64,
    /// What it was submitted as, e.g. `check`.
    pub kind: String,
    pub stage: Stage,
}

#[derive(Debug, Clone)]
pub enum Stage {
    /// Waiting for a worker. If none claims it, it is given up at
    /// `give_up_at` (microseconds) at the earliest. Past its max wait, while
    /// no worker that can run it has been live that long, it waits on, and
    /// `give_up_at` is [`LOST_CEILING`] after it was queued; one no worker
    /// has claimed waits on only while a worker that can run it has a row,
    /// live or stopped within [`STALE_RETENTION`].
    Queued { give_up_at: i64 },
    /// A worker holds a live claim on it.
    Running,
    /// Its worker's result.
    Done(Value),
    /// Nobody could run it: no worker claimed it in time if `attempts` is
    /// 0, or its last worker was lost. It was claimed `attempts` times;
    /// `exhausted` if that was all it was allowed, else no worker that could
    /// run it came back in time.
    GaveUp { attempts: u32, exhausted: bool },
    /// Its row is gone, and a tombstone says what it was and how it ended,
    /// but never its answer. `finished_micros` is when it ended.
    Expired {
        kind: String,
        outcome: ExpiredOutcome,
        /// The status its answer had, if it was done or given up; see
        /// [`ExpiredOutcome::of`].
        http_status: Option<u16>,
        finished_micros: i64,
    },
}

/// How an expired async job ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpiredOutcome {
    /// A worker answered it.
    Done,
    /// Nobody could run it, or its worker was lost.
    Lost,
    /// Its client withdrew it.
    Cancelled,
}

impl ExpiredOutcome {
    pub(crate) fn name(self) -> &'static str {
        match self {
            ExpiredOutcome::Done => "done",
            ExpiredOutcome::Lost => "lost",
            ExpiredOutcome::Cancelled => "cancelled",
        }
    }

    pub(crate) fn parse(name: &str) -> ExpiredOutcome {
        match name {
            "done" => ExpiredOutcome::Done,
            "cancelled" => ExpiredOutcome::Cancelled,
            _ => ExpiredOutcome::Lost,
        }
    }

    /// How an async job of `kind` (as `status` names it) that ended with
    /// `result`, claimed `attempts` times, ended, and the status its answer
    /// had: one given up answered a 429 if no worker took it, else a 503. A
    /// result that doesn't read was answered as a worker that couldn't run
    /// the job is; `None` for a kind this release doesn't know.
    pub(crate) fn of(kind: &str, result: Option<&Value>, attempts: i32) -> (Self, Option<u16>) {
        let result = match result {
            Some(Value::Null) | None => {
                let given_up = match attempts {
                    0 => ErrorKind::Capacity,
                    _ => ErrorKind::WorkerLost,
                };
                return (ExpiredOutcome::Lost, Some(given_up.status()));
            }
            Some(result) => serde_json::from_value(result.clone()).unwrap_or(JobResult::Failed {
                error: String::new(),
            }),
        };
        let kind = serde_json::from_value::<JobKind>(Value::String(kind.into()));
        (
            ExpiredOutcome::Done,
            kind.ok().map(|k| result.http_status(k)),
        )
    }
}

/// An open job, as a worker picks between them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub id: i64,
    pub priority: i16,
    pub header_key: Option<String>,
    /// The worker that holds open the document it works on.
    pub affinity: Option<String>,
    pub enqueued_micros: i64,
    /// The open document it works on.
    pub document: Option<String>,
}

/// A live worker, as another picks jobs around it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    pub name: String,
    pub slots: u32,
    /// Jobs on its REPL slots it holds live claims on.
    pub busy: u32,
    pub warm: Vec<String>,
    pub draining: bool,
    /// The Lean Beam processes it runs for documents.
    pub beam_slots: u32,
    /// The documents it has open on them.
    pub documents_open: u32,
}

#[derive(Debug, Clone)]
pub struct Claimed {
    pub id: i64,
    pub job: Value,
    /// When the job was queued, in microseconds since the Unix epoch.
    pub enqueued_micros: i64,
}

/// The queue's state.
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

    /// Adds `job`, which its client reads back as `kind` by the returned
    /// token rather than waiting for it, and tells the workers; returns its
    /// id and token. Its outcome is kept for [`ASYNC_RETENTION`].
    pub async fn enqueue_async(&self, job: &NewJob, kind: &str) -> Result<(i64, String), DbError> {
        dispatch!(&self.db, enqueue_async(&self.name, job, kind))
    }

    /// Where the async job `token` stands, if it is still kept, or was, in
    /// the current scope. Reading leaves it, except that one nobody may
    /// claim any more is closed as [`Stage::GaveUp`]. `id` is 0 for
    /// [`Stage::Expired`].
    pub async fn status(&self, token: &str) -> Result<Option<Status>, DbError> {
        dispatch!(&self.db, status(&self.name, token))
    }

    /// Takes the async job `token` out of the queue unless a worker holds a
    /// live claim on it or it is done; returns its id if it did.
    pub async fn cancel(&self, token: &str) -> Result<Option<i64>, DbError> {
        dispatch!(&self.db, cancel(&self.name, token))
    }

    /// Where job `id` stands, taking it out of the queue if it is done or
    /// lost.
    pub async fn poll(&self, id: i64) -> Result<Poll, DbError> {
        dispatch!(&self.db, poll(id))
    }

    /// Takes job `id` out of the queue unless a worker holds a live claim on
    /// it or it is done. Returns how many workers had claimed it and whether
    /// that was all it was allowed, or `None` if it wasn't taken out.
    pub async fn give_up(&self, id: i64) -> Result<Option<(u32, bool)>, DbError> {
        dispatch!(&self.db, give_up(id))
    }

    /// Workers count as live if seen within `stale`.
    pub async fn counts(&self, stale: Duration) -> Result<Counts, DbError> {
        dispatch!(&self.db, counts(&self.name, stale))
    }

    /// Deletes results older than [`RESULT_RETENTION`], async outcomes older
    /// than their [`answer_retention`], and jobs and workers nobody has
    /// touched for [`STALE_RETENTION`]; closes async jobs nobody may claim
    /// any more. An async outcome leaves a tombstone, which goes after
    /// [`TOMBSTONE_RETENTION`]. Returns how many rows.
    pub async fn sweep(&self) -> Result<usize, DbError> {
        let now = crate::catalog::now_micros() as i64;
        dispatch!(&self.db, sweep_jobs(&self.name, now))
    }

    /// [`Queue::sweep`] as if it were `now` (microseconds), for tests that
    /// can't wait out the retention.
    pub async fn sweep_as_of(&self, now: i64) -> Result<usize, DbError> {
        dispatch!(&self.db, sweep_jobs(&self.name, now))
    }

    /// Adds or resets the worker's row, with its REPL slots and Beam slots,
    /// whether it runs Lean without limits and the commit it was built from,
    /// live from now, and lets its claims from before, of a process under
    /// the same name, lapse at once. Returns when, which
    /// [`Queue::heartbeat`] takes.
    pub async fn register(&self, r: &Registration) -> Result<i64, DbError> {
        dispatch!(&self.db, register(&self.name, r))
    }

    /// The live workers, seen within `stale`, that run Lean without memory
    /// or process limits, by name.
    pub async fn unlimited_workers(&self, stale: Duration) -> Result<Vec<String>, DbError> {
        dispatch!(&self.db, unlimited_workers(&self.name, stale))
    }

    /// Up to `limit` jobs that a worker of `version` could claim for its REPL
    /// slots, the interactive ones first, then oldest first. Jobs on
    /// documents are left out: see [`Queue::document_candidates`].
    pub async fn candidates(&self, version: &str, limit: usize) -> Result<Vec<Candidate>, DbError> {
        dispatch!(&self.db, candidates(&self.name, version, limit, false))
    }

    /// As [`Queue::candidates`], for the jobs on documents, which only
    /// workers with Beam slots claim.
    pub async fn document_candidates(
        &self,
        version: &str,
        limit: usize,
    ) -> Result<Vec<Candidate>, DbError> {
        dispatch!(&self.db, candidates(&self.name, version, limit, true))
    }

    /// The workers of `version` seen within `stale`.
    pub async fn registry(&self, version: &str, stale: Duration) -> Result<Vec<Peer>, DbError> {
        dispatch!(&self.db, registry(&self.name, version, stale))
    }

    /// The commits the workers seen within `stale` were built from, each once,
    /// in order.
    pub async fn live_revisions(&self, stale: Duration) -> Result<Vec<String>, DbError> {
        dispatch!(&self.db, live_revisions(&self.name, stale))
    }

    /// Records whether `worker`'s last probe of the module store reached it.
    pub async fn report_store(&self, worker: &str, reachable: bool) -> Result<(), DbError> {
        dispatch!(&self.db, report_store(&self.name, worker, reachable))
    }

    /// The module store as the workers seen within `stale` last saw it:
    /// `Unreachable` if any can't reach it, `Ok` if all of them can, and
    /// `Unknown` if none has probed.
    pub async fn store_state(&self, stale: Duration) -> Result<DependencyState, DbError> {
        dispatch!(&self.db, store_state(&self.name, stale))
    }

    /// The versions of the workers seen within `stale`, in order.
    pub async fn live_versions(&self, stale: Duration) -> Result<Vec<String>, DbError> {
        dispatch!(&self.db, live_versions(&self.name, stale))
    }

    /// How many workers of `version` (of any, for `None`) seen within `stale`
    /// serve documents: those with Beam slots that aren't draining.
    pub async fn serving_documents(
        &self,
        version: Option<&str>,
        stale: Duration,
    ) -> Result<u64, DbError> {
        dispatch!(&self.db, serving_documents(&self.name, version, stale))
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
    pub async fn complete(&self, worker: &str, id: i64, result: &Value) -> Result<bool, DbError> {
        dispatch!(&self.db, complete(worker, id, result))
    }

    /// Renews the leases of the claims `worker` made since it registered at
    /// `since`, and marks it seen: live anew if it was last seen longer
    /// than [`LIVE`] ago.
    pub async fn heartbeat(
        &self,
        worker: &str,
        since: i64,
        lease: Duration,
    ) -> Result<(), DbError> {
        let now = crate::catalog::now_micros() as i64;
        dispatch!(&self.db, heartbeat(&self.name, worker, since, lease, now))
    }

    /// [`Queue::heartbeat`] as if it were `now` (microseconds).
    #[cfg(test)]
    pub async fn heartbeat_as_of(
        &self,
        worker: &str,
        since: i64,
        lease: Duration,
        now: i64,
    ) -> Result<(), DbError> {
        dispatch!(&self.db, heartbeat(&self.name, worker, since, lease, now))
    }

    pub async fn set_warm(&self, worker: &str, warm: &[String]) -> Result<(), DbError> {
        dispatch!(&self.db, set_warm(&self.name, worker, warm))
    }

    /// Records that `worker` has `open` documents open on Lean Beam.
    pub async fn set_documents_open(&self, worker: &str, open: u32) -> Result<(), DbError> {
        dispatch!(&self.db, set_documents_open(&self.name, worker, open))
    }

    /// Tells the other workers that `worker` takes no more jobs.
    pub async fn set_draining(&self, worker: &str) -> Result<(), DbError> {
        dispatch!(&self.db, set_draining(&self.name, worker))
    }

    /// Records that `worker` stopped: its row is kept, draining and no
    /// longer live, until the sweep removes it [`STALE_RETENTION`] after it
    /// was last seen, so that an unclaimed async job of its version waits
    /// for a worker to come back rather than being given up as one of a
    /// version no worker runs. Registering again under its name replaces it.
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
            document: None,
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

    fn reg(
        worker: &str,
        version: &str,
        slots: u32,
        beam_slots: u32,
        unlimited: bool,
    ) -> Registration {
        Registration {
            worker: worker.into(),
            version: version.into(),
            slots,
            beam_slots,
            unlimited,
            ..Registration::default()
        }
    }

    #[tokio::test]
    async fn a_worker_registers_its_revision() {
        for q in queues().await {
            let abc = Registration {
                revision: "abc".into(),
                ..reg("w1", "v", 1, 0, false)
            };
            q.register(&abc).await.unwrap();
            let again = Registration {
                worker: "w2".into(),
                ..abc.clone()
            };
            q.register(&again).await.unwrap();
            q.register(&Registration {
                revision: "def".into(),
                ..reg("w3", "v", 1, 0, false)
            })
            .await
            .unwrap();
            assert_eq!(q.live_revisions(LEASE).await.unwrap(), ["abc", "def"]);
            assert!(q.live_revisions(Duration::ZERO).await.unwrap().is_empty());
            // Processes under the same names, of another build.
            for worker in ["w1", "w2"] {
                q.register(&Registration {
                    revision: "def".into(),
                    ..reg(worker, "v", 1, 0, false)
                })
                .await
                .unwrap();
            }
            assert_eq!(q.live_revisions(LEASE).await.unwrap(), ["def"]);
        }
    }

    #[tokio::test]
    async fn the_store_is_as_the_live_workers_last_saw_it() {
        for q in queues().await {
            assert_eq!(
                q.store_state(LEASE).await.unwrap(),
                DependencyState::Unknown
            );
            q.register(&reg("w1", "v", 1, 0, false)).await.unwrap();
            q.register(&reg("w2", "v", 1, 0, false)).await.unwrap();
            // Registered, but not yet probed.
            assert_eq!(
                q.store_state(LEASE).await.unwrap(),
                DependencyState::Unknown
            );
            q.report_store("w1", true).await.unwrap();
            assert_eq!(q.store_state(LEASE).await.unwrap(), DependencyState::Ok);
            q.report_store("w2", false).await.unwrap();
            assert_eq!(
                q.store_state(LEASE).await.unwrap(),
                DependencyState::Unreachable
            );
            // Workers not seen lately don't count.
            assert_eq!(
                q.store_state(Duration::ZERO).await.unwrap(),
                DependencyState::Unknown
            );
            q.report_store("w2", true).await.unwrap();
            assert_eq!(q.store_state(LEASE).await.unwrap(), DependencyState::Ok);
            // A new process under the name hasn't probed yet.
            q.register(&reg("w2", "v", 1, 0, false)).await.unwrap();
            q.report_store("w1", false).await.unwrap();
            assert_eq!(
                q.store_state(LEASE).await.unwrap(),
                DependencyState::Unreachable
            );
        }
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

            assert!(q.complete("w", id, &json!({"ok": 1})).await.unwrap());
            let Poll::Done(result) = q.poll(id).await.unwrap() else {
                panic!("not done");
            };
            assert_eq!(result, json!({"ok": 1}));
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
            assert!(!q.complete("a", id, &json!(1)).await.unwrap());
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert!(ids(&q, "v").await.is_empty());
            assert!(q.claim("c", &[id], LEASE).await.unwrap().is_empty());
            assert!(matches!(
                q.poll(id).await.unwrap(),
                Poll::Lost {
                    attempts: 2,
                    exhausted: true
                }
            ));
            assert!(matches!(q.poll(id).await.unwrap(), Poll::Gone));
        }
    }

    #[tokio::test]
    async fn a_job_is_read_and_cancelled_in_its_scope_only() {
        use crate::scope::{Scope, within};
        for q in queues().await {
            let a = Scope::parse("user=a").unwrap();
            let (_, token) = within(a.clone(), q.enqueue_async(&job(Some("v"), 1), "check"))
                .await
                .unwrap();
            let elsewhere = Scope::parse("user=b").unwrap();
            assert!(
                within(elsewhere.clone(), q.status(&token))
                    .await
                    .unwrap()
                    .is_none()
            );
            assert!(within(elsewhere, q.cancel(&token)).await.unwrap().is_none());
            assert!(within(a.clone(), q.status(&token)).await.unwrap().is_some());
            assert!(q.status(&token).await.unwrap().is_some());
            assert!(within(a, q.cancel(&token)).await.unwrap().is_some());
        }
    }

    #[tokio::test]
    async fn an_async_job_is_read_by_its_token_until_swept() {
        for q in queues().await {
            let (id, token) = q.enqueue_async(&job(Some("v"), 1), "check").await.unwrap();
            assert_eq!(token.len(), 32);
            assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
            let status = q.status(&token).await.unwrap().unwrap();
            assert_eq!((status.id, status.kind.as_str()), (id, "check"));
            assert!(matches!(status.stage, Stage::Queued { .. }));
            assert_eq!(q.claim("w", &[id], LEASE).await.unwrap().len(), 1);
            let stage = q.status(&token).await.unwrap().unwrap().stage;
            assert!(matches!(stage, Stage::Running));
            assert!(q.complete("w", id, &json!({"ok": true})).await.unwrap());
            // Reading leaves it.
            for _ in 0..2 {
                let stage = q.status(&token).await.unwrap().unwrap().stage;
                assert!(matches!(stage, Stage::Done(v) if v == json!({"ok": true})));
            }
            let (_, other) = q.enqueue_async(&job(Some("v"), 1), "check").await.unwrap();
            assert_ne!(other, token);
            // Another queue's jobs are its own.
            let elsewhere = Queue::new(q.db().clone(), "elsewhere");
            assert!(elsewhere.status(&token).await.unwrap().is_none());
            assert!(elsewhere.cancel(&token).await.unwrap().is_none());
        }
    }

    #[tokio::test]
    async fn an_async_job_nobody_claims_in_time_is_closed_given_up() {
        for q in queues().await {
            // Its version's worker, quiet since it registered.
            let since = q.register(&reg("w", "v", 1, 0, false)).await.unwrap();
            let mut new = job(Some("v"), 1);
            new.max_wait = Duration::from_millis(10);
            let (id, token) = q.enqueue_async(&new, "check").await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
            // With no worker live, it waits for one.
            let stage = q.status(&token).await.unwrap().unwrap().stage;
            assert!(matches!(stage, Stage::Queued { .. }), "{stage:?}");
            // The worker is live for its max wait, and doesn't take it.
            q.heartbeat("w", since, LEASE).await.unwrap();
            for _ in 0..2 {
                let stage = q.status(&token).await.unwrap().unwrap().stage;
                assert!(matches!(stage, Stage::GaveUp { attempts: 0, .. }));
            }
            // Closed: nobody can claim or complete it, and it isn't queued.
            assert!(q.claim("w", &[id], LEASE).await.unwrap().is_empty());
            assert!(!q.complete("w", id, &json!(1)).await.unwrap());
            assert_eq!(q.counts(LIVE).await.unwrap().queued, [0, 0]);
        }
    }

    #[tokio::test]
    async fn an_async_job_lost_with_its_last_worker_stays_lost() {
        for q in queues().await {
            let mut new = job(Some("v"), 1);
            new.max_attempts = 1;
            let (id, token) = q.enqueue_async(&new, "check").await.unwrap();
            let short = Duration::from_millis(1);
            assert_eq!(q.claim("a", &[id], short).await.unwrap().len(), 1);
            tokio::time::sleep(Duration::from_millis(20)).await;
            let stage = q.status(&token).await.unwrap().unwrap().stage;
            assert!(matches!(
                stage,
                Stage::GaveUp {
                    attempts: 1,
                    exhausted: true
                }
            ));
            // The worker answers too late.
            assert!(!q.complete("a", id, &json!(1)).await.unwrap());
            let stage = q.status(&token).await.unwrap().unwrap().stage;
            assert!(matches!(stage, Stage::GaveUp { attempts: 1, .. }));
        }
    }

    fn micros(d: Duration) -> i64 {
        d.as_micros() as i64
    }

    #[tokio::test]
    async fn an_async_job_lost_while_no_worker_is_live_waits_for_one() {
        for q in queues().await {
            let mut new = job(Some("v"), 1);
            new.max_wait = Duration::from_millis(10);
            new.max_attempts = 2;
            let (id, token) = q.enqueue_async(&new, "check").await.unwrap();
            // Its worker claims it and dies, as every other does. Its row
            // stays, last seen before the claim lapsed.
            q.register(&reg("a", "v", 1, 0, false)).await.unwrap();
            let short = Duration::from_millis(1);
            let claimed = q.claim("a", &[id], short).await.unwrap();
            let ceiling = claimed[0].enqueued_micros + micros(LOST_CEILING);
            tokio::time::sleep(Duration::from_millis(30)).await;
            for _ in 0..2 {
                let stage = q.status(&token).await.unwrap().unwrap().stage;
                assert!(
                    matches!(stage, Stage::Queued { give_up_at } if give_up_at == ceiling),
                    "{stage:?}"
                );
            }
            // The sweep leaves it too, however late short of the ceiling.
            let hour = micros(Duration::from_secs(3600));
            q.sweep_as_of(ceiling - hour).await.unwrap();
            assert_eq!(ids(&q, "v").await, [id]);
            // A worker comes back, and runs it again: its second and last
            // attempt.
            q.register(&reg("b", "v", 1, 0, false)).await.unwrap();
            assert_eq!(q.claim("b", &[id], short).await.unwrap().len(), 1);
            tokio::time::sleep(Duration::from_millis(20)).await;
            let stage = q.status(&token).await.unwrap().unwrap().stage;
            assert!(
                matches!(
                    stage,
                    Stage::GaveUp {
                        attempts: 2,
                        exhausted: true
                    }
                ),
                "{stage:?}"
            );
        }
    }

    #[tokio::test]
    async fn an_async_job_is_lost_after_max_wait_with_a_live_worker() {
        for q in queues().await {
            let since = q.register(&reg("w", "v", 1, 0, false)).await.unwrap();
            let mut new = job(Some("v"), 1);
            new.max_wait = Duration::from_millis(10);
            let (id, token) = q.enqueue_async(&new, "check").await.unwrap();
            let (swept, unread) = q.enqueue_async(&new, "check").await.unwrap();
            let short = Duration::from_millis(1);
            assert_eq!(q.claim("a", &[id, swept], short).await.unwrap().len(), 2);
            tokio::time::sleep(Duration::from_millis(30)).await;
            // `w` is live past the lapse and the max wait, and doesn't take
            // them, e.g. as it is busy.
            q.heartbeat("w", since, LEASE).await.unwrap();
            for _ in 0..2 {
                let stage = q.status(&token).await.unwrap().unwrap().stage;
                assert!(
                    matches!(
                        stage,
                        Stage::GaveUp {
                            attempts: 1,
                            exhausted: false
                        }
                    ),
                    "{stage:?}"
                );
            }
            assert!(q.claim("w", &[id], LEASE).await.unwrap().is_empty());
            // The sweep, a minute behind, agrees: it closes the one nobody
            // read.
            let now = crate::catalog::now_micros() as i64;
            let later = now + micros(Duration::from_secs(120));
            assert_eq!(q.sweep_as_of(later).await.unwrap(), 1);
            let stage = q.status(&unread).await.unwrap().unwrap().stage;
            assert!(
                matches!(stage, Stage::GaveUp { attempts: 1, .. }),
                "{stage:?}"
            );
        }
    }

    #[tokio::test]
    async fn an_async_job_is_lost_at_the_ceiling() {
        for q in queues().await {
            // Its version's worker, quiet since it registered.
            q.register(&reg("w", "v", 1, 0, false)).await.unwrap();
            let (unclaimed, never_run) =
                q.enqueue_async(&job(Some("v"), 1), "check").await.unwrap();
            let (id, token) = q.enqueue_async(&job(Some("v"), 1), "check").await.unwrap();
            let short = Duration::from_millis(1);
            let claimed = q.claim("a", &[id], short).await.unwrap();
            let ceiling = claimed[0].enqueued_micros + micros(LOST_CEILING);
            // No worker is ever live. The sweep runs a minute behind.
            let margin = micros(Duration::from_secs(120));
            q.sweep_as_of(ceiling - margin).await.unwrap();
            for token in [&never_run, &token] {
                let stage = q.status(token).await.unwrap().unwrap().stage;
                assert!(matches!(stage, Stage::Queued { .. }), "{stage:?}");
            }
            q.sweep_as_of(ceiling + margin).await.unwrap();
            let stage = q.status(&never_run).await.unwrap().unwrap().stage;
            assert!(
                matches!(stage, Stage::GaveUp { attempts: 0, .. }),
                "{stage:?}"
            );
            let stage = q.status(&token).await.unwrap().unwrap().stage;
            assert!(
                matches!(stage, Stage::GaveUp { attempts: 1, .. }),
                "{stage:?}"
            );
            let both = [unclaimed, id];
            assert!(q.claim("b", &both, LEASE).await.unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn a_sync_job_keeps_its_bound_with_no_worker_live() {
        for q in queues().await {
            let mut new = job(Some("v"), 1);
            new.max_wait = Duration::from_millis(10);
            let id = q.enqueue(&new).await.unwrap();
            let short = Duration::from_millis(1);
            assert_eq!(q.claim("a", &[id], short).await.unwrap().len(), 1);
            tokio::time::sleep(Duration::from_millis(30)).await;
            // Its request doesn't wait for a worker to come back.
            assert!(ids(&q, "v").await.is_empty());
            assert!(q.claim("b", &[id], LEASE).await.unwrap().is_empty());
            // Attempts remained, but no worker came back in its wait.
            assert!(matches!(
                q.poll(id).await.unwrap(),
                Poll::Lost {
                    attempts: 1,
                    exhausted: false
                }
            ));
        }
    }

    /// A worker that went quiet, e.g. while the database was down, is live
    /// again only from its return: the time it was away doesn't count
    /// toward a lost job's wait.
    #[tokio::test]
    async fn a_worker_back_from_a_gap_counts_from_its_return() {
        for q in queues().await {
            let since = q.register(&reg("w", "v", 1, 0, false)).await.unwrap();
            // It waits a minute for a worker.
            let (id, token) = q.enqueue_async(&job(Some("v"), 1), "check").await.unwrap();
            let short = Duration::from_millis(1);
            assert_eq!(q.claim("a", &[id], short).await.unwrap().len(), 1);
            // `w` is back two minutes later, and heartbeats every 20 s.
            let back = since + micros(Duration::from_secs(120));
            let at = |secs: u64| back + micros(Duration::from_secs(secs));
            for secs in [0, 20, 40] {
                q.heartbeat_as_of("w", since, LEASE, at(secs))
                    .await
                    .unwrap();
            }
            q.sweep_as_of(at(40 + 120)).await.unwrap();
            let stage = q.status(&token).await.unwrap().unwrap().stage;
            assert!(matches!(stage, Stage::Queued { .. }), "{stage:?}");
            // A minute after it came back, it has been live long enough.
            q.heartbeat_as_of("w", since, LEASE, at(60)).await.unwrap();
            q.sweep_as_of(at(60 + 120)).await.unwrap();
            let stage = q.status(&token).await.unwrap().unwrap().stage;
            assert!(
                matches!(stage, Stage::GaveUp { attempts: 1, .. }),
                "{stage:?}"
            );
        }
    }

    /// Only a worker that could claim a lost job counts: one of another
    /// version doesn't, however long it is live.
    #[tokio::test]
    async fn an_async_job_waits_for_a_worker_of_its_version() {
        for q in queues().await {
            let mut new = job(Some("v1"), 1);
            new.max_wait = Duration::from_millis(10);
            let (id, token) = q.enqueue_async(&new, "check").await.unwrap();
            q.register(&reg("a", "v1", 1, 0, false)).await.unwrap();
            let short = Duration::from_millis(1);
            assert_eq!(q.claim("a", &[id], short).await.unwrap().len(), 1);
            // `a` dies; a worker of v2 is live past the max wait.
            let since = q.register(&reg("w", "v2", 1, 0, false)).await.unwrap();
            tokio::time::sleep(Duration::from_millis(30)).await;
            q.heartbeat("w", since, LEASE).await.unwrap();
            let stage = q.status(&token).await.unwrap().unwrap().stage;
            assert!(matches!(stage, Stage::Queued { .. }), "{stage:?}");
            assert_eq!(ids(&q, "v1").await, [id]);
            // A worker of v1 comes back and takes it.
            q.register(&reg("b", "v1", 1, 0, false)).await.unwrap();
            assert_eq!(q.claim("b", &[id], LEASE).await.unwrap().len(), 1);
        }
    }

    /// A lost job on a document waits for a worker with Beam slots, which
    /// only can claim it.
    #[tokio::test]
    async fn an_async_document_job_waits_for_a_worker_with_beam_slots() {
        for q in queues().await {
            let mut new = document_job("d");
            new.max_wait = Duration::from_millis(10);
            let (id, token) = q.enqueue_async(&new, "check").await.unwrap();
            let short = Duration::from_millis(1);
            assert_eq!(q.claim("a", &[id], short).await.unwrap().len(), 1);
            // Only a worker without Beam slots is live past the max wait.
            let since = q.register(&reg("repl", "v", 1, 0, false)).await.unwrap();
            tokio::time::sleep(Duration::from_millis(30)).await;
            q.heartbeat("repl", since, LEASE).await.unwrap();
            let stage = q.status(&token).await.unwrap().unwrap().stage;
            assert!(matches!(stage, Stage::Queued { .. }), "{stage:?}");
            let found = q.document_candidates("v", 10).await.unwrap();
            assert_eq!(found.iter().map(|c| c.id).collect::<Vec<_>>(), [id]);
            // One with Beam slots is, and doesn't take it.
            let since = q.register(&reg("beam", "v", 1, 1, false)).await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
            q.heartbeat("beam", since, LEASE).await.unwrap();
            let stage = q.status(&token).await.unwrap().unwrap().stage;
            assert!(
                matches!(stage, Stage::GaveUp { attempts: 1, .. }),
                "{stage:?}"
            );
        }
    }

    /// A job of a version no worker has run within the hour workers' rows
    /// are kept isn't waiting on a restart: it is given up after its max
    /// wait, so that its client gets a 429.
    #[tokio::test]
    async fn an_async_job_of_a_version_no_worker_runs_gives_up_after_max_wait() {
        for q in queues().await {
            // A worker of `v`, quiet since it registered.
            q.register(&reg("w", "v", 1, 0, false)).await.unwrap();
            let mut new = job(Some("nope"), 1);
            new.max_wait = Duration::from_millis(10);
            let (_, nope) = q.enqueue_async(&new, "check").await.unwrap();
            new.version = Some("v".into());
            let (_, waits) = q.enqueue_async(&new, "check").await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
            let stage = q.status(&nope).await.unwrap().unwrap().stage;
            assert!(
                matches!(stage, Stage::GaveUp { attempts: 0, .. }),
                "{stage:?}"
            );
            // The job of `v` waits for its worker to come back.
            let stage = q.status(&waits).await.unwrap().unwrap().stage;
            assert!(matches!(stage, Stage::Queued { .. }), "{stage:?}");
        }
    }

    /// Across a clean restart of every worker of its version, as a reboot
    /// does, an unclaimed job waits for one to come back: the stopped
    /// worker's row is kept, though it is no worker any more.
    #[tokio::test]
    async fn an_unclaimed_async_job_outlasts_a_clean_stop_of_its_workers() {
        for q in queues().await {
            q.register(&reg("w", "v", 1, 0, false)).await.unwrap();
            let mut new = job(Some("v"), 1);
            new.max_wait = Duration::from_millis(10);
            let (id, token) = q.enqueue_async(&new, "check").await.unwrap();
            q.set_draining("w").await.unwrap();
            q.unregister("w").await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
            let stage = q.status(&token).await.unwrap().unwrap().stage;
            assert!(matches!(stage, Stage::Queued { .. }), "{stage:?}");
            // The sweep, however late short of an hour, leaves it too.
            q.sweep_as_of(later(50)).await.unwrap();
            let stage = q.status(&token).await.unwrap().unwrap().stage;
            assert!(matches!(stage, Stage::Queued { .. }), "{stage:?}");
            // The stopped worker is no worker.
            assert_eq!(q.counts(LIVE).await.unwrap().workers, 0);
            assert!(q.registry("v", LIVE).await.unwrap().is_empty());
            assert!(q.live_versions(LIVE).await.unwrap().is_empty());
            // One comes back, under its name, and takes it.
            q.register(&reg("w", "v", 1, 0, false)).await.unwrap();
            let peers = q.registry("v", LIVE).await.unwrap();
            assert_eq!(peers.len(), 1);
            assert!(!peers[0].draining);
            assert_eq!(ids(&q, "v").await, [id]);
            assert_eq!(q.claim("w", &[id], LEASE).await.unwrap().len(), 1);
        }
    }

    /// A cleanly stopped worker's row goes an hour later, as any other's
    /// does: then a job of its version is of a version no worker runs.
    #[tokio::test]
    async fn a_stopped_workers_row_goes_after_an_hour() {
        for q in queues().await {
            q.register(&reg("w", "v", 1, 0, false)).await.unwrap();
            q.unregister("w").await.unwrap();
            assert_eq!(q.sweep_as_of(later(61)).await.unwrap(), 1);
            let mut new = job(Some("v"), 1);
            new.max_wait = Duration::from_millis(10);
            let (_, token) = q.enqueue_async(&new, "check").await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
            let stage = q.status(&token).await.unwrap().unwrap().stage;
            assert!(
                matches!(stage, Stage::GaveUp { attempts: 0, .. }),
                "{stage:?}"
            );
        }
    }

    #[tokio::test]
    async fn only_an_async_job_no_worker_holds_is_cancelled() {
        for q in queues().await {
            let (id, token) = q.enqueue_async(&job(Some("v"), 1), "check").await.unwrap();
            assert_eq!(q.cancel(&token).await.unwrap(), Some(id));
            let stage = q.status(&token).await.unwrap().unwrap().stage;
            assert!(matches!(stage, Stage::Expired { .. }), "{stage:?}");

            let (id, token) = q.enqueue_async(&job(Some("v"), 1), "check").await.unwrap();
            q.claim("w", &[id], LEASE).await.unwrap();
            assert_eq!(q.cancel(&token).await.unwrap(), None);
            let stage = q.status(&token).await.unwrap().unwrap().stage;
            assert!(matches!(stage, Stage::Running));
            q.complete("w", id, &json!(1)).await.unwrap();
            assert_eq!(q.cancel(&token).await.unwrap(), None);
            let stage = q.status(&token).await.unwrap().unwrap().stage;
            assert!(matches!(stage, Stage::Done(_)));
        }
    }

    #[tokio::test]
    async fn the_sweep_keeps_async_outcomes_an_hour() {
        for q in queues().await {
            let since = q.register(&reg("w", "v", 1, 0, false)).await.unwrap();
            let sync = q.enqueue(&job(Some("v"), 1)).await.unwrap();
            let (id, done) = q.enqueue_async(&job(Some("v"), 1), "check").await.unwrap();
            q.claim("w", &[sync, id], LEASE).await.unwrap();
            q.complete("w", sync, &json!(1)).await.unwrap();
            q.complete("w", id, &json!(2)).await.unwrap();
            let mut unclaimed = job(Some("v"), 1);
            unclaimed.max_wait = Duration::from_millis(1);
            let sync_unclaimed = q.enqueue(&unclaimed).await.unwrap();
            let (_, given_up) = q.enqueue_async(&unclaimed, "check").await.unwrap();
            // `w` is live past `given_up`'s max wait, and doesn't take it.
            tokio::time::sleep(Duration::from_millis(5)).await;
            q.heartbeat("w", since, LEASE).await.unwrap();
            let later = |minutes: u64| {
                (crate::catalog::now_micros()
                    + Duration::from_secs(60 * minutes).as_micros() as u64) as i64
            };

            q.sweep_as_of(later(11)).await.unwrap();
            assert!(matches!(q.poll(sync).await.unwrap(), Poll::Gone));
            assert!(matches!(q.poll(sync_unclaimed).await.unwrap(), Poll::Gone));
            let stage = q.status(&done).await.unwrap().unwrap().stage;
            assert!(matches!(stage, Stage::Done(_)));
            let stage = q.status(&given_up).await.unwrap().unwrap().stage;
            assert!(matches!(stage, Stage::GaveUp { attempts: 0, .. }));

            // The sweep closed `given_up` at +11 minutes; both are tombstones
            // an hour after.
            q.sweep_as_of(later(72)).await.unwrap();
            for token in [&done, &given_up] {
                let stage = q.status(token).await.unwrap().unwrap().stage;
                assert!(matches!(stage, Stage::Expired { .. }), "{stage:?}");
            }
        }
    }

    fn later(minutes: u64) -> i64 {
        (crate::catalog::now_micros() + Duration::from_secs(60 * minutes).as_micros() as u64) as i64
    }

    fn expired(stage: Stage) -> (String, ExpiredOutcome, Option<u16>) {
        match stage {
            Stage::Expired {
                kind,
                outcome,
                http_status,
                ..
            } => (kind, outcome, http_status),
            other => panic!("not expired: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_swept_job_leaves_a_tombstone() {
        use crate::scope::{Scope, within};
        for q in queues().await {
            let a = Scope::parse("user=a").unwrap();
            q.register(&reg("w", "v", 1, 0, false)).await.unwrap();
            let (id, token) = within(a.clone(), q.enqueue_async(&job(Some("v"), 1), "check"))
                .await
                .unwrap();
            q.claim("w", &[id], LEASE).await.unwrap();
            q.complete("w", id, &json!({"kind": "check", "time": 1.0}))
                .await
                .unwrap();
            q.sweep_as_of(later(61)).await.unwrap();
            let stage = within(a.clone(), q.status(&token))
                .await
                .unwrap()
                .unwrap()
                .stage;
            assert_eq!(
                expired(stage),
                ("check".to_string(), ExpiredOutcome::Done, Some(200))
            );
            // Anyone without a scope sees it; another scope doesn't.
            assert!(q.status(&token).await.unwrap().is_some());
            let b = Scope::parse("user=b").unwrap();
            assert!(within(b.clone(), q.status(&token)).await.unwrap().is_none());
            assert!(within(b, q.cancel(&token)).await.unwrap().is_none());
            // Another queue's is its own.
            let elsewhere = Queue::new(q.db().clone(), "elsewhere");
            assert!(elsewhere.status(&token).await.unwrap().is_none());
        }
    }

    #[tokio::test]
    async fn a_swept_job_keeps_its_status_but_never_its_answer() {
        for q in queues().await {
            q.register(&reg("w", "v", 1, 0, false)).await.unwrap();
            let (id, token) = q
                .enqueue_async(&job(Some("v"), 1), "accept_run")
                .await
                .unwrap();
            q.claim("w", &[id], LEASE).await.unwrap();
            let refused = json!({
                "kind": "snapshot_accepted",
                "Err": {"kind": "conflict", "message": "SECRET-ANSWER"}
            });
            q.complete("w", id, &refused).await.unwrap();
            q.sweep_as_of(later(25 * 60)).await.unwrap();
            let status = q.status(&token).await.unwrap();
            assert!(!format!("{status:?}").contains("SECRET"));
            assert_eq!(
                expired(status.unwrap().stage),
                ("accept_run".to_string(), ExpiredOutcome::Done, Some(409))
            );
        }
    }

    /// One no worker took keeps the 429 it answered.
    #[tokio::test]
    async fn a_job_given_up_leaves_a_tombstone() {
        for q in queues().await {
            let since = q.register(&reg("w", "v", 1, 0, false)).await.unwrap();
            let mut new = job(Some("v"), 1);
            new.max_wait = Duration::from_millis(1);
            let (_, token) = q.enqueue_async(&new, "check").await.unwrap();
            tokio::time::sleep(Duration::from_millis(5)).await;
            q.heartbeat("w", since, LEASE).await.unwrap();
            let stage = q.status(&token).await.unwrap().unwrap().stage;
            assert!(
                matches!(stage, Stage::GaveUp { attempts: 0, .. }),
                "{stage:?}"
            );
            q.sweep_as_of(later(72)).await.unwrap();
            let stage = q.status(&token).await.unwrap().unwrap().stage;
            assert_eq!(
                expired(stage),
                ("check".to_string(), ExpiredOutcome::Lost, Some(429))
            );
        }
    }

    /// One whose worker was lost keeps the 503 it answered.
    #[tokio::test]
    async fn a_lost_job_leaves_a_tombstone() {
        for q in queues().await {
            q.register(&reg("w", "v", 1, 0, false)).await.unwrap();
            let mut new = job(Some("v"), 1);
            new.max_attempts = 1;
            let (id, token) = q.enqueue_async(&new, "check").await.unwrap();
            let short = Duration::from_millis(1);
            assert_eq!(q.claim("w", &[id], short).await.unwrap().len(), 1);
            tokio::time::sleep(Duration::from_millis(5)).await;
            let stage = q.status(&token).await.unwrap().unwrap().stage;
            assert!(
                matches!(stage, Stage::GaveUp { attempts: 1, .. }),
                "{stage:?}"
            );
            q.sweep_as_of(later(72)).await.unwrap();
            let stage = q.status(&token).await.unwrap().unwrap().stage;
            assert_eq!(
                expired(stage),
                ("check".to_string(), ExpiredOutcome::Lost, Some(503))
            );
        }
    }

    /// A worker that couldn't run the job failed a check or a create in-band,
    /// with a 200, and anything else with a 500, as is one whose result
    /// doesn't read.
    #[tokio::test]
    async fn a_failed_job_keeps_the_status_it_answered() {
        let failed = json!({"kind": "failed", "error": "boom"});
        let unreadable = json!({"kind": "from_a_newer_worker"});
        let cases = [
            ("check", &failed, 200),
            ("check_session", &failed, 200),
            ("create_snapshot", &failed, 200),
            ("start_session", &failed, 500),
            ("accept_run", &failed, 500),
            ("start_session", &unreadable, 500),
        ];
        for q in queues().await {
            q.register(&reg("w", "v", 1, 0, false)).await.unwrap();
            let mut tokens = Vec::new();
            for (kind, result, _) in cases {
                let (id, token) = q.enqueue_async(&job(Some("v"), 1), kind).await.unwrap();
                q.claim("w", &[id], LEASE).await.unwrap();
                q.complete("w", id, result).await.unwrap();
                tokens.push(token);
            }
            q.sweep_as_of(later(25 * 60)).await.unwrap();
            for ((kind, _, status), token) in cases.iter().zip(&tokens) {
                let stage = q.status(token).await.unwrap().unwrap().stage;
                assert_eq!(
                    expired(stage),
                    (kind.to_string(), ExpiredOutcome::Done, Some(*status)),
                    "{kind}"
                );
            }
        }
    }

    #[tokio::test]
    async fn a_cancelled_job_leaves_a_tombstone() {
        for q in queues().await {
            let (id, token) = q.enqueue_async(&job(Some("v"), 1), "check").await.unwrap();
            assert_eq!(q.cancel(&token).await.unwrap(), Some(id));
            let stage = q.status(&token).await.unwrap().unwrap().stage;
            assert_eq!(
                expired(stage),
                ("check".to_string(), ExpiredOutcome::Cancelled, None)
            );
            // It is not cancelled twice.
            assert_eq!(q.cancel(&token).await.unwrap(), None);
        }
    }

    #[tokio::test]
    async fn a_sync_job_leaves_no_tombstone() {
        for q in queues().await {
            q.register(&reg("w", "v", 1, 0, false)).await.unwrap();
            let id = q.enqueue(&job(Some("v"), 1)).await.unwrap();
            q.claim("w", &[id], LEASE).await.unwrap();
            q.complete("w", id, &json!(1)).await.unwrap();
            q.unregister("w").await.unwrap();
            q.sweep_as_of(later(11)).await.unwrap();
            assert!(matches!(q.poll(id).await.unwrap(), Poll::Gone));
            // The stopped worker's row goes after an hour.
            assert_eq!(q.sweep_as_of(later(61)).await.unwrap(), 1);
            // Nothing is left to sweep: no tombstone was written.
            assert_eq!(q.sweep_as_of(later(31 * 24 * 60)).await.unwrap(), 0);
        }
    }

    #[tokio::test]
    async fn tombstones_go_after_thirty_days() {
        for q in queues().await {
            let (_, token) = q.enqueue_async(&job(Some("v"), 1), "check").await.unwrap();
            q.cancel(&token).await.unwrap();
            let day = 24 * 60;
            q.sweep_as_of(later(29 * day)).await.unwrap();
            assert!(q.status(&token).await.unwrap().is_some());
            q.sweep_as_of(later(31 * day)).await.unwrap();
            assert!(q.status(&token).await.unwrap().is_none());
        }
    }

    #[tokio::test]
    async fn a_create_is_kept_a_day() {
        assert_eq!(
            answer_retention("create_snapshot"),
            Duration::from_secs(86_400)
        );
        assert_eq!(answer_retention("accept_run"), Duration::from_secs(86_400));
        assert_eq!(answer_retention("check"), ASYNC_RETENTION);
        for q in queues().await {
            q.register(&reg("w", "v", 1, 0, false)).await.unwrap();
            let mut tokens = vec![];
            for kind in ["create_snapshot", "accept_run", "check"] {
                let (id, token) = q.enqueue_async(&job(Some("v"), 1), kind).await.unwrap();
                q.claim("w", &[id], LEASE).await.unwrap();
                q.complete("w", id, &json!(1)).await.unwrap();
                tokens.push(token);
            }
            q.sweep_as_of(later(61)).await.unwrap();
            let stage = |i: usize| {
                let q = q.clone();
                let token = tokens[i].clone();
                async move { q.status(&token).await.unwrap().unwrap().stage }
            };
            assert!(matches!(stage(0).await, Stage::Done(_)));
            assert!(matches!(stage(1).await, Stage::Done(_)));
            assert!(matches!(stage(2).await, Stage::Expired { .. }));
            q.sweep_as_of(later(24 * 60 + 1)).await.unwrap();
            for i in 0..2 {
                let stage = stage(i).await;
                assert!(matches!(stage, Stage::Expired { .. }), "{stage:?}");
            }
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
            assert_eq!(q.give_up(id).await.unwrap(), Some((0, false)));
            assert!(matches!(q.poll(id).await.unwrap(), Poll::Gone));

            // A running job isn't given up.
            let id = q.enqueue(&job(Some("v"), 1)).await.unwrap();
            q.claim("w", &[id], LEASE).await.unwrap();
            assert_eq!(q.give_up(id).await.unwrap(), None);
            assert!(q.complete("w", id, &json!(1)).await.unwrap());
        }
    }

    fn document_job(id: &str) -> NewJob {
        NewJob {
            document: Some(id.into()),
            ..job(Some("v"), 0)
        }
    }

    #[tokio::test]
    async fn jobs_on_documents_are_candidates_apart() {
        for q in queues().await {
            let repl = q.enqueue(&job(Some("v"), 1)).await.unwrap();
            let on_doc = q.enqueue(&document_job("d")).await.unwrap();
            assert_eq!(ids(&q, "v").await, [repl]);
            let found = q.document_candidates("v", 10).await.unwrap();
            assert_eq!(
                found
                    .iter()
                    .map(|c| (c.id, c.document.as_deref()))
                    .collect::<Vec<_>>(),
                [(on_doc, Some("d"))]
            );
            assert!(q.document_candidates("w", 10).await.unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn the_worker_holding_a_document_is_its_jobs_affinity() {
        for q in queues().await {
            let records = Records::new(q.db().clone(), "v".into());
            let id = uuid::Uuid::new_v4().simple().to_string();
            records
                .put_document(&id, "theorem t : True := trivial", 60, "w1", 0, &[])
                .await
                .unwrap();
            q.enqueue(&document_job(&id)).await.unwrap();
            q.enqueue(&document_job("nope")).await.unwrap();
            let affinity: Vec<_> = q
                .document_candidates("v", 10)
                .await
                .unwrap()
                .into_iter()
                .map(|c| c.affinity)
                .collect();
            assert_eq!(affinity, [Some("w1".to_string()), None]);
            records.delete_document(&id).await.unwrap();
        }
    }

    #[tokio::test]
    async fn the_live_workers_without_limits_are_named() {
        for q in queues().await {
            q.register(&reg("free", "v", 1, 0, true)).await.unwrap();
            q.register(&reg("held", "v", 1, 0, false)).await.unwrap();
            assert_eq!(q.unlimited_workers(LEASE).await.unwrap(), ["free"]);
            assert!(
                q.unlimited_workers(Duration::ZERO)
                    .await
                    .unwrap()
                    .is_empty()
            );
            // A process under the same name, with limits.
            q.register(&reg("free", "v", 1, 0, false)).await.unwrap();
            assert!(q.unlimited_workers(LEASE).await.unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn only_live_workers_with_beam_slots_serve_documents() {
        for q in queues().await {
            assert_eq!(q.serving_documents(Some("v"), LEASE).await.unwrap(), 0);
            q.register(&reg("repl", "v", 2, 0, false)).await.unwrap();
            q.register(&reg("beam", "v", 2, 1, false)).await.unwrap();
            q.register(&reg("leaving", "v", 2, 1, false)).await.unwrap();
            q.register(&reg("other", "w", 2, 1, false)).await.unwrap();
            q.set_draining("leaving").await.unwrap();
            assert_eq!(q.serving_documents(Some("v"), LEASE).await.unwrap(), 1);
            assert_eq!(q.serving_documents(None, LEASE).await.unwrap(), 2);
            let stale = Duration::ZERO;
            assert_eq!(q.serving_documents(Some("v"), stale).await.unwrap(), 0);
        }
    }

    #[tokio::test]
    async fn the_registry_has_live_workers_and_their_claims() {
        for q in queues().await {
            q.register(&reg("w1", "v", 2, 1, false)).await.unwrap();
            q.register(&reg("w2", "v", 1, 0, false)).await.unwrap();
            q.register(&reg("x", "other", 4, 0, false)).await.unwrap();
            let id = q.enqueue(&job(Some("v"), 0)).await.unwrap();
            q.enqueue(&job(Some("v"), 1)).await.unwrap();
            // A job on a document takes none of its REPL slots.
            let on_doc = q.enqueue(&document_job("d")).await.unwrap();
            q.claim("w1", &[id, on_doc], LEASE).await.unwrap();
            q.set_warm("w2", &["k".into()]).await.unwrap();
            q.set_documents_open("w1", 3).await.unwrap();
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
                        draining: false,
                        beam_slots: 1,
                        documents_open: 3,
                    },
                    Peer {
                        name: "w2".into(),
                        slots: 1,
                        busy: 0,
                        warm: vec!["k".into()],
                        draining: true,
                        beam_slots: 0,
                        documents_open: 0,
                    },
                ]
            );
            assert_eq!(
                q.counts(LEASE).await.unwrap(),
                Counts {
                    queued: [0, 1],
                    running: 2,
                    workers: 3,
                    slots: 7
                }
            );
            q.unregister("w2").await.unwrap();
            assert_eq!(q.registry("v", LIVE).await.unwrap().len(), 1);
            assert!(q.registry("v", Duration::ZERO).await.unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn heartbeats_renew_only_this_registrations_claims() {
        for q in queues().await {
            let since = q.register(&reg("w", "v", 1, 0, false)).await.unwrap();
            let id = q.enqueue(&job(Some("v"), 1)).await.unwrap();
            q.claim("w", &[id], Duration::from_millis(1)).await.unwrap();
            q.heartbeat("w", since, LEASE).await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert!(ids(&q, "v").await.is_empty(), "renewed");

            // A new process under the same name doesn't run the old one's
            // jobs: they lapse, and its heartbeats don't renew them.
            let since = q.register(&reg("w", "v", 1, 0, false)).await.unwrap();
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
            q.complete("w", id, &json!(1)).await.unwrap();
            // Other tests' queues on a shared PostgreSQL notify too.
            while !heard.contains(&format!("{:?}", Notice::Done(id))) {
                heard.insert(format!("{:?}", next().await));
            }
            assert!(heard.contains(&format!("{:?}", Notice::Queued(q.name().into()))));
        }
    }
}
