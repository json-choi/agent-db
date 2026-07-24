//! Scope-aware database monitoring status and fixed PostgreSQL role changes.

use std::fmt;

use chrono::Utc;
use uuid::Uuid;

use crate::audit::{self, RecordArgs};
use crate::connection::{
    ConnectionAccess, ConnectionLease, ConnectionManager, ConnectionOperationScope,
};
use crate::error::AppError;
use crate::model::{Engine, HistoryEntry, MonitoringStatus, QueryKind};
use crate::monitoring;
use crate::store::{PinnedConnection, Store};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MonitoringChangeRequest {
    pub(crate) connection_id: Uuid,
    pub(crate) enabled: bool,
    pub(crate) approved: bool,
}

/// Monitoring response retaining either a metadata scope or a live target lease
/// through adapter serialization.
pub(crate) struct MonitoringStatusReceipt {
    status: MonitoringStatus,
    _scope: Option<ConnectionOperationScope>,
    _lease: Option<ConnectionLease>,
}

impl MonitoringStatusReceipt {
    fn scoped(status: MonitoringStatus, scope: ConnectionOperationScope) -> Self {
        Self {
            status,
            _scope: Some(scope),
            _lease: None,
        }
    }

    fn leased(status: MonitoringStatus, lease: ConnectionLease) -> Self {
        Self {
            status,
            _scope: None,
            _lease: Some(lease),
        }
    }
}

impl serde::Serialize for MonitoringStatusReceipt {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serde::Serialize::serialize(&self.status, serializer)
    }
}

#[derive(Debug)]
pub(crate) enum MonitoringServiceError {
    Application(AppError),
    Scoped(MonitoringScopedFailure),
    Execution(Box<MonitoringExecutionFailure>),
}

impl MonitoringServiceError {
    pub(crate) fn into_error(self) -> AppError {
        match self {
            Self::Application(error) => error,
            Self::Scoped(failure) => failure.into_error(),
            Self::Execution(failure) => failure.into_error(),
        }
    }
}

pub(crate) struct MonitoringScopedFailure {
    error: AppError,
    _scope: ConnectionOperationScope,
}

impl fmt::Debug for MonitoringScopedFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MonitoringScopedFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl MonitoringScopedFailure {
    fn into_error(self) -> AppError {
        self.error
    }
}

pub(crate) struct MonitoringExecutionFailure {
    error: AppError,
    _lease: ConnectionLease,
}

impl fmt::Debug for MonitoringExecutionFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MonitoringExecutionFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl MonitoringExecutionFailure {
    fn into_error(self) -> AppError {
        self.error
    }
}

#[derive(Clone)]
pub(crate) struct MonitoringService {
    store: Store,
    connections: ConnectionManager,
}

impl MonitoringService {
    pub(super) fn new(store: Store, connections: ConnectionManager) -> Self {
        Self { store, connections }
    }

    pub(crate) async fn status(
        &self,
        connection_id: Uuid,
    ) -> Result<MonitoringStatusReceipt, MonitoringServiceError> {
        let operation_scope = self.connections.begin_operation_scope().await;
        let pin = match operation_scope.pin_connection_for_view(connection_id).await {
            Ok(pin) => pin,
            Err(error) => {
                return Err(MonitoringServiceError::Scoped(MonitoringScopedFailure {
                    error,
                    _scope: operation_scope,
                }))
            }
        };
        if pin.profile.engine.is_document() {
            return Ok(MonitoringStatusReceipt::scoped(
                MonitoringStatus {
                    engine: pin.profile.engine,
                    coverage: "basic".into(),
                    role_available: false,
                    role_granted: false,
                    current_user: None,
                    can_manage: false,
                    note: "MongoDB connections use the basic, role-free collector.".into(),
                },
                operation_scope,
            ));
        }

        let engine = pin.profile.engine;
        let lease = operation_scope
            .connect(pin, ConnectionAccess::Read)
            .await
            .map_err(MonitoringServiceError::Application)?;
        let live = match lease.live().sql() {
            Ok(live) => live,
            Err(error) => {
                return Err(MonitoringServiceError::Execution(Box::new(
                    MonitoringExecutionFailure {
                        error,
                        _lease: lease,
                    },
                )))
            }
        };
        match monitoring::status(live, engine).await {
            Ok(status) => Ok(MonitoringStatusReceipt::leased(status, lease)),
            Err(error) => Err(MonitoringServiceError::Execution(Box::new(
                MonitoringExecutionFailure {
                    error,
                    _lease: lease,
                },
            ))),
        }
    }

