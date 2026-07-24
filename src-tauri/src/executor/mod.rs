//! Query executor. Dispatches an already-classified statement to the read path
//! (L2 read-only pool) or the guarded write path. L3 EXPLAIN-only impact previews
//! live in `safety::l3_preview`.
//!
//! The executor is the LAST stage: it assumes L1 classified the SQL and L4 decided
//! whether it may run. A target mutation additionally requires the unforgeable
//! [`ExecutionGrant`] issued by the durable Operation Runtime.

pub mod cancel;
pub mod read;
pub mod write;

pub use read::run_read;
pub use write::run_write;

use uuid::Uuid;

use crate::connection::LiveConnection;
use crate::error::{AppError, AppResult};
use crate::model::{Classification, Engine, ExecOutcome, QueryKind, SafetySettings};
use crate::operations::ExecutionGrant;

/// Single entry point the `run_sql` command calls. Reads run against the read-only
/// pool; writes/DDL/privilege require an exact Operation grant and route through the
/// guarded write path (which additionally enforces `allow_writes`).
pub async fn execute(
    live: &LiveConnection,
    engine: Engine,
    classification: &Classification,
    sql: &str,
    settings: &SafetySettings,
    grant: Option<&ExecutionGrant>,
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
            let grant = grant.ok_or_else(|| AppError::Blocked {
                reason: "this statement requires an exact approved operation grant".into(),
            })?;
            run_write(live, engine, sql, settings, grant, query_id).await
        }
    }
}
