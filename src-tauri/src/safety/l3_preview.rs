//! L3 — dry-run / impact preview.
//!
//! - **Reads:** `EXPLAIN` only, never executed. Parse the row estimate + plan.
//! - **Writes:** first take the `EXPLAIN` estimate; if it exceeds
//!   `settings.exec_preview_row_limit` (default 50_000, design-review #4) skip the
//!   execute-preview and show `"would lock ~N rows"` only. Otherwise open a txn,
//!   run the real statement under a short timeout, capture `rows_affected` as the
//!   **exact N**, and `ROLLBACK` unconditionally.
//! - **DDL / privilege:** no row-count preview.
//!
//! Never `EXPLAIN ANALYZE` a write (it executes). Statements using `RETURNING`
//! are flagged: triggers with external effects (NOTIFY, dblink) fire before the
//! rollback.

use std::time::Duration;

use sqlx::Row;
use tokio::time::timeout;

use crate::error::AppResult;
use crate::model::{Classification, PreviewMode, PreviewReport, QueryKind, SafetySettings};

use super::{PoolRef, STATEMENT_TIMEOUT_MS};

const PREVIEW_TIMEOUT: Duration = Duration::from_millis(STATEMENT_TIMEOUT_MS + 2_000);

/// Produce an impact preview for `sql`. `classification` decides the strategy;
/// `settings.exec_preview_row_limit` gates the execute-preview for writes.
pub async fn preview(
    pool: PoolRef<'_>,
    sql: &str,
    classification: &Classification,
    settings: &SafetySettings,
) -> AppResult<PreviewReport> {
    match classification.kind {
        QueryKind::Read => {
            let (estimated_rows, plan) = explain(pool, sql).await;
            Ok(PreviewReport {
                mode: PreviewMode::Explain,
                estimated_rows,
                exact_rows: None,
                plan,
                note: None,
            })
        }

        QueryKind::Ddl | QueryKind::Privilege => Ok(PreviewReport {
            mode: PreviewMode::Skipped,
            estimated_rows: None,
            exact_rows: None,
            plan: None,
            note: Some(
                "DDL / privilege change — no row-count preview; review the statement directly."
                    .into(),
            ),
        }),

        QueryKind::Write => {
            // EXPLAIN (no ANALYZE) plans a write without executing it.
            let (estimated_rows, plan) = explain(pool, sql).await;
            let side_effect = side_effect_note(sql);

            // Only a plain INSERT/UPDATE/DELETE rolls back cleanly. DDL/utility
            // (RENAME/OPTIMIZE/LOAD DATA…), multi-statement, and fail-safe/parse-error
            // writes implicit-commit or can't be trusted — running exec_rollback on them
            // would take PERMANENT effect before L4 approval. Estimate-only for those.
            if !classification.rollback_safe {
                return Ok(PreviewReport {
                    mode: PreviewMode::Skipped,
                    estimated_rows,
                    exact_rows: None,
                    plan,
                    note: Some(
                        "impact preview skipped — statement is not a plain \
                         INSERT/UPDATE/DELETE (would not roll back safely)"
                            .into(),
                    ),
                });
            }

            // Gate: over the threshold → estimate only, don't lock rows.
            if let Some(est) = estimated_rows {
                if est > settings.exec_preview_row_limit {
                    return Ok(PreviewReport {
                        mode: PreviewMode::Skipped,
                        estimated_rows: Some(est),
                        exact_rows: None,
                        plan,
                        note: Some(format!(
                            "would lock ~{est} rows; exact count not run (over the \
                             {}-row preview limit)",
                            settings.exec_preview_row_limit
                        )),
                    });
                }
            }

            match exec_rollback(pool, sql).await {
                Ok(exact) => Ok(PreviewReport {
                    mode: PreviewMode::ExecRollback,
                    estimated_rows,
                    exact_rows: Some(exact),
                    plan,
                    note: side_effect,
                }),
                // Preview failed (timeout, etc.) — degrade to estimate, never execute for real.
                Err(e) => Ok(PreviewReport {
                    mode: PreviewMode::Skipped,
                    estimated_rows,
                    exact_rows: None,
                    plan,
                    note: Some(format!("execute-preview did not complete: {e}")),
                }),
            }
        }
    }
}

/// Best-effort EXPLAIN → (estimated_rows, plan text). Failures are non-fatal
/// (returns `(None, None)`): a missing preview must not block classification.
async fn explain(pool: PoolRef<'_>, sql: &str) -> (Option<i64>, Option<String>) {
    let out = match pool {
        PoolRef::Postgres(p) => {
            let q = format!("EXPLAIN (FORMAT JSON) {sql}");
            timeout(PREVIEW_TIMEOUT, sqlx::query(&q).fetch_one(p))
                .await
                .ok()
                .and_then(|r| r.ok())
                .map(|row| {
                    let v: serde_json::Value = row
                        .try_get::<serde_json::Value, _>(0)
                        .ok()
                        .or_else(|| {
                            row.try_get::<String, _>(0)
                                .ok()
                                .and_then(|s| serde_json::from_str(&s).ok())
                        })
                        .unwrap_or(serde_json::Value::Null);
                    (find_number(&v, &["Plan Rows"]), Some(v.to_string()))
                })
        }
        PoolRef::Mysql(p) => {
            let q = format!("EXPLAIN FORMAT=JSON {sql}");
            timeout(PREVIEW_TIMEOUT, sqlx::query(&q).fetch_one(p))
                .await
                .ok()
                .and_then(|r| r.ok())
                .and_then(|row| row.try_get::<String, _>(0).ok())
                .map(|s| {
                    let v: serde_json::Value =
                        serde_json::from_str(&s).unwrap_or(serde_json::Value::Null);
                    let est = find_number(
                        &v,
                        &["rows_produced_per_join", "rows_examined_per_scan", "rows"],
                    );
                    (est, Some(s))
                })
        }
        PoolRef::Sqlite(p) => {
            let q = format!("EXPLAIN QUERY PLAN {sql}");
            timeout(PREVIEW_TIMEOUT, sqlx::query(&q).fetch_all(p))
                .await
                .ok()
                .and_then(|r| r.ok())
                .map(|rows| {
                    // Columns: id, parent, notused, detail. No row estimate available.
                    let plan = rows
                        .iter()
                        .filter_map(|r| r.try_get::<String, _>(3).ok())
                        .collect::<Vec<_>>()
                        .join("\n");
                    (None, Some(plan))
                })
        }
    };
    out.unwrap_or((None, None))
}