    /// Apply one fixed GRANT/REVOKE after explicit legacy approval. FND-04 replaces
    /// that boolean with an exact stored Operation approval.
    pub(crate) async fn set_postgres_role(
        &self,
        request: MonitoringChangeRequest,
    ) -> Result<MonitoringStatusReceipt, MonitoringServiceError> {
        let operation_scope = self.connections.begin_operation_scope().await;
        let operation_pin = match operation_scope.pin_connection(request.connection_id).await {
            Ok(pin) => pin,
            Err(error) => {
                return Err(MonitoringServiceError::Scoped(MonitoringScopedFailure {
                    error,
                    _scope: operation_scope,
                }))
            }
        };
        if !operation_pin.profile.workspace_access.can_write() {
            return Err(MonitoringServiceError::Scoped(MonitoringScopedFailure {
                error: AppError::Blocked {
                    reason: "your workspace role cannot change database monitoring grants".into(),
                },
                _scope: operation_scope,
            }));
        }
        if !matches!(operation_pin.profile.engine, Engine::Postgres) {
            return Err(MonitoringServiceError::Scoped(MonitoringScopedFailure {
                error: AppError::Config(
                    "pg_monitor is only available for PostgreSQL connections".into(),
                ),
                _scope: operation_scope,
            }));
        }
        let sql = if request.enabled {
            "GRANT pg_monitor TO CURRENT_USER"
        } else {
            "REVOKE pg_monitor FROM CURRENT_USER"
        };
        if !request.approved {
            record_monitoring_change(
                &self.store,
                &operation_pin,
                MonitoringRunRecord {
                    sql,
                    status: "blocked",
                    error: Some("pg_monitor role changes require explicit confirmation".into()),
                    approved_by: None,
                },
            )
            .await;
            return Err(MonitoringServiceError::Scoped(MonitoringScopedFailure {
                error: AppError::Blocked {
                    reason: "pg_monitor role changes require explicit confirmation".into(),
                },
                _scope: operation_scope,
            }));
        }

        if let Err(error) = audit::record(
            &self.store,
            RecordArgs {
                connection_id: operation_pin.connection_id,
                engine: operation_pin.profile.engine,
                agent_prompt: None,
                sql: sql.into(),
                kind: QueryKind::Privilege,
                action: if request.enabled {
                    "monitoring:grant:attempt"
                } else {
                    "monitoring:revoke:attempt"
                }
                .into(),
                approved_by: Some("local-user".into()),
                affected_estimate: None,
                error: None,
            },
        )
        .await
        {
            return Err(MonitoringServiceError::Scoped(MonitoringScopedFailure {
                error: AppError::Config(format!(
                    "audit pre-record failed — refusing to change pg_monitor: {error}"
                )),
                _scope: operation_scope,
            }));
        }

        let lease = match operation_scope
            .connect(operation_pin.clone(), ConnectionAccess::Write)
            .await
        {
            Ok(lease) => lease,
            Err(error) => {
                record_monitoring_change(
                    &self.store,
                    &operation_pin,
                    MonitoringRunRecord {
                        sql,
                        status: "error",
                        error: Some(error.to_string()),
                        approved_by: Some("local-user"),
                    },
                )
                .await;
                return Err(MonitoringServiceError::Application(error));
            }
        };
        let live = match lease.live().sql() {
            Ok(live) => live,
            Err(error) => {
                return Err(MonitoringServiceError::Execution(Box::new(
                    MonitoringExecutionFailure {
                        error,
                        _lease: lease,
                    },
                )))
            }
        };
        if let Err(error) = monitoring::set_postgres_role(live, request.enabled).await {
            record_monitoring_change(
                &self.store,
                &operation_pin,
                MonitoringRunRecord {
                    sql,
                    status: "error",
                    error: Some(error.to_string()),
                    approved_by: Some("local-user"),
                },
            )
            .await;
            return Err(MonitoringServiceError::Execution(Box::new(
                MonitoringExecutionFailure {
                    error,
                    _lease: lease,
                },
            )));
        }
        record_monitoring_change(
            &self.store,
            &operation_pin,
            MonitoringRunRecord {
                sql,
                status: "ok",
                error: None,
                approved_by: Some("local-user"),
            },
        )
        .await;
        match monitoring::status(live, operation_pin.profile.engine).await {
            Ok(status) => Ok(MonitoringStatusReceipt::leased(status, lease)),
            Err(error) => Err(MonitoringServiceError::Execution(Box::new(
                MonitoringExecutionFailure {
                    error,
                    _lease: lease,
                },
            ))),
        }
    }
}

struct MonitoringRunRecord<'a> {
    sql: &'a str,
    status: &'a str,
    error: Option<String>,
    approved_by: Option<&'a str>,
}

