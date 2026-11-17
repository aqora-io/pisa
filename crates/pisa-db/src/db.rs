//! The metadata database: PostgreSQL in deployments, SQLite for tests and
//! single-process runs. Both have the same tables (see `migrations/`), which
//! [`Db::connect`] creates or updates.
//!
//! Transactions that write are serializable: on PostgreSQL a transaction that
//! raced another fails with [`DbError::Conflict`], and [`retry`] runs it
//! again. SQLite takes the database's write lock when such a transaction
//! begins, so they run one at a time.

use std::sync::{Arc, Weak};
use std::time::Duration;

use diesel::connection::SimpleConnection;
use diesel::{Connection, SqliteConnection};
use diesel_async::AsyncPgConnection;
use diesel_async::async_connection_wrapper::AsyncConnectionWrapper;
use diesel_async::pooled_connection::deadpool::Pool;
use diesel_async::pooled_connection::{AsyncDieselConnectionManager, ManagerConfig};
use diesel_async::sync_connection_wrapper::SyncConnectionWrapper;
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};
use futures::StreamExt;
use tokio::sync::{Mutex, OnceCell, broadcast};
use tokio_postgres_rustls::MakeRustlsConnect;

/// The most times [`retry`] runs an attempt: enough that its sleeps come to
/// about [`RETRY_BUDGET`] even when the jitter keeps them all short.
const MAX_ATTEMPTS: usize = 16;

/// [`retry`]'s first sleep; each next one is twice as long, up to
/// [`MAX_BACKOFF`].
const FIRST_BACKOFF: Duration = Duration::from_millis(10);

const MAX_BACKOFF: Duration = Duration::from_secs(1);

/// The most [`retry`] sleeps in all.
const RETRY_BUDGET: Duration = Duration::from_secs(5);

/// The most connections one process opens to PostgreSQL: workers scale out,
/// and PostgreSQL allows 100 connections by default.
const POOL_SIZE: usize = 8;

/// How long a request waits for a PostgreSQL connection, or for one to open,
/// before it fails as unavailable.
const POOL_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the check of a pooled PostgreSQL connection, before it is handed
/// out, may take; past it the connection is dropped and another is used.
const POOL_RECYCLE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a SQLite statement waits for another process's lock before it
/// fails as a conflict, which [`retry`] runs again.
const SQLITE_BUSY_MILLIS: u32 = 250;

/// The key of the PostgreSQL advisory lock held while migrating, so that
/// processes starting together migrate one at a time.
const MIGRATION_LOCK: i64 = 0x7069_7361_6462; // "pisadb"

const PG_MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations/postgres");
const SQLITE_MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations/sqlite");

/// How long [`Db::notices`] waits for PostgreSQL's `LISTEN` before going on
/// without it, when waiters only poll.
const LISTEN_TIMEOUT: Duration = Duration::from_secs(5);

/// The channels of the queue's notifications: a job was queued (the payload
/// is its queue), or one is done (its id).
pub(crate) const QUEUED: &str = "pisa_jobs";
pub(crate) const DONE: &str = "pisa_done";

pub(crate) type PgPool = Pool<AsyncPgConnection>;

/// PostgreSQL's connections, and what this process hears through `LISTEN`.
pub struct PgDb {
    pub(crate) pool: PgPool,
    pub(crate) notices: broadcast::Sender<Notice>,
    /// Set once this process listens for notifications.
    listening: OnceCell<()>,
}

/// One connection, which keeps an in-memory database alive and runs one
/// query at a time, and the notices of this process's queue writes: SQLite
/// has no `NOTIFY`.
pub struct SqliteDb {
    pub(crate) conn: Mutex<SyncConnectionWrapper<SqliteConnection>>,
    pub(crate) notices: broadcast::Sender<Notice>,
}

/// A notification from the job queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    /// Jobs were queued on this queue.
    Queued(String),
    /// This job is done.
    Done(i64),
}

