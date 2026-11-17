// The queries for `Queue` (see `crate::queue`). Included by `backend.rs`
// once per backend.

use std::collections::HashMap;
use std::time::Duration;

use diesel::dsl::{case_when, exists, not};
use diesel::sql_types::BigInt;
use diesel::{
    BoolExpressionMethods, ExpressionMethods, NullableExpressionMethods, OptionalExtension,
    QueryDsl,
};
use diesel_async::RunQueryDsl;
use serde_json::Value;

use pisa_protocol::api::DependencyState;

use crate::catalog::now_micros;
use crate::db::{DbError, Notice, retry};
use crate::queue::{
    ASYNC_RETENTION, Candidate, Claimed, Counts, LIVE, LOST_CEILING, NewJob, Peer, Poll,
    RESULT_RETENTION, Registration, STALE_RETENTION, Stage, Status,
};
use crate::schema::{documents, jobs, workers};

/// How long a server's own giving up gets before a sweep deletes a job
/// nobody can claim any more.
const SWEEP_GRACE: Duration = Duration::from_secs(60);

fn now() -> i64 {
    now_micros() as i64
}

fn micros(d: Duration) -> i64 {
    d.as_micros() as i64
}

/// Whether a `workers` row is of a worker that could claim the job, as
/// `candidates` and the workers' rounds decide: of the job's version, or of
/// any for a job of none, with REPL slots for a job on no document, and Beam
/// slots for one on a document.
macro_rules! can_run {
    () => {
        jobs::version
            .is_null()
            .or(jobs::version.eq(workers::version.nullable()))
            .and(
                jobs::document
                    .is_null()
                    .and(workers::slots.gt(0))
                    .or(jobs::document.is_not_null().and(workers::beam_slots.gt(0))),
            )
    };
}

/// Whether an open job has waited out its time for a worker at `$now`:
/// `unclaimed`, counting from when it was enqueued, or `lapsed`, from when
/// its last claim lapsed.
///
/// A sync job waits its `max_wait`. An async one waits until a worker that
/// [`can_run!`] it, and takes jobs, was seen live for its `max_wait` past
/// then, without a break (see `workers.live_since_micros`), or until
/// [`LOST_CEILING`] after it was enqueued. So it outlasts a restart of every
/// worker that could run it, while its client, which isn't waiting on a
/// request, looks again later.
///
/// An unclaimed async job is waited out at its `max_wait` also when no
/// worker that could run it has a row, live or not: it is of a version no
/// worker runs, rather than one restarting, since a worker's row is kept
/// [`STALE_RETENTION`] after its last heartbeat, after a clean stop too.
/// A lapsed one had a worker that could, so it waits.
macro_rules! waited {
    (unclaimed, $now:expr) => {
        waited!(@ jobs::enqueued_micros, $now, or not(exists(
            workers::table
                .filter(workers::queue.eq(jobs::queue))
                .filter(can_run!()),
        )))
    };
    (lapsed, $now:expr) => {
        waited!(@ jobs::lease_micros, $now)
    };
    (@ $since:expr, $now:expr $(, or $otherwise:expr)?) => {{
        let due = $since.nullable() + jobs::max_wait_micros.nullable();
        // The max wait from when a worker came live.
        let worker_due = workers::live_since_micros + jobs::max_wait_micros;
        // A worker that could run it and takes jobs, which heartbeat after
        // both were up, and both are up by `$now`.
        let live = exists(
            workers::table
                .filter(workers::queue.eq(jobs::queue))
                .filter(can_run!())
                .filter(workers::draining.eq(false))
                .filter(worker_due.le(workers::seen_micros))
                .filter(worker_due.le($now))
                .filter(due.le(workers::seen_micros.nullable())),
        );
        let ceiling = (jobs::enqueued_micros + micros(LOST_CEILING)).le($now);
        jobs::token
            .is_null()
            .and(due.le($now))
            .or(jobs::token
                .is_not_null()
                .and(ceiling.or(due.le($now).and(live $(.or($otherwise))?))))
    }};
}

/// Jobs a worker may claim at `$now`: unclaimed ones that haven't
/// [`waited!`] since they were enqueued, and lapsed claims with attempts
/// left that haven't waited since they lapsed.
macro_rules! claimable {
    ($now:expr) => {
        jobs::worker
            .is_null()
            .and(not(waited!(unclaimed, $now)))
            .or(jobs::lease_micros
                .lt($now)
                .and(jobs::attempts.lt(jobs::max_attempts))
                .and(not(waited!(lapsed, $now))))
    };
}

