//! Lightweight, read-mostly database health snapshots for MCP query planning.
//!
//! The collector deliberately returns aggregates only: no other session's SQL text,
//! parameters, usernames, or client addresses leave the Rust trust boundary. PostgreSQL
//! can opt in to its built-in `pg_monitor` role through a separate fixed GRANT/REVOKE
//! command; MySQL and SQLite degrade to basic coverage without setup.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::Row;
use tokio::time::timeout;

use crate::connection::{LiveConnection, Pool};
use crate::error::{AppError, AppResult};
use crate::model::{Engine, MonitoringStatus};

const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const LONG_QUERY_SECONDS: i64 = 30;

/// Aggregate-only health context supplied to an MCP agent before query execution.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthSnapshot {
    pub level: String,
    pub coverage: String,
    pub total_connections: Option<i64>,
    pub max_connections: Option<i64>,
    pub connection_usage_percent: Option<f64>,
    pub active_queries: Option<i64>,
    pub long_running_queries: Option<i64>,
    pub lock_waits: Option<i64>,
    pub replication_lag_seconds: Option<f64>,
    pub reasons: Vec<String>,
    pub captured_at: DateTime<Utc>,
}

impl HealthSnapshot {
    fn unknown(coverage: &str, reason: impl Into<String>) -> Self {
        Self {
            level: "caution".into(),
            coverage: coverage.into(),
            total_connections: None,
            max_connections: None,
            connection_usage_percent: None,
            active_queries: None,
            long_running_queries: None,
            lock_waits: None,
            replication_lag_seconds: None,
            reasons: vec![reason.into()],
            captured_at: Utc::now(),
        }
    }
}

/// Inspect whether the current connection has PostgreSQL monitoring visibility.
pub async fn status(live: &LiveConnection, engine: Engine) -> AppResult<MonitoringStatus> {
    match (engine, &live.read_pool) {
        (Engine::Postgres, Pool::Postgres(pool)) => {
            let probe = sqlx::query(
                r#"SELECT current_user::text AS current_user,
                    EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'pg_monitor') AS role_available,
                    COALESCE((
                      SELECT pg_has_role(oid, 'member')
                      FROM pg_roles WHERE rolname = 'pg_monitor'
                    ), false) AS role_granted,
                    COALESCE((
                      SELECT rolsuper
                      FROM pg_roles WHERE rolname = current_user
                    ), false) AS can_manage"#,
            )
            .fetch_one(pool);
            let row = timeout(PROBE_TIMEOUT, probe)
                .await
                .map_err(|_| AppError::Safety("monitoring status check timed out".into()))??;
            let role_available: bool = row.try_get("role_available")?;
            let role_granted: bool = row.try_get("role_granted")?;
            Ok(MonitoringStatus {
                engine,
                coverage: if role_granted { "full" } else { "limited" }.into(),
                role_available,
                role_granted,
                current_user: Some(row.try_get("current_user")?),
                can_manage: row.try_get("can_manage")?,
                note: if role_granted {
                    "pg_monitor is enabled; MCP planning can see aggregate server activity."
                } else if role_available {
                    "Only limited activity is visible until pg_monitor is granted."
                } else {
                    "This PostgreSQL server does not expose the pg_monitor predefined role."
                }
                .into(),
            })
        }
        (Engine::Mysql, Pool::Mysql(_)) => Ok(MonitoringStatus {
            engine,
            coverage: "basic".into(),
            role_available: false,
            role_granted: false,
            current_user: None,
            can_manage: false,
            note: "MCP planning uses available Performance Schema aggregates; no role setup is required."
                .into(),
        }),
        (Engine::Sqlite, Pool::Sqlite(_)) => Ok(MonitoringStatus {
            engine,
            coverage: "basic".into(),
            role_available: false,
            role_granted: false,
            current_user: None,
            can_manage: false,
            note: "SQLite is local; MCP planning relies on query plans and bounded execution."
                .into(),
        }),
        _ => Err(AppError::Config(
            "connection engine does not match its monitoring pool".into(),
        )),
    }
}

