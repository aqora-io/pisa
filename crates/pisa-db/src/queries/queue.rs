// The queries for `Queue` (see `crate::queue`). Included by `backend.rs`
// once per backend.

use std::collections::HashMap;
use std::time::Duration;

use diesel::{
    BoolExpressionMethods, ExpressionMethods, NullableExpressionMethods, OptionalExtension,
    QueryDsl,
};
use diesel_async::RunQueryDsl;
use serde_json::Value;

use crate::catalog::now_micros;
use crate::db::{DbError, Notice, retry};
use crate::queue::{
    Candidate, Claimed, Counts, Done, NewJob, Peer, Poll, RESULT_RETENTION, STALE_RETENTION,
};
use crate::schema::{jobs, states, workers};

/// How long a server's own giving up gets before a sweep deletes a job
/// nobody can claim any more.
const SWEEP_GRACE: Duration = Duration::from_secs(60);

fn now() -> i64 {
    now_micros() as i64
}

fn micros(d: Duration) -> i64 {
    d.as_micros() as i64
}

/// Jobs a worker may claim at `$now`: unclaimed ones that haven't waited
/// their `max_wait`, and lapsed claims with attempts left that haven't
/// waited it since.
macro_rules! claimable {
    ($now:expr) => {
        jobs::worker
            .is_null()
            .and((jobs::enqueued_micros + jobs::max_wait_micros).gt($now))
            .or(jobs::lease_micros
                .lt($now)
                .and(jobs::attempts.lt(jobs::max_attempts))
                .and((jobs::lease_micros + jobs::max_wait_micros.nullable()).gt($now)))
    };
}

/// A notification is only a wake-up: whoever waits also polls.
async fn wake(db: &Handle, conn: &mut Conn, notice: Notice) {
    if let Err(e) = notify(db, conn, notice).await {
        tracing::warn!("sending a queue notification failed: {e}");
    }
}

pub(crate) async fn enqueue(db: &Handle, q: &str, job: &NewJob) -> Result<i64, DbError> {
    retry(|| async {
        let mut c = conn(db).await?;
        let affinity = match &job.state {
            Some((v, hash)) => states::table
                .find((v, hash))
                .select(states::holder)
                .first::<Option<String>>(&mut *c)
                .await
                .optional()?
                .flatten(),
            None => None,
        };
        let id = diesel::insert_into(jobs::table)
            .values((
                jobs::queue.eq(q),
                jobs::version.eq(&job.version),
                jobs::priority.eq(job.priority),
                jobs::header_key.eq(&job.header_key),
                jobs::header.eq(&job.header),
                jobs::affinity.eq(&affinity),
                jobs::job.eq(&job.job),
                jobs::enqueued_micros.eq(now()),
                jobs::max_wait_micros.eq(micros(job.max_wait)),
                jobs::max_attempts.eq(job.max_attempts as i32),
            ))
            .returning(jobs::id)
            .get_result::<i64>(&mut *c)
            .await?;
        wake(db, &mut c, Notice::Queued(q.to_string())).await;
        Ok(id)
    })
    .await
}

type Status = (Option<String>, Option<i64>, i32, i32, i64, i64);

pub(crate) async fn poll(db: &Handle, id: i64) -> Result<Poll, DbError> {
    retry(|| async {
        let mut c = conn(db).await?;
        let done = diesel::delete(jobs::table.find(id).filter(jobs::result.is_not_null()))
            .returning((
                jobs::result,
                jobs::worker,
                jobs::attempts,
                jobs::enqueued_micros,
                jobs::claimed_micros,
                jobs::restarts,
            ))
            .get_result::<(
                Option<Value>,
                Option<String>,
                i32,
                i64,
                Option<i64>,
                Option<Value>,
            )>(&mut *c)
            .await
            .optional()?;
        if let Some((result, worker, attempts, enqueued, claimed, restarts)) = done {
            let waited = claimed.unwrap_or(enqueued).saturating_sub(enqueued).max(0);
            return Ok(Poll::Done(Done {
                result: result.unwrap_or_default(),
                worker: worker.unwrap_or_default(),
                attempts: attempts as u32,
                waited: Duration::from_micros(waited as u64),
                restarts: restarts
                    .and_then(|r| serde_json::from_value(r).ok())
                    .unwrap_or_default(),
            }));
        }
        let status = jobs::table
            .find(id)
            .select((
                jobs::worker,
                jobs::lease_micros,
                jobs::attempts,
                jobs::max_attempts,
                jobs::enqueued_micros,
                jobs::max_wait_micros,
            ))
            .first::<Status>(&mut *c)
            .await
            .optional()?;
        let Some((worker, lease, attempts, max_attempts, enqueued, max_wait)) = status else {
            return Ok(Poll::Gone);
        };
        let now = now();
        let lapsed = match (worker, lease) {
            (None, _) => {
                return Ok(Poll::Waiting {
                    give_up_at: Some(enqueued + max_wait),
                });
            }
            (Some(_), Some(lease)) if lease >= now => {
                return Ok(Poll::Waiting { give_up_at: None });
            }
            (Some(_), lease) => lease.unwrap_or(now),
        };
        if attempts < max_attempts && lapsed + max_wait > now {
            return Ok(Poll::Waiting {
                give_up_at: Some(lapsed + max_wait),
            });
        }
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
            }
        } else {
            Poll::Waiting { give_up_at: None }
        })
    })
    .await
}

