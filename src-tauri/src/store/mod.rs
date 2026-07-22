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
use sqlx::{AssertSqlSafe, Row, Sqlite, SqlitePool, Transaction};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::agent::{AgentProvider, ChatMessageRecord, ChatThread};
use crate::error::{AppError, AppResult};
use crate::model::{
    ConnectionProfile, Dashboard, DashboardDraft, Engine, HistoryEntry, Provider, QueryKind,
    SafetySettings, Workspace, WorkspaceKind, WorkspaceLifecycleState,
    WorkspaceConnectionAccess,
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
        let _ = sqlx::query(
            "ALTER TABLE connections ADD COLUMN workspace_access TEXT NOT NULL DEFAULT 'local'",
        )
        .execute(&pool)
        .await;
        let _ = sqlx::query("ALTER TABLE agent_chat_threads ADD COLUMN connection_id TEXT")
            .execute(&pool)
            .await;
        add_workspace_columns(&pool).await;
        migrate_workspace_foundation(&pool).await?;
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

    // ── workspaces ─────────────────────────────────────────────────────────

    /// List locally available, active workspaces. Milestone 0 normally returns
    /// only the account-free Personal Workspace created by the migration.
    pub async fn list_workspaces(&self) -> AppResult<Vec<Workspace>> {
        let rows = sqlx::query("SELECT * FROM workspaces WHERE lifecycle_state = 'active' ORDER BY kind, name")
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_workspace).collect()
    }

    /// Reconcile the active Better Auth organizations into the local workspace index.
    /// Removed memberships are archived rather than deleted so already-synced local
    /// resources remain recoverable, and an inaccessible active team falls back safely
    /// to the account-free Personal Workspace.
    pub async fn sync_team_workspaces(&self, workspaces: &[(Uuid, String)]) -> AppResult<()> {
        let personal_id = Uuid::parse_str(migrations::PERSONAL_WORKSPACE_ID)
            .map_err(|_| AppError::Config("invalid personal workspace id".into()))?;
        if workspaces.iter().any(|(id, _)| *id == personal_id) {
            return Err(AppError::Config(
                "remote workspace conflicts with the Personal Workspace".into(),
            ));
        }
        let now = Utc::now();
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "UPDATE workspaces SET lifecycle_state = 'archived', updated_at = ?1
             WHERE kind = 'team'",
        )
        .bind(now)
        .execute(&mut *tx)
        .await?;
        for (id, name) in workspaces {
            sqlx::query(
                "INSERT INTO workspaces
                    (id, name, kind, lifecycle_state, created_at, updated_at)
                 VALUES (?1, ?2, 'team', 'active', ?3, ?3)
                 ON CONFLICT(id) DO UPDATE SET
                    name = excluded.name,
                    lifecycle_state = 'active',
                    updated_at = excluded.updated_at
                 WHERE workspaces.kind = 'team'",
            )
            .bind(id.to_string())
            .bind(name)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(
            "UPDATE app_settings SET value = ?1
             WHERE key = 'active_workspace_id'
               AND value IN (
                 SELECT id FROM workspaces
                 WHERE kind = 'team' AND lifecycle_state != 'active'
               )",
        )
        .bind(personal_id.to_string())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn active_workspace(&self) -> AppResult<Workspace> {
        let row = sqlx::query(
            "SELECT w.* FROM workspaces w
             JOIN app_settings s ON s.key = 'active_workspace_id' AND s.value = w.id
             WHERE w.lifecycle_state = 'active'",
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::Config("no active workspace is configured".into()))?;
        row_to_workspace(&row)
    }

    pub async fn active_workspace_id(&self) -> AppResult<Uuid> {
        Ok(self.active_workspace().await?.id)
    }

    pub async fn set_active_workspace(&self, id: Uuid) -> AppResult<Workspace> {
        let row = sqlx::query(
            "SELECT * FROM workspaces WHERE id = ?1 AND lifecycle_state = 'active'",
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("workspace {id}")))?;
        sqlx::query(
            "INSERT INTO app_settings (key, value) VALUES ('active_workspace_id', ?1)
             ON CONFLICT(key) DO UPDATE SET value = ?1",
        )
        .bind(id.to_string())
        .execute(&self.pool)
        .await?;
        row_to_workspace(&row)
    }

    // ── connections ────────────────────────────────────────────────────────

    /// Accept a new UUID or one already owned by the active workspace. Callers that
    /// may touch the credential store use this before any secret-side effect.
    pub async fn ensure_connection_write_scope(&self, id: Uuid) -> AppResult<()> {
        let workspace_id = self.active_workspace_id().await?;
        let owner: Option<String> =
            sqlx::query_scalar("SELECT workspace_id FROM connections WHERE id = ?1")
                .bind(id.to_string())
                .fetch_optional(&self.pool)
                .await?;
        if owner.is_some_and(|owner| owner != workspace_id.to_string()) {
            return Err(AppError::NotFound(format!("connection {id}")));
        }
        Ok(())
    }

    /// Insert or update a connection profile; ensures a default safety row exists.
    pub async fn upsert_connection(
        &self,
        p: &ConnectionProfile,
    ) -> AppResult<ConnectionProfile> {
        let now = Utc::now();
        let extra = serde_json::to_string(&p.extra_params)?;
        let workspace_id = self.active_workspace_id().await?;
        self.ensure_connection_write_scope(p.id).await?;
        let existing: Option<(String, i64)> = sqlx::query_as(
            "SELECT workspace_id, revision FROM connections WHERE id = ?1",
        )
        .bind(p.id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        let revision = existing.map_or(1, |(_, revision)| revision + 1);
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            r#"INSERT INTO connections
                (id, name, engine, provider, driver_id, host, port, db_name, username, sslmode,
                 extra_params, secret_ref, readonly_default, allow_writes,
                 created_at, updated_at, env, schema_group, workspace_id, revision, sync_status,
                 workspace_access, deleted_at)
               VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?15,?16,?17,
                       ?18,?19,'dirty',?20,NULL)
               ON CONFLICT(id) DO UPDATE SET
                 name=?2, engine=?3, provider=?4, driver_id=?5, host=?6, port=?7,
                 db_name=?8, username=?9, sslmode=?10, extra_params=?11, secret_ref=?12,
                 readonly_default=?13, allow_writes=?14, updated_at=?15,
                 env=?16, schema_group=?17, revision=?19, sync_status='dirty',
                 workspace_access=?20, deleted_at=NULL"#,
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
        .bind(&p.env)
        .bind(&p.schema_group)
        .bind(workspace_id.to_string())
        .bind(revision)
        .bind(workspace_access_str(p.workspace_access))
        .execute(&mut *tx)
        .await?;

        // Guarantee a safety row for the connection (defaults on first insert).
        sqlx::query("INSERT OR IGNORE INTO connection_safety (connection_id) VALUES (?1)")
            .bind(p.id.to_string())
            .execute(&mut *tx)
            .await?;

        enqueue_outbox(
            &mut tx,
            workspace_id,
            "connection",
            p.id,
            "upsert",
            revision,
        )
        .await?;
        tx.commit().await?;

        Ok(p.clone())
    }

    /// Reconcile shared connection templates for one team workspace. Existing local
    /// usernames, per-member options, and secret references are preserved; only the
    /// non-secret shared template and cached server permission are replaced.
    pub async fn sync_remote_connections(
        &self,
        workspace_id: Uuid,
        connections: &[(ConnectionProfile, i64)],
    ) -> AppResult<()> {
        for (profile, _) in connections {
            let owner: Option<String> = sqlx::query_scalar(
                "SELECT workspace_id FROM connections WHERE id = ?1",
            )
            .bind(profile.id.to_string())
            .fetch_optional(&self.pool)
            .await?;
            if owner.is_some_and(|owner| owner != workspace_id.to_string()) {
                return Err(AppError::Config(format!(
                    "remote connection {} conflicts with another workspace",
                    profile.id
                )));
            }
        }

        let now = Utc::now();
        let mut tx = self.pool.begin().await?;
        let existing_remote: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM connections WHERE workspace_id = ?1 AND remote_id IS NOT NULL",
        )
        .bind(workspace_id.to_string())
        .fetch_all(&mut *tx)
        .await?;
        let incoming = connections
            .iter()
            .map(|(profile, _)| profile.id.to_string())
            .collect::<Vec<_>>();
        for id in existing_remote.iter().filter(|id| !incoming.contains(id)) {
            sqlx::query(
                "UPDATE connections SET deleted_at = ?2, updated_at = ?2
                 WHERE id = ?1 AND workspace_id = ?3 AND remote_id IS NOT NULL",
            )
            .bind(id)
            .bind(now)
            .bind(workspace_id.to_string())
            .execute(&mut *tx)
            .await?;
        }

        for (profile, revision) in connections {
            let extra = serde_json::to_string(&profile.extra_params)?;
            sqlx::query(
                r#"INSERT INTO connections
                    (id, name, engine, provider, driver_id, host, port, db_name, username,
                     sslmode, extra_params, secret_ref, readonly_default, allow_writes,
                     created_at, updated_at, env, schema_group, workspace_id, remote_id,
                     revision, sync_status, workspace_access, deleted_at)
                   VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?15,
                           ?16,?17,?18,?1,?19,'synced',?20,NULL)
                   ON CONFLICT(id) DO UPDATE SET
                     name=?2, engine=?3, provider=?4, driver_id=?5, host=?6, port=?7,
                     db_name=?8, sslmode=?10, readonly_default=?13, allow_writes=?14,
                     updated_at=?15, env=?16, schema_group=?17, remote_id=?1, revision=?19,
                     sync_status='synced', workspace_access=?20, deleted_at=NULL
                   WHERE connections.workspace_id=?18"#,
            )
            .bind(profile.id.to_string())
            .bind(&profile.name)
            .bind(engine_str(profile.engine))
            .bind(provider_str(profile.provider))
            .bind(&profile.driver_id)
            .bind(&profile.host)
            .bind(profile.port as i64)
            .bind(&profile.database)
            .bind(&profile.username)
            .bind(&profile.sslmode)
            .bind(extra)
            .bind(&profile.secret_ref)
            .bind(profile.readonly_default)
            .bind(profile.allow_writes)
            .bind(now)
            .bind(&profile.env)
            .bind(&profile.schema_group)
            .bind(workspace_id.to_string())
            .bind(*revision)
            .bind(workspace_access_str(profile.workspace_access))
            .execute(&mut *tx)
            .await?;
            sqlx::query("INSERT OR IGNORE INTO connection_safety (connection_id) VALUES (?1)")
                .bind(profile.id.to_string())
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "UPDATE connection_safety SET allow_writes = ?2 WHERE connection_id = ?1",
            )
            .bind(profile.id.to_string())
            .bind(profile.allow_writes && profile.workspace_access.can_write())
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Save only this member's username and OS-secret reference for a shared template.
    pub async fn bind_connection_credentials(
        &self,
        id: Uuid,
        username: &str,
    ) -> AppResult<ConnectionProfile> {
        let profile = self.get_connection(id).await?;
        if !profile.workspace_access.can_read() {
            return Err(AppError::Blocked { reason: "workspace role cannot execute this connection".into() });
        }
        let result = sqlx::query(
            "UPDATE connections SET username=?2, secret_ref=?1, updated_at=?3
             WHERE id=?1 AND workspace_id=?4 AND remote_id IS NOT NULL AND deleted_at IS NULL",
        )
        .bind(id.to_string())
        .bind(username.trim())
        .bind(Utc::now())
        .bind(self.active_workspace_id().await?.to_string())
        .execute(&self.pool)
        .await?;
        if result.rows_affected() != 1 {
            return Err(AppError::NotFound(format!("shared connection {id}")));
        }
        self.get_connection(id).await
    }

    pub async fn list_connections(&self) -> AppResult<Vec<ConnectionProfile>> {
        let workspace_id = self.active_workspace_id().await?;
        let rows = sqlx::query(
            "SELECT * FROM connections
             WHERE workspace_id = ?1 AND deleted_at IS NULL ORDER BY name",
        )
            .bind(workspace_id.to_string())
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(row_to_connection).collect()
    }

    pub async fn get_connection(&self, id: Uuid) -> AppResult<ConnectionProfile> {
        let workspace_id = self.active_workspace_id().await?;
        let row = sqlx::query(
            "SELECT * FROM connections
             WHERE id = ?1 AND workspace_id = ?2 AND deleted_at IS NULL",
        )
            .bind(id.to_string())
            .bind(workspace_id.to_string())
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
        let workspace_id = self.active_workspace_id().await?;
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(
            "UPDATE connections SET schema_group = ?2, updated_at = ?3,
                    revision = revision + 1, sync_status = 'dirty'
             WHERE id = ?1 AND workspace_id = ?4 AND deleted_at IS NULL",
        )
            .bind(id.to_string())
            .bind(schema_group)
            .bind(Utc::now())
            .bind(workspace_id.to_string())
            .execute(&mut *tx)
            .await?;
        if result.rows_affected() != 1 {
            return Err(AppError::NotFound(format!("connection {id}")));
        }
        let revision: i64 = sqlx::query_scalar("SELECT revision FROM connections WHERE id = ?1")
            .bind(id.to_string())
            .fetch_one(&mut *tx)
            .await?;
        enqueue_outbox(&mut tx, workspace_id, "connection", id, "upsert", revision).await?;
        tx.commit().await?;
        self.get_connection(id).await
    }

    /// Update several connections as one transaction so a failed group operation
    /// cannot leave only part of the requested membership persisted.
    pub async fn set_connections_schema_group(
        &self,
        ids: &[Uuid],
        schema_group: Option<String>,
    ) -> AppResult<()> {
        let workspace_id = self.active_workspace_id().await?;
        let mut tx = self.pool.begin().await?;
        let updated_at = Utc::now();
        for id in ids {
            let result = sqlx::query(
                "UPDATE connections SET schema_group = ?2, updated_at = ?3,
                        revision = revision + 1, sync_status = 'dirty'
                 WHERE id = ?1 AND workspace_id = ?4 AND deleted_at IS NULL",
            )
            .bind(id.to_string())
            .bind(schema_group.as_deref())
            .bind(updated_at)
            .bind(workspace_id.to_string())
            .execute(&mut *tx)
            .await?;
            if result.rows_affected() != 1 {
                return Err(AppError::NotFound(format!("connection {id}")));
            }
            let revision: i64 =
                sqlx::query_scalar("SELECT revision FROM connections WHERE id = ?1")
                    .bind(id.to_string())
                    .fetch_one(&mut *tx)
                    .await?;
            enqueue_outbox(&mut tx, workspace_id, "connection", *id, "upsert", revision)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Tombstone a connection for future synchronization. Local history and audit rows
    /// remain available to their dedicated ledgers, while scoped resource reads stop
    /// resolving the connection immediately.
    pub async fn delete_connection(&self, id: Uuid) -> AppResult<()> {
        let workspace_id = self.active_workspace_id().await?;
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(
            "UPDATE connections SET deleted_at = ?2, updated_at = ?2,
                    revision = revision + 1, sync_status = 'dirty'
             WHERE id = ?1 AND workspace_id = ?3 AND deleted_at IS NULL",
        )
            .bind(id.to_string())
            .bind(Utc::now())
            .bind(workspace_id.to_string())
            .execute(&mut *tx)
            .await?;
        if result.rows_affected() != 1 {
            return Err(AppError::NotFound(format!("connection {id}")));
        }
        let revision: i64 = sqlx::query_scalar("SELECT revision FROM connections WHERE id = ?1")
            .bind(id.to_string())
            .fetch_one(&mut *tx)
            .await?;
        enqueue_outbox(&mut tx, workspace_id, "connection", id, "delete", revision).await?;
        tx.commit().await?;
        Ok(())
    }

    // ── safety settings ────────────────────────────────────────────────────

    /// Returns stored safety settings, or the type default if none exist yet.
    pub async fn get_safety(&self, connection_id: Uuid) -> AppResult<SafetySettings> {
        self.get_connection(connection_id).await?;
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
        self.get_connection(connection_id).await?;
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
        self.get_connection(connection_id).await?;
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
        let workspace_id = self.active_workspace_id().await?;
        let row = sqlx::query(
            "SELECT h.* FROM query_history h
             JOIN connections c ON c.id = h.connection_id
             WHERE h.id = ?1 AND c.workspace_id = ?2 AND c.deleted_at IS NULL",
        )
            .bind(id.to_string())
            .bind(workspace_id.to_string())
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
        let workspace_id = self.active_workspace_id().await?;
        let visualization_json = serde_json::to_string(&draft.visualization)?;
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            r#"INSERT INTO dashboards
                (id, connection_id, title, description, sql, visualization_json,
                 workspace_id, revision, sync_status, created_at, updated_at)
               SELECT ?1,?2,?3,?4,?5,?6,?7,1,'dirty',?8,?8
               WHERE EXISTS (
                 SELECT 1 FROM connections
                 WHERE id = ?2 AND workspace_id = ?7 AND deleted_at IS NULL
               )"#,
        )
        .bind(id.to_string())
        .bind(draft.connection_id.to_string())
        .bind(&draft.title)
        .bind(&draft.description)
        .bind(&draft.sql)
        .bind(visualization_json)
        .bind(workspace_id.to_string())
        .bind(now)
        .execute(&mut *tx)
        .await?;
        enqueue_outbox(&mut tx, workspace_id, "dashboard", id, "upsert", 1).await?;
        tx.commit().await?;

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
        let workspace_id = self.active_workspace_id().await?;
        let rows = sqlx::query(
            "SELECT d.* FROM dashboards d
             JOIN connections c ON c.id = d.connection_id
             WHERE d.connection_id = ?1 AND d.workspace_id = ?2 AND d.deleted_at IS NULL
               AND c.workspace_id = ?2 AND c.deleted_at IS NULL
             ORDER BY d.updated_at DESC, d.rowid DESC",
        )
        .bind(connection_id.to_string())
        .bind(workspace_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_dashboard).collect()
    }

    pub async fn get_dashboard(&self, id: Uuid) -> AppResult<Dashboard> {
        let workspace_id = self.active_workspace_id().await?;
        let row = sqlx::query(
            "SELECT d.* FROM dashboards d
             JOIN connections c ON c.id = d.connection_id
             WHERE d.id = ?1 AND d.workspace_id = ?2 AND d.deleted_at IS NULL
               AND c.workspace_id = ?2 AND c.deleted_at IS NULL",
        )
            .bind(id.to_string())
            .bind(workspace_id.to_string())
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("dashboard {id}")))?;
        row_to_dashboard(&row)
    }

    pub async fn delete_dashboard(&self, id: Uuid) -> AppResult<()> {
        let workspace_id = self.active_workspace_id().await?;
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(
            "UPDATE dashboards SET deleted_at = ?2, updated_at = ?2,
                    revision = revision + 1, sync_status = 'dirty'
             WHERE id = ?1 AND workspace_id = ?3 AND deleted_at IS NULL",
        )
            .bind(id.to_string())
            .bind(Utc::now())
            .bind(workspace_id.to_string())
            .execute(&mut *tx)
            .await?;
        if result.rows_affected() == 0 {
            return Err(AppError::NotFound(format!("dashboard {id}")));
        }
        let revision: i64 = sqlx::query_scalar("SELECT revision FROM dashboards WHERE id = ?1")
            .bind(id.to_string())
            .fetch_one(&mut *tx)
            .await?;
        enqueue_outbox(&mut tx, workspace_id, "dashboard", id, "delete", revision).await?;
        tx.commit().await?;
        Ok(())
    }

    // ── schema cache ───────────────────────────────────────────────────────

    /// Returns the cached catalog JSON for a connection, if any.
    pub async fn get_schema_cache(&self, connection_id: Uuid) -> AppResult<Option<String>> {
        self.get_connection(connection_id).await?;
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
        self.get_connection(connection_id).await?;
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
        self.get_connection(connection_id).await?;
        sqlx::query("DELETE FROM schema_cache WHERE connection_id = ?1")
            .bind(connection_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ── agent chat threads & messages ───────────────────────────────────────

    /// Create a new chat thread (the store side of the frontend's "draft" turning
    /// real). Title starts empty — [`Store::finish_chat_turn`] sets it from the first
    /// user message once that turn completes.
    pub async fn create_chat_thread(
        &self,
        provider: AgentProvider,
        connection_id: Option<Uuid>,
        model: Option<String>,
        effort: Option<String>,
    ) -> AppResult<ChatThread> {
        let id = Uuid::new_v4();
        let now = Utc::now();
        sqlx::query(
            r#"INSERT INTO agent_chat_threads
                (id, provider, connection_id, title, cli_session_id, model, effort, created_at, updated_at)
               VALUES (?1,?2,?3,'',NULL,?4,?5,?6,?6)"#,
        )
        .bind(id.to_string())
        .bind(agent_provider_str(provider))
        .bind(connection_id.map(|id| id.to_string()))
        .bind(&model)
        .bind(&effort)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(ChatThread {
            id,
            provider,
            connection_id,
            title: String::new(),
            cli_session_id: None,
            model,
            effort,
            created_at: now,
            updated_at: now,
        })
    }

    pub async fn list_chat_threads(&self) -> AppResult<Vec<ChatThread>> {
        let rows = sqlx::query("SELECT * FROM agent_chat_threads ORDER BY updated_at DESC")
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(row_to_chat_thread).collect()
    }

    pub async fn get_chat_thread(&self, id: Uuid) -> AppResult<ChatThread> {
        let row = sqlx::query("SELECT * FROM agent_chat_threads WHERE id = ?1")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("chat thread {id}")))?;
        row_to_chat_thread(&row)
    }

    /// Deletes the thread; `agent_chat_messages` cascades via its FK.
    pub async fn delete_chat_thread(&self, id: Uuid) -> AppResult<()> {
        let result = sqlx::query("DELETE FROM agent_chat_threads WHERE id = ?1")
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(AppError::NotFound(format!("chat thread {id}")));
        }
        Ok(())
    }

    /// Update the thread's session/model/effort after a turn ends (success OR
    /// failure — a failed turn still advances the resumable session and the title),
    /// and set the title IFF it is still empty (a `CASE` in the same statement, so a
    /// second turn can never clobber the title the first one set).
    pub async fn finish_chat_turn(
        &self,
        thread_id: Uuid,
        cli_session_id: Option<String>,
        model: Option<String>,
        effort: Option<String>,
        title_if_empty: &str,
    ) -> AppResult<()> {
        sqlx::query(
            r#"UPDATE agent_chat_threads
               SET cli_session_id = ?2, model = ?3, effort = ?4, updated_at = ?5,
                   title = CASE WHEN title = '' THEN ?6 ELSE title END
               WHERE id = ?1"#,
        )
        .bind(thread_id.to_string())
        .bind(cli_session_id)
        .bind(model)
        .bind(effort)
        .bind(Utc::now())
        .bind(title_if_empty)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn insert_chat_message(
        &self,
        thread_id: Uuid,
        role: &str,
        text: &str,
        error: Option<&str>,
    ) -> AppResult<ChatMessageRecord> {
        let id = Uuid::new_v4();
        let now = Utc::now();
        sqlx::query(
            r#"INSERT INTO agent_chat_messages (id, thread_id, role, text, error, created_at)
               VALUES (?1,?2,?3,?4,?5,?6)"#,
        )
        .bind(id.to_string())
        .bind(thread_id.to_string())
        .bind(role)
        .bind(text)
        .bind(error)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(ChatMessageRecord {
            id,
            thread_id,
            role: role.to_string(),
            text: text.to_string(),
            error: error.map(str::to_string),
            created_at: now,
        })
    }

    pub async fn list_chat_messages(&self, thread_id: Uuid) -> AppResult<Vec<ChatMessageRecord>> {
        let rows = sqlx::query(
            "SELECT * FROM agent_chat_messages WHERE thread_id = ?1 ORDER BY created_at ASC",
        )
        .bind(thread_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_chat_message).collect()
    }
}

