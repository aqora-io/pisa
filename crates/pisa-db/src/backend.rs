//! The queries, written once in `queries/` and compiled once per backend, as
//! diesel checks each query against one backend's types. Each module gives
//! the queries:
//! - `Handle`, what a [`Db`](crate::db::Db) variant holds;
//! - `conn`, a connection from it;
//! - `begin`, which begins a serializable transaction, and `begin_read`, a
//!   transaction that reads one state of the database;
//! - `label_filter`, which narrows an entries query to those with labels;
//! - `notify`, which sends a [`Notice`](crate::db::Notice) to whoever
//!   listens.

use diesel_async::{AsyncConnection, TransactionManager};

use crate::catalog::Fail;

/// Runs `$f` on `$db`'s backend, with its handle as the first argument.
macro_rules! dispatch {
    ($db:expr, $f:ident($($arg:expr),* $(,)?)) => {
        match $db {
            $crate::db::Db::Pg(h) => $crate::backend::pg::$f(h, $($arg),*).await,
            $crate::db::Db::Sqlite(h) => $crate::backend::sqlite::$f(h, $($arg),*).await,
        }
    };
}
pub(crate) use dispatch;

/// Runs `$body` in a transaction on `$conn`, committing it if it returns
/// `Ok` and rolling it back if it returns `Err`.
macro_rules! tx {
    ($conn:ident, $body:block) => {{
        begin($conn).await?;
        let result: Result<_, Fail> = async { $body }.await;
        crate::backend::finish($conn, result).await
    }};
}

/// Runs `$body` in a read-only transaction on `$conn`, whose reads all see
/// one state of the database.
macro_rules! read_tx {
    ($conn:ident, $body:block) => {{
        begin_read($conn).await?;
        let result: Result<_, Fail> = async { $body }.await;
        crate::backend::finish($conn, result).await
    }};
}

async fn finish<C: AsyncConnection, T>(conn: &mut C, result: Result<T, Fail>) -> Result<T, Fail> {
    match result {
        Ok(value) => {
            C::TransactionManager::commit_transaction(conn).await?;
            Ok(value)
        }
        Err(e) => {
            // A connection left in the transaction is discarded by the pool,
            // or rolled back by the next `conn`.
            let _ = C::TransactionManager::rollback_transaction(conn).await;
            Err(e)
        }
    }
}

pub(crate) mod pg {
    use std::sync::Arc;

    use diesel::pg::Pg;
    use diesel::sql_types::Text;
    use diesel::{PgJsonbExpressionMethods, QueryDsl, QueryResult};
    use diesel_async::pooled_connection::deadpool::Object;
    use diesel_async::{AnsiTransactionManager, AsyncPgConnection, RunQueryDsl};

    use crate::db::{DbError, Notice, PgDb};
    use crate::schema::entries;

    type Handle = Arc<PgDb>;
    type Conn = AsyncPgConnection;

    async fn conn(db: &PgDb) -> Result<Object<Conn>, DbError> {
        db.pool
            .get()
            .await
            .map_err(|e| DbError::Other(e.to_string()))
    }

    /// `pg_notify`, which every process's listener hears once the
    /// statements before it on `conn` are committed.
    async fn notify(_db: &PgDb, conn: &mut Conn, notice: Notice) -> QueryResult<()> {
        let (channel, payload) = notice.channel_and_payload();
        diesel::sql_query("SELECT pg_notify($1, $2)")
            .bind::<Text, _>(channel)
            .bind::<Text, _>(payload)
            .execute(conn)
            .await
            .map(drop)
    }

    async fn begin(conn: &mut Conn) -> QueryResult<()> {
        AnsiTransactionManager::begin_transaction_sql(
            conn,
            "BEGIN TRANSACTION ISOLATION LEVEL SERIALIZABLE",
        )
        .await
    }

    /// A snapshot of the database for the transaction's reads, which never
    /// fails on serialization.
    async fn begin_read(conn: &mut Conn) -> QueryResult<()> {
        AnsiTransactionManager::begin_transaction_sql(
            conn,
            "BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY",
        )
        .await
    }

