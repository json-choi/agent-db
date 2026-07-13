//! The local application store: a WAL SQLite DB at
//! `dirs::data_dir()/dopedb/app.db` holding connections, safety settings,
//! query history, the audit log, saved dashboards, snippets, and the schema cache.
//!
//! Secrets are NEVER stored here — connections carry only a `secret_ref` that
//! points at an OS credential-store item. Row⇄model mapping is manual (`sqlx::query`,
//! runtime, not the compile-time `query!` macro) because this is a
//! runtime-arbitrary-SQL client.

mod migrations;

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use chrono::Utc;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::model::{
    ConnectionProfile, Dashboard, DashboardDraft, Engine, HistoryEntry, Provider, QueryKind,
    SafetySettings,
};

/// Handle to the local app.db. Cheap to clone (the pool is an `Arc` internally).
#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
    /// Serializes audit-chain appends. The chain is read-tail-then-insert, which two
    /// concurrent `audit::record` calls on the pooled (multi-connection) SQLite store
    /// would otherwise interleave — both reading the same tail hash and forking the
    /// chain, making `verify_chain` report false tampering.
    // ponytail: one global async lock; audit writes are rare, contention is a non-issue.
    audit_lock: Arc<Mutex<()>>,
}

impl Store {
    /// Open (creating if needed) the app.db and run migrations.
    pub async fn open() -> AppResult<Store> {
        let dir = dirs::data_dir()
            .ok_or_else(|| AppError::Config("no OS data dir (dirs::data_dir)".into()))?
            .join("dopedb");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("app.db");

        let opts = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .foreign_keys(true);

        let pool = SqlitePoolOptions::new().connect_with(opts).await?;
        sqlx::raw_sql(migrations::SCHEMA).execute(&pool).await?;
        // Idempotent column adds for DBs created before the column existed (SQLite has
        // no `ADD COLUMN IF NOT EXISTS`, so we run it and ignore the duplicate-column error).
        let _ = sqlx::query("ALTER TABLE connections ADD COLUMN project_dir TEXT")
            .execute(&pool)
            .await;
        let _ = sqlx::query("ALTER TABLE connections ADD COLUMN env TEXT")
            .execute(&pool)
            .await;
        let _ = sqlx::query("ALTER TABLE connections ADD COLUMN schema_group TEXT")
            .execute(&pool)
            .await;
        let _ = sqlx::query(
            "ALTER TABLE connections ADD COLUMN provider TEXT NOT NULL DEFAULT 'auto'",
        )
        .execute(&pool)
        .await;
        let _ = sqlx::query("ALTER TABLE connections ADD COLUMN driver_id TEXT")
            .execute(&pool)
            .await;
        migrate_audit_no_cascade(&pool).await?;
        Ok(Store { pool, audit_lock: Arc::new(Mutex::new(())) })
    }

    /// Escape hatch for sibling modules (audit) that own their own SQL.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Lock guarding audit-chain appends (see the field doc). `audit::record` holds
    /// this across its read-tail + insert so the chain can't fork under concurrency.
    pub(crate) fn audit_lock(&self) -> &Mutex<()> {
        &self.audit_lock
    }

    /// Wrap an already-open pool as a `Store` (tests only — bypasses `open`'s data-dir).
    #[cfg(test)]
    pub(crate) fn from_pool_for_test(pool: SqlitePool) -> Store {
        Store { pool, audit_lock: Arc::new(Mutex::new(())) }
    }

    // ── connections ────────────────────────────────────────────────────────