/// Add synchronizable resource columns to databases created before the workspace
/// schema existed. SQLite lacks `ADD COLUMN IF NOT EXISTS`, so duplicate errors are
/// expected and deliberately ignored after each independent statement.
async fn add_workspace_columns(pool: &SqlitePool) {
    let statements = [
        "ALTER TABLE connections ADD COLUMN workspace_id TEXT NOT NULL DEFAULT '00000000-0000-0000-0000-000000000001'",
        "ALTER TABLE connections ADD COLUMN remote_id TEXT",
        "ALTER TABLE connections ADD COLUMN revision INTEGER NOT NULL DEFAULT 1",
        "ALTER TABLE connections ADD COLUMN sync_status TEXT NOT NULL DEFAULT 'local'",
        "ALTER TABLE connections ADD COLUMN deleted_at TEXT",
        "ALTER TABLE dashboards ADD COLUMN workspace_id TEXT NOT NULL DEFAULT '00000000-0000-0000-0000-000000000001'",
        "ALTER TABLE dashboards ADD COLUMN remote_id TEXT",
        "ALTER TABLE dashboards ADD COLUMN revision INTEGER NOT NULL DEFAULT 1",
        "ALTER TABLE dashboards ADD COLUMN sync_status TEXT NOT NULL DEFAULT 'local'",
        "ALTER TABLE dashboards ADD COLUMN deleted_at TEXT",
        "ALTER TABLE snippets ADD COLUMN workspace_id TEXT NOT NULL DEFAULT '00000000-0000-0000-0000-000000000001'",
        "ALTER TABLE snippets ADD COLUMN remote_id TEXT",
        "ALTER TABLE snippets ADD COLUMN revision INTEGER NOT NULL DEFAULT 1",
        "ALTER TABLE snippets ADD COLUMN sync_status TEXT NOT NULL DEFAULT 'local'",
        "ALTER TABLE snippets ADD COLUMN deleted_at TEXT",
    ];
    for statement in statements {
        let _ = sqlx::query(statement).execute(pool).await;
    }
}

