//! Query executor. Dispatches an already-classified statement to the read path
//! (L2 read-only pool) or the guarded write path. The L3 exec+rollback preview
//! harness lives in `safety::l3_preview`.
//!
//! The executor is the LAST stage: it assumes L1 classified the SQL, L4 decided
//! whether it may run, and (for writes) approval happened. It still re-checks the
//! two structural gates — `approved` and `allow_writes` — as defense in depth.

pub mod cancel;
pub mod read;
pub mod write;

pub use read::run_read;
pub use write::run_write;

use uuid::Uuid;

use crate::connection::LiveConnection;
use crate::error::{AppError, AppResult};
use crate::model::{Classification, Engine, ExecOutcome, QueryKind, SafetySettings};

/// Single entry point the `run_sql` command calls. Reads run against the read-only
/// pool; writes/DDL/privilege require explicit approval and route through the guarded
/// write path (which additionally enforces `allow_writes`).
pub async fn execute(
    live: &LiveConnection,
    engine: Engine,
    classification: &Classification,
    sql: &str,
    settings: &SafetySettings,
    approved: bool,
    query_id: Option<Uuid>,
) -> AppResult<ExecOutcome> {
    match classification.kind {
        QueryKind::Read => {
            let result = run_read(live, engine, sql, settings.max_rows, query_id).await?;
            Ok(ExecOutcome {
                result: Some(result),
                affected: None,
                committed: false,
            })
        }
        QueryKind::Write | QueryKind::Ddl | QueryKind::Privilege => {
            if !approved {
                return Err(AppError::Blocked {
                    reason: "this statement modifies data and requires explicit approval".into(),
                });
            }
            run_write(live, engine, sql, settings, query_id).await
        }
    }
}