pub(crate) async fn give_up(db: &Handle, id: i64) -> Result<Option<u32>, DbError> {
    let now = now();
    retry(|| async {
        let attempts = diesel::delete(
            jobs::table
                .find(id)
                .filter(jobs::result.is_null())
                .filter(jobs::worker.is_null().or(jobs::lease_micros.lt(now))),
        )
        .returning(jobs::attempts)
        .get_result::<i32>(&mut *conn(db).await?)
        .await
        .optional()?;
        Ok(attempts.map(|a| a as u32))
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

pub(crate) async fn sweep_jobs(db: &Handle, q: &str) -> Result<usize, DbError> {
    let now = now();
    let cutoff = now - micros(SWEEP_GRACE);
    retry(|| async {
        let mut c = conn(db).await?;
        let results = diesel::delete(
            jobs::table
                .filter(jobs::queue.eq(q))
                .filter(jobs::done_micros.lt(now - micros(RESULT_RETENTION))),
        )
        .execute(&mut *c)
        .await?;
        let unclaimable = diesel::delete(
            jobs::table
                .filter(jobs::queue.eq(q))
                .filter(jobs::result.is_null())
                .filter(
                    jobs::worker
                        .is_null()
                        .and((jobs::enqueued_micros + jobs::max_wait_micros).lt(cutoff))
                        .or(jobs::lease_micros.lt(cutoff).and(
                            jobs::attempts
                                .ge(jobs::max_attempts)
                                .or((jobs::lease_micros + jobs::max_wait_micros.nullable())
                                    .lt(cutoff)),
                        )),
                ),
        )
        .execute(&mut *c)
        .await?;
        let workers = diesel::delete(
            workers::table
                .filter(workers::queue.eq(q))
                .filter(workers::seen_micros.lt(now - micros(STALE_RETENTION))),
        )
        .execute(&mut *c)
        .await?;
        Ok(results + unclaimable + workers)
    })
    .await
}

pub(crate) async fn register(
    db: &Handle,
    q: &str,
    worker: &str,
    version: &str,
    slots: u32,
) -> Result<i64, DbError> {
    let now = now();
    retry(|| async {
        let mut c = conn(db).await?;
        let row = || {
            (
                workers::version.eq(version),
                workers::slots.eq(slots as i32),
                workers::warm.eq(Value::Array(Vec::new())),
                workers::draining.eq(false),
                workers::seen_micros.eq(now),
            )
        };
        diesel::insert_into(workers::table)
            .values((workers::queue.eq(q), workers::name.eq(worker), row()))
            .on_conflict((workers::queue, workers::name))
            .do_update()
            .set(row())
            .execute(&mut *c)
            .await?;
        diesel::update(
            jobs::table
                .filter(jobs::queue.eq(q))
                .filter(jobs::worker.eq(worker))
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
) -> Result<Vec<Candidate>, DbError> {
    let now = now();
    let rows = jobs::table
        .filter(jobs::queue.eq(q))
        .filter(jobs::result.is_null())
        .filter(jobs::version.eq(version).or(jobs::version.is_null()))
        .filter(claimable!(now))
        .order((jobs::priority, jobs::id))
        .limit(limit as i64)
        .select((
            jobs::id,
            jobs::priority,
            jobs::header_key,
            jobs::affinity,
            jobs::enqueued_micros,
        ))
        .load::<(i64, i16, Option<String>, Option<String>, i64)>(&mut *conn(db).await?)
        .await?;
    Ok(rows
        .into_iter()
        .map(
            |(id, priority, header_key, affinity, enqueued_micros)| Candidate {
                id,
                priority,
                header_key,
                affinity,
                enqueued_micros,
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
        ))
        .load::<(String, i32, Value, bool)>(&mut *c)
        .await?;
    Ok(rows
        .into_iter()
        .map(|(name, slots, warm, draining)| Peer {
            busy: busy.get(&name).copied().unwrap_or(0) as u32,
            name,
            slots: slots as u32,
            warm: serde_json::from_value(warm).unwrap_or_default(),
            draining,
        })
        .collect())
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
        .returning((jobs::id, jobs::job))
        .get_results::<(i64, Value)>(&mut *conn(db).await?)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, job)| Claimed { id, job })
            .collect())
    })
    .await
}

pub(crate) async fn complete(
    db: &Handle,
    worker: &str,
    id: i64,
    result: &Value,
    restarts: &[String],
) -> Result<bool, DbError> {
    let now = now();
    let restarts = (!restarts.is_empty()).then(|| Value::from(restarts.to_vec()));
    retry(|| async {
        let mut c = conn(db).await?;
        let n = diesel::update(
            jobs::table
                .find(id)
                .filter(jobs::worker.eq(worker))
                .filter(jobs::result.is_null()),
        )
        .set((
            jobs::result.eq(result),
            jobs::restarts.eq(&restarts),
            jobs::done_micros.eq(now),
        ))
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
) -> Result<(), DbError> {
    let now = now();
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
        diesel::update(workers::table.find((q, worker)))
            .set(workers::seen_micros.eq(now))
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
    retry(|| async {
        diesel::delete(workers::table.find((q, worker)))
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