/// Apply or remove PostgreSQL's fixed `pg_monitor` membership for CURRENT_USER.
/// This is intentionally not an arbitrary-SQL surface and does not enable general writes.
pub async fn set_postgres_role(live: &LiveConnection, enabled: bool) -> AppResult<()> {
    let Pool::Postgres(pool) = &live.write_pool else {
        return Err(AppError::Config(
            "pg_monitor is only available for PostgreSQL connections".into(),
        ));
    };
    let sql = if enabled {
        "GRANT pg_monitor TO CURRENT_USER"
    } else {
        "REVOKE pg_monitor FROM CURRENT_USER"
    };
    timeout(PROBE_TIMEOUT, sqlx::query(sql).execute(pool))
        .await
        .map_err(|_| AppError::Safety("pg_monitor role change timed out".into()))??;
    Ok(())
}

/// Capture aggregate server pressure without exposing query text or session identity.
/// Probe failures degrade to a caution snapshot so MCP planning can continue safely.
pub async fn snapshot(live: &LiveConnection, engine: Engine) -> HealthSnapshot {
    let status = match status(live, engine).await {
        Ok(status) => status,
        Err(e) => {
            return HealthSnapshot::unknown("unknown", format!("Monitoring unavailable: {e}"))
        }
    };
    match (&live.read_pool, engine) {
        (Pool::Postgres(pool), Engine::Postgres) => {
            let probe = sqlx::query(
                r#"SELECT
                    (SELECT count(*)::bigint FROM pg_stat_activity) AS total_connections,
                    current_setting('max_connections')::bigint AS max_connections,
                    count(*) FILTER (WHERE state = 'active')::bigint AS active_queries,
                    count(*) FILTER (
                      WHERE state = 'active'
                        AND query_start < now() - interval '30 seconds'
                    )::bigint AS long_running_queries,
                    count(*) FILTER (WHERE wait_event_type = 'Lock')::bigint AS lock_waits,
                    CASE
                      WHEN NOT pg_is_in_recovery() THEN NULL
                      WHEN pg_last_wal_receive_lsn() IS NULL THEN NULL
                      WHEN pg_last_wal_receive_lsn() = pg_last_wal_replay_lsn() THEN 0
                      WHEN pg_last_xact_replay_timestamp() IS NOT NULL
                        THEN EXTRACT(EPOCH FROM now() - pg_last_xact_replay_timestamp())::double precision
                      ELSE NULL
                    END AS replication_lag_seconds
                  FROM pg_stat_activity
                  WHERE datname = current_database()"#,
            )
            .fetch_one(pool);
            match timeout(PROBE_TIMEOUT, probe).await {
                Ok(Ok(row)) => evaluate(
                    &status.coverage,
                    row.try_get("total_connections").ok(),
                    row.try_get("max_connections").ok(),
                    row.try_get("active_queries").ok(),
                    row.try_get("long_running_queries").ok(),
                    row.try_get("lock_waits").ok(),
                    row.try_get("replication_lag_seconds").ok(),
                ),
                Ok(Err(e)) => HealthSnapshot::unknown(
                    &status.coverage,
                    format!("PostgreSQL monitoring probe failed: {e}"),
                ),
                Err(_) => HealthSnapshot::unknown(
                    &status.coverage,
                    "PostgreSQL monitoring probe timed out",
                ),
            }
        }
        (Pool::Mysql(pool), Engine::Mysql) => {
            let probe = sqlx::query(
                r#"SELECT
                    (SELECT CAST(VARIABLE_VALUE AS SIGNED) FROM performance_schema.global_status
                      WHERE VARIABLE_NAME = 'Threads_connected') AS total_connections,
                    (SELECT CAST(VARIABLE_VALUE AS SIGNED) FROM performance_schema.global_variables
                      WHERE VARIABLE_NAME = 'max_connections') AS max_connections,
                    (SELECT COUNT(*) FROM performance_schema.threads
                      WHERE TYPE = 'FOREGROUND' AND PROCESSLIST_COMMAND <> 'Sleep') AS active_queries,
                    (SELECT COUNT(*) FROM performance_schema.threads
                      WHERE TYPE = 'FOREGROUND' AND PROCESSLIST_COMMAND <> 'Sleep'
                        AND PROCESSLIST_TIME >= 30) AS long_running_queries,
                    (SELECT COUNT(*) FROM performance_schema.data_lock_waits) AS lock_waits"#,
            )
            .fetch_one(pool);
            match timeout(PROBE_TIMEOUT, probe).await {
                Ok(Ok(row)) => evaluate(
                    &status.coverage,
                    row.try_get("total_connections").ok(),
                    row.try_get("max_connections").ok(),
                    row.try_get("active_queries").ok(),
                    row.try_get("long_running_queries").ok(),
                    row.try_get("lock_waits").ok(),
                    None,
                ),
                Ok(Err(e)) => HealthSnapshot::unknown(
                    &status.coverage,
                    format!("MySQL Performance Schema monitoring is unavailable: {e}"),
                ),
                Err(_) => HealthSnapshot::unknown(
                    &status.coverage,
                    "MySQL monitoring probe timed out",
                ),
            }
        }
        (Pool::Sqlite(_), Engine::Sqlite) => HealthSnapshot {
            level: "normal".into(),
            coverage: status.coverage,
            total_connections: None,
            max_connections: None,
            connection_usage_percent: None,
            active_queries: None,
            long_running_queries: None,
            lock_waits: None,
            replication_lag_seconds: None,
            reasons: vec![
                "SQLite is local; server saturation metrics do not apply. Query-plan and timeout guards remain active."
                    .into(),
            ],
            captured_at: Utc::now(),
        },
        _ => HealthSnapshot::unknown("unknown", "Monitoring pool/engine mismatch"),
    }
}