    /// Insert or update a connection profile; ensures a default safety row exists.
    pub async fn upsert_connection(
        &self,
        p: &ConnectionProfile,
    ) -> AppResult<ConnectionProfile> {
        let now = Utc::now();
        let extra = serde_json::to_string(&p.extra_params)?;
        sqlx::query(
            r#"INSERT INTO connections
                (id, name, engine, provider, driver_id, host, port, db_name, username, sslmode,
                 extra_params, secret_ref, readonly_default, allow_writes,
                 created_at, updated_at, project_dir, env, schema_group)
               VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?15,?16,?17,?18)
               ON CONFLICT(id) DO UPDATE SET
                 name=?2, engine=?3, provider=?4, driver_id=?5, host=?6, port=?7,
                 db_name=?8, username=?9, sslmode=?10, extra_params=?11, secret_ref=?12,
                 readonly_default=?13, allow_writes=?14, updated_at=?15, project_dir=?16,
                 env=?17, schema_group=?18"#,
        )
        .bind(p.id.to_string())
        .bind(&p.name)
        .bind(engine_str(p.engine))
        .bind(provider_str(p.provider))
        .bind(&p.driver_id)
        .bind(&p.host)
        .bind(p.port as i64)
        .bind(&p.database)
        .bind(&p.username)
        .bind(&p.sslmode)
        .bind(extra)
        .bind(&p.secret_ref)
        .bind(p.readonly_default)
        .bind(p.allow_writes)
        .bind(now)
        .bind(&p.project_dir)
        .bind(&p.env)
        .bind(&p.schema_group)
        .execute(&self.pool)
        .await?;

        // Guarantee a safety row for the connection (defaults on first insert).
        sqlx::query("INSERT OR IGNORE INTO connection_safety (connection_id) VALUES (?1)")
            .bind(p.id.to_string())
            .execute(&self.pool)
            .await?;