impl Notice {
    pub(crate) fn channel_and_payload(&self) -> (&'static str, String) {
        match self {
            Notice::Queued(queue) => (QUEUED, queue.clone()),
            Notice::Done(id) => (DONE, id.to_string()),
        }
    }

    fn parse(channel: &str, payload: &str) -> Option<Notice> {
        match channel {
            QUEUED => Some(Notice::Queued(payload.to_string())),
            DONE => payload.parse().ok().map(Notice::Done),
            _ => None,
        }
    }
}

fn notices() -> broadcast::Sender<Notice> {
    broadcast::channel(1024).0
}

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// Another transaction changed what this one read or wrote; run it again.
    #[error("transaction conflict")]
    Conflict,
    #[error("{0}")]
    Other(String),
}

/// Whether an error means the transaction lost a race and may be retried.
pub trait IsConflict {
    fn is_conflict(&self) -> bool;
}

impl IsConflict for DbError {
    fn is_conflict(&self) -> bool {
        matches!(self, DbError::Conflict)
    }
}

/// What a request whose database access failed tells the client: the
/// database couldn't be reached, or stayed contended through every
/// [`retry`], so the request may succeed later.
impl From<DbError> for pisa_protocol::api::Error {
    fn from(e: DbError) -> Self {
        pisa_protocol::api::Error::new(
            pisa_protocol::api::ErrorKind::Unavailable,
            format!("the metadata store is unavailable, retry later: {e}"),
        )
    }
}

/// A transaction that raced another: a serialization failure, an insert of a
/// key another transaction inserted first, a deadlock, or SQLite's lock held
/// by another process. Each transaction reads what it writes first, so run
/// again it sees the other's write.
impl From<diesel::result::Error> for DbError {
    fn from(e: diesel::result::Error) -> Self {
        use diesel::result::DatabaseErrorKind::{SerializationFailure, UniqueViolation};
        use diesel::result::Error::DatabaseError;
        match &e {
            DatabaseError(SerializationFailure | UniqueViolation, _) => DbError::Conflict,
            DatabaseError(_, info)
                if [
                    "deadlock detected",
                    "database is locked",
                    "database table is locked",
                ]
                .iter()
                .any(|m| info.message().contains(m)) =>
            {
                DbError::Conflict
            }
            _ => DbError::Other(e.to_string()),
        }
    }
}

impl From<diesel::ConnectionError> for DbError {
    fn from(e: diesel::ConnectionError) -> Self {
        DbError::Other(e.to_string())
    }
}

/// Runs `attempt` (which begins and commits its own transaction) again while
/// it fails with a conflict, sleeping in between: from 10 ms, doubling up to
/// 1 s, each sleep jittered down by up to half so that racing workers spread
/// out. It gives up after 16 attempts or about 5 s of sleeping, whichever
/// comes first, with the last conflict.
pub async fn retry<T, E: IsConflict, F: Future<Output = Result<T, E>>>(
    mut attempt: impl FnMut() -> F,
) -> Result<T, E> {
    let mut attempts = 1;
    let mut backoff = FIRST_BACKOFF;
    let mut slept = Duration::ZERO;
    loop {
        match attempt().await {
            Err(e) if e.is_conflict() && attempts < MAX_ATTEMPTS && slept < RETRY_BUDGET => {
                let pause = jittered(backoff).min(RETRY_BUDGET - slept);
                tokio::time::sleep(pause).await;
                slept += pause;
                backoff = (backoff * 2).min(MAX_BACKOFF);
                attempts += 1;
            }
            result => return result,
        }
    }
}

/// A random duration from half of `d` to `d`.
fn jittered(d: Duration) -> Duration {
    let half = d / 2;
    let random = uuid::Uuid::new_v4().as_u128() as u64;
    half + Duration::from_nanos(random % (half.as_nanos() as u64 + 1))
}

#[derive(Clone)]
pub enum Db {
    Pg(Arc<PgDb>),
    Sqlite(Arc<SqliteDb>),
}