fn evaluate(
    coverage: &str,
    total_connections: Option<i64>,
    max_connections: Option<i64>,
    active_queries: Option<i64>,
    long_running_queries: Option<i64>,
    lock_waits: Option<i64>,
    replication_lag_seconds: Option<f64>,
) -> HealthSnapshot {
    let connection_usage_percent = match (total_connections, max_connections) {
        (Some(total), Some(max)) if max > 0 => Some(total as f64 * 100.0 / max as f64),
        _ => None,
    };
    let mut score = 0u8;
    let mut reasons = Vec::new();
    if let Some(usage) = connection_usage_percent {
        if usage >= 80.0 {
            score = 2;
            reasons.push(format!("Connection usage is high ({usage:.0}%)."));
        } else if usage >= 60.0 {
            score = score.max(1);
            reasons.push(format!("Connection usage is elevated ({usage:.0}%)."));
        }
    }
    if let Some(n) = long_running_queries.filter(|n| *n > 0) {
        score = score.max(1);
        reasons.push(format!(
            "{n} queries have been active for at least {LONG_QUERY_SECONDS}s."
        ));
    }
    if let Some(n) = lock_waits.filter(|n| *n > 0) {
        score = if n >= 3 { 2 } else { score.max(1) };
        reasons.push(format!("{n} sessions are waiting on locks."));
    }
    if let Some(lag) = replication_lag_seconds.filter(|lag| *lag >= 30.0) {
        score = score.max(1);
        reasons.push(format!("Read-replica replay lag is {lag:.0}s."));
    }
    if coverage == "limited" {
        reasons.push("Monitoring coverage is limited without pg_monitor.".into());
    }
    if reasons.is_empty() {
        reasons.push("No aggregate database-pressure warning was detected.".into());
    }
    HealthSnapshot {
        level: match score {
            2 => "busy",
            1 => "caution",
            _ => "normal",
        }
        .into(),
        coverage: coverage.into(),
        total_connections,
        max_connections,
        connection_usage_percent,
        active_queries,
        long_running_queries,
        lock_waits,
        replication_lag_seconds,
        reasons,
        captured_at: Utc::now(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pressure_evaluation_is_aggregate_and_deterministic() {
        let health = evaluate("full", Some(85), Some(100), Some(8), Some(2), Some(1), None);
        assert_eq!(health.level, "busy");
        assert_eq!(health.connection_usage_percent, Some(85.0));
        assert!(health
            .reasons
            .iter()
            .any(|r| r.contains("Connection usage")));
        assert!(health.reasons.iter().all(|r| !r.contains("SELECT")));
    }

    #[test]
    fn limited_coverage_is_always_disclosed() {
        let health = evaluate(
            "limited",
            Some(1),
            Some(100),
            Some(1),
            Some(0),
            Some(0),
            None,
        );
        assert_eq!(health.level, "normal");
        assert!(health.reasons.iter().any(|r| r.contains("pg_monitor")));
    }
}
