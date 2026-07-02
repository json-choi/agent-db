//! Write path (Phase 3). ONLY reachable after L4 approval AND with the connection's
//! `allow_writes` gate on. Runs the statement inside BEGIN..COMMIT on the read-write
//! pool and reports exactly how many rows committed.

use uuid::Uuid;

use crate::connection::{LiveConnection, Pool};
use crate::error::{AppError, AppResult};
use crate::executor::cancel;
use crate::model::{Engine, ExecOutcome, SafetySettings};

/// Execute an approved write inside a transaction and commit it.
///
/// Hard guard: returns `AppError::Blocked` unless `settings.allow_writes`. On any
/// error the `?` short-circuits before COMMIT and the dropped transaction rolls back,
/// so a failed statement leaves the DB unchanged.
pub async fn run_write(
    live: &LiveConnection,
    _engine: Engine, // pool enum is self-describing; kept to honor the executor contract
    sql: &str,
    settings: &SafetySettings,
    query_id: Option<Uuid>,
) -> AppResult<ExecOutcome> {
    if !settings.allow_writes {
        return Err(AppError::Blocked {
            reason: "writes are disabled for this connection (allow_writes = 0)".into(),
        });
    }

    // Cancel/timeout guard: aborting drops the in-flight txn future (uncommitted →
    // rolled back) and closes the pooled connection, so a hung write frees the tab.
    let inner = async {
        let affected: u64 = match &live.write_pool {
            Pool::Postgres(pool) => {
                let mut tx = pool.begin().await?;
                let n = sqlx::query(sql).execute(&mut *tx).await?.rows_affected();
                tx.commit().await?;
                n
            }
            Pool::Mysql(pool) => {
                let mut tx = pool.begin().await?;
                let n = sqlx::query(sql).execute(&mut *tx).await?.rows_affected();
                tx.commit().await?;
                n
            }
            Pool::Sqlite(pool) => {
                let mut tx = pool.begin().await?;
                let n = sqlx::query(sql).execute(&mut *tx).await?.rows_affected();
                tx.commit().await?;
                n
            }
        };
        Ok::<u64, AppError>(affected)
    };

    let affected = cancel::guard(query_id, cancel::QUERY_TIMEOUT, inner).await?;

    Ok(ExecOutcome {
        result: None,
        affected: Some(affected),
        committed: true,
    })
}