/// Open jobs nobody may claim at `$now` any more: those without a live claim
/// that [`claimable!`] leaves out.
macro_rules! unclaimable {
    ($now:expr) => {
        jobs::worker
            .is_null()
            .and(waited!(unclaimed, $now))
            .or(jobs::lease_micros.lt($now).and(
                jobs::attempts
                    .ge(jobs::max_attempts)
                    .or(waited!(lapsed, $now)),
            ))
    };
}

/// A notification is only a wake-up: whoever waits also polls.
async fn wake(db: &Handle, conn: &mut Conn, notice: Notice) {
    if let Err(e) = notify(db, conn, notice).await {
        tracing::warn!("sending a queue notification failed: {e}");
    }
}

pub(crate) async fn enqueue(db: &Handle, q: &str, job: &NewJob) -> Result<i64, DbError> {
    Ok(insert(db, q, job, None).await?.0)
}

pub(crate) async fn enqueue_async(
    db: &Handle,
    q: &str,
    job: &NewJob,
    kind: &str,
) -> Result<(i64, String), DbError> {
    let (id, token) = insert(db, q, job, Some(kind)).await?;
    Ok((id, token.unwrap_or_default()))
}

/// Adds `job`, with a token for its client to read it back by if it is an
/// async job of `kind`.
async fn insert(
    db: &Handle,
    q: &str,
    job: &NewJob,
    kind: Option<&str>,
) -> Result<(i64, Option<String>), DbError> {
    retry(|| async {
        let mut c = conn(db).await?;
        let affinity = match (&job.version, &job.document) {
            (Some(v), Some(id)) => documents::table
                .find((v, id))
                .select(documents::holder)
                .first::<Option<String>>(&mut *c)
                .await
                .optional()?
                .flatten(),
            _ => None,
        };
        let token = kind.map(|_| uuid::Uuid::new_v4().simple().to_string());
        let id = diesel::insert_into(jobs::table)
            .values((
                jobs::queue.eq(q),
                jobs::token.eq(&token),
                jobs::kind.eq(kind),
                jobs::version.eq(&job.version),
                jobs::priority.eq(job.priority),
                jobs::header_key.eq(&job.header_key),
                jobs::header.eq(&job.header),
                jobs::affinity.eq(&affinity),
                jobs::document.eq(&job.document),
                jobs::job.eq(&job.job),
                jobs::enqueued_micros.eq(now()),
                jobs::max_wait_micros.eq(micros(job.max_wait)),
                jobs::max_attempts.eq(job.max_attempts as i32),
                jobs::labels.eq(crate::scope::stamp()),
            ))
            .returning(jobs::id)
            .get_result::<i64>(&mut *c)
            .await?;
        wake(db, &mut c, Notice::Queued(q.to_string())).await;
        Ok((id, token))
    })
    .await
}

/// An open job's claim: its worker, lease, attempts, max attempts, when it
/// was enqueued, and its max wait.
type Open = (Option<String>, Option<i64>, i32, i32, i64, i64);

/// When an open sync job stops being claimable, as [`claimable!`] decides
/// it: `None` while a worker holds a live claim. A lapsed claim with no
/// attempts left is due at once. An async job may be given up then at the
/// earliest; see [`waited!`].
fn give_up_at(now: i64, open: &Open) -> Option<i64> {
    let (worker, lease, attempts, max_attempts, enqueued, max_wait) = open;
    match (worker, lease) {
        (None, _) => Some(enqueued + max_wait),
        (Some(_), Some(lease)) if *lease >= now => None,
        (Some(_), lease) => {
            let lapsed = lease.unwrap_or(now);
            Some(if attempts < max_attempts {
                lapsed + max_wait
            } else {
                lapsed
            })
        }
    }
}