impl Db {
    /// Connects to the database at `url` and migrates it: PostgreSQL for a
    /// `postgres://` or `postgresql://` URL, else the SQLite file at that
    /// path, or a new in-memory database if `url` is `None` or empty.
    pub async fn connect(url: Option<&str>) -> Result<Db, DbError> {
        match url.filter(|url| !url.is_empty()) {
            Some(url) if url.starts_with("postgres://") || url.starts_with("postgresql://") => {
                connect_pg(url).await
            }
            Some(path) => open_sqlite(path).await,
            None => open_sqlite(":memory:").await,
        }
    }

    /// An empty in-memory SQLite database, shared by its clones.
    pub async fn memory() -> Db {
        open_sqlite(":memory:")
            .await
            .expect("an in-memory SQLite database opens")
    }

    /// The job queue's notifications from now on: from every process on
    /// PostgreSQL, from this one on SQLite. The first call on PostgreSQL
    /// starts listening, and waits a moment for it. Notifications can be
    /// missed, so whoever waits for one also polls, every
    /// [`Db::poll_interval`]; a lagging receiver should look again too.
    pub async fn notices(&self) -> broadcast::Receiver<Notice> {
        match self {
            Db::Pg(pg) => {
                pg.listening
                    .get_or_init(|| async {
                        let (ready, listening) = tokio::sync::oneshot::channel();
                        tokio::spawn(listen(Arc::downgrade(pg), ready));
                        if tokio::time::timeout(LISTEN_TIMEOUT, listening)
                            .await
                            .is_err()
                        {
                            tracing::warn!("not listening for notifications yet; polling");
                        }
                    })
                    .await;
                pg.notices.subscribe()
            }
            Db::Sqlite(sqlite) => sqlite.notices.subscribe(),
        }
    }

    /// How often a waiter looks again without a notification. Other
    /// processes' writes to a SQLite file send none.
    pub fn poll_interval(&self) -> Duration {
        match self {
            Db::Pg(_) => Duration::from_secs(2),
            Db::Sqlite(_) => Duration::from_millis(100),
        }
    }

    /// Holds the database's connection (SQLite), or every connection of the
    /// pool (PostgreSQL), until the guard is dropped, so that every query
    /// this process makes waits. For tests of what happens when the
    /// database does not answer.
    #[doc(hidden)]
    pub async fn stall(&self) -> StallGuard {
        let (held, release) = tokio::sync::oneshot::channel::<()>();
        let (ready, holding) = tokio::sync::oneshot::channel::<()>();
        match self {
            Db::Sqlite(sqlite) => {
                let sqlite = sqlite.clone();
                tokio::spawn(async move {
                    let _conn = sqlite.conn.lock().await;
                    let _ = ready.send(());
                    let _ = release.await;
                });
            }
            Db::Pg(pg) => {
                let pg = pg.clone();
                tokio::spawn(async move {
                    let mut conns = Vec::new();
                    for _ in 0..POOL_SIZE {
                        match pg.pool.get().await {
                            Ok(conn) => conns.push(conn),
                            Err(_) => break,
                        }
                    }
                    let _ = ready.send(());
                    let _ = release.await;
                });
            }
        }
        let _ = holding.await;
        StallGuard(held)
    }

    /// Every row kept for pool version `version` but the change log, each as
    /// its table and key, sorted. For tests.
    pub async fn rows(&self, version: &str) -> Result<Vec<Vec<String>>, DbError> {
        let mut rows = crate::backend::dispatch!(self, rows(version))?;
        rows.sort();
        Ok(rows)
    }
}

