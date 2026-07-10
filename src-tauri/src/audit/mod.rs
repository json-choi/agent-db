//! Append-only, hash-chained audit log (compliance record). Every
//! ask/classify/preview/run action is recordable here via [`record`].
//!
//! Rows are inserted, never updated or deleted. [`verify_chain`] recomputes the
//! chain to surface post-hoc edits — see `chain` for the tamper-EVIDENT (not
//! tamper-proof) caveat.

pub mod chain;

use chrono::Utc;
use sqlx::Row;
use uuid::Uuid;

use crate::error::AppResult;
use crate::model::{AuditEntry, Engine, QueryKind};
use crate::store::{self, Store};

use chain::AuditFields;

/// Owned inputs for one audit record. The caller supplies the semantic fields;
/// `record` assigns `id`/`ts`, resolves `prev_hash`, and computes `hash`.
pub struct RecordArgs {
    pub connection_id: Uuid,
    pub engine: Engine,
    pub agent_prompt: Option<String>,
    pub sql: String,
    pub kind: QueryKind,
    /// e.g. "propose" | "approve" | "reject" | "execute" | "blocked".
    pub action: String,
    pub approved_by: Option<String>,
    pub affected_estimate: Option<i64>,
    pub error: Option<String>,
}

/// Append one entry: fetch the connection's latest hash, chain onto it, insert.
pub async fn record(store: &Store, args: RecordArgs) -> AppResult<AuditEntry> {
    let id = Uuid::new_v4();
    let ts = Utc::now();

    // Hold the chain lock across read-tail + insert. Without it two concurrent records
    // on the pooled store read the same tail hash and both insert with the same
    // prev_hash, forking the chain (verify_chain then reports false tampering).
    let _chain = store.audit_lock().lock().await;

    // Latest hash for THIS connection is the chain tail we link onto. Ordered by
    // rowid (insertion order) so concurrent same-ts rows still chain stably.
    let prev_hash: Option<String> = sqlx::query(
        "SELECT hash FROM audit_log WHERE connection_id = ?1 ORDER BY rowid DESC LIMIT 1",
    )
    .bind(args.connection_id.to_string())
    .fetch_optional(store.pool())
    .await?
    .map(|r| r.try_get("hash"))
    .transpose()?;

    let fields = AuditFields {
        connection_id: args.connection_id,
        ts,
        engine: args.engine,
        agent_prompt: args.agent_prompt.as_deref(),
        sql: &args.sql,
        kind: args.kind,
        action: &args.action,
        approved_by: args.approved_by.as_deref(),
        affected_estimate: args.affected_estimate,
        error: args.error.as_deref(),
    };
    let hash = chain::compute_hash(prev_hash.as_deref(), &fields);

    sqlx::query(
        r#"INSERT INTO audit_log
            (id, connection_id, ts, engine, agent_prompt, sql, kind, action,
             approved_by, affected_estimate, error, prev_hash, hash)
           VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)"#,
    )
    .bind(id.to_string())
    .bind(args.connection_id.to_string())
    .bind(ts)
    .bind(store::engine_str(args.engine))
    .bind(&args.agent_prompt)
    .bind(&args.sql)
    .bind(store::kind_str(args.kind))
    .bind(&args.action)
    .bind(&args.approved_by)
    .bind(args.affected_estimate)
    .bind(&args.error)
    .bind(&prev_hash)
    .bind(&hash)
    .execute(store.pool())
    .await?;

    Ok(AuditEntry {
        id,
        connection_id: args.connection_id,
        ts,
        engine: args.engine,
        agent_prompt: args.agent_prompt,
        sql: args.sql,
        kind: args.kind,
        action: args.action,
        approved_by: args.approved_by,
        affected_estimate: args.affected_estimate,
        error: args.error,
        prev_hash,
        hash,
    })
}

/// Audit rows and their verification result from one ordered database read.
/// Entries are returned newest-first for the UI, while verification runs over the
/// exact same rows in insertion order so the verdict cannot describe a different
/// snapshot if another audit record is appended concurrently.
pub async fn snapshot(
    store: &Store,
    connection_id: Uuid,
) -> AppResult<(Vec<AuditEntry>, bool, Option<i64>)> {
    let rows = sqlx::query("SELECT * FROM audit_log WHERE connection_id = ?1 ORDER BY rowid ASC")
        .bind(connection_id.to_string())
        .fetch_all(store.pool())
        .await?;

    let mut entries: Vec<AuditEntry> = rows.iter().map(row_to_audit).collect::<AppResult<_>>()?;
    let (ok, first_bad_index) = verify_entries(&entries);
    entries.reverse();
    Ok((entries, ok, first_bad_index))
}