pub(crate) async fn poll(db: &Handle, id: i64) -> Result<Poll, DbError> {
    retry(|| async {
        let mut c = conn(db).await?;
        let done = diesel::delete(jobs::table.find(id).filter(jobs::result.is_not_null()))
            .returning(jobs::result)
            .get_result::<Option<Value>>(&mut *c)
            .await
            .optional()?;
        if let Some(result) = done {
            return Ok(Poll::Done(result.unwrap_or_default()));
        }
        let open = jobs::table
            .find(id)
            .select((
                jobs::worker,
                jobs::lease_micros,
                jobs::attempts,
                jobs::max_attempts,
                jobs::enqueued_micros,
                jobs::max_wait_micros,
            ))
            .first::<Open>(&mut *c)
            .await
            .optional()?;
        let Some(open) = open else {
            return Ok(Poll::Gone);
        };
        let now = now();
        match give_up_at(now, &open) {
            None => return Ok(Poll::Waiting { give_up_at: None }),
            // The server gives an unclaimed job up itself.
            Some(at) if open.0.is_none() || at > now => {
                return Ok(Poll::Waiting {
                    give_up_at: Some(at),
                });
            }
            Some(_) => {}
        }
        let attempts = open.2;
        // Nobody may claim it again. Unless it was completed meanwhile, it
        // is lost.
        let taken = diesel::delete(
            jobs::table
                .find(id)
                .filter(jobs::result.is_null())
                .filter(jobs::lease_micros.lt(now)),
        )
        .execute(&mut *c)
        .await?;
        Ok(if taken == 1 {
            Poll::Lost {
                attempts: attempts as u32,
                exhausted: attempts >= open.3,
            }
        } else {
            Poll::Waiting { give_up_at: None }
        })
    })
    .await
}

/// An async job's row: its id and kind, its result, its claim, and whether
/// it is [`unclaimable!`].
type Row = (
    i64,
    Option<String>,
    Option<Value>,
    Option<String>,
    Option<i64>,
    i32,
    i32,
    i64,
    i64,
    Option<bool>,
);

/// Whether the job of `token` is in the request's scope: its token alone
/// finds it otherwise.
async fn token_in_scope(db: &Handle, q: &str, token: &str) -> Result<bool, DbError> {
    if crate::scope::current().is_empty() {
        return Ok(true);
    }
    let labels: Option<String> = jobs::table
        .filter(jobs::queue.eq(q))
        .filter(jobs::token.eq(token))
        .select(jobs::labels)
        .first(&mut *conn(db).await?)
        .await
        .optional()?;
    Ok(labels.is_some_and(|l| crate::scope::in_scope(&l)))
}

pub(crate) async fn status(db: &Handle, q: &str, token: &str) -> Result<Option<Status>, DbError> {
    if !token_in_scope(db, q, token).await? {
        return Ok(None);
    }
    retry(|| async {
        let mut c = conn(db).await?;
        // Closing it races the workers, other servers and the sweep: whoever
        // loses reads it again, and finds what the winner made of it.
        for _ in 0..3 {
            let now = now();
            let row = jobs::table
                .filter(jobs::queue.eq(q))
                .filter(jobs::token.eq(token))
                .select((
                    jobs::id,
                    jobs::kind,
                    jobs::result,
                    jobs::worker,
                    jobs::lease_micros,
                    jobs::attempts,
                    jobs::max_attempts,
                    jobs::enqueued_micros,
                    jobs::max_wait_micros,
                    unclaimable!(now),
                ))
                .first::<Row>(&mut *c)
                .await
                .optional()?;
            let Some(row) = row else {
                return Ok(None);
            };
            let (id, kind, result, worker, lease, attempts, max_attempts, enqueued, max_wait, lost) =
                row;
            let open = (worker, lease, attempts, max_attempts, enqueued, max_wait);
            let stage = match result {
                Some(Value::Null) => Stage::GaveUp {
                    attempts: attempts as u32,
                    exhausted: attempts >= max_attempts,
                },
                Some(result) => Stage::Done(result),
                None if lost == Some(true) => {
                    let closed = diesel::update(
                        jobs::table
                            .find(id)
                            .filter(jobs::result.is_null())
                            .filter(unclaimable!(now)),
                    )
                    .set((jobs::result.eq(&Value::Null), jobs::done_micros.eq(now)))
                    .returning(jobs::attempts)
                    .get_result::<i32>(&mut *c)
                    .await
                    .optional()?;
                    let Some(attempts) = closed else { continue };
                    wake(db, &mut c, Notice::Done(id)).await;
                    Stage::GaveUp {
                        attempts: attempts as u32,
                        exhausted: attempts >= max_attempts,
                    }
                }
                None => match give_up_at(now, &open) {
                    None => Stage::Running,
                    // Past its max wait, it waits for a worker to come back.
                    Some(at) => {
                        let ceiling = enqueued + micros(LOST_CEILING);
                        Stage::Queued {
                            give_up_at: if at > now { at.min(ceiling) } else { ceiling },
                        }
                    }
                },
            };
            let kind = kind.unwrap_or_default();
            return Ok(Some(Status { id, kind, stage }));
        }
        // It kept changing as it was read: run it again.
        Err(DbError::Conflict)
    })
    .await
}