        Ok(p.clone())
    }

    pub async fn list_connections(&self) -> AppResult<Vec<ConnectionProfile>> {
        let rows = sqlx::query("SELECT * FROM connections ORDER BY name")
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(row_to_connection).collect()
    }

    pub async fn get_connection(&self, id: Uuid) -> AppResult<ConnectionProfile> {
        let row = sqlx::query("SELECT * FROM connections WHERE id = ?1")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("connection {id}")))?;
        row_to_connection(&row)
    }

    pub async fn set_connection_schema_group(
        &self,
        id: Uuid,
        schema_group: Option<String>,
    ) -> AppResult<ConnectionProfile> {
        sqlx::query("UPDATE connections SET schema_group = ?2, updated_at = ?3 WHERE id = ?1")
            .bind(id.to_string())
            .bind(schema_group)
            .bind(Utc::now())
            .execute(&self.pool)
            .await?;
        self.get_connection(id).await
    }

    /// Delete a connection; child rows cascade (safety, history, cache). The audit log
    /// deliberately does NOT cascade — compliance history survives connection deletion.
    pub async fn delete_connection(&self, id: Uuid) -> AppResult<()> {
        sqlx::query("DELETE FROM connections WHERE id = ?1")
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ── safety settings ────────────────────────────────────────────────────

    /// Returns stored safety settings, or the type default if none exist yet.
    pub async fn get_safety(&self, connection_id: Uuid) -> AppResult<SafetySettings> {
        let row = sqlx::query(
            "SELECT require_approval, allow_writes, wrap_writes_in_tx, explain_preview,
                    auto_run_reads, max_rows, exec_preview_row_limit
             FROM connection_safety WHERE connection_id = ?1",
        )
        .bind(connection_id.to_string())
        .fetch_optional(&self.pool)
        .await?;

        Ok(match row {
            None => SafetySettings::default(),
            Some(r) => SafetySettings {
                require_approval: r.try_get("require_approval")?,
                allow_writes: r.try_get("allow_writes")?,
                wrap_writes_in_tx: r.try_get("wrap_writes_in_tx")?,
                explain_preview: r.try_get("explain_preview")?,
                auto_run_reads: r.try_get("auto_run_reads")?,
                max_rows: r.try_get::<i64, _>("max_rows")? as u64,
                exec_preview_row_limit: r.try_get("exec_preview_row_limit")?,
            },
        })
    }

    pub async fn set_safety(
        &self,
        connection_id: Uuid,
        s: &SafetySettings,
    ) -> AppResult<()> {
        sqlx::query(
            r#"INSERT INTO connection_safety
                (connection_id, require_approval, allow_writes, wrap_writes_in_tx,
                 explain_preview, auto_run_reads, max_rows, exec_preview_row_limit)
               VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
               ON CONFLICT(connection_id) DO UPDATE SET
                 require_approval=?2, allow_writes=?3, wrap_writes_in_tx=?4,
                 explain_preview=?5, auto_run_reads=?6, max_rows=?7,
                 exec_preview_row_limit=?8"#,
        )
        .bind(connection_id.to_string())
        .bind(s.require_approval)
        .bind(s.allow_writes)
        .bind(s.wrap_writes_in_tx)
        .bind(s.explain_preview)
        .bind(s.auto_run_reads)
        .bind(s.max_rows as i64)
        .bind(s.exec_preview_row_limit)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ── query history ──────────────────────────────────────────────────────

    pub async fn insert_history(&self, h: &HistoryEntry) -> AppResult<()> {
        sqlx::query(
            r#"INSERT INTO query_history
                (id, connection_id, sql, kind, status, row_count, duration_ms,
                 error, executed_at, origin)
               VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)"#,
        )
        .bind(h.id.to_string())
        .bind(h.connection_id.to_string())
        .bind(&h.sql)
        .bind(kind_str(h.kind))
        .bind(&h.status)
        .bind(h.row_count)
        .bind(h.duration_ms)
        .bind(&h.error)
        .bind(h.executed_at)
        .bind(&h.origin)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_history(&self, connection_id: Uuid) -> AppResult<Vec<HistoryEntry>> {
        let rows = sqlx::query(
            "SELECT * FROM query_history WHERE connection_id = ?1
             ORDER BY executed_at DESC",
        )
        .bind(connection_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_history).collect()
    }

    pub async fn get_history(&self, id: Uuid) -> AppResult<HistoryEntry> {
        let row = sqlx::query("SELECT * FROM query_history WHERE id = ?1")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("query history {id}")))?;
        row_to_history(&row)
    }

    // ── saved dashboards ────────────────────────────────────────────────────

    /// Persist a new saved dashboard. IDs and timestamps are assigned here so
    /// Tauri and MCP callers share exactly the same creation semantics.
    pub async fn save_dashboard(&self, draft: &DashboardDraft) -> AppResult<Dashboard> {
        let id = Uuid::new_v4();
        let now = Utc::now();
        let visualization_json = serde_json::to_string(&draft.visualization)?;
        sqlx::query(
            r#"INSERT INTO dashboards
                (id, connection_id, title, description, sql, visualization_json,
                 created_at, updated_at)
               VALUES (?1,?2,?3,?4,?5,?6,?7,?7)"#,
        )
        .bind(id.to_string())
        .bind(draft.connection_id.to_string())
        .bind(&draft.title)
        .bind(&draft.description)
        .bind(&draft.sql)
        .bind(visualization_json)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(Dashboard {
            id,
            connection_id: draft.connection_id,
            title: draft.title.clone(),
            description: draft.description.clone(),
            sql: draft.sql.clone(),
            visualization: draft.visualization.clone(),
            created_at: now,
            updated_at: now,
        })
    }

    pub async fn list_dashboards(&self, connection_id: Uuid) -> AppResult<Vec<Dashboard>> {
        let rows = sqlx::query(
            "SELECT * FROM dashboards WHERE connection_id = ?1
             ORDER BY updated_at DESC, rowid DESC",
        )
        .bind(connection_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_dashboard).collect()
    }

    pub async fn get_dashboard(&self, id: Uuid) -> AppResult<Dashboard> {
        let row = sqlx::query("SELECT * FROM dashboards WHERE id = ?1")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("dashboard {id}")))?;
        row_to_dashboard(&row)
    }

    pub async fn delete_dashboard(&self, id: Uuid) -> AppResult<()> {
        let result = sqlx::query("DELETE FROM dashboards WHERE id = ?1")
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(AppError::NotFound(format!("dashboard {id}")));
        }
        Ok(())
    }

    // ── schema cache ───────────────────────────────────────────────────────

    /// Returns the cached catalog JSON for a connection, if any.
    pub async fn get_schema_cache(&self, connection_id: Uuid) -> AppResult<Option<String>> {
        let row = sqlx::query(
            "SELECT catalog_json FROM schema_cache WHERE connection_id = ?1",
        )
        .bind(connection_id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        Ok(match row {
            Some(r) => Some(r.try_get("catalog_json")?),
            None => None,
        })
    }

    pub async fn set_schema_cache(
        &self,
        connection_id: Uuid,
        catalog_json: &str,
    ) -> AppResult<()> {
        sqlx::query(
            r#"INSERT INTO schema_cache (connection_id, introspected_at, catalog_json)
               VALUES (?1, ?2, ?3)
               ON CONFLICT(connection_id) DO UPDATE SET
                 introspected_at=?2, catalog_json=?3"#,
        )
        .bind(connection_id.to_string())
        .bind(Utc::now())
        .bind(catalog_json)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Drop the cached catalog so the next introspection reads live — after a
    /// connection edit, or an explicit schema refresh.
    pub async fn clear_schema_cache(&self, connection_id: Uuid) -> AppResult<()> {
        sqlx::query("DELETE FROM schema_cache WHERE connection_id = ?1")
            .bind(connection_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

/// Startup migration: rebuild `audit_log` WITHOUT the old `ON DELETE CASCADE` so a
/// connection deletion can never erase its compliance history. Idempotent — only fires
/// when the stored table def still carries the cascade (fresh DBs skip it).
async fn migrate_audit_no_cascade(pool: &SqlitePool) -> AppResult<()> {
    let def: Option<String> =
        sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE type='table' AND name='audit_log'")
            .fetch_optional(pool)
            .await?;
    // Only the old schema mentions CASCADE (the new one has no FK at all).
    if !def
        .as_deref()
        .map(|s| s.to_uppercase().contains("CASCADE"))
        .unwrap_or(false)
    {
        return Ok(());
    }

    // SQLite can't ALTER away a constraint — rebuild the table, preserving every row.
    // audit_log has no incoming FKs, so this is safe with foreign_keys enabled.
    sqlx::raw_sql(
        r#"
        BEGIN;
        CREATE TABLE audit_log_new (
            id                TEXT PRIMARY KEY,
            connection_id     TEXT NOT NULL,
            ts                TEXT NOT NULL,
            engine            TEXT NOT NULL,
            agent_prompt      TEXT,
            sql               TEXT NOT NULL,
            kind              TEXT NOT NULL,
            action            TEXT NOT NULL,
            approved_by       TEXT,
            affected_estimate INTEGER,
            error             TEXT,
            prev_hash         TEXT,
            hash              TEXT NOT NULL
        );
        INSERT INTO audit_log_new
            SELECT id, connection_id, ts, engine, agent_prompt, sql, kind, action,
                   approved_by, affected_estimate, error, prev_hash, hash
            FROM audit_log;
        DROP TABLE audit_log;
        ALTER TABLE audit_log_new RENAME TO audit_log;
        CREATE INDEX IF NOT EXISTS idx_audit_conn ON audit_log(connection_id, ts);
        COMMIT;
        "#,
    )
    .execute(pool)
    .await?;
    Ok(())
}

// ── row → model mappers ─────────────────────────────────────────────────────

fn row_to_connection(r: &sqlx::sqlite::SqliteRow) -> AppResult<ConnectionProfile> {
    let extra_raw: String = r.try_get("extra_params")?;
    let extra_params: HashMap<String, String> =
        serde_json::from_str(&extra_raw).unwrap_or_default();
    Ok(ConnectionProfile {
        id: parse_uuid(r.try_get("id")?)?,
        name: r.try_get("name")?,
        engine: parse_engine(r.try_get("engine")?)?,
        provider: parse_provider(
            r.try_get("provider").unwrap_or_else(|_| "auto".to_string()),
        )?,
        driver_id: r.try_get("driver_id").unwrap_or(None),
        host: r.try_get("host")?,
        port: r.try_get::<i64, _>("port")? as u16,
        database: r.try_get("db_name")?,
        username: r.try_get("username")?,
        sslmode: r.try_get("sslmode")?,
        extra_params,
        readonly_default: r.try_get("readonly_default")?,
        allow_writes: r.try_get("allow_writes")?,
        secret_ref: r.try_get("secret_ref")?,
        project_dir: r.try_get("project_dir").unwrap_or(None),
        env: r.try_get("env").unwrap_or(None),
        schema_group: r.try_get("schema_group").unwrap_or(None),
    })
}

fn row_to_history(r: &sqlx::sqlite::SqliteRow) -> AppResult<HistoryEntry> {
    Ok(HistoryEntry {
        id: parse_uuid(r.try_get("id")?)?,
        connection_id: parse_uuid(r.try_get("connection_id")?)?,
        sql: r.try_get("sql")?,
        kind: parse_kind(r.try_get("kind")?)?,
        status: r.try_get("status")?,
        row_count: r.try_get("row_count")?,
        duration_ms: r.try_get("duration_ms")?,
        error: r.try_get("error")?,
        executed_at: r.try_get("executed_at")?,
        origin: r.try_get("origin")?,
    })
}

fn row_to_dashboard(r: &sqlx::sqlite::SqliteRow) -> AppResult<Dashboard> {
    let visualization_json: String = r.try_get("visualization_json")?;
    let visualization = serde_json::from_str(&visualization_json)?;
    crate::dashboard::validate_visualization(&visualization)?;
    Ok(Dashboard {
        id: parse_uuid(r.try_get("id")?)?,
        connection_id: parse_uuid(r.try_get("connection_id")?)?,
        title: r.try_get("title")?,
        description: r.try_get("description")?,
        sql: r.try_get("sql")?,
        visualization,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

// ── enum ⇄ text (kept in sync with model.rs serde `camelCase`) ──────────────

pub(crate) fn engine_str(e: Engine) -> &'static str {
    match e {
        Engine::Postgres => "postgres",
        Engine::Mysql => "mysql",
        Engine::Sqlite => "sqlite",
    }
}

pub(crate) fn parse_engine(s: String) -> AppResult<Engine> {
    match s.as_str() {
        "postgres" => Ok(Engine::Postgres),
        "mysql" => Ok(Engine::Mysql),
        "sqlite" => Ok(Engine::Sqlite),
        other => Err(AppError::Config(format!("unknown engine '{other}'"))),
    }
}

pub(crate) fn provider_str(provider: Provider) -> &'static str {
    match provider {
        Provider::Auto => "auto",
        Provider::Generic => "generic",
        Provider::Neon => "neon",
        Provider::PlanetScale => "planetScale",
    }
}

pub(crate) fn parse_provider(s: String) -> AppResult<Provider> {
    match s.as_str() {
        "auto" => Ok(Provider::Auto),
        "generic" => Ok(Provider::Generic),
        "neon" => Ok(Provider::Neon),
        "planetScale" => Ok(Provider::PlanetScale),
        other => Err(AppError::Config(format!("unknown provider '{other}'"))),
    }
}

pub(crate) fn kind_str(k: QueryKind) -> &'static str {
    match k {
        QueryKind::Read => "read",
        QueryKind::Write => "write",
        QueryKind::Ddl => "ddl",
        QueryKind::Privilege => "privilege",
    }
}

pub(crate) fn parse_kind(s: String) -> AppResult<QueryKind> {
    match s.as_str() {
        "read" => Ok(QueryKind::Read),
        "write" => Ok(QueryKind::Write),
        "ddl" => Ok(QueryKind::Ddl),
        "privilege" => Ok(QueryKind::Privilege),
        other => Err(AppError::Config(format!("unknown query kind '{other}'"))),
    }
}

pub(crate) fn parse_uuid(s: String) -> AppResult<Uuid> {
    Uuid::from_str(&s).map_err(|e| AppError::Config(format!("bad uuid '{s}': {e}")))
}

#[cfg(test)]
mod tests {
    use super::{migrate_audit_no_cascade, migrations, Store};
    use crate::error::AppError;
    use crate::model::{
        ConnectionProfile, DashboardDraft, DashboardKind, DashboardVisualization, Engine,
        HistoryEntry, Provider, QueryKind,
    };
    use chrono::Utc;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::collections::HashMap;
    use std::str::FromStr;
    use uuid::Uuid;

    // The OLD schema cascades; after migration, deleting a connection must NOT erase
    // its audit rows (the compliance guarantee), and re-running must be a no-op.
    #[tokio::test]
    async fn audit_survives_connection_delete_after_migration() {
        // max_connections(1) so the whole test shares one in-memory DB.
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap();

        sqlx::raw_sql(
            r#"
            CREATE TABLE connections (id TEXT PRIMARY KEY, name TEXT NOT NULL);
            CREATE TABLE audit_log (
                id TEXT PRIMARY KEY,
                connection_id TEXT NOT NULL REFERENCES connections(id) ON DELETE CASCADE,
                ts TEXT NOT NULL, engine TEXT NOT NULL, agent_prompt TEXT,
                sql TEXT NOT NULL, kind TEXT NOT NULL, action TEXT NOT NULL,
                approved_by TEXT, affected_estimate INTEGER, error TEXT,
                prev_hash TEXT, hash TEXT NOT NULL
            );
            INSERT INTO connections (id, name) VALUES ('c1','x');
            INSERT INTO audit_log (id, connection_id, ts, engine, sql, kind, action, hash)
                VALUES ('a1','c1','t','postgres','SELECT 1','read','execute','h');
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        migrate_audit_no_cascade(&pool).await.unwrap();
        migrate_audit_no_cascade(&pool).await.unwrap(); // idempotent

        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(n, 1, "row preserved by the rebuild");

        sqlx::query("DELETE FROM connections WHERE id='c1'")
            .execute(&pool)
            .await
            .unwrap();
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(n, 1, "audit history must survive connection deletion");
    }

    #[tokio::test]
    async fn dashboard_round_trip_delete_and_connection_cascade() {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap();
        sqlx::raw_sql(migrations::SCHEMA)
            .execute(&pool)
            .await
            .unwrap();
        let store = Store::from_pool_for_test(pool);
        let connection_id = Uuid::new_v4();
        store
            .upsert_connection(&ConnectionProfile {
                id: connection_id,
                name: "analytics".into(),
                engine: Engine::Sqlite,
                provider: Provider::Generic,
                driver_id: Some("sqlx-sqlite".into()),
                host: String::new(),
                port: 0,
                database: ":memory:".into(),
                username: String::new(),
                sslmode: "disable".into(),
                extra_params: HashMap::new(),
                readonly_default: true,
                allow_writes: false,
                secret_ref: None,
                project_dir: None,
                env: None,
                schema_group: None,
            })
            .await
            .unwrap();

        let loaded = store.get_connection(connection_id).await.unwrap();
        assert_eq!(loaded.provider, Provider::Generic);
        assert_eq!(loaded.driver_id.as_deref(), Some("sqlx-sqlite"));

        let draft = DashboardDraft {
            connection_id,
            title: "Daily visitors".into(),
            description: "Unique visitors per day".into(),
            sql: "SELECT day, visitors FROM daily_visitors".into(),
            visualization: DashboardVisualization {
                version: 1,
                kind: DashboardKind::Line,
                x_column: Some("day".into()),
                y_columns: vec!["visitors".into()],
            },
        };
        let saved = store.save_dashboard(&draft).await.unwrap();
        let listed = store.list_dashboards(connection_id).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, saved.id);
        assert_eq!(listed[0].visualization, draft.visualization);
        assert_eq!(store.get_dashboard(saved.id).await.unwrap().id, saved.id);

        let history = HistoryEntry {
            id: Uuid::new_v4(),
            connection_id,
            sql: "SELECT 1".into(),
            kind: QueryKind::Read,
            status: "ok".into(),
            row_count: Some(1),
            duration_ms: Some(1),
            error: None,
            executed_at: Utc::now(),
            origin: "agent".into(),
        };
        store.insert_history(&history).await.unwrap();
        assert_eq!(store.get_history(history.id).await.unwrap().id, history.id);

        sqlx::query(
            r#"UPDATE dashboards
               SET visualization_json = '{"version":2,"kind":"line","xColumn":null,"yColumns":[]}'
               WHERE id = ?1"#,
        )
        .bind(saved.id.to_string())
        .execute(store.pool())
        .await
        .unwrap();
        assert!(matches!(
            store.get_dashboard(saved.id).await,
            Err(AppError::Config(_))
        ));

        store.delete_dashboard(saved.id).await.unwrap();
        assert!(store
            .list_dashboards(connection_id)
            .await
            .unwrap()
            .is_empty());

        store.save_dashboard(&draft).await.unwrap();
        store.delete_connection(connection_id).await.unwrap();
        assert!(store
            .list_dashboards(connection_id)
            .await
            .unwrap()
            .is_empty());
    }
}