/// Backfill every legacy synchronizable resource into the Personal Workspace while
/// preserving its UUID. The migration copies no credential value and creates no
/// outbox payload, so local secret references cannot leak into synchronization data.
async fn migrate_workspace_foundation(pool: &SqlitePool) -> AppResult<()> {
    let personal = migrations::PERSONAL_WORKSPACE_ID;
    let mut tx = pool.begin().await?;
    for table in ["connections", "dashboards", "snippets"] {
        let sql = format!(
            "UPDATE {table} SET workspace_id = ?1 WHERE workspace_id IS NULL OR workspace_id = ''"
        );
        sqlx::query(AssertSqlSafe(sql))
            .bind(personal)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query(
        "UPDATE app_settings SET value = ?1
         WHERE key = 'active_workspace_id'
           AND NOT EXISTS (SELECT 1 FROM workspaces WHERE id = app_settings.value)",
    )
    .bind(personal)
    .execute(&mut *tx)
    .await?;
    sqlx::query("INSERT OR IGNORE INTO sync_state (workspace_id) VALUES (?1)")
        .bind(personal)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Queue only mutation identity and revision. A future sync serializer may populate
/// `payload_json`, but must explicitly redact `secret_ref` before doing so.
async fn enqueue_outbox(
    tx: &mut Transaction<'_, Sqlite>,
    workspace_id: Uuid,
    resource_type: &str,
    resource_id: Uuid,
    operation: &str,
    revision: i64,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO sync_outbox
         (id, workspace_id, resource_type, resource_id, operation, revision, payload_json, created_at)
         VALUES (?1,?2,?3,?4,?5,?6,NULL,?7)",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(workspace_id.to_string())
    .bind(resource_type)
    .bind(resource_id.to_string())
    .bind(operation)
    .bind(revision)
    .bind(Utc::now())
    .execute(&mut **tx)
    .await?;
    Ok(())
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

fn row_to_workspace(r: &sqlx::sqlite::SqliteRow) -> AppResult<Workspace> {
    Ok(Workspace {
        id: parse_uuid(r.try_get("id")?)?,
        name: r.try_get("name")?,
        kind: match r.try_get::<String, _>("kind")?.as_str() {
            "personal" => WorkspaceKind::Personal,
            "team" => WorkspaceKind::Team,
            other => return Err(AppError::Config(format!("unknown workspace kind '{other}'"))),
        },
        lifecycle_state: match r.try_get::<String, _>("lifecycle_state")?.as_str() {
            "active" => WorkspaceLifecycleState::Active,
            "archived" => WorkspaceLifecycleState::Archived,
            "deleted" => WorkspaceLifecycleState::Deleted,
            other => {
                return Err(AppError::Config(format!(
                    "unknown workspace lifecycle state '{other}'"
                )))
            }
        },
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

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
        env: r.try_get("env").unwrap_or(None),
        schema_group: r.try_get("schema_group").unwrap_or(None),
        workspace_access: parse_workspace_access(
            r.try_get("workspace_access").unwrap_or_else(|_| "local".to_string()),
        )?,
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

fn row_to_chat_thread(r: &sqlx::sqlite::SqliteRow) -> AppResult<ChatThread> {
    Ok(ChatThread {
        id: parse_uuid(r.try_get("id")?)?,
        provider: parse_agent_provider(r.try_get("provider")?)?,
        connection_id: parse_uuid_opt(r.try_get("connection_id")?)?,
        title: r.try_get("title")?,
        cli_session_id: r.try_get("cli_session_id")?,
        model: r.try_get("model")?,
        effort: r.try_get("effort")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

fn row_to_chat_message(r: &sqlx::sqlite::SqliteRow) -> AppResult<ChatMessageRecord> {
    Ok(ChatMessageRecord {
        id: parse_uuid(r.try_get("id")?)?,
        thread_id: parse_uuid(r.try_get("thread_id")?)?,
        role: r.try_get("role")?,
        text: r.try_get("text")?,
        error: r.try_get("error")?,
        created_at: r.try_get("created_at")?,
    })
}

// ── enum ⇄ text (kept in sync with model.rs serde `camelCase`) ──────────────

pub(crate) fn engine_str(e: Engine) -> &'static str {
    match e {
        Engine::Postgres => "postgres",
        Engine::Mysql => "mysql",
        Engine::Sqlite => "sqlite",
        Engine::Mongodb => "mongodb",
    }
}

pub(crate) fn parse_engine(s: String) -> AppResult<Engine> {
    match s.as_str() {
        "postgres" => Ok(Engine::Postgres),
        "mysql" => Ok(Engine::Mysql),
        "sqlite" => Ok(Engine::Sqlite),
        "mongodb" => Ok(Engine::Mongodb),
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

pub(crate) fn workspace_access_str(access: WorkspaceConnectionAccess) -> &'static str {
    match access {
        WorkspaceConnectionAccess::View => "view",
        WorkspaceConnectionAccess::Read => "read",
        WorkspaceConnectionAccess::Write => "write",
        WorkspaceConnectionAccess::Manage => "manage",
        WorkspaceConnectionAccess::Local => "local",
    }
}

pub(crate) fn parse_workspace_access(s: String) -> AppResult<WorkspaceConnectionAccess> {
    match s.as_str() {
        "view" => Ok(WorkspaceConnectionAccess::View),
        "read" => Ok(WorkspaceConnectionAccess::Read),
        "write" => Ok(WorkspaceConnectionAccess::Write),
        "manage" => Ok(WorkspaceConnectionAccess::Manage),
        "local" => Ok(WorkspaceConnectionAccess::Local),
        other => Err(AppError::Config(format!("unknown workspace connection access '{other}'"))),
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

pub(crate) fn parse_uuid_opt(s: Option<String>) -> AppResult<Option<Uuid>> {
    s.map(parse_uuid).transpose()
}

pub(crate) fn agent_provider_str(p: AgentProvider) -> &'static str {
    match p {
        AgentProvider::Claude => "claude",
        AgentProvider::Codex => "codex",
    }
}

pub(crate) fn parse_agent_provider(s: String) -> AppResult<AgentProvider> {
    match s.as_str() {
        "claude" => Ok(AgentProvider::Claude),
        "codex" => Ok(AgentProvider::Codex),
        other => Err(AppError::Config(format!("unknown agent provider '{other}'"))),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        add_workspace_columns, engine_str, migrate_audit_no_cascade,
        migrate_workspace_foundation, migrations, parse_engine, Store,
    };
    use crate::agent::AgentProvider;
    use crate::error::AppError;
    use crate::model::{
        ConnectionProfile, DashboardDraft, DashboardKind, DashboardVisualization, Engine,
        HistoryEntry, Provider, QueryKind,
    };
    use chrono::Utc;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use sqlx::SqlitePool;
    use std::collections::HashMap;
    use std::str::FromStr;
    use uuid::Uuid;

    async fn memory_pool() -> SqlitePool {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .foreign_keys(true);
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap()
    }

    fn sqlite_profile(id: Uuid, name: &str) -> ConnectionProfile {
        ConnectionProfile {
            id,
            name: name.into(),
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
            env: None,
            schema_group: None,
            workspace_access: crate::model::WorkspaceConnectionAccess::Local,
        }
    }

    #[tokio::test]
    async fn legacy_resources_migrate_without_uuid_or_secret_changes() {
        let pool = memory_pool().await;
        sqlx::raw_sql(
            r#"
            CREATE TABLE connections (
                id TEXT PRIMARY KEY, name TEXT NOT NULL, engine TEXT NOT NULL,
                provider TEXT NOT NULL DEFAULT 'auto', driver_id TEXT, host TEXT NOT NULL,
                port INTEGER NOT NULL, db_name TEXT NOT NULL, username TEXT NOT NULL,
                sslmode TEXT NOT NULL, extra_params TEXT NOT NULL DEFAULT '{}', secret_ref TEXT,
                readonly_default INTEGER NOT NULL DEFAULT 1, allow_writes INTEGER NOT NULL DEFAULT 0,
                env TEXT, schema_group TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL
            );
            CREATE TABLE snippets (
                id TEXT PRIMARY KEY, connection_id TEXT, title TEXT NOT NULL, sql TEXT NOT NULL,
                tags TEXT NOT NULL DEFAULT '[]', updated_at TEXT NOT NULL
            );
            CREATE TABLE dashboards (
                id TEXT PRIMARY KEY, connection_id TEXT NOT NULL, title TEXT NOT NULL,
                description TEXT NOT NULL DEFAULT '', sql TEXT NOT NULL,
                visualization_json TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL
            );
            INSERT INTO connections
                (id,name,engine,host,port,db_name,username,sslmode,secret_ref,created_at,updated_at)
            VALUES ('10000000-0000-0000-0000-000000000001','legacy','sqlite','',0,':memory:','',
                    'disable','keychain-only','2026-01-01','2026-01-01');
            INSERT INTO snippets (id,connection_id,title,sql,updated_at)
            VALUES ('10000000-0000-0000-0000-000000000002',NULL,'s','SELECT 1','2026-01-01');
            INSERT INTO dashboards
                (id,connection_id,title,sql,visualization_json,created_at,updated_at)
            VALUES ('10000000-0000-0000-0000-000000000003',
                    '10000000-0000-0000-0000-000000000001','d','SELECT 1',
                    '{"version":1,"kind":"table","xColumn":null,"yColumns":[]}',
                    '2026-01-01','2026-01-01');
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::raw_sql(migrations::SCHEMA).execute(&pool).await.unwrap();
        add_workspace_columns(&pool).await;
        migrate_workspace_foundation(&pool).await.unwrap();

        for table in ["connections", "dashboards", "snippets"] {
            let workspace_id: String = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT workspace_id FROM {table} LIMIT 1"
            )))
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(workspace_id, migrations::PERSONAL_WORKSPACE_ID);
        }
        let secret_ref: String = sqlx::query_scalar("SELECT secret_ref FROM connections")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(secret_ref, "keychain-only");
        let outbox_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM sync_outbox")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(outbox_rows, 0, "migration must not serialize legacy resources");
    }

    #[tokio::test]
    async fn active_workspace_scopes_connections_and_tombstones_mutations() {
        let pool = memory_pool().await;
        sqlx::raw_sql(migrations::SCHEMA).execute(&pool).await.unwrap();
        let store = Store::from_pool_for_test(pool);
        let personal_id = Uuid::parse_str(migrations::PERSONAL_WORKSPACE_ID).unwrap();
        let personal_connection = sqlite_profile(Uuid::new_v4(), "personal");
        store.upsert_connection(&personal_connection).await.unwrap();
        let personal_dashboard = store
            .save_dashboard(&DashboardDraft {
                connection_id: personal_connection.id,
                title: "personal dashboard".into(),
                description: String::new(),
                sql: "SELECT 1".into(),
                visualization: DashboardVisualization {
                    version: 1,
                    kind: DashboardKind::Table,
                    x_column: None,
                    y_columns: Vec::new(),
                },
            })
            .await
            .unwrap();

        let team_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO workspaces (id,name,kind,lifecycle_state,created_at,updated_at)
             VALUES (?1,'Team','team','active',?2,?2)",
        )
        .bind(team_id.to_string())
        .bind(Utc::now())
        .execute(store.pool())
        .await
        .unwrap();
        store.set_active_workspace(team_id).await.unwrap();
        assert!(store.list_connections().await.unwrap().is_empty());
        assert!(matches!(
            store.get_connection(personal_connection.id).await,
            Err(AppError::NotFound(_))
        ));
        assert!(matches!(
            store.get_dashboard(personal_dashboard.id).await,
            Err(AppError::NotFound(_))
        ));

        let team_connection = sqlite_profile(Uuid::new_v4(), "team");
        store.upsert_connection(&team_connection).await.unwrap();
        assert_eq!(store.list_connections().await.unwrap()[0].id, team_connection.id);
        store.delete_connection(team_connection.id).await.unwrap();
        assert!(store.list_connections().await.unwrap().is_empty());
        let tombstone: Option<String> =
            sqlx::query_scalar("SELECT deleted_at FROM connections WHERE id = ?1")
                .bind(team_connection.id.to_string())
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert!(tombstone.is_some());
        let delete_payload: Option<String> = sqlx::query_scalar(
            "SELECT payload_json FROM sync_outbox
             WHERE resource_id = ?1 AND operation = 'delete' ORDER BY created_at DESC LIMIT 1",
        )
        .bind(team_connection.id.to_string())
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert!(delete_payload.is_none());

        store.set_active_workspace(personal_id).await.unwrap();
        assert_eq!(store.list_connections().await.unwrap()[0].id, personal_connection.id);
    }

    #[tokio::test]
    async fn team_membership_sync_upserts_archives_and_restores_personal_scope() {
        let pool = memory_pool().await;
        sqlx::raw_sql(migrations::SCHEMA).execute(&pool).await.unwrap();
        let store = Store::from_pool_for_test(pool);
        let alpha = Uuid::new_v4();
        let beta = Uuid::new_v4();

        store
            .sync_team_workspaces(&[(alpha, "Alpha".into()), (beta, "Beta".into())])
            .await
            .unwrap();
        let listed = store.list_workspaces().await.unwrap();
        assert_eq!(listed.len(), 3);
        assert!(listed.iter().any(|workspace| workspace.id == alpha));
        assert!(listed.iter().any(|workspace| workspace.id == beta));

        store.set_active_workspace(alpha).await.unwrap();
        store
            .sync_team_workspaces(&[(beta, "Beta renamed".into())])
            .await
            .unwrap();
        let listed = store.list_workspaces().await.unwrap();
        assert_eq!(listed.len(), 2);
        assert!(!listed.iter().any(|workspace| workspace.id == alpha));
        assert_eq!(
            listed
                .iter()
                .find(|workspace| workspace.id == beta)
                .unwrap()
                .name,
            "Beta renamed"
        );
        assert_eq!(
            store.active_workspace().await.unwrap().id.to_string(),
            migrations::PERSONAL_WORKSPACE_ID
        );
        let alpha_state: String =
            sqlx::query_scalar("SELECT lifecycle_state FROM workspaces WHERE id = ?1")
                .bind(alpha.to_string())
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert_eq!(alpha_state, "archived");
    }

    #[tokio::test]
    async fn remote_template_sync_preserves_member_local_credential_binding() {
        let pool = memory_pool().await;
        sqlx::raw_sql(migrations::SCHEMA).execute(&pool).await.unwrap();
        let store = Store::from_pool_for_test(pool);
        let workspace_id = Uuid::new_v4();
        let now = Utc::now();
        sqlx::query(
            "INSERT INTO workspaces (id, name, kind, lifecycle_state, created_at, updated_at)
             VALUES (?1, 'Team', 'team', 'active', ?2, ?2)",
        )
        .bind(workspace_id.to_string())
        .bind(now)
        .execute(store.pool())
        .await
        .unwrap();

        let id = Uuid::new_v4();
        let mut local_binding = sqlite_profile(id, "shared");
        local_binding.username = "member-account".into();
        local_binding.extra_params.insert("member-local-option".into(), "on".into());
        local_binding.secret_ref = Some(id.to_string());
        local_binding.workspace_access = crate::model::WorkspaceConnectionAccess::Write;
        store
            .sync_remote_connections(workspace_id, &[(local_binding, 1)])
            .await
            .unwrap();

        let mut remote_update = sqlite_profile(id, "renamed");
        remote_update.username.clear();
        remote_update.extra_params.clear();
        remote_update.secret_ref = None;
        remote_update.allow_writes = false;
        remote_update.workspace_access = crate::model::WorkspaceConnectionAccess::Read;
        store
            .sync_remote_connections(workspace_id, &[(remote_update, 2)])
            .await
            .unwrap();
        store.set_active_workspace(workspace_id).await.unwrap();

        let loaded = store.get_connection(id).await.unwrap();
        assert_eq!(loaded.name, "renamed");
        assert_eq!(loaded.username, "member-account");
        assert_eq!(loaded.extra_params.get("member-local-option").map(String::as_str), Some("on"));
        let expected_secret_ref = id.to_string();
        assert_eq!(loaded.secret_ref.as_deref(), Some(expected_secret_ref.as_str()));
        assert_eq!(loaded.workspace_access, crate::model::WorkspaceConnectionAccess::Read);
        assert!(!loaded.allow_writes);
    }

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
    async fn connections_with_legacy_project_dir_column_still_round_trip() {
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
        sqlx::query("ALTER TABLE connections ADD COLUMN project_dir TEXT")
            .execute(&pool)
            .await
            .unwrap();

        let store = Store::from_pool_for_test(pool);
        let profile = ConnectionProfile {
            id: Uuid::new_v4(),
            name: "legacy".into(),
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
            env: Some("dev".into()),
            schema_group: Some("core".into()),
            workspace_access: crate::model::WorkspaceConnectionAccess::Local,
        };
        store.upsert_connection(&profile).await.unwrap();
        sqlx::query("UPDATE connections SET project_dir = '/old/project' WHERE id = ?1")
            .bind(profile.id.to_string())
            .execute(store.pool())
            .await
            .unwrap();

        let loaded = store.get_connection(profile.id).await.unwrap();
        assert_eq!(loaded.name, "legacy");
        assert_eq!(loaded.schema_group.as_deref(), Some("core"));
        store.upsert_connection(&loaded).await.unwrap();
    }

    #[tokio::test]
    async fn schema_group_batch_rolls_back_when_any_connection_is_missing() {
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
                name: "dev".into(),
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
                env: Some("dev".into()),
                schema_group: None,
                workspace_access: crate::model::WorkspaceConnectionAccess::Local,
            })
            .await
            .unwrap();

        let missing_id = Uuid::new_v4();
        let error = store
            .set_connections_schema_group(&[connection_id, missing_id], Some("core".into()))
            .await
            .unwrap_err();
        assert!(matches!(error, AppError::NotFound(_)));
        assert_eq!(
            store
                .get_connection(connection_id)
                .await
                .unwrap()
                .schema_group,
            None
        );
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
                env: None,
                schema_group: None,
                workspace_access: crate::model::WorkspaceConnectionAccess::Local,
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

    #[test]
    fn mongodb_engine_text_round_trips() {
        assert_eq!(engine_str(Engine::Mongodb), "mongodb");
        assert_eq!(parse_engine("mongodb".into()).unwrap(), Engine::Mongodb);
    }

    #[tokio::test]
    async fn chat_thread_and_messages_round_trip_delete_cascades() {
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

        let thread = store
            .create_chat_thread(AgentProvider::Codex, None, None, None)
            .await
            .unwrap();
        assert_eq!(thread.title, "");
        assert!(thread.cli_session_id.is_none());

        store
            .insert_chat_message(thread.id, "user", "hello there", None)
            .await
            .unwrap();
        store
            .insert_chat_message(thread.id, "assistant", "hi!", None)
            .await
            .unwrap();

        store
            .finish_chat_turn(
                thread.id,
                Some("thr-123".into()),
                Some("gpt-5.6-sol".into()),
                Some("high".into()),
                "hello there",
            )
            .await
            .unwrap();

        let reloaded = store.get_chat_thread(thread.id).await.unwrap();
        assert_eq!(reloaded.title, "hello there");
        assert_eq!(reloaded.cli_session_id.as_deref(), Some("thr-123"));
        assert_eq!(reloaded.model.as_deref(), Some("gpt-5.6-sol"));
        assert_eq!(reloaded.effort.as_deref(), Some("high"));

        // A second turn must NOT clobber the title the first one set.
        store
            .finish_chat_turn(thread.id, Some("thr-124".into()), None, None, "ignored title")
            .await
            .unwrap();
        assert_eq!(store.get_chat_thread(thread.id).await.unwrap().title, "hello there");

        let messages = store.list_chat_messages(thread.id).await.unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[1].role, "assistant");

        let listed = store.list_chat_threads().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, thread.id);

        store.delete_chat_thread(thread.id).await.unwrap();
        assert!(store.list_chat_messages(thread.id).await.unwrap().is_empty());
        assert!(matches!(
            store.get_chat_thread(thread.id).await,
            Err(AppError::NotFound(_))
        ));
        assert!(matches!(
            store.delete_chat_thread(thread.id).await,
            Err(AppError::NotFound(_))
        ));
    }
}