pub(crate) async fn cancel(db: &Handle, q: &str, token: &str) -> Result<Option<i64>, DbError> {
    if !token_in_scope(db, q, token).await? {
        return Ok(None);
    }
    let now = now();
    retry(|| async {
        let mut c = conn(db).await?;
        let id = diesel::delete(
            jobs::table
                .filter(jobs::queue.eq(q))
                .filter(jobs::token.eq(token))
                .filter(jobs::result.is_null())
                .filter(jobs::worker.is_null().or(jobs::lease_micros.lt(now))),
        )
        .returning(jobs::id)
        .get_result::<i64>(&mut *c)
        .await
        .optional()?;
        if let Some(id) = id {
            wake(db, &mut c, Notice::Done(id)).await;
        }
        Ok(id)
    })
    .await
}

pub(crate) async fn give_up(db: &Handle, id: i64) -> Result<Option<(u32, bool)>, DbError> {
    let now = now();
    retry(|| async {
        let attempts = diesel::delete(
            jobs::table
                .find(id)
                .filter(jobs::result.is_null())
                .filter(jobs::worker.is_null().or(jobs::lease_micros.lt(now))),
        )
        .returning((jobs::attempts, jobs::max_attempts))
        .get_result::<(i32, i32)>(&mut *conn(db).await?)
        .await
        .optional()?;
        Ok(attempts.map(|(a, max)| (a as u32, a >= max)))
    })
    .await
}

pub(crate) async fn counts(db: &Handle, q: &str, stale: Duration) -> Result<Counts, DbError> {
    let now = now();
    let mut c = conn(db).await?;
    let mut counts = Counts::default();
    let queued = jobs::table
        .filter(jobs::queue.eq(q))
        .filter(jobs::result.is_null())
        .filter(jobs::worker.is_null())
        .group_by(jobs::priority)
        .select((jobs::priority, diesel::dsl::count_star()))
        .load::<(i16, i64)>(&mut *c)
        .await?;
    for (priority, n) in queued {
        counts.queued[(priority.clamp(0, 1)) as usize] += n as u64;
    }
    counts.running = jobs::table
        .filter(jobs::queue.eq(q))
        .filter(jobs::result.is_null())
        .filter(jobs::lease_micros.ge(now))
        .count()
        .get_result::<i64>(&mut *c)
        .await? as u64;
    let slots = workers::table
        .filter(workers::queue.eq(q))
        .filter(workers::seen_micros.ge(now - micros(stale)))
        .select(workers::slots)
        .load::<i32>(&mut *c)
        .await?;
    counts.workers = slots.len() as u64;
    counts.slots = slots.iter().map(|&s| s as u64).sum();
    Ok(counts)
}

pub(crate) async fn unlimited_workers(
    db: &Handle,
    q: &str,
    stale: Duration,
) -> Result<Vec<String>, DbError> {
    let names = workers::table
        .filter(workers::queue.eq(q))
        .filter(workers::seen_micros.ge(now() - micros(stale)))
        .filter(workers::unlimited.eq(true))
        .order(workers::name)
        .select(workers::name)
        .load(&mut *conn(db).await?)
        .await?;
    Ok(names)
}

pub(crate) async fn live_revisions(
    db: &Handle,
    q: &str,
    stale: Duration,
) -> Result<Vec<String>, DbError> {
    Ok(workers::table
        .filter(workers::queue.eq(q))
        .filter(workers::seen_micros.ge(now() - micros(stale)))
        .select(workers::revision)
        .distinct()
        .order(workers::revision)
        .load::<String>(&mut *conn(db).await?)
        .await?)
}

