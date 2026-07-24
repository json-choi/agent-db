//! Transport-neutral saved-dashboard workflows shared by Tauri and MCP.

use std::fmt;

use chrono::Utc;
use uuid::Uuid;

use crate::audit::{self, RecordArgs};
use crate::connection::{
    ConnectionAccess, ConnectionLease, ConnectionManager, ConnectionOperationScope, DbPool,
};
use crate::error::{AppError, AppResult};
use crate::executor;
use crate::model::{
    Dashboard, DashboardDraft, DashboardKind, DashboardVisualization, HistoryEntry, QueryKind,
    QueryResult,
};
use crate::safety::{self, PoolRef};
use crate::store::{PinnedConnection, Store};

fn is_eligible_agent_run(source: &HistoryEntry) -> bool {
    source.origin == "agent" && source.status == "ok" && matches!(source.kind, QueryKind::Read)
}

/// Agent-selected presentation metadata. Connection and SQL provenance are never
/// accepted here; they are loaded from the exact stored query run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentDashboardPresentation {
    pub(crate) title: String,
    pub(crate) description: String,
    pub(crate) kind: DashboardKind,
    pub(crate) x_column: Option<String>,
    pub(crate) y_columns: Vec<String>,
}

/// Explicitly allowlisted context used to preserve the existing MCP event payload.
/// It intentionally omits the full connection profile and every credential field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentDashboardEventContext {
    pub(crate) query_run_id: Uuid,
    pub(crate) connection_id: Uuid,
    pub(crate) connection_name: String,
    pub(crate) title: String,
    pub(crate) sql: String,
}

/// Domain failures that occur before MCP emits `agent:tool_call`.
#[derive(Debug)]
pub(crate) enum AgentDashboardPrepareError {
    QueryRunNotFound,
    QueryRunIneligible,
    Application(AppError),
}

/// Domain failures that occur after MCP has emitted `agent:tool_call`.
#[derive(Debug)]
pub(crate) enum AgentDashboardCommitError {
    InvalidDraft(AppError),
    Persistence(AppError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DashboardRunRequest {
    pub(crate) dashboard_id: Uuid,
    pub(crate) query_id: Option<Uuid>,
}

/// Successful saved-dashboard execution retaining the connection authority until
/// the adapter has serialized the established [`QueryResult`] wire.
pub(crate) struct DashboardRunReceipt {
    result: QueryResult,
    _lease: ConnectionLease,
}

impl serde::Serialize for DashboardRunReceipt {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serde::Serialize::serialize(&self.result, serializer)
    }
}

#[derive(Debug)]
pub(crate) enum DashboardRunError {
    Application(AppError),
    Scoped(DashboardRunScopedFailure),
    Execution(Box<DashboardRunExecutionFailure>),
}

impl DashboardRunError {
    pub(crate) fn into_error(self) -> AppError {
        match self {
            Self::Application(error) => error,
            Self::Scoped(failure) => failure.into_error(),
            Self::Execution(failure) => failure.into_error(),
        }
    }
}

pub(crate) struct DashboardRunScopedFailure {
    error: AppError,
    _scope: ConnectionOperationScope,
}

impl fmt::Debug for DashboardRunScopedFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DashboardRunScopedFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl DashboardRunScopedFailure {
    fn into_error(self) -> AppError {
        self.error
    }
}

pub(crate) struct DashboardRunExecutionFailure {
    error: AppError,
    _lease: ConnectionLease,
}

impl fmt::Debug for DashboardRunExecutionFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DashboardRunExecutionFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl DashboardRunExecutionFailure {
    fn into_error(self) -> AppError {
        self.error
    }
}

/// Opaque creation capability. Retaining the operation scope across the adapter's
/// start event prevents a workspace/account switch before validation and persistence.
pub(crate) struct PreparedAgentDashboard {
    store: Store,
    _operation_scope: ConnectionOperationScope,
    connection: PinnedConnection,
    draft: DashboardDraft,
    event_context: AgentDashboardEventContext,
}

impl PreparedAgentDashboard {
    /// Return only the fields required by the compatibility event.
    pub(crate) fn event_context(&self) -> &AgentDashboardEventContext {
        &self.event_context
    }

