//! The metadata database: PostgreSQL in deployments, SQLite for tests and
//! single-process runs. Both have the same tables (see `migrations/`), which
//! [`Db::connect`] creates or updates.
//!
//! Transactions that write are serializable: on PostgreSQL a transaction that
//! raced another fails with [`DbError::Conflict`], and [`retry`] runs it
//! again. SQLite takes the database's write lock when such a transaction
//! begins, so they run one at a time.

use std::sync::Arc;
use std::time::Duration;

use diesel::connection::SimpleConnection;
use diesel::{Connection, SqliteConnection};
use diesel_async::AsyncPgConnection;
use diesel_async::async_connection_wrapper::AsyncConnectionWrapper;
use diesel_async::pooled_connection::deadpool::Pool;
use diesel_async::pooled_connection::{AsyncDieselConnectionManager, ManagerConfig};
use diesel_async::sync_connection_wrapper::SyncConnectionWrapper;
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};
use tokio::sync::Mutex;
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

/// How long a SQLite statement waits for another process's lock before it
/// fails as a conflict, which [`retry`] runs again.
const SQLITE_BUSY_MILLIS: u32 = 250;

/// The key of the PostgreSQL advisory lock held while migrating, so that
/// processes starting together migrate one at a time.
const MIGRATION_LOCK: i64 = 0x7069_7361_6462; // "pisadb"

const PG_MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations/postgres");
const SQLITE_MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations/sqlite");

pub(crate) type PgPool = Pool<AsyncPgConnection>;

/// One connection, which keeps an in-memory database alive and runs one
/// query at a time.
pub(crate) type SqliteDb = Arc<Mutex<SyncConnectionWrapper<SqliteConnection>>>;

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
    Pg(PgPool),
    Sqlite(SqliteDb),
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

    /// Every row kept for pool version `version` but the change log, each as
    /// its table and key, sorted. For tests.
    pub async fn rows(&self, version: &str) -> Result<Vec<Vec<String>>, DbError> {
        let mut rows = crate::backend::dispatch!(self, rows(version))?;
        rows.sort();
        Ok(rows)
    }
}

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
    Ok(Db::Sqlite(Arc::new(Mutex::new(
        SyncConnectionWrapper::new(conn),
    ))))
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
        .runtime(deadpool::Runtime::Tokio1)
        .build()
        .map_err(|e| DbError::Other(e.to_string()))?;
    Ok(Db::Pg(pool))
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