pub(crate) async fn report_store(
    db: &Handle,
    q: &str,
    worker: &str,
    reachable: bool,
) -> Result<(), DbError> {
    let now = now();
    retry(|| async {
        diesel::update(workers::table.find((q, worker)))
            .set((
                workers::store_reachable.eq(Some(reachable)),
                workers::store_checked_micros.eq(Some(now)),
            ))
            .execute(&mut *conn(db).await?)
            .await?;
        Ok(())
    })
    .await
}

pub(crate) async fn store_state(
    db: &Handle,
    q: &str,
    stale: Duration,
) -> Result<DependencyState, DbError> {
    let reports = workers::table
        .filter(workers::queue.eq(q))
        .filter(workers::seen_micros.ge(now() - micros(stale)))
        .filter(workers::store_reachable.is_not_null())
        .select(workers::store_reachable)
        .distinct()
        .load::<Option<bool>>(&mut *conn(db).await?)
        .await?;
    Ok(if reports.contains(&Some(false)) {
        DependencyState::Unreachable
    } else if reports.is_empty() {
        DependencyState::Unknown
    } else {
        DependencyState::Ok
    })
}

pub(crate) async fn sweep_jobs(db: &Handle, q: &str, now: i64) -> Result<usize, DbError> {
    let cutoff = now - micros(SWEEP_GRACE);
    retry(|| async {
        let mut c = conn(db).await?;
        let results = diesel::delete(
            jobs::table
                .filter(jobs::queue.eq(q))
                .filter(jobs::token.is_null())
                .filter(jobs::done_micros.lt(now - micros(RESULT_RETENTION))),
        )
        .execute(&mut *c)
        .await?;
        let kept = diesel::delete(
            jobs::table
                .filter(jobs::queue.eq(q))
                .filter(jobs::token.is_not_null())
                .filter(jobs::done_micros.lt(now - micros(ASYNC_RETENTION))),
        )
        .execute(&mut *c)
        .await?;
        let unclaimable = diesel::delete(
            jobs::table
                .filter(jobs::queue.eq(q))
                .filter(jobs::token.is_null())
                .filter(jobs::result.is_null())
                .filter(unclaimable!(cutoff)),
        )
        .execute(&mut *c)
        .await?;
        // An async job's client reads that it was given up.
        let closed = diesel::update(
            jobs::table
                .filter(jobs::queue.eq(q))
                .filter(jobs::token.is_not_null())
                .filter(jobs::result.is_null())
                .filter(unclaimable!(cutoff)),
        )
        .set((jobs::result.eq(&Value::Null), jobs::done_micros.eq(now)))
        .execute(&mut *c)
        .await?;
        let workers = diesel::delete(
            workers::table
                .filter(workers::queue.eq(q))
                .filter(workers::seen_micros.lt(now - micros(STALE_RETENTION))),
        )
        .execute(&mut *c)
        .await?;
        Ok(results + kept + unclaimable + closed + workers)
    })
    .await
}

pub(crate) async fn register(
    db: &Handle,
    q: &str,
    r: &Registration,
) -> Result<i64, DbError> {
    let now = now();
    retry(|| async {
        let mut c = conn(db).await?;
        let row = || {
            (
                workers::version.eq(&r.version),
                workers::slots.eq(r.slots as i32),
                workers::beam_slots.eq(r.beam_slots as i32),
                workers::unlimited.eq(r.unlimited),
                workers::revision.eq(&r.revision),
                workers::store_reachable.eq(None::<bool>),
                workers::store_checked_micros.eq(None::<i64>),
                workers::warm.eq(Value::Array(Vec::new())),
                workers::documents_open.eq(0),
                workers::draining.eq(false),
                workers::seen_micros.eq(now),
                workers::live_since_micros.eq(now),
            )
        };
        diesel::insert_into(workers::table)
            .values((workers::queue.eq(q), workers::name.eq(&r.worker), row()))
            .on_conflict((workers::queue, workers::name))
            .do_update()
            .set(row())
            .execute(&mut *c)
            .await?;
        diesel::update(
            jobs::table
                .filter(jobs::queue.eq(q))
                .filter(jobs::worker.eq(&r.worker))
                .filter(jobs::result.is_null()),
        )
        .set(jobs::lease_micros.eq(now - 1))
        .execute(&mut *c)
        .await?;
        Ok(now)
    })
    .await
}

