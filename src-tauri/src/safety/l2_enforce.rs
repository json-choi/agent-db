//! L2 — DB-level enforcement. **This is the authoritative security boundary.**
//!
//! [`run_read_only`] runs a statement inside a session the database itself
//! constrains to read-only, so a write cannot commit even when L1 misclassified
//! it (writable CTE, side-effecting function, dialect quirk). When the DB rejects
//! a write, we surface a typed [`AppError::Blocked`] — never a generic DB error.
//!
//! Per engine:
//! - **PostgreSQL:** `BEGIN; SET TRANSACTION READ ONLY; SET LOCAL statement_timeout`
//!   → a write raises SQLSTATE `25006`.
//! - **MySQL:** `SET SESSION max_execution_time; START TRANSACTION READ ONLY`
//!   → a write raises `1792`.
//! - **SQLite:** relies on the connection module opening a `read_only(true)` pool;
//!   L2 also sets `PRAGMA query_only=ON` → a write raises `SQLITE_READONLY`.

use std::time::{Duration, Instant};

use sqlx::{AssertSqlSafe, Executor, SqlSafeStr};
use tokio::time::timeout;

use crate::error::{AppError, AppResult};
use crate::executor::read::{describe_cols, mysql_value, pg_value, sqlite_value, stream_capped};
use crate::model::{Engine, QueryResult};

use super::{PoolRef, STATEMENT_TIMEOUT_MS};

/// Run `sql` under a DB-enforced read-only session and materialize the rows.
/// Rows are STREAMED (fetch stops one past `max_rows` → bounded memory, `truncated`
/// flagged) instead of buffering the whole result. Decoding reuses the executor's
/// per-engine mappers so this path and the desktop path render cells identically.
/// Any write the model slipped through is rejected by the DB → [`AppError::Blocked`].
pub async fn run_read_only(pool: PoolRef<'_>, sql: &str, max_rows: u64) -> AppResult<QueryResult> {
    let max = max_rows as usize;
    let started = Instant::now();
    let engine = pool.engine();

    let (columns, rows, truncated) = match pool {
        PoolRef::Postgres(p) => {
            let mut conn = p.acquire().await?;
            // Establishing READ ONLY is safety-critical: if any setup statement
            // fails, the `?` inside the block short-circuits and we never run the
            // user SQL. ROLLBACK always fires afterwards (no leaked txn).
            let res = async {
                sqlx::query("BEGIN").execute(&mut *conn).await?;
                sqlx::query("SET TRANSACTION READ ONLY")
                    .execute(&mut *conn)
                    .await?;
                let _ = sqlx::query(AssertSqlSafe(format!(
                    "SET LOCAL statement_timeout = {STATEMENT_TIMEOUT_MS}"
                )))
                .execute(&mut *conn)
                .await;
                guarded(stream_capped(
                    sqlx::query(AssertSqlSafe(sql)).fetch(&mut *conn),
                    max,
                    pg_value,
                ))
                .await
            }
            .await;
            let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
            let (c, r, t) = res.map_err(|e| map_readonly(engine, e))?;
            let c = if c.is_empty() {
                (&mut *conn)
                    .describe(AssertSqlSafe(sql).into_sql_str())
                    .await
                    .ok()
                    .map(describe_cols)
                    .unwrap_or_default()
            } else {
                c
            };
            (c, r, t)
        }
        PoolRef::Mysql(p) => {
            let mut conn = p.acquire().await?;
            let res = async {
                let _ = sqlx::query(AssertSqlSafe(format!(
                    "SET SESSION max_execution_time = {STATEMENT_TIMEOUT_MS}"
                )))
                .execute(&mut *conn)
                .await;
                sqlx::query("START TRANSACTION READ ONLY")
                    .execute(&mut *conn)
                    .await?;
                guarded(stream_capped(
                    sqlx::query(AssertSqlSafe(sql)).fetch(&mut *conn),
                    max,
                    mysql_value,
                ))
                .await
            }
            .await;
            let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
            let (c, r, t) = res.map_err(|e| map_readonly(engine, e))?;
            let c = if c.is_empty() {
                (&mut *conn)
                    .describe(AssertSqlSafe(sql).into_sql_str())
                    .await
                    .ok()
                    .map(describe_cols)
                    .unwrap_or_default()
            } else {
                c
            };
            (c, r, t)
        }
        PoolRef::Sqlite(p) => {
            let mut conn = p.acquire().await?;
            let res = async {
                // Belt-and-suspenders on top of the read_only(true) pool.
                sqlx::query("PRAGMA query_only = ON")
                    .execute(&mut *conn)
                    .await?;
                guarded(stream_capped(
                    sqlx::query(AssertSqlSafe(sql)).fetch(&mut *conn),
                    max,
                    sqlite_value,
                ))
                .await
            }
            .await;
            let _ = sqlx::query("PRAGMA query_only = OFF")
                .execute(&mut *conn)
                .await;
            let (c, r, t) = res.map_err(|e| map_readonly(engine, e))?;
            // Zero-row results yield no columns from the stream; fall back to the
            // prepared statement's metadata so an empty grid still has headers.
            // `describe` prepares without executing — safe on the read-only conn.
            let c = if c.is_empty() {
                (&mut *conn)
                    .describe(AssertSqlSafe(sql).into_sql_str())
                    .await
                    .ok()
                    .map(describe_cols)
                    .unwrap_or_default()
            } else {
                c
            };
            (c, r, t)
        }
    };

    Ok(QueryResult {
        row_count: rows.len(),
        columns,
        rows,
        truncated,
        duration_ms: started.elapsed().as_millis() as u64,
    })
}