/// Open a txn, execute the real write, capture exact `rows_affected`, then
/// `ROLLBACK` unconditionally. PG bounds it with `statement_timeout`; all engines
/// get the wall-clock guard (a dropped future rolls the txn back).
async fn exec_rollback(pool: PoolRef<'_>, sql: &str) -> AppResult<i64> {
    // Each arm resolves its own concrete `*QueryResult`, whose inherent
    // `rows_affected()` gives the exact N before the unconditional rollback.
    macro_rules! run {
        ($conn:expr) => {{
            let res = timeout(PREVIEW_TIMEOUT, sqlx::query(sql).execute(&mut *$conn)).await;
            let _ = sqlx::query("ROLLBACK").execute(&mut *$conn).await;
            match res {
                Ok(Ok(r)) => r.rows_affected() as i64,
                Ok(Err(e)) => return Err(e.into()),
                Err(_) => {
                    return Err(crate::error::AppError::Safety(
                        "execute-preview exceeded the statement timeout (rolled back)".into(),
                    ))
                }
            }
        }};
    }

    let affected = match pool {
        PoolRef::Postgres(p) => {
            let mut conn = p.acquire().await?;
            sqlx::query("BEGIN").execute(&mut *conn).await?;
            let _ = sqlx::query(&format!("SET LOCAL statement_timeout = {STATEMENT_TIMEOUT_MS}"))
                .execute(&mut *conn)
                .await;
            run!(conn)
        }
        PoolRef::Mysql(p) => {
            let mut conn = p.acquire().await?;
            sqlx::query("START TRANSACTION").execute(&mut *conn).await?;
            run!(conn)
        }
        PoolRef::Sqlite(p) => {
            let mut conn = p.acquire().await?;
            sqlx::query("BEGIN").execute(&mut *conn).await?;
            run!(conn)
        }
    };
    Ok(affected)
}

/// Note statements whose external side effects fire before the rollback.
fn side_effect_note(sql: &str) -> Option<String> {
    if sql.to_uppercase().contains("RETURNING") {
        Some(
            "uses RETURNING and may reference functions/triggers — external effects \
             (NOTIFY, dblink, writes in trigger bodies) can fire before the rollback."
                .into(),
        )
    } else {
        None
    }
}

/// Recursively find the first value for any of `keys`, coercing to i64.
fn find_number(v: &serde_json::Value, keys: &[&str]) -> Option<i64> {
    match v {
        serde_json::Value::Object(m) => {
            for k in keys {
                if let Some(n) = m.get(*k).and_then(as_i64) {
                    return Some(n);
                }
            }
            m.values().find_map(|vv| find_number(vv, keys))
        }
        serde_json::Value::Array(a) => a.iter().find_map(|vv| find_number(vv, keys)),
        _ => None,
    }
}

fn as_i64(v: &serde_json::Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_f64().map(|f| f as i64))
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_nested_row_estimate() {
        let v: serde_json::Value =
            serde_json::from_str(r#"[{"Plan":{"Node Type":"Seq Scan","Plan Rows":1234}}]"#).unwrap();
        assert_eq!(find_number(&v, &["Plan Rows"]), Some(1234));
    }

    #[test]
    fn returning_is_flagged() {
        assert!(side_effect_note("delete from t where id=1 returning *").is_some());
        assert!(side_effect_note("delete from t where id=1").is_none());
    }

    // The execute+ROLLBACK branch runs ONLY for a rollback_safe write. A DDL and a
    // fail-safe/parse-error write both classify Write but rollback_safe=false, so L3
    // must estimate-only (Skipped), never ExecRollback (which would commit before L4).
    #[test]
    fn non_rollback_safe_write_is_skipped_not_execrollback() {
        use crate::model::Engine;
        use crate::safety::classify;

        for sql in ["RENAME TABLE a TO b", "OPTIMIZE TABLE t", "this is not sql"] {
            let c = classify(sql, Engine::Mysql).unwrap();
            assert_eq!(c.kind, QueryKind::Write, "{sql} classifies as write");
            assert!(!c.rollback_safe, "{sql} must not be rollback-safe");
            // rollback_safe=false is exactly the guard that forces Skipped in preview().
        }

        // A plain UPDATE is the only shape that reaches the ExecRollback path.
        let ok = classify("UPDATE t SET x=1 WHERE id=1", Engine::Mysql).unwrap();
        assert_eq!(ok.kind, QueryKind::Write);
        assert!(ok.rollback_safe);
    }
}