async fn record_monitoring_change(
    store: &Store,
    pin: &PinnedConnection,
    record: MonitoringRunRecord<'_>,
) {
    let action = if record.status == "blocked" {
        "monitoring:blocked"
    } else if record.sql.starts_with("GRANT") {
        "monitoring:grant"
    } else {
        "monitoring:revoke"
    };
    if let Err(error) = audit::record(
        store,
        RecordArgs {
            connection_id: pin.connection_id,
            engine: pin.profile.engine,
            agent_prompt: None,
            sql: record.sql.into(),
            kind: QueryKind::Privilege,
            action: action.into(),
            approved_by: record.approved_by.map(str::to_string),
            affected_estimate: None,
            error: record.error.clone(),
        },
    )
    .await
    {
        tracing::error!(
            connection_id = %pin.connection_id,
            %error,
            "monitoring audit record failed"
        );
    }
    if let Err(error) = store
        .insert_history_if_current(
            pin,
            &HistoryEntry {
                id: Uuid::new_v4(),
                connection_id: pin.connection_id,
                sql: record.sql.into(),
                kind: QueryKind::Privilege,
                status: record.status.into(),
                row_count: None,
                duration_ms: None,
                error: record.error,
                executed_at: Utc::now(),
                origin: "manual".into(),
            },
        )
        .await
    {
        tracing::error!(
            connection_id = %pin.connection_id,
            %error,
            "monitoring history insert failed"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::str::FromStr;
    use std::time::Duration;

    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    use super::*;
    use crate::model::{
        ConnectionProfile, Provider, WorkspaceConnectionAccess, WorkspaceCredentialMode,
    };
    use crate::store::TEST_SCHEMA;

    async fn harness(engine: Engine) -> (MonitoringService, Store, ConnectionManager, Uuid) {
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
                name: "monitoring-test".into(),
                engine,
                provider: Provider::Generic,
                driver_id: None,
                host: "127.0.0.1".into(),
                port: if matches!(engine, Engine::Postgres) {
                    5432
                } else {
                    27017
                },
                database: "test".into(),
                username: "tester".into(),
                sslmode: "disable".into(),
                extra_params: HashMap::new(),
                readonly_default: true,
                allow_writes: true,
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
            MonitoringService::new(store.clone(), connections.clone()),
            store,
            connections,
            connection_id,
        )
    }

    #[tokio::test]
    async fn document_status_preserves_basic_wire_without_target_probe() {
        let (service, store, connections, connection_id) = harness(Engine::Mongodb).await;
        let receipt = service.status(connection_id).await.unwrap();
        assert_eq!(
            serde_json::to_value(&receipt).unwrap(),
            serde_json::json!({
                "engine": "mongodb",
                "coverage": "basic",
                "roleAvailable": false,
                "roleGranted": false,
                "currentUser": null,
                "canManage": false,
                "note": "MongoDB connections use the basic, role-free collector."
            })
        );
        assert!(tokio::time::timeout(
            Duration::from_millis(100),
            connections.begin_scope_mutation(),
        )
        .await
        .is_err());
        drop(receipt);
        let mutation = connections.begin_scope_mutation().await;
        drop(mutation);
        store.pool().close().await;
    }

    #[tokio::test]
    async fn non_postgres_change_rejects_before_audit_or_target_touch() {
        let (service, store, _, connection_id) = harness(Engine::Mongodb).await;
        let error = match service
            .set_postgres_role(MonitoringChangeRequest {
                connection_id,
                enabled: true,
                approved: true,
            })
            .await
        {
            Err(error) => error.into_error(),
            Ok(_) => panic!("non-PostgreSQL monitoring change must be rejected"),
        };
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            serde_json::json!({
                "kind": "config",
                "message": "config error: pg_monitor is only available for PostgreSQL connections"
            })
        );
        assert!(audit::snapshot(&store, connection_id)
            .await
            .unwrap()
            .0
            .is_empty());
        assert!(store.list_history(connection_id).await.unwrap().is_empty());
        store.pool().close().await;
    }

    #[tokio::test]
    async fn unapproved_postgres_change_preserves_exact_ledger_contract_without_connecting() {
        let (service, store, _, connection_id) = harness(Engine::Postgres).await;
        let error = match service
            .set_postgres_role(MonitoringChangeRequest {
                connection_id,
                enabled: true,
                approved: false,
            })
            .await
        {
            Err(error) => error.into_error(),
            Ok(_) => panic!("unapproved PostgreSQL monitoring change must be rejected"),
        };
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            serde_json::json!({
                "kind": "blocked",
                "message": "blocked: pg_monitor role changes require explicit confirmation"
            })
        );
        let (audit, valid, first_bad) = audit::snapshot(&store, connection_id).await.unwrap();
        assert!(valid);
        assert_eq!(first_bad, None);
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].action, "monitoring:blocked");
        assert_eq!(audit[0].approved_by, None);
        let history = store.list_history(connection_id).await.unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].status, "blocked");
        assert_eq!(history[0].origin, "manual");
        store.pool().close().await;
    }
}