    /// Validate and persist the capability-bound draft after the adapter announces
    /// the tool call. Error variants let each transport retain its existing mapping.
    pub(crate) async fn commit(self) -> Result<Dashboard, AgentDashboardCommitError> {
        crate::dashboard::validate_draft(&self.draft, self.connection.profile.engine)
            .map_err(AgentDashboardCommitError::InvalidDraft)?;
        self.store
            .save_dashboard_if_current(&self.connection, &self.draft)
            .await
            .map_err(AgentDashboardCommitError::Persistence)
    }
}

/// Scope-aware dashboard metadata service. It contains no Tauri or MCP types and
/// emits no events; adapters remain responsible for their public wire contracts.
#[derive(Clone)]
pub(crate) struct DashboardService {
    store: Store,
    connections: ConnectionManager,
}

impl DashboardService {
    pub(super) fn new(store: Store, connections: ConnectionManager) -> Self {
        Self { store, connections }
    }

    /// List dashboards under one stable, view-compatible connection identity.
    pub(crate) async fn list(&self, connection_id: Uuid) -> AppResult<Vec<Dashboard>> {
        let operation_scope = self.connections.begin_operation_scope().await;
        let connection = operation_scope
            .pin_dashboard_connection(connection_id)
            .await?;
        self.store.list_dashboards_if_current(&connection).await
    }

    /// Validate and save a dashboard under the same authority snapshot.
    pub(crate) async fn save(&self, draft: DashboardDraft) -> AppResult<Dashboard> {
        let operation_scope = self.connections.begin_operation_scope().await;
        let connection = operation_scope
            .pin_dashboard_connection(draft.connection_id)
            .await?;
        crate::dashboard::validate_draft(&draft, connection.profile.engine)?;
        self.store
            .save_dashboard_if_current(&connection, &draft)
            .await
    }

    /// Tombstone a dashboard only while its dashboard and connection pin stay current.
    pub(crate) async fn delete(&self, id: Uuid) -> AppResult<()> {
        let operation_scope = self.connections.begin_operation_scope().await;
        let dashboard = operation_scope.pin_dashboard(id).await?;
        self.store.delete_dashboard_if_current(&dashboard).await
    }