pub(crate) async fn candidates(
    db: &Handle,
    q: &str,
    version: &str,
    limit: usize,
    documents: bool,
) -> Result<Vec<Candidate>, DbError> {
    let now = now();
    let rows = jobs::table
        .filter(jobs::queue.eq(q))
        .filter(jobs::result.is_null())
        .filter(jobs::version.eq(version).or(jobs::version.is_null()))
        .filter(jobs::document.is_not_null().eq(documents))
        .filter(claimable!(now))
        .order((jobs::priority, jobs::id))
        .limit(limit as i64)
        .select((
            jobs::id,
            jobs::priority,
            jobs::header_key,
            jobs::affinity,
            jobs::enqueued_micros,
            jobs::document,
        ))
        .load::<(i64, i16, Option<String>, Option<String>, i64, Option<String>)>(
            &mut *conn(db).await?,
        )
        .await?;
    Ok(rows
        .into_iter()
        .map(
            |(id, priority, header_key, affinity, enqueued_micros, document)| Candidate {
                id,
                priority,
                header_key,
                affinity,
                enqueued_micros,
                document,
            },
        )
        .collect())
}

pub(crate) async fn registry(
    db: &Handle,
    q: &str,
    version: &str,
    stale: Duration,
) -> Result<Vec<Peer>, DbError> {
    let now = now();
    let mut c = conn(db).await?;
    let busy: HashMap<String, i64> = jobs::table
        .filter(jobs::queue.eq(q))
        .filter(jobs::result.is_null())
        .filter(jobs::document.is_null())
        .filter(jobs::lease_micros.ge(now))
        .group_by(jobs::worker)
        .select((jobs::worker, diesel::dsl::count_star()))
        .load::<(Option<String>, i64)>(&mut *c)
        .await?
        .into_iter()
        .filter_map(|(w, n)| Some((w?, n)))
        .collect();
    let rows = workers::table
        .filter(workers::queue.eq(q))
        .filter(workers::version.eq(version))
        .filter(workers::seen_micros.ge(now - micros(stale)))
        .select((
            workers::name,
            workers::slots,
            workers::warm,
            workers::draining,
            workers::beam_slots,
            workers::documents_open,
        ))
        .load::<(String, i32, Value, bool, i32, i32)>(&mut *c)
        .await?;
    Ok(rows
        .into_iter()
        .map(|(name, slots, warm, draining, beam_slots, open)| Peer {
            busy: busy.get(&name).copied().unwrap_or(0) as u32,
            name,
            slots: slots as u32,
            warm: serde_json::from_value(warm).unwrap_or_default(),
            draining,
            beam_slots: beam_slots as u32,
            documents_open: open as u32,
        })
        .collect())
}

pub(crate) async fn live_versions(
    db: &Handle,
    q: &str,
    stale: Duration,
) -> Result<Vec<String>, DbError> {
    let now = now();
    Ok(workers::table
        .filter(workers::queue.eq(q))
        .filter(workers::seen_micros.ge(now - micros(stale)))
        .select(workers::version)
        .distinct()
        .order(workers::version)
        .load::<String>(&mut *conn(db).await?)
        .await?)
}

pub(crate) async fn serving_documents(
    db: &Handle,
    q: &str,
    version: Option<&str>,
    stale: Duration,
) -> Result<u64, DbError> {
    let now = now();
    let mut serving = workers::table
        .filter(workers::queue.eq(q))
        .filter(workers::beam_slots.gt(0))
        .filter(workers::draining.eq(false))
        .filter(workers::seen_micros.ge(now - micros(stale)))
        .into_boxed();
    if let Some(version) = version {
        serving = serving.filter(workers::version.eq(version));
    }
    Ok(serving
        .count()
        .get_result::<i64>(&mut *conn(db).await?)
        .await? as u64)
}

pub(crate) async fn claim(
    db: &Handle,
    q: &str,
    worker: &str,
    ids: &[i64],
    lease: Duration,
) -> Result<Vec<Claimed>, DbError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let now = now();
    retry(|| async {
        let rows = diesel::update(
            jobs::table
                .filter(jobs::queue.eq(q))
                .filter(jobs::id.eq_any(ids.to_vec()))
                .filter(jobs::result.is_null())
                .filter(claimable!(now)),
        )
        .set((
            jobs::worker.eq(worker),
            jobs::claimed_micros.eq(now),
            jobs::lease_micros.eq(now + micros(lease)),
            jobs::attempts.eq(jobs::attempts + 1),
        ))
        .returning((jobs::id, jobs::job, jobs::enqueued_micros))
        .get_results::<(i64, Value, i64)>(&mut *conn(db).await?)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, job, enqueued_micros)| Claimed {
                id,
                job,
                enqueued_micros,
            })
            .collect())
    })
    .await
}