/// What [`Db::stall`] returns: the database answers again once it is dropped.
#[doc(hidden)]
pub struct StallGuard(#[allow(dead_code)] tokio::sync::oneshot::Sender<()>);

async fn open_sqlite(path: &str) -> Result<Db, DbError> {
    let path = path.to_string();
    let conn = tokio::task::spawn_blocking(move || -> Result<_, DbError> {
        let mut conn = SqliteConnection::establish(&path)?;
        conn.batch_execute(&format!(
            "PRAGMA foreign_keys = ON; PRAGMA busy_timeout = {SQLITE_BUSY_MILLIS};"
        ))?;
        if path != ":memory:" {
            conn.batch_execute("PRAGMA journal_mode = WAL;")?;
        }
        conn.run_pending_migrations(SQLITE_MIGRATIONS)
            .map_err(|e| DbError::Other(format!("migrating the database: {e}")))?;
        Ok(conn)
    })
    .await
    .map_err(|e| DbError::Other(e.to_string()))??;
    Ok(Db::Sqlite(Arc::new(SqliteDb {
        conn: Mutex::new(SyncConnectionWrapper::new(conn)),
        notices: notices(),
    })))
}

async fn connect_pg(url: &str) -> Result<Db, DbError> {
    let tls = tls(url)?;
    let conn = establish_pg(url.to_string(), tls.clone()).await?;
    // The lock goes when the connection closes.
    tokio::task::spawn_blocking(move || -> Result<_, DbError> {
        let mut conn = AsyncConnectionWrapper::<AsyncPgConnection>::from(conn);
        conn.batch_execute(&format!("SELECT pg_advisory_lock({MIGRATION_LOCK})"))?;
        conn.run_pending_migrations(PG_MIGRATIONS)
            .map_err(|e| DbError::Other(format!("migrating the database: {e}")))?;
        Ok(())
    })
    .await
    .map_err(|e| DbError::Other(e.to_string()))??;

    let mut config = ManagerConfig::default();
    config.custom_setup = Box::new(move |url| Box::pin(establish_pg(url.to_string(), tls.clone())));
    let manager = AsyncDieselConnectionManager::new_with_config(url, config);
    let pool = Pool::builder(manager)
        .max_size(POOL_SIZE)
        .wait_timeout(Some(POOL_TIMEOUT))
        .create_timeout(Some(POOL_TIMEOUT))
        .recycle_timeout(Some(POOL_RECYCLE_TIMEOUT))
        .runtime(deadpool::Runtime::Tokio1)
        .build()
        .map_err(|e| DbError::Other(e.to_string()))?;
    Ok(Db::Pg(Arc::new(PgDb {
        pool,
        notices: notices(),
        listening: OnceCell::new(),
    })))
}

/// Listens for the queue's notifications on a connection of its own, taken
/// out of the pool, and passes them on until the database is dropped. A
/// broken connection is replaced; what was sent meanwhile is missed.
async fn listen(db: Weak<PgDb>, ready: tokio::sync::oneshot::Sender<()>) {
    let mut ready = Some(ready);
    let mut backoff = Duration::from_millis(100);
    loop {
        let Some(pg) = db.upgrade() else { return };
        let conn = pg.pool.get().await;
        drop(pg);
        match conn {
            Ok(conn) => {
                let mut conn = deadpool::managed::Object::take(conn);
                let listened = diesel_async::SimpleAsyncConnection::batch_execute(
                    &mut conn,
                    &format!("LISTEN {QUEUED}; LISTEN {DONE}"),
                )
                .await;
                match listened {
                    Ok(()) => {
                        if let Some(ready) = ready.take() {
                            let _ = ready.send(());
                        }
                        backoff = Duration::from_millis(100);
                        let mut stream = std::pin::pin!(conn.notifications_stream());
                        while let Some(n) = stream.next().await {
                            let Some(pg) = db.upgrade() else { return };
                            match n {
                                Ok(n) => {
                                    if let Some(notice) = Notice::parse(&n.channel, &n.payload) {
                                        let _ = pg.notices.send(notice);
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!("listening for notifications failed: {e}");
                                    break;
                                }
                            }
                        }
                        tracing::warn!("the notification connection closed; reconnecting");
                    }
                    Err(e) => tracing::warn!("listening for notifications failed: {e}"),
                }
            }
            Err(e) => tracing::warn!("no connection to listen for notifications on: {e}"),
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(5));
    }
}

/// A TLS connector if `url` has `sslmode=require`, which then verifies the
/// server's certificate against the system's roots, or those in
/// `SSL_CERT_FILE` or `SSL_CERT_DIR`. Other modes connect without TLS.
fn tls(url: &str) -> Result<Option<MakeRustlsConnect>, DbError> {
    let config: tokio_postgres::Config = url
        .parse()
        .map_err(|e| DbError::Other(format!("bad database URL: {e}")))?;
    if config.get_ssl_mode() != tokio_postgres::config::SslMode::Require {
        return Ok(None);
    }
    let mut roots = rustls::RootCertStore::empty();
    let found = rustls_native_certs::load_native_certs();
    let (added, _) = roots.add_parsable_certificates(found.certs);
    if added == 0 {
        return Err(DbError::Other(format!(
            "sslmode=require, but no root certificates were found: {:?}",
            found.errors
        )));
    }
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| DbError::Other(e.to_string()))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Some(MakeRustlsConnect::new(config)))
}

