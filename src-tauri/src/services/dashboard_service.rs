//! Transport-neutral saved-dashboard metadata workflows shared by Tauri and MCP.
//! Execution remains in the legacy query path until `QueryService` owns it.

use uuid::Uuid;

use crate::connection::{ConnectionManager, ConnectionOperationScope};
use crate::error::{AppError, AppResult};
use crate::model::{
    Dashboard, DashboardDraft, DashboardKind, DashboardVisualization, HistoryEntry, QueryKind,
};
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::str::FromStr;

    use chrono::Utc;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

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
}