    /// Re-run one saved dashboard through the authoritative read-only executor.
    /// Stored SQL is revalidated against the current connection engine on every run.
    pub(crate) async fn run(
        &self,
        request: DashboardRunRequest,
    ) -> Result<DashboardRunReceipt, DashboardRunError> {
        let operation_scope = self.connections.begin_operation_scope().await;
        let dashboard = match self.store.get_dashboard(request.dashboard_id).await {
            Ok(dashboard) => dashboard,
            Err(error) => {
                return Err(DashboardRunError::Scoped(DashboardRunScopedFailure {
                    error,
                    _scope: operation_scope,
                }))
            }
        };
        let operation_pin = match operation_scope
            .pin_connection(dashboard.connection_id)
            .await
        {
            Ok(pin) => pin,
            Err(error) => {
                return Err(DashboardRunError::Scoped(DashboardRunScopedFailure {
                    error,
                    _scope: operation_scope,
                }))
            }
        };
        let draft = DashboardDraft {
            connection_id: dashboard.connection_id,
            title: dashboard.title.clone(),
            description: dashboard.description.clone(),
            sql: dashboard.sql.clone(),
            visualization: dashboard.visualization.clone(),
        };
        if let Err(error) = crate::dashboard::validate_draft(&draft, operation_pin.profile.engine) {
            let kind = safety::classify(&dashboard.sql, operation_pin.profile.engine)
                .map(|classification| classification.kind)
                .unwrap_or(QueryKind::Write);
            record_dashboard_run(
                &self.store,
                &operation_pin,
                DashboardRunRecord {
                    sql: &dashboard.sql,
                    kind,
                    status: "blocked",
                    row_count: None,
                    duration_ms: None,
                    error: Some(error.to_string()),
                },
            )
            .await;
            return Err(DashboardRunError::Scoped(DashboardRunScopedFailure {
                error,
                _scope: operation_scope,
            }));
        }

        let settings = match self.store.get_safety(dashboard.connection_id).await {
            Ok(settings) => settings,
            Err(error) => {
                return Err(DashboardRunError::Scoped(DashboardRunScopedFailure {
                    error,
                    _scope: operation_scope,
                }))
            }
        };
        let lease = match operation_scope
            .connect(operation_pin.clone(), ConnectionAccess::Read)
            .await
        {
            Ok(lease) => lease,
            Err(error) => {
                record_dashboard_run(
                    &self.store,
                    &operation_pin,
                    DashboardRunRecord {
                        sql: &dashboard.sql,
                        kind: QueryKind::Read,
                        status: "error",
                        row_count: None,
                        duration_ms: None,
                        error: Some(error.to_string()),
                    },
                )
                .await;
                return Err(DashboardRunError::Application(error));
            }
        };
        let live = match lease.live().sql() {
            Ok(live) => live,
            Err(error) => {
                return Err(DashboardRunError::Execution(Box::new(
                    DashboardRunExecutionFailure {
                        error,
                        _lease: lease,
                    },
                )));
            }
        };
        let max_rows = settings.max_rows.clamp(1, 100_000);
        let run = safety::run_read_only(pool_ref(live.ro()), &dashboard.sql, max_rows);
        match executor::cancel::guard(request.query_id, executor::cancel::QUERY_TIMEOUT, run).await
        {
            Ok(result) => {
                record_dashboard_run(
                    &self.store,
                    &operation_pin,
                    DashboardRunRecord {
                        sql: &dashboard.sql,
                        kind: QueryKind::Read,
                        status: "ok",
                        row_count: Some(result.row_count as i64),
                        duration_ms: Some(result.duration_ms as i64),
                        error: None,
                    },
                )
                .await;
                Ok(DashboardRunReceipt {
                    result,
                    _lease: lease,
                })
            }
            Err(error) => {
                record_dashboard_run(
                    &self.store,
                    &operation_pin,
                    DashboardRunRecord {
                        sql: &dashboard.sql,
                        kind: QueryKind::Read,
                        status: "error",
                        row_count: None,
                        duration_ms: None,
                        error: Some(error.to_string()),
                    },
                )
                .await;
                Err(DashboardRunError::Execution(Box::new(
                    DashboardRunExecutionFailure {
                        error,
                        _lease: lease,
                    },
                )))
            }
        }
    }