async fn establish_pg(
    url: String,
    tls: Option<MakeRustlsConnect>,
) -> diesel::ConnectionResult<AsyncPgConnection> {
    use diesel_async::AsyncConnection;
    let Some(tls) = tls else {
        return AsyncPgConnection::establish(&url).await;
    };
    let (client, conn) = tokio_postgres::connect(&url, tls).await.map_err(|e| {
        // The causes say why, e.g. why the TLS handshake failed.
        let mut message = e.to_string();
        let mut cause = std::error::Error::source(&e);
        while let Some(c) = cause {
            message = format!("{message}: {c}");
            cause = c.source();
        }
        diesel::ConnectionError::BadConnection(message)
    })?;
    AsyncPgConnection::try_from_client_and_connection(client, conn).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Queries wait while the database is stalled, and run again once the
    /// guard is dropped.
    #[tokio::test]
    async fn a_stalled_database_answers_again_once_released() {
        let db = Db::memory().await;
        let stalled = db.stall().await;
        let rows = db.rows("v");
        tokio::pin!(rows);
        assert!(
            tokio::time::timeout(Duration::from_millis(200), &mut rows)
                .await
                .is_err()
        );
        drop(stalled);
        assert!(rows.await.unwrap().is_empty());
    }

    #[derive(diesel::QueryableByName, Debug, PartialEq)]
    struct LinkRow {
        #[diesel(sql_type = diesel::sql_types::Text)]
        entry: String,
        #[diesel(sql_type = diesel::sql_types::Text)]
        replaces: String,
        #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
        allow: Option<String>,
        #[diesel(sql_type = diesel::sql_types::Text)]
        replacement: String,
        #[diesel(sql_type = diesel::sql_types::Text)]
        run_root: String,
    }

    /// Module records an earlier release kept whole, closures included, read
    /// back the same from their columns and imports.
    #[tokio::test]
    async fn module_records_move_to_columns_and_read_back_the_same() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pisa.db");
        let mut conn = SqliteConnection::establish(&path.to_string_lossy()).unwrap();
        conn.batch_execute("PRAGMA foreign_keys = ON;").unwrap();
        let pending = conn.pending_migrations(SQLITE_MIGRATIONS).unwrap();
        let at = pending
            .iter()
            .position(|m| m.name().to_string().ends_with("_module_columns"))
            .unwrap();
        for m in &pending[..at] {
            conn.run_migration(m).unwrap();
        }
        let record = |id: &str, imports: &[&str], closure: &[&str], status: &str| {
            let module = |m: &str| {
                serde_json::json!({"id": m, "build": format!("b{m}"),
                                   "files": [{"ext": "olean", "sha256": format!("h{m}")}]})
            };
            serde_json::json!({
                "imports": imports, "modules": closure.iter().map(|m| module(m)).collect::<Vec<_>>(),
                "header": format!("import PisaSnap.s{id}"), "code": format!("-- {id}"),
                "status": status, "verified": id == "a", "macros": false,
                "result": {"ok": true, "time": 0.5}, "created": 7,
            })
        };
        // `c` imports `b` before `a`; `d` imports `a` before `c`.
        let rows = [
            ("a", record("a", &[], &["a"], "proved")),
            ("b", record("b", &[], &["b"], "sorry")),
            ("c", record("c", &["b", "a"], &["b", "a", "c"], "proved")),
            (
                "d",
                record("d", &["a", "c"], &["a", "b", "c", "d"], "conditional"),
            ),
        ];
        for (id, meta) in &rows {
            let meta = meta.to_string().replace('\'', "''");
            conn.batch_execute(&format!(
                "INSERT INTO modules (version, id, meta) VALUES ('v', '{id}', jsonb('{meta}'));"
            ))
            .unwrap();
        }
        conn.batch_execute(
            "INSERT INTO module_imports (version, module, parent) VALUES
               ('v', 'c', 'b'), ('v', 'c', 'a'), ('v', 'd', 'a'), ('v', 'd', 'c');",
        )
        .unwrap();
        drop(conn);

        let db = Db::connect(Some(&path.to_string_lossy())).await.unwrap();
        let catalog = crate::catalog::Catalog::new(db, "v".into());
        for (id, meta) in &rows {
            let read = catalog.module(id).await.unwrap();
            let stored: crate::catalog::ModuleMeta = serde_json::from_value(meta.clone()).unwrap();
            assert_eq!(read.modules, stored.modules, "{id}");
            assert_eq!(read.imports, stored.imports, "{id}");
            assert_eq!(
                (
                    &read.header,
                    read.status,
                    read.verified,
                    read.created,
                    &read.code
                ),
                (
                    &stored.header,
                    stored.status,
                    stored.verified,
                    stored.created,
                    &stored.code
                ),
                "{id}"
            );
            assert!(read.result.ok, "{id}");
        }
    }

    /// The links an earlier release kept on its entries move to `links`, each
    /// with the entry its run starts at.
    #[test]
    fn links_move_to_their_table_with_their_runs_root() {
        let mut conn = SqliteConnection::establish(":memory:").unwrap();
        conn.batch_execute("PRAGMA foreign_keys = ON;").unwrap();
        let pending = conn.pending_migrations(SQLITE_MIGRATIONS).unwrap();
        let at = pending
            .iter()
            .position(|m| m.name().to_string().ends_with("_links"))
            .unwrap();
        for m in &pending[..at] {
            conn.run_migration(m).unwrap();
        }
        // `a` starts a run that `b`, then `c` and the fork `e` continue; `d`
        // replaces `z`, which is gone.
        conn.batch_execute(
            "INSERT INTO modules (version, id, meta) VALUES ('v', 'm', jsonb('{}'));
             INSERT INTO entries (version, id, module, labels, created_micros, replaces,
                                  replaces_allow, replacement) VALUES
               ('v', 'a', 'm', jsonb('{}'), 1, NULL, NULL, NULL),
               ('v', 'b', 'm', jsonb('{}'), 2, 'a', '{\"definitions\":[\"f\"]}', '{\"filled\":[\"t\"]}'),
               ('v', 'c', 'm', jsonb('{}'), 3, 'b', NULL, '{}'),
               ('v', 'e', 'm', jsonb('{}'), 4, 'b', NULL, NULL),
               ('v', 'd', 'm', jsonb('{}'), 5, 'z', NULL, '{}');",
        )
        .unwrap();
        conn.run_pending_migrations(SQLITE_MIGRATIONS).unwrap();
        let links: Vec<LinkRow> = diesel::RunQueryDsl::load(
            diesel::sql_query(
                "SELECT entry, replaces, allow, replacement, run_root FROM links ORDER BY entry",
            ),
            &mut conn,
        )
        .unwrap();
        let link =
            |entry: &str, replaces: &str, allow: Option<&str>, replacement: &str, root: &str| {
                LinkRow {
                    entry: entry.into(),
                    replaces: replaces.into(),
                    allow: allow.map(String::from),
                    replacement: replacement.into(),
                    run_root: root.into(),
                }
            };
        assert_eq!(
            links,
            [
                link(
                    "b",
                    "a",
                    Some("{\"definitions\":[\"f\"]}"),
                    "{\"filled\":[\"t\"]}",
                    "a"
                ),
                link("c", "b", None, "{}", "a"),
                link("d", "z", None, "{}", "z"),
                link("e", "b", None, "{}", "a"),
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn retry_reruns_on_conflict() {
        let start = tokio::time::Instant::now();
        let mut calls = 0;
        let result = retry(|| {
            calls += 1;
            let n = calls;
            async move {
                if n <= 2 {
                    Err(DbError::Conflict)
                } else {
                    Ok(7)
                }
            }
        })
        .await;
        assert!(matches!(result, Ok(7)));
        assert_eq!(calls, 3);
        // Two sleeps: 5–10 ms, then 10–20 ms. The timer rounds each up to the
        // next millisecond.
        let slept = start.elapsed();
        assert!(
            (Duration::from_millis(15)..=Duration::from_millis(32)).contains(&slept),
            "{slept:?}"
        );

        let start = tokio::time::Instant::now();
        let mut calls = 0;
        let result: Result<(), DbError> = retry(|| {
            calls += 1;
            async { Err(DbError::Conflict) }
        })
        .await;
        assert!(matches!(result, Err(DbError::Conflict)));
        // Unjittered, the budget runs out during the 11th sleep.
        assert!((12..=MAX_ATTEMPTS).contains(&calls), "{calls}");
        // At least the halves of 10 ms, …, 640 ms, then eight of 1 s.
        let slept = start.elapsed();
        let rounding = Duration::from_millis(MAX_ATTEMPTS as u64);
        assert!(
            (Duration::from_millis(4635)..=RETRY_BUDGET + rounding).contains(&slept),
            "{slept:?}"
        );

        let mut calls = 0;
        let result: Result<(), DbError> = retry(|| {
            calls += 1;
            async { Err(DbError::Other("boom".into())) }
        })
        .await;
        assert!(matches!(result, Err(DbError::Other(_))));
        assert_eq!(calls, 1);
    }

    #[test]
    fn jitter_stays_within_half_of_the_backoff() {
        for _ in 0..1000 {
            let d = jittered(Duration::from_millis(10));
            assert!((Duration::from_millis(5)..=Duration::from_millis(10)).contains(&d));
        }
    }

    #[test]
    fn errors_are_conflicts_only_when_raced() {
        use diesel::result::{DatabaseErrorKind, Error};
        let db = |kind, message: &str| {
            DbError::from(Error::DatabaseError(kind, Box::new(message.to_string())))
        };
        assert!(db(DatabaseErrorKind::SerializationFailure, "x").is_conflict());
        assert!(db(DatabaseErrorKind::UniqueViolation, "x").is_conflict());
        assert!(db(DatabaseErrorKind::Unknown, "deadlock detected").is_conflict());
        assert!(db(DatabaseErrorKind::Unknown, "database is locked").is_conflict());
        assert!(!db(DatabaseErrorKind::ForeignKeyViolation, "x").is_conflict());
        assert!(!db(DatabaseErrorKind::Unknown, "boom").is_conflict());
        assert!(!DbError::from(Error::NotFound).is_conflict());
    }

    #[tokio::test]
    async fn a_url_picks_the_backend() {
        assert!(matches!(Db::connect(None).await.unwrap(), Db::Sqlite(_)));
        assert!(matches!(
            Db::connect(Some("")).await.unwrap(),
            Db::Sqlite(_)
        ));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pisa.db");
        assert!(matches!(
            Db::connect(Some(path.to_str().unwrap())).await.unwrap(),
            Db::Sqlite(_)
        ));
        assert!(path.exists());
        assert!(tls("postgres://u@h/d").unwrap().is_none());
        assert!(tls("postgres://u@h/d?sslmode=disable").unwrap().is_none());
        assert!(tls("postgres://u@h/d?sslmode=bogus").is_err());
    }
}