    /// `labels @> {…}`, which the GIN index on labels serves.
    fn label_filter<'a>(
        query: entries::BoxedQuery<'a, Pg>,
        labels: &'a [(String, String)],
    ) -> entries::BoxedQuery<'a, Pg> {
        if labels.is_empty() {
            return query;
        }
        let wanted: serde_json::Map<String, serde_json::Value> = labels
            .iter()
            .map(|(k, v)| (k.clone(), v.clone().into()))
            .collect();
        query.filter(entries::labels.contains(serde_json::Value::Object(wanted)))
    }

    mod catalog {
        use super::*;
        include!("queries/catalog.rs");
    }
    mod records {
        use super::*;
        include!("queries/records.rs");
    }
    mod queue {
        use super::*;
        include!("queries/queue.rs");
    }
    pub(crate) use catalog::*;
    pub(crate) use queue::*;
    pub(crate) use records::*;
}

pub(crate) mod sqlite {
    use std::sync::Arc;

    use diesel::sql_types::{Jsonb, Nullable, Text};
    use diesel::sqlite::Sqlite;
    use diesel::{ExpressionMethods, QueryDsl, QueryResult, SqliteConnection};
    use diesel_async::sync_connection_wrapper::SyncConnectionWrapper;
    use diesel_async::{AsyncConnection, TransactionManager};
    use tokio::sync::MutexGuard;

    use crate::db::{DbError, Notice, SqliteDb};
    use crate::schema::entries;

    type Handle = Arc<SqliteDb>;
    type Conn = SyncConnectionWrapper<SqliteConnection>;

    /// The connection, rolled back first if a transaction was left open on it
    /// by a future dropped partway.
    async fn conn(db: &SqliteDb) -> Result<MutexGuard<'_, Conn>, DbError> {
        type Tm = <Conn as AsyncConnection>::TransactionManager;
        let mut conn = db.conn.lock().await;
        if Tm::transaction_manager_status_mut(&mut conn)
            .transaction_depth()?
            .is_some()
        {
            Tm::rollback_transaction(&mut conn).await?;
        }
        Ok(conn)
    }

    /// Takes the database's write lock at once, so that a transaction never
    /// finds it taken after reading.
    async fn begin(conn: &mut Conn) -> QueryResult<()> {
        conn.spawn_blocking(|conn| {
            diesel::connection::AnsiTransactionManager::begin_transaction_sql(
                conn,
                "BEGIN IMMEDIATE",
            )
        })
        .await
    }

    /// Reads from one snapshot of the database from the first read on, as
    /// SQLite's WAL gives a transaction.
    async fn begin_read(conn: &mut Conn) -> QueryResult<()> {
        conn.spawn_blocking(|conn| {
            diesel::connection::AnsiTransactionManager::begin_transaction_sql(
                conn,
                "BEGIN DEFERRED",
            )
        })
        .await
    }

    /// A broadcast to this process only: SQLite has no `NOTIFY`, and other
    /// processes on the same file poll.
    async fn notify(db: &SqliteDb, _conn: &mut Conn, notice: Notice) -> QueryResult<()> {
        let _ = db.notices.send(notice);
        Ok(())
    }

    diesel::define_sql_function! {
        fn json_extract(json: Jsonb, path: Text) -> Nullable<Text>;
    }

    /// One `json_extract` per label: SQLite has no `@>`. Label keys are
    /// `[A-Za-z0-9._/-]`, so they can be quoted in a path as they are.
    fn label_filter<'a>(
        mut query: entries::BoxedQuery<'a, Sqlite>,
        labels: &'a [(String, String)],
    ) -> entries::BoxedQuery<'a, Sqlite> {
        for (k, v) in labels {
            query = query.filter(json_extract(entries::labels, format!("$.\"{k}\"")).eq(v));
        }
        query
    }

    mod catalog {
        use super::*;
        include!("queries/catalog.rs");
    }
    mod records {
        use super::*;
        include!("queries/records.rs");
    }
    mod queue {
        use super::*;
        include!("queries/queue.rs");
    }
    pub(crate) use catalog::*;
    pub(crate) use queue::*;
    pub(crate) use records::*;
}