    /// Resolve an eligible agent query run into an opaque, scope-bound capability.
    /// Validation intentionally remains in `commit` so MCP can preserve its historic
    /// `tool_call -> result` event sequence for invalid presentation metadata.
    pub(crate) async fn prepare_agent_create(
        &self,
        query_run_id: Uuid,
        presentation: AgentDashboardPresentation,
    ) -> Result<PreparedAgentDashboard, AgentDashboardPrepareError> {
        let operation_scope = self.connections.begin_operation_scope().await;
        let resolved = match self
            .store
            .resolve_history_for_dashboard_prepare(query_run_id)
            .await
        {
            Ok(resolved) => resolved,
            Err(AppError::NotFound(_)) => return Err(AgentDashboardPrepareError::QueryRunNotFound),
            Err(error) => return Err(AgentDashboardPrepareError::Application(error)),
        };
        if !is_eligible_agent_run(&resolved.history) {
            return Err(AgentDashboardPrepareError::QueryRunIneligible);
        }
        let connection = operation_scope
            .pin_dashboard_connection(resolved.history.connection_id)
            .await
            .map_err(AgentDashboardPrepareError::Application)?;
        let source = match self
            .store
            .get_history_if_current(&connection, &resolved)
            .await
        {
            Ok(source) => source,
            Err(AppError::NotFound(_)) => return Err(AgentDashboardPrepareError::QueryRunNotFound),
            Err(error) => return Err(AgentDashboardPrepareError::Application(error)),
        };
        if !is_eligible_agent_run(&source) {
            return Err(AgentDashboardPrepareError::QueryRunIneligible);
        }

        let event_context = AgentDashboardEventContext {
            query_run_id,
            connection_id: connection.connection_id,
            connection_name: connection.profile.name.clone(),
            title: presentation.title.clone(),
            sql: source.sql.clone(),
        };
        let draft = DashboardDraft {
            connection_id: source.connection_id,
            title: presentation.title,
            description: presentation.description,
            sql: source.sql,
            visualization: DashboardVisualization {
                version: crate::dashboard::VISUALIZATION_VERSION,
                kind: presentation.kind,
                x_column: presentation.x_column,
                y_columns: presentation.y_columns,
            },
        };

        Ok(PreparedAgentDashboard {
            store: self.store.clone(),
            _operation_scope: operation_scope,
            connection,
            draft,
            event_context,
        })
    }
}

struct DashboardRunRecord<'a> {
    sql: &'a str,
    kind: QueryKind,
    status: &'a str,
    row_count: Option<i64>,
    duration_ms: Option<i64>,
    error: Option<String>,
}

async fn record_dashboard_run(
    store: &Store,
    pin: &PinnedConnection,
    record: DashboardRunRecord<'_>,
) {
    if let Err(error) = audit::record(
        store,
        RecordArgs {
            connection_id: pin.connection_id,
            engine: pin.profile.engine,
            agent_prompt: None,
            sql: record.sql.to_string(),
            kind: record.kind,
            action: "dashboard:run".into(),
            approved_by: None,
            affected_estimate: record.row_count,
            error: record.error.clone(),
        },
    )
    .await
    {
        tracing::error!(
            connection_id = %pin.connection_id,
            %error,
            "dashboard run audit record failed"
        );
    }
    if let Err(error) = store
        .insert_history_if_current(
            pin,
            &HistoryEntry {
                id: Uuid::new_v4(),
                connection_id: pin.connection_id,
                sql: record.sql.to_string(),
                kind: record.kind,
                status: record.status.to_string(),
                row_count: record.row_count,
                duration_ms: record.duration_ms,
                error: record.error,
                executed_at: Utc::now(),
                origin: "dashboard".into(),
            },
        )
        .await
    {
        tracing::error!(
            connection_id = %pin.connection_id,
            %error,
            "dashboard run history insert failed"
        );
    }
}