/// Wrap a DB future in a wall-clock guard (SQLite has no server-side timeout; for
/// PG/MySQL this backstops the session timeout).
async fn guarded<T>(
    fut: impl std::future::Future<Output = Result<T, sqlx::Error>>,
) -> Result<T, sqlx::Error> {
    match timeout(Duration::from_millis(STATEMENT_TIMEOUT_MS + 2_000), fut).await {
        Ok(r) => r,
        Err(_) => Err(sqlx::Error::PoolTimedOut),
    }
}

/// Map a driver error to [`AppError::Blocked`] iff it is a read-only violation;
/// otherwise pass it through as a plain DB error.
pub fn map_readonly(engine: Engine, e: sqlx::Error) -> AppError {
    if is_read_only_violation(engine, &e) {
        AppError::Blocked {
            reason: format!(
                "read-only session rejected a write ({}). The statement was classified \
                 as a read but the database detected a write — nothing was executed.",
                engine_label(engine)
            ),
        }
    } else {
        AppError::Db(e)
    }
}

/// True when `e` is the database refusing a write in a read-only session.
pub fn is_read_only_violation(engine: Engine, e: &sqlx::Error) -> bool {
    let Some(db) = e.as_database_error() else {
        return false;
    };
    let code = db.code().unwrap_or_default();
    let msg = db.message().to_lowercase();
    match engine {
        // 25006 = read_only_sql_transaction
        Engine::Postgres => code == "25006" || msg.contains("read-only transaction"),
        // 1792 = ER_CANT_EXECUTE_IN_READ_ONLY_TRANSACTION
        Engine::Mysql => {
            code == "1792" || msg.contains("read only transaction") || msg.contains("read-only")
        }
        // SQLITE_READONLY surfaces as "attempt to write a readonly database".
        Engine::Sqlite => msg.contains("readonly") || msg.contains("read only"),
        // MongoDB never reaches the SQLx error mapper. Its document adapter uses a
        // typed read allowlist and maps driver errors independently.
        Engine::Mongodb => false,
    }
}

fn engine_label(engine: Engine) -> &'static str {
    match engine {
        Engine::Postgres => "postgres",
        Engine::Mysql => "mysql",
        Engine::Sqlite => "sqlite",
        Engine::Mongodb => "mongodb",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    // max_connections(1) keeps the in-memory db on one connection so inserts persist.
    async fn mem_pool() -> sqlx::SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        for i in 1..=5i64 {
            sqlx::query("INSERT INTO t(id, name) VALUES(?, ?)")
                .bind(i)
                .bind(format!("n{i}"))
                .execute(&pool)
                .await
                .unwrap();
        }
        pool
    }

    #[tokio::test]
    async fn caps_and_flags_truncation() {
        let pool = mem_pool().await;
        let r = run_read_only(
            PoolRef::Sqlite(&pool),
            "SELECT id, name FROM t ORDER BY id",
            3,
        )
        .await
        .unwrap();
        assert_eq!(r.row_count, 3);
        assert!(r.truncated);
        assert_eq!(r.columns, vec!["id".to_string(), "name".to_string()]);
    }

    #[tokio::test]
    async fn no_truncation_under_cap() {
        let pool = mem_pool().await;
        let r = run_read_only(PoolRef::Sqlite(&pool), "SELECT id FROM t", 100)
            .await
            .unwrap();
        assert_eq!(r.row_count, 5);
        assert!(!r.truncated);
    }

    #[tokio::test]
    async fn zero_rows_still_has_columns() {
        let pool = mem_pool().await;
        let r = run_read_only(
            PoolRef::Sqlite(&pool),
            "SELECT id, name FROM t WHERE 1=0",
            100,
        )
        .await
        .unwrap();
        assert_eq!(r.row_count, 0);
        assert_eq!(r.columns, vec!["id".to_string(), "name".to_string()]);
    }

    #[tokio::test]
    async fn big_int_is_string_not_corrupted() {
        let pool = mem_pool().await;
        let r = run_read_only(
            PoolRef::Sqlite(&pool),
            "SELECT 9223372036854775807 AS big",
            10,
        )
        .await
        .unwrap();
        assert_eq!(
            r.rows[0][0],
            serde_json::Value::String("9223372036854775807".into())
        );
    }
}
