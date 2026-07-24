//! Scope-aware database monitoring status and fixed PostgreSQL role changes.

use std::fmt;

use chrono::{Duration as ChronoDuration, Utc};
use dopedb_protocol::{OperationKind, OperationRiskLevel, OperationState};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::audit::{self, RecordArgs};
use crate::connection::{
    ConnectionAccess, ConnectionLease, ConnectionManager, ConnectionOperationScope,
};
use crate::error::AppError;
use crate::model::{Engine, HistoryEntry, MonitoringStatus, QueryKind};
use crate::monitoring;
use crate::operations::{NewOperation, OperationPlanDisposition, OperationRuntime};
use crate::store::{PinnedConnection, Store};

use super::operation_service::{
    actor_for_pin, capture_policy, ensure_operation_scope, required_confirmation,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MonitoringProposalRequest {
    pub(crate) connection_id: Uuid,
    pub(crate) enabled: bool,
}

/// Exact fixed-role operation rendered before the desktop may approve it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MonitoringProposalReceipt {
    pub(crate) operation_id: Uuid,
    pub(crate) payload_hash: String,
    pub(crate) state: OperationState,
    pub(crate) enabled: bool,
    pub(crate) sql: String,
    pub(crate) confirmation_phrase: Option<String>,
    pub(crate) expires_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredMonitoringPayload {
    enabled: bool,
    sql: String,
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
    operation: OperationRuntime,
}

impl MonitoringService {
    pub(super) fn new(
        store: Store,
        connections: ConnectionManager,
        operation: OperationRuntime,
    ) -> Self {
        Self {
            store,
            connections,
            operation,
        }
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

    /// Persist one fixed PostgreSQL role change as a high-risk exact proposal.
    /// The literal SQL is part of the immutable payload rendered to the user.
    pub(crate) async fn propose_postgres_role(
        &self,
        request: MonitoringProposalRequest,
    ) -> Result<MonitoringProposalReceipt, MonitoringServiceError> {
        let operation_scope = self.connections.begin_operation_scope().await;
        let pin = match operation_scope
            .pin_connection_for_view(request.connection_id)
            .await
        {
            Ok(pin) => pin,
            Err(error) => {
                return Err(MonitoringServiceError::Scoped(MonitoringScopedFailure {
                    error,
                    _scope: operation_scope,
                }));
            }
        };
        if !pin.profile.workspace_access.can_write() {
            return Err(MonitoringServiceError::Scoped(MonitoringScopedFailure {
                error: AppError::Blocked {
                    reason: "your workspace role cannot change database monitoring grants".into(),
                },
                _scope: operation_scope,
            }));
        }
        if !matches!(pin.profile.engine, Engine::Postgres) {
            return Err(MonitoringServiceError::Scoped(MonitoringScopedFailure {
                error: AppError::Config(
                    "pg_monitor is only available for PostgreSQL connections".into(),
                ),
                _scope: operation_scope,
            }));
        }
        let settings = self
            .store
            .get_safety(pin.connection_id)
            .await
            .map_err(MonitoringServiceError::Application)?;
        if !settings.allow_writes {
            return Err(MonitoringServiceError::Scoped(MonitoringScopedFailure {
                error: AppError::Blocked {
                    reason: "writes are disabled for this connection".into(),
                },
                _scope: operation_scope,
            }));
        }
        let policy =
            capture_policy(&pin, &settings).map_err(MonitoringServiceError::Application)?;
        let sql = monitoring_role_sql(request.enabled);
        let payload = serde_json::to_value(StoredMonitoringPayload {
            enabled: request.enabled,
            sql: sql.into(),
        })
        .map_err(AppError::from)
        .map_err(MonitoringServiceError::Application)?;
        let operation_id = Uuid::new_v4();
        let expires_at = Utc::now() + ChronoDuration::minutes(5);
        let operation = self
            .operation
            .plan(
                NewOperation {
                    id: operation_id,
                    workspace_id: pin.scope.workspace_id,
                    account_scope: pin.scope.account_scope.storage_key().into(),
                    connection_id: pin.connection_id,
                    connection_revision: pin.connection_revision,
                    terminal_session_id: None,
                    actor: actor_for_pin(&pin, "settings-safety-monitoring".into()),
                    kind: OperationKind::Privilege,
                    payload_schema_version: 1,
                    payload,
                    schema_fingerprint: None,
                    risk_level: OperationRiskLevel::High,
                    preview: serde_json::json!({
                        "action": if request.enabled { "grant" } else { "revoke" },
                        "role": "pg_monitor",
                        "sql": sql,
                    }),
                    policy_snapshot: policy.snapshot,
                    policy_revision: policy.revision,
                    single_use: true,
                    idempotency_key: operation_id.to_string(),
                    expires_at: Some(expires_at),
                },
                OperationPlanDisposition::ApprovalRequired,
            )
            .await
            .map_err(MonitoringServiceError::Application)?;
        let confirmation_phrase = required_confirmation(&operation).map(str::to_owned);
        Ok(MonitoringProposalReceipt {
            operation_id: operation.id,
            payload_hash: operation.payload_hash,
            state: operation.state,
            enabled: request.enabled,
            sql: sql.into(),
            confirmation_phrase,
            expires_at,
        })
    }

    /// Execute an exactly approved fixed-role proposal by operation id only.
    pub(crate) async fn run_postgres_role(
        &self,
        operation_id: Uuid,
    ) -> Result<MonitoringStatusReceipt, MonitoringServiceError> {
        let planned = self
            .operation
            .get(operation_id)
            .await
            .map_err(MonitoringServiceError::Application)?;
        if planned.payload_schema_version != 1 || planned.kind != OperationKind::Privilege {
            return Err(MonitoringServiceError::Application(AppError::Blocked {
                reason: "operation is not a PostgreSQL monitoring-role proposal".into(),
            }));
        }
        let payload: StoredMonitoringPayload = serde_json::from_value(planned.payload.clone())
            .map_err(AppError::from)
            .map_err(MonitoringServiceError::Application)?;
        if payload.sql != monitoring_role_sql(payload.enabled) {
            return Err(MonitoringServiceError::Application(AppError::Blocked {
                reason: "stored monitoring operation does not match the fixed role action".into(),
            }));
        }

        let operation_scope = self.connections.begin_operation_scope().await;
        let operation_pin = match operation_scope.pin_connection(planned.connection_id).await {
            Ok(pin) => pin,
            Err(error) => {
                return Err(MonitoringServiceError::Scoped(MonitoringScopedFailure {
                    error,
                    _scope: operation_scope,
                }));
            }
        };
        ensure_operation_scope(&planned, &operation_pin)
            .map_err(MonitoringServiceError::Application)?;
        if !operation_pin.profile.workspace_access.can_write() {
            return Err(MonitoringServiceError::Scoped(MonitoringScopedFailure {
                error: AppError::Blocked {
                    reason: "your workspace role no longer grants monitoring changes".into(),
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
        let settings = self
            .store
            .get_safety(operation_pin.connection_id)
            .await
            .map_err(MonitoringServiceError::Application)?;
        if !settings.allow_writes {
            return Err(MonitoringServiceError::Scoped(MonitoringScopedFailure {
                error: AppError::Blocked {
                    reason: "writes are disabled for this connection".into(),
                },
                _scope: operation_scope,
            }));
        }
        let policy = capture_policy(&operation_pin, &settings)
            .map_err(MonitoringServiceError::Application)?;
        if policy.revision != planned.policy_revision {
            return Err(MonitoringServiceError::Scoped(MonitoringScopedFailure {
                error: AppError::Blocked {
                    reason: "the connection or safety policy changed; create a new proposal".into(),
                },
                _scope: operation_scope,
            }));
        }

        let claimed = match self.operation.claim(operation_id).await {
            Ok(claimed) => claimed,
            Err(error) => {
                return Err(MonitoringServiceError::Scoped(MonitoringScopedFailure {
                    error,
                    _scope: operation_scope,
                }));
            }
        };
        let approved_by = claimed.record().actor.id.as_str();
        if let Err(error) = audit::record(
            &self.store,
            RecordArgs {
                connection_id: operation_pin.connection_id,
                engine: operation_pin.profile.engine,
                agent_prompt: None,
                sql: payload.sql.clone(),
                kind: QueryKind::Privilege,
                action: if payload.enabled {
                    "monitoring:grant:attempt"
                } else {
                    "monitoring:revoke:attempt"
                }
                .into(),
                approved_by: Some(approved_by.into()),
                affected_estimate: None,
                error: None,
            },
        )
        .await
        {
            let _ = self
                .operation
                .fail(
                    operation_id,
                    &serde_json::json!({
                        "error": error.to_string(),
                        "reason": "audit_pre_record_failed",
                    }),
                )
                .await;
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
                        sql: &payload.sql,
                        status: "error",
                        error: Some(error.to_string()),
                        approved_by: Some(approved_by),
                    },
                )
                .await;
                let _ = self
                    .operation
                    .fail(
                        operation_id,
                        &serde_json::json!({
                            "error": error.to_string(),
                            "reason": "target_connection_failed",
                        }),
                    )
                    .await;
                return Err(MonitoringServiceError::Application(error));
            }
        };
        let live = match lease.live().sql() {
            Ok(live) => live,
            Err(error) => {
                let _ = self
                    .operation
                    .fail(
                        operation_id,
                        &serde_json::json!({
                            "error": error.to_string(),
                            "reason": "target_pool_unavailable",
                        }),
                    )
                    .await;
                return Err(MonitoringServiceError::Execution(Box::new(
                    MonitoringExecutionFailure {
                        error,
                        _lease: lease,
                    },
                )));
            }
        };
        if let Err(error) = monitoring::set_postgres_role(
            live,
            payload.enabled,
            claimed.grant(),
            operation_id,
            operation_pin.connection_id,
        )
        .await
        {
            record_monitoring_change(
                &self.store,
                &operation_pin,
                MonitoringRunRecord {
                    sql: &payload.sql,
                    status: "error",
                    error: Some(error.to_string()),
                    approved_by: Some(approved_by),
                },
            )
            .await;
            let _ = self
                .operation
                .mark_outcome_unknown(
                    operation_id,
                    &serde_json::json!({
                        "error": error.to_string(),
                        "reason": "monitoring_role_execution_failed",
                    }),
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
                sql: &payload.sql,
                status: "ok",
                error: None,
                approved_by: Some(approved_by),
            },
        )
        .await;
        if let Err(error) = self
            .operation
            .succeed(
                operation_id,
                &serde_json::json!({
                    "enabled": payload.enabled,
                    "role": "pg_monitor",
                }),
            )
            .await
        {
            let _ = self
                .operation
                .mark_outcome_unknown(
                    operation_id,
                    &serde_json::json!({"reason": "local_receipt_failed"}),
                )
                .await;
            return Err(MonitoringServiceError::Execution(Box::new(
                MonitoringExecutionFailure {
                    error,
                    _lease: lease,
                },
            )));
        }
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

const fn monitoring_role_sql(enabled: bool) -> &'static str {
    if enabled {
        "GRANT pg_monitor TO CURRENT_USER"
    } else {
        "REVOKE pg_monitor FROM CURRENT_USER"
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

    async fn harness(
        engine: Engine,
    ) -> (
        MonitoringService,
        Store,
        ConnectionManager,
        OperationRuntime,
        Uuid,
    ) {
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
        let safety = crate::model::SafetySettings {
            allow_writes: true,
            ..crate::model::SafetySettings::default()
        };
        store.set_safety(connection_id, &safety).await.unwrap();
        let connections = ConnectionManager::new(store.clone());
        let (operation, _approval) = OperationRuntime::new(&store);
        (
            MonitoringService::new(store.clone(), connections.clone(), operation.clone()),
            store,
            connections,
            operation,
            connection_id,
        )
    }

    #[tokio::test]
    async fn document_status_preserves_basic_wire_without_target_probe() {
        let (service, store, connections, _, connection_id) = harness(Engine::Mongodb).await;
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
        let (service, store, _, _, connection_id) = harness(Engine::Mongodb).await;
        let error = match service
            .propose_postgres_role(MonitoringProposalRequest {
                connection_id,
                enabled: true,
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
    async fn unapproved_postgres_change_remains_pending_without_target_or_audit_touch() {
        let (service, store, _, operation, connection_id) = harness(Engine::Postgres).await;
        let proposal = service
            .propose_postgres_role(MonitoringProposalRequest {
                connection_id,
                enabled: true,
            })
            .await
            .unwrap();
        assert_eq!(proposal.state, OperationState::PendingApproval);
        assert_eq!(proposal.sql, "GRANT pg_monitor TO CURRENT_USER");
        assert_eq!(proposal.payload_hash.len(), 64);
        let error = match service.run_postgres_role(proposal.operation_id).await {
            Err(error) => error.into_error(),
            Ok(_) => panic!("unapproved PostgreSQL monitoring change must be rejected"),
        };
        assert!(matches!(error, AppError::Blocked { .. }));
        assert_eq!(
            operation.get(proposal.operation_id).await.unwrap().state,
            OperationState::PendingApproval
        );
        let (audit, valid, first_bad) = audit::snapshot(&store, connection_id).await.unwrap();
        assert!(valid);
        assert_eq!(first_bad, None);
        assert!(audit.is_empty());
        assert!(store.list_history(connection_id).await.unwrap().is_empty());
        store.pool().close().await;
    }
}