pub(crate) async fn complete(
    db: &Handle,
    worker: &str,
    id: i64,
    result: &Value,
) -> Result<bool, DbError> {
    let now = now();
    retry(|| async {
        let mut c = conn(db).await?;
        let n = diesel::update(
            jobs::table
                .find(id)
                .filter(jobs::worker.eq(worker))
                .filter(jobs::result.is_null()),
        )
        .set((jobs::result.eq(result), jobs::done_micros.eq(now)))
        .execute(&mut *c)
        .await?;
        if n == 1 {
            wake(db, &mut c, Notice::Done(id)).await;
        }
        Ok(n == 1)
    })
    .await
}

pub(crate) async fn heartbeat(
    db: &Handle,
    q: &str,
    worker: &str,
    since: i64,
    lease: Duration,
    now: i64,
) -> Result<(), DbError> {
    retry(|| async {
        let mut c = conn(db).await?;
        diesel::update(
            jobs::table
                .filter(jobs::queue.eq(q))
                .filter(jobs::worker.eq(worker))
                .filter(jobs::result.is_null())
                .filter(jobs::claimed_micros.ge(since)),
        )
        .set(jobs::lease_micros.eq(now + micros(lease)))
        .execute(&mut *c)
        .await?;
        // Back from a break longer than the live window, it is live anew.
        let gap = workers::seen_micros.lt(now - micros(LIVE));
        diesel::update(workers::table.find((q, worker)))
            .set((
                workers::live_since_micros
                    .eq(case_when::<_, _, BigInt>(gap, now).otherwise(workers::live_since_micros)),
                workers::seen_micros.eq(now),
            ))
            .execute(&mut *c)
            .await?;
        Ok(())
    })
    .await
}

pub(crate) async fn set_warm(
    db: &Handle,
    q: &str,
    worker: &str,
    warm: &[String],
) -> Result<(), DbError> {
    let warm = Value::from(warm.to_vec());
    retry(|| async {
        diesel::update(workers::table.find((q, worker)))
            .set(workers::warm.eq(&warm))
            .execute(&mut *conn(db).await?)
            .await?;
        Ok(())
    })
    .await
}

pub(crate) async fn set_documents_open(
    db: &Handle,
    q: &str,
    worker: &str,
    open: u32,
) -> Result<(), DbError> {
    retry(|| async {
        diesel::update(workers::table.find((q, worker)))
            .set(workers::documents_open.eq(open as i32))
            .execute(&mut *conn(db).await?)
            .await?;
        Ok(())
    })
    .await
}

pub(crate) async fn set_draining(db: &Handle, q: &str, worker: &str) -> Result<(), DbError> {
    retry(|| async {
        diesel::update(workers::table.find((q, worker)))
            .set(workers::draining.eq(true))
            .execute(&mut *conn(db).await?)
            .await?;
        Ok(())
    })
    .await
}

pub(crate) async fn unregister(db: &Handle, q: &str, worker: &str) -> Result<(), DbError> {
    let now = now();
    retry(|| async {
        // Last seen just outside the live window: no live read counts it,
        // and the sweep removes it an hour on.
        diesel::update(workers::table.find((q, worker)))
            .set((
                workers::draining.eq(true),
                workers::seen_micros.eq(now - micros(LIVE) - 1),
            ))
            .execute(&mut *conn(db).await?)
            .await?;
        Ok(())
    })
    .await
}

pub(crate) async fn popular_headers(
    db: &Handle,
    q: &str,
    version: &str,
    n: usize,
) -> Result<Vec<String>, DbError> {
    let mut rows = jobs::table
        .filter(jobs::queue.eq(q))
        .filter(jobs::version.eq(version))
        .filter(jobs::result.is_null())
        .filter(jobs::header.is_not_null())
        .group_by(jobs::header)
        .select((jobs::header, diesel::dsl::count_star()))
        .load::<(Option<String>, i64)>(&mut *conn(db).await?)
        .await?;
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Ok(rows.into_iter().filter_map(|(h, _)| h).take(n).collect())
}