fn pool_ref(db: &DbPool) -> PoolRef<'_> {
    match db {
        DbPool::Postgres(pool) => PoolRef::Postgres(pool),
        DbPool::Mysql(pool) => PoolRef::Mysql(pool),
        DbPool::Sqlite(pool) => PoolRef::Sqlite(pool),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;
    use std::str::FromStr;
    use std::time::Duration;

    use chrono::Utc;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use tempfile::TempDir;

    use super::*;
    use crate::model::{
        ConnectionProfile, Engine, HistoryEntry, Provider, WorkspaceConnectionAccess,
        WorkspaceCredentialMode,
    };
    use crate::store::TEST_SCHEMA;

    async fn harness() -> (DashboardService, Store, Uuid) {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(TEST_SCHEMA).execute(&pool).await.unwrap();
        let store = Store::from_pool_for_test(pool);
        let connection_id = Uuid::new_v4();
        store
            .upsert_connection(&ConnectionProfile {
                id: connection_id,
                name: "dashboard-test".into(),
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
                env: Some("test".into()),
                schema_group: None,
                workspace_access: WorkspaceConnectionAccess::Local,
                credential_mode: WorkspaceCredentialMode::Local,
            })
            .await
            .unwrap();
        let connections = ConnectionManager::new(store.clone());
        (
            DashboardService::new(store.clone(), connections),
            store,
            connection_id,
        )
    }

    struct DashboardRunHarness {
        _temp_dir: TempDir,
        store: Store,
        connections: ConnectionManager,
        service: DashboardService,
        connection_id: Uuid,
        dashboard_id: Uuid,
        target_path: std::path::PathBuf,
    }

    impl DashboardRunHarness {
        async fn new(sql: &str) -> Self {
            let app_options = SqliteConnectOptions::from_str("sqlite::memory:")
                .unwrap()
                .foreign_keys(true);
            let app_pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(app_options)
                .await
                .unwrap();
            sqlx::raw_sql(TEST_SCHEMA).execute(&app_pool).await.unwrap();
            let store = Store::from_pool_for_test(app_pool);

            let temp_dir = TempDir::new().unwrap();
            let target_path = temp_dir.path().join("dashboard-target.sqlite");
            initialize_dashboard_target(&target_path).await;
            let connection_id = Uuid::new_v4();
            store
                .upsert_connection(&ConnectionProfile {
                    id: connection_id,
                    name: "dashboard-run-test".into(),
                    engine: Engine::Sqlite,
                    provider: Provider::Generic,
                    driver_id: Some("sqlx-sqlite".into()),
                    host: String::new(),
                    port: 0,
                    database: target_path.display().to_string(),
                    username: String::new(),
                    sslmode: "disable".into(),
                    extra_params: HashMap::new(),
                    readonly_default: true,
                    allow_writes: false,
                    secret_ref: None,
                    env: Some("test".into()),
                    schema_group: None,
                    workspace_access: WorkspaceConnectionAccess::Local,
                    credential_mode: WorkspaceCredentialMode::Local,
                })
                .await
                .unwrap();
            let connections = ConnectionManager::new(store.clone());
            let service = DashboardService::new(store.clone(), connections.clone());
            let dashboard = service
                .save(DashboardDraft {
                    connection_id,
                    title: "People".into(),
                    description: "Dashboard execution contract".into(),
                    sql: sql.into(),
                    visualization: DashboardVisualization {
                        version: crate::dashboard::VISUALIZATION_VERSION,
                        kind: DashboardKind::Table,
                        x_column: None,
                        y_columns: Vec::new(),
                    },
                })
                .await
                .unwrap();
            Self {
                _temp_dir: temp_dir,
                store,
                connections,
                service,
                connection_id,
                dashboard_id: dashboard.id,
                target_path,
            }
        }

        async fn overwrite_dashboard_sql(&self, sql: &str) {
            sqlx::query("UPDATE dashboards SET sql = ?1 WHERE id = ?2")
                .bind(sql)
                .bind(self.dashboard_id.to_string())
                .execute(self.store.pool())
                .await
                .unwrap();
        }

        async fn user_name(&self, id: i64) -> String {
            let options = SqliteConnectOptions::new()
                .filename(&self.target_path)
                .read_only(true);
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(options)
                .await
                .unwrap();
            let name = sqlx::query_scalar::<_, String>("SELECT name FROM users WHERE id = ?1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
            pool.close().await;
            name
        }

        async fn close(self) {
            let mutation = self
                .connections
                .begin_connection_mutation(self.connection_id, ConnectionAccess::Read)
                .await
                .unwrap();
            mutation.retire_connection(self.connection_id).await;
            let Self {
                _temp_dir,
                store,
                connections,
                service,
                ..
            } = self;
            drop(service);
            drop(connections);
            store.pool().close().await;
            drop(store);
            _temp_dir
                .close()
                .expect("temporary dashboard directory must be removable after pool shutdown");
        }
    }

    async fn initialize_dashboard_target(path: &Path) {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
             INSERT INTO users (id, name) VALUES (1, 'Ada'), (2, 'Linus');",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
    }

    fn presentation(title: &str) -> AgentDashboardPresentation {
        AgentDashboardPresentation {
            title: title.into(),
            description: "Saved from one agent run".into(),
            kind: DashboardKind::Table,
            x_column: None,
            y_columns: Vec::new(),
        }
    }

    async fn insert_history(
        store: &Store,
        connection_id: Uuid,
        origin: &str,
        status: &str,
        kind: QueryKind,
    ) -> Uuid {
        let id = Uuid::new_v4();
        let pin = store.pin_connection_for_read(connection_id).await.unwrap();
        store
            .insert_history_if_current(
                &pin,
                &HistoryEntry {
                    id,
                    connection_id,
                    sql: "SELECT id, name FROM users".into(),
                    kind,
                    status: status.into(),
                    row_count: Some(1),
                    duration_ms: Some(1),
                    error: None,
                    executed_at: Utc::now(),
                    origin: origin.into(),
                },
            )
            .await
            .unwrap();
        id
    }

    #[tokio::test]
    async fn prepare_distinguishes_missing_and_ineligible_query_runs() {
        let (service, store, connection_id) = harness().await;
        let missing = service
            .prepare_agent_create(Uuid::new_v4(), presentation("Missing"))
            .await;
        assert!(matches!(
            missing,
            Err(AgentDashboardPrepareError::QueryRunNotFound)
        ));

        let manual_run =
            insert_history(&store, connection_id, "manual", "ok", QueryKind::Read).await;
        let ineligible = service
            .prepare_agent_create(manual_run, presentation("Manual"))
            .await;
        assert!(matches!(
            ineligible,
            Err(AgentDashboardPrepareError::QueryRunIneligible)
        ));

        let failed_run =
            insert_history(&store, connection_id, "agent", "error", QueryKind::Read).await;
        assert!(matches!(
            service
                .prepare_agent_create(failed_run, presentation("Failed"))
                .await,
            Err(AgentDashboardPrepareError::QueryRunIneligible)
        ));

        let write_run =
            insert_history(&store, connection_id, "agent", "ok", QueryKind::Write).await;
        assert!(matches!(
            service
                .prepare_agent_create(write_run, presentation("Write"))
                .await,
            Err(AgentDashboardPrepareError::QueryRunIneligible)
        ));
    }

    #[tokio::test]
    async fn prepared_context_is_allowlisted_and_validation_waits_for_commit() {
        let (service, store, connection_id) = harness().await;
        let query_run_id =
            insert_history(&store, connection_id, "agent", "ok", QueryKind::Read).await;
        let prepared = service
            .prepare_agent_create(query_run_id, presentation("Agent result"))
            .await
            .unwrap();
        assert_eq!(
            prepared.event_context(),
            &AgentDashboardEventContext {
                query_run_id,
                connection_id,
                connection_name: "dashboard-test".into(),
                title: "Agent result".into(),
                sql: "SELECT id, name FROM users".into(),
            }
        );
        let saved = prepared.commit().await.unwrap();
        assert_eq!(saved.connection_id, connection_id);
        assert_eq!(saved.sql, "SELECT id, name FROM users");

        let invalid_run =
            insert_history(&store, connection_id, "agent", "ok", QueryKind::Read).await;
        let invalid = service
            .prepare_agent_create(invalid_run, presentation(" "))
            .await
            .unwrap()
            .commit()
            .await;
        assert!(matches!(
            invalid,
            Err(AgentDashboardCommitError::InvalidDraft(AppError::Config(_)))
        ));
    }

    #[tokio::test]
    async fn dashboard_run_preserves_wire_ledger_and_lease_contract() {
        let harness = DashboardRunHarness::new("SELECT id, name FROM users ORDER BY id").await;
        let receipt = harness
            .service
            .run(DashboardRunRequest {
                dashboard_id: harness.dashboard_id,
                query_id: None,
            })
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(&receipt).unwrap(),
            serde_json::json!({
                "columns": ["id", "name"],
                "rows": [[1, "Ada"], [2, "Linus"]],
                "rowCount": 2,
                "truncated": false,
                "durationMs": receipt.result.duration_ms
            }),
            "dashboard receipt must serialize as the literal legacy QueryResult"
        );
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                harness.connections.begin_scope_mutation(),
            )
            .await
            .is_err(),
            "dashboard receipt must retain authority through adapter serialization"
        );
        let history = harness
            .store
            .list_history(harness.connection_id)
            .await
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].origin, "dashboard");
        assert_eq!(history[0].status, "ok");
        assert_eq!(history[0].kind, QueryKind::Read);
        assert_eq!(history[0].row_count, Some(2));
        let (audit, valid, first_bad) = audit::snapshot(&harness.store, harness.connection_id)
            .await
            .unwrap();
        assert!(valid);
        assert_eq!(first_bad, None);
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].action, "dashboard:run");
        assert_eq!(audit[0].affected_estimate, Some(2));

        drop(receipt);
        let mutation = tokio::time::timeout(
            Duration::from_secs(5),
            harness.connections.begin_scope_mutation(),
        )
        .await
        .expect("scope mutation must proceed after dashboard receipt drop");
        drop(mutation);
        harness.close().await;
    }

    #[tokio::test]
    async fn dashboard_run_revalidates_tampered_sql_before_target_touch() {
        let harness = DashboardRunHarness::new("SELECT id, name FROM users ORDER BY id").await;
        harness
            .overwrite_dashboard_sql("UPDATE users SET name = 'Grace' WHERE id = 1")
            .await;
        let failure = match harness
            .service
            .run(DashboardRunRequest {
                dashboard_id: harness.dashboard_id,
                query_id: None,
            })
            .await
        {
            Err(DashboardRunError::Scoped(failure)) => failure,
            _ => panic!("tampered dashboard write must fail before target touch"),
        };
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                harness.connections.begin_scope_mutation(),
            )
            .await
            .is_err(),
            "blocked dashboard error must retain its scope through adapter mapping"
        );
        let error = failure.into_error();
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            serde_json::json!({
                "kind": "blocked",
                "message": "blocked: dashboards may only save one read-only SQL statement"
            })
        );
        assert_eq!(harness.user_name(1).await, "Ada");
        let history = harness
            .store
            .list_history(harness.connection_id)
            .await
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].origin, "dashboard");
        assert_eq!(history[0].status, "blocked");
        assert_eq!(history[0].kind, QueryKind::Write);
        let (audit, valid, first_bad) = audit::snapshot(&harness.store, harness.connection_id)
            .await
            .unwrap();
        assert!(valid);
        assert_eq!(first_bad, None);
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].action, "dashboard:run");
        assert!(audit[0]
            .error
            .as_deref()
            .is_some_and(|message| message.contains("dashboards may only save")));
        harness.close().await;
    }

    #[tokio::test]
    async fn dashboard_run_execution_error_preserves_original_error_and_ledger() {
        let harness = DashboardRunHarness::new("SELECT * FROM missing_users").await;
        let failure = match harness
            .service
            .run(DashboardRunRequest {
                dashboard_id: harness.dashboard_id,
                query_id: None,
            })
            .await
        {
            Err(DashboardRunError::Execution(failure)) => failure,
            _ => panic!("missing table must fail inside the read-only executor"),
        };
        let original = failure.error.to_string();
        assert!(original.contains("missing_users"));
        assert_eq!(failure.into_error().to_string(), original);
        let history = harness
            .store
            .list_history(harness.connection_id)
            .await
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].status, "error");
        assert!(history[0]
            .error
            .as_deref()
            .is_some_and(|message| message.contains("missing_users")));
        let (audit, valid, first_bad) = audit::snapshot(&harness.store, harness.connection_id)
            .await
            .unwrap();
        assert!(valid);
        assert_eq!(first_bad, None);
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].action, "dashboard:run");
        assert!(audit[0]
            .error
            .as_deref()
            .is_some_and(|message| message.contains("missing_users")));
        assert_eq!(harness.user_name(1).await, "Ada");
        harness.close().await;
    }
}