/// Recompute the chain in insertion order and confirm every stored hash matches.
/// Returns `(false, Some(index))` at the first row that was edited, reordered, or had
/// its `prev_hash` broken (index = 0-based insertion-order position); `(true, None)`
/// if the whole chain verifies.
pub async fn verify_chain(store: &Store, connection_id: Uuid) -> AppResult<(bool, Option<i64>)> {
    let rows = sqlx::query(
        "SELECT * FROM audit_log WHERE connection_id = ?1 ORDER BY rowid ASC",
    )
    .bind(connection_id.to_string())
    .fetch_all(store.pool())
    .await?;

    let entries: Vec<AuditEntry> = rows.iter().map(row_to_audit).collect::<AppResult<_>>()?;
    Ok(verify_entries(&entries))
}

fn verify_entries(entries: &[AuditEntry]) -> (bool, Option<i64>) {
    let mut expected_prev: Option<String> = None;
    for (i, e) in entries.iter().enumerate() {
        // The stored prev_hash must equal the running tail…
        if e.prev_hash != expected_prev {
            return (false, Some(i as i64));
        }
        // …and the stored hash must match a fresh recomputation.
        let fields = AuditFields {
            connection_id: e.connection_id,
            ts: e.ts,
            engine: e.engine,
            agent_prompt: e.agent_prompt.as_deref(),
            sql: &e.sql,
            kind: e.kind,
            action: &e.action,
            approved_by: e.approved_by.as_deref(),
            affected_estimate: e.affected_estimate,
            error: e.error.as_deref(),
        };
        if chain::compute_hash(e.prev_hash.as_deref(), &fields) != e.hash {
            return (false, Some(i as i64));
        }
        expected_prev = Some(e.hash.clone());
    }
    (true, None)
}

fn row_to_audit(r: &sqlx::sqlite::SqliteRow) -> AppResult<AuditEntry> {
    Ok(AuditEntry {
        id: store::parse_uuid(r.try_get("id")?)?,
        connection_id: store::parse_uuid(r.try_get("connection_id")?)?,
        ts: r.try_get("ts")?,
        engine: store::parse_engine(r.try_get("engine")?)?,
        agent_prompt: r.try_get("agent_prompt")?,
        sql: r.try_get("sql")?,
        kind: store::parse_kind(r.try_get("kind")?)?,
        action: r.try_get("action")?,
        approved_by: r.try_get("approved_by")?,
        affected_estimate: r.try_get("affected_estimate")?,
        error: r.try_get("error")?,
        prev_hash: r.try_get("prev_hash")?,
        hash: r.try_get("hash")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
    use std::time::Duration;

    // Many concurrent records on ONE connection must not fork the hash chain. The
    // read-tail + insert is serialized by the store's audit lock; without it, parallel
    // records read the same tail and insert rows with a duplicated prev_hash, and
    // verify_chain then reports spurious tampering.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_records_keep_one_unbroken_chain() {
        let path =
            std::env::temp_dir().join(format!("dopedb-auditchain-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let opts = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_secs(5));
        // >1 connection so the records genuinely run in parallel (the fork condition).
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(opts)
            .await
            .unwrap();
        sqlx::raw_sql(
            "CREATE TABLE audit_log (
                id TEXT PRIMARY KEY, connection_id TEXT NOT NULL, ts TEXT NOT NULL,
                engine TEXT NOT NULL, agent_prompt TEXT, sql TEXT NOT NULL, kind TEXT NOT NULL,
                action TEXT NOT NULL, approved_by TEXT, affected_estimate INTEGER, error TEXT,
                prev_hash TEXT, hash TEXT NOT NULL);",
        )
        .execute(&pool)
        .await
        .unwrap();

        let store = Store::from_pool_for_test(pool);
        let conn = Uuid::new_v4();
        const N: usize = 40;
        let mut set = tokio::task::JoinSet::new();
        for i in 0..N {
            let s = store.clone();
            set.spawn(async move {
                record(
                    &s,
                    RecordArgs {
                        connection_id: conn,
                        engine: Engine::Sqlite,
                        agent_prompt: None,
                        sql: format!("SELECT {i}"),
                        kind: QueryKind::Read,
                        action: "execute".into(),
                        approved_by: None,
                        affected_estimate: None,
                        error: None,
                    },
                )
                .await
                .unwrap();
            });
        }
        while let Some(r) = set.join_next().await {
            r.unwrap();
        }

        let (ok, bad) = verify_chain(&store, conn).await.unwrap();
        assert!(ok, "chain must verify unbroken (first bad index {bad:?})");
        let (entries, snapshot_ok, snapshot_bad) = snapshot(&store, conn).await.unwrap();
        assert!(
            snapshot_ok,
            "snapshot must verify (first bad index {snapshot_bad:?})"
        );
        assert_eq!(entries.len(), N, "snapshot returns every audit row");
        assert!(
            entries
                .windows(2)
                .all(|pair| pair[0].prev_hash.as_deref() == Some(pair[1].hash.as_str())),
            "snapshot rows must be newest-first"
        );
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log")
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(n, N as i64, "every concurrent record was inserted");
        let _ = std::fs::remove_file(&path);
    }
}
