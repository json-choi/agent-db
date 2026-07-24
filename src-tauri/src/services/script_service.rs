//! Transport-neutral multi-statement SQL script execution.

use std::fmt;

use chrono::{Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::AssertSqlSafe;
use uuid::Uuid;

use crate::audit::{self, RecordArgs};
use crate::connection::{
    ConnectionAccess, ConnectionLease, ConnectionManager, ConnectionOperationScope, DbPool,
};
use crate::error::{AppError, AppResult};
use crate::executor;
use crate::model::{HistoryEntry, QueryKind, ScriptOutcome, ScriptStatement};
use crate::operations::{
    ClaimedOperation, ExecutionGrant, NewOperation, OperationKind, OperationPlanDisposition,
    OperationRiskLevel, OperationRuntime, OperationState,
};
use crate::safety;
use crate::store::{PinnedConnection, Store};

use super::operation_service::{
    actor_for_pin, capture_policy, ensure_operation_scope, required_confirmation,
};
use super::query_service::QUERY_PLAN_TTL;

/// Desktop script input accepted only at the immutable proposal boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DesktopScriptProposalRequest {
    pub(crate) connection_id: Uuid,
    pub(crate) sql: String,
    pub(crate) origin: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopScriptProposalReceipt {
    pub(crate) operation_id: Uuid,
    pub(crate) payload_hash: String,
    pub(crate) state: OperationState,
    pub(crate) approval_required: bool,
    pub(crate) confirmation_phrase: Option<String>,
    pub(crate) statement_count: usize,
    pub(crate) expires_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredDesktopScriptPayload {
    sql: String,
    history_origin: String,
}

/// Successful script execution retaining target authority until the adapter has
/// serialized the established [`ScriptOutcome`] payload.
pub(crate) struct DesktopScriptRunReceipt {
    outcome: ScriptOutcome,
    _lease: ConnectionLease,
}

impl serde::Serialize for DesktopScriptRunReceipt {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serde::Serialize::serialize(&self.outcome, serializer)
    }
}

#[derive(Debug)]
pub(crate) enum DesktopScriptRunError {
    Application(AppError),
    Scoped(DesktopScriptScopedFailure),
    Execution(Box<DesktopScriptExecutionFailure>),
}

impl DesktopScriptRunError {
    pub(crate) fn into_error(self) -> AppError {
        match self {
            Self::Application(error) => error,
            Self::Scoped(failure) => failure.into_error(),
            Self::Execution(failure) => failure.into_error(),
        }
    }
}

pub(crate) struct DesktopScriptScopedFailure {
    error: AppError,
    _scope: ConnectionOperationScope,
}

impl fmt::Debug for DesktopScriptScopedFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DesktopScriptScopedFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl DesktopScriptScopedFailure {
    fn into_error(self) -> AppError {
        self.error
    }
}

pub(crate) struct DesktopScriptExecutionFailure {
    error: AppError,
    _lease: ConnectionLease,
}

impl fmt::Debug for DesktopScriptExecutionFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DesktopScriptExecutionFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl DesktopScriptExecutionFailure {
    fn into_error(self) -> AppError {
        self.error
    }
}

#[derive(Clone)]
pub(crate) struct ScriptService {
    store: Store,
    connections: ConnectionManager,
    operation: OperationRuntime,
}

struct PreparedScriptRun {
    operation_scope: ConnectionOperationScope,
    operation_pin: PinnedConnection,
    operation: ClaimedOperation,
    payload: StoredDesktopScriptPayload,
    statements: Vec<String>,
    kinds: Vec<QueryKind>,
    settings: crate::model::SafetySettings,
    engine: crate::model::Engine,
    history_origin: String,
}

impl ScriptService {
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

    /// Persist one exact multi-statement proposal after classifying every statement.
    /// All-read scripts become ready single-use plans; any mutation always waits for
    /// exact approval regardless of the legacy prompt preference.
    pub(crate) async fn propose_desktop(
        &self,
        request: DesktopScriptProposalRequest,
    ) -> Result<DesktopScriptProposalReceipt, DesktopScriptRunError> {
        let operation_scope = self.connections.begin_operation_scope().await;
        let pin = match operation_scope
            .pin_connection_for_view(request.connection_id)
            .await
        {
            Ok(pin) => pin,
            Err(error) => {
                return Err(DesktopScriptRunError::Scoped(DesktopScriptScopedFailure {
                    error,
                    _scope: operation_scope,
                }))
            }
        };
        if pin.profile.engine.is_document() {
            return Err(DesktopScriptRunError::Scoped(DesktopScriptScopedFailure {
                error: AppError::Blocked {
                    reason: "SQL scripts are unavailable for document connections".into(),
                },
                _scope: operation_scope,
            }));
        }
        let settings = self
            .store
            .get_safety(pin.connection_id)
            .await
            .map_err(DesktopScriptRunError::Application)?;
        let statements = crate::sql_script::split_statements(&request.sql, pin.profile.engine);
        if statements.is_empty() {
            return Err(DesktopScriptRunError::Scoped(DesktopScriptScopedFailure {
                error: AppError::Config("no executable statements in the script".into()),
                _scope: operation_scope,
            }));
        }
        let classifications = statements
            .iter()
            .map(|statement| safety::classify(statement, pin.profile.engine))
            .collect::<AppResult<Vec<_>>>()
            .map_err(DesktopScriptRunError::Application)?;
        let kinds = classifications
            .iter()
            .map(|classification| classification.kind)
            .collect::<Vec<_>>();
        if kinds
            .iter()
            .any(|kind| matches!(kind, QueryKind::Privilege))
        {
            return Err(DesktopScriptRunError::Scoped(DesktopScriptScopedFailure {
                error: AppError::Blocked {
                    reason: "arbitrary privilege SQL is blocked; use a supported, narrowly scoped administrative action"
                        .into(),
                },
                _scope: operation_scope,
            }));
        }
        let has_write = script_has_write(&kinds);
        let access_allowed = if has_write {
            pin.profile.workspace_access.can_write()
        } else {
            pin.profile.workspace_access.can_read()
        };
        if !access_allowed {
            return Err(DesktopScriptRunError::Scoped(DesktopScriptScopedFailure {
                error: AppError::Blocked {
                    reason: "your workspace role no longer grants this script access".into(),
                },
                _scope: operation_scope,
            }));
        }
        if has_write && !settings.allow_writes {
            return Err(DesktopScriptRunError::Scoped(DesktopScriptScopedFailure {
                error: AppError::Blocked {
                    reason: "writes are disabled for this connection".into(),
                },
                _scope: operation_scope,
            }));
        }
        let policy = capture_policy(&pin, &settings).map_err(DesktopScriptRunError::Application)?;
        let history_origin = request.origin.unwrap_or_else(|| "manual".into());
        let payload = serde_json::to_value(StoredDesktopScriptPayload {
            sql: request.sql,
            history_origin: history_origin.clone(),
        })
        .map_err(AppError::from)
        .map_err(DesktopScriptRunError::Application)?;
        let operation_id = Uuid::new_v4();
        let expires_at = Utc::now()
            + if has_write {
                ChronoDuration::minutes(5)
            } else {
                ChronoDuration::from_std(QUERY_PLAN_TTL)
                    .expect("query plan TTL is representable by chrono")
            };
        let disposition = if has_write {
            OperationPlanDisposition::ApprovalRequired
        } else {
            OperationPlanDisposition::Ready
        };
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
                    actor: actor_for_pin(&pin, history_origin),
                    kind: if has_write {
                        OperationKind::SqlScript
                    } else {
                        OperationKind::ReadQuery
                    },
                    payload_schema_version: 1,
                    payload,
                    schema_fingerprint: None,
                    risk_level: script_operation_risk(&classifications),
                    preview: serde_json::json!({
                        "classifications": classifications,
                        "statementCount": statements.len(),
                    }),
                    policy_snapshot: policy.snapshot,
                    policy_revision: policy.revision,
                    single_use: true,
                    idempotency_key: operation_id.to_string(),
                    expires_at: Some(expires_at),
                },
                disposition,
            )
            .await
            .map_err(DesktopScriptRunError::Application)?;
        let confirmation_phrase = required_confirmation(&operation).map(str::to_owned);
        Ok(DesktopScriptProposalReceipt {
            operation_id: operation.id,
            payload_hash: operation.payload_hash,
            state: operation.state,
            approval_required: has_write,
            confirmation_phrase,
            statement_count: statements.len(),
            expires_at,
        })
    }

    /// Execute an immutable script by operation id only.
    pub(crate) async fn run_desktop(
        &self,
        operation_id: Uuid,
    ) -> Result<DesktopScriptRunReceipt, DesktopScriptRunError> {
        let planned = self
            .operation
            .get(operation_id)
            .await
            .map_err(DesktopScriptRunError::Application)?;
        if planned.payload_schema_version != 1
            || !matches!(
                planned.kind,
                OperationKind::ReadQuery | OperationKind::SqlScript
            )
        {
            return Err(DesktopScriptRunError::Application(AppError::Blocked {
                reason: "operation is not a supported SQL script proposal".into(),
            }));
        }
        let payload: StoredDesktopScriptPayload = serde_json::from_value(planned.payload.clone())
            .map_err(AppError::from)
            .map_err(DesktopScriptRunError::Application)?;
        let operation_scope = self.connections.begin_operation_scope().await;
        let operation_pin = match operation_scope.pin_connection(planned.connection_id).await {
            Ok(pin) => pin,
            Err(error) => {
                return Err(DesktopScriptRunError::Scoped(DesktopScriptScopedFailure {
                    error,
                    _scope: operation_scope,
                }))
            }
        };
        ensure_operation_scope(&planned, &operation_pin)
            .map_err(DesktopScriptRunError::Application)?;
        let settings = match self.store.get_safety(operation_pin.connection_id).await {
            Ok(settings) => settings,
            Err(error) => {
                return Err(DesktopScriptRunError::Scoped(DesktopScriptScopedFailure {
                    error,
                    _scope: operation_scope,
                }))
            }
        };
        let policy = capture_policy(&operation_pin, &settings)
            .map_err(DesktopScriptRunError::Application)?;
        if policy.revision != planned.policy_revision {
            return Err(DesktopScriptRunError::Scoped(DesktopScriptScopedFailure {
                error: AppError::Blocked {
                    reason: "the connection or safety policy changed; create a new proposal".into(),
                },
                _scope: operation_scope,
            }));
        }
        let engine = operation_pin.profile.engine;
        let history_origin = payload.history_origin.clone();
        let statements = crate::sql_script::split_statements(&payload.sql, engine);
        if statements.is_empty() {
            return Err(DesktopScriptRunError::Scoped(DesktopScriptScopedFailure {
                error: AppError::Config("no executable statements in the script".into()),
                _scope: operation_scope,
            }));
        }
        let kinds = match statements
            .iter()
            .map(|statement| safety::classify(statement, engine).map(|result| result.kind))
            .collect::<AppResult<Vec<_>>>()
        {
            Ok(kinds) => kinds,
            Err(error) => {
                return Err(DesktopScriptRunError::Scoped(DesktopScriptScopedFailure {
                    error,
                    _scope: operation_scope,
                }))
            }
        };
        if kinds
            .iter()
            .any(|kind| matches!(kind, QueryKind::Privilege))
        {
            return Err(DesktopScriptRunError::Scoped(DesktopScriptScopedFailure {
                error: AppError::Blocked {
                    reason: "stored script contains blocked arbitrary privilege SQL".into(),
                },
                _scope: operation_scope,
            }));
        }

        let has_write = script_has_write(&kinds);
        let expected_kind = if has_write {
            OperationKind::SqlScript
        } else {
            OperationKind::ReadQuery
        };
        if planned.kind != expected_kind {
            return Err(DesktopScriptRunError::Scoped(DesktopScriptScopedFailure {
                error: AppError::Blocked {
                    reason: "stored script classification no longer matches its proposal".into(),
                },
                _scope: operation_scope,
            }));
        }
        let operation = self
            .operation
            .claim(operation_id)
            .await
            .map_err(DesktopScriptRunError::Application)?;
        let prepared = PreparedScriptRun {
            operation_scope,
            operation_pin,
            operation,
            payload,
            statements,
            kinds,
            settings,
            engine,
            history_origin,
        };
        if has_write {
            self.run_write(prepared).await
        } else {
            self.run_reads(prepared).await
        }
    }

    async fn run_reads(
        &self,
        prepared: PreparedScriptRun,
    ) -> Result<DesktopScriptRunReceipt, DesktopScriptRunError> {
        let PreparedScriptRun {
            operation_scope,
            operation_pin,
            operation,
            payload,
            statements,
            kinds: _,
            settings,
            engine,
            history_origin,
        } = prepared;
        let operation_id = operation.record().id;

        let lease = match operation_scope
            .connect(operation_pin.clone(), ConnectionAccess::Read)
            .await
        {
            Ok(lease) => lease,
            Err(error) => {
                record_script_run(
                    &self.store,
                    &operation_pin,
                    ScriptRunRecord {
                        sql: &payload.sql,
                        kind: QueryKind::Read,
                        action: "script:execute",
                        status: "error",
                        row_count: None,
                        error: Some(error.to_string()),
                        origin: &history_origin,
                    },
                )
                .await;
                let _ = self
                    .operation
                    .fail(
                        operation_id,
                        &serde_json::json!({"reason": "connection_failed"}),
                    )
                    .await;
                return Err(DesktopScriptRunError::Application(error));
            }
        };
        let live = match lease.live().sql() {
            Ok(live) => live,
            Err(error) => {
                let _ = self
                    .operation
                    .fail(
                        operation_id,
                        &serde_json::json!({"reason": "sql_backend_unavailable"}),
                    )
                    .await;
                return Err(DesktopScriptRunError::Execution(Box::new(
                    DesktopScriptExecutionFailure {
                        error,
                        _lease: lease,
                    },
                )));
            }
        };
        let mut outcomes = Vec::with_capacity(statements.len());
        let mut failure = None;
        for statement in &statements {
            if failure.is_some() {
                outcomes.push(statement_skipped(statement));
                continue;
            }
            match executor::run_read(
                live,
                engine,
                statement,
                settings.max_rows,
                Some(operation_id),
            )
            .await
            {
                Ok(result) => outcomes.push(ScriptStatement {
                    sql: statement.clone(),
                    result: Some(result),
                    affected: None,
                    error: None,
                }),
                Err(error) => {
                    let message = error.to_string();
                    outcomes.push(statement_error(statement, message.clone()));
                    failure = Some(message);
                }
            }
        }
        let total = outcomes
            .iter()
            .filter_map(|statement| statement.result.as_ref())
            .map(|result| result.row_count as i64)
            .sum();
        let failed = failure.is_some();
        let (status, error) = match failure {
            Some(error) => ("error", Some(error)),
            None => ("ok", None),
        };
        record_script_run(
            &self.store,
            &operation_pin,
            ScriptRunRecord {
                sql: &payload.sql,
                kind: QueryKind::Read,
                action: "script:execute",
                status,
                row_count: Some(total),
                error,
                origin: &history_origin,
            },
        )
        .await;
        let operation_result = if failed {
            self.operation
                .fail(
                    operation_id,
                    &serde_json::json!({"reason": "script_statement_failed"}),
                )
                .await
        } else {
            self.operation
                .succeed(
                    operation_id,
                    &serde_json::json!({"rowCount": total, "statementCount": statements.len()}),
                )
                .await
        };
        operation_result.map_err(DesktopScriptRunError::Application)?;
        Ok(DesktopScriptRunReceipt {
            outcome: ScriptOutcome {
                statements: outcomes,
                committed: false,
                all_reads: true,
            },
            _lease: lease,
        })
    }

    async fn run_write(
        &self,
        prepared: PreparedScriptRun,
    ) -> Result<DesktopScriptRunReceipt, DesktopScriptRunError> {
        let PreparedScriptRun {
            operation_scope,
            operation_pin,
            operation,
            payload,
            statements,
            kinds,
            settings,
            engine,
            history_origin,
        } = prepared;
        let operation_id = operation.record().id;
        if !operation_pin.profile.workspace_access.can_write() {
            return Err(DesktopScriptRunError::Scoped(DesktopScriptScopedFailure {
                error: AppError::Blocked {
                    reason: "your workspace role grants read-only database access".into(),
                },
                _scope: operation_scope,
            }));
        }
        if !settings.allow_writes {
            let reason = "writing is disabled for this connection (writes are off by default). \
                          Enable writes in the connection's safety settings to run this script."
                .to_string();
            record_script_run(
                &self.store,
                &operation_pin,
                ScriptRunRecord {
                    sql: &payload.sql,
                    kind: QueryKind::Write,
                    action: "blocked",
                    status: "blocked",
                    row_count: None,
                    error: Some(reason.clone()),
                    origin: &history_origin,
                },
            )
            .await;
            return Err(DesktopScriptRunError::Scoped(DesktopScriptScopedFailure {
                error: AppError::Blocked { reason },
                _scope: operation_scope,
            }));
        }
        let has_ddl = kinds.iter().any(|kind| matches!(kind, QueryKind::Ddl));
        let script_kind = if has_ddl {
            QueryKind::Ddl
        } else if kinds
            .iter()
            .any(|kind| matches!(kind, QueryKind::Privilege))
        {
            QueryKind::Privilege
        } else {
            QueryKind::Write
        };
        if let Err(error) = audit::record(
            &self.store,
            RecordArgs {
                connection_id: operation_pin.connection_id,
                engine,
                agent_prompt: None,
                sql: payload.sql.clone(),
                kind: script_kind,
                action: "script:execute:attempt".into(),
                approved_by: Some(operation.record().actor.id.clone()),
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
                    &serde_json::json!({"reason": "audit_pre_record_failed"}),
                )
                .await;
            return Err(DesktopScriptRunError::Scoped(DesktopScriptScopedFailure {
                error: AppError::Config(format!(
                    "audit pre-record failed — refusing to run script: {error}"
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
                record_script_run(
                    &self.store,
                    &operation_pin,
                    ScriptRunRecord {
                        sql: &payload.sql,
                        kind: script_kind,
                        action: "script:execute",
                        status: "error",
                        row_count: None,
                        error: Some(error.to_string()),
                        origin: &history_origin,
                    },
                )
                .await;
                let _ = self
                    .operation
                    .fail(
                        operation_id,
                        &serde_json::json!({"reason": "connection_failed"}),
                    )
                    .await;
                return Err(DesktopScriptRunError::Application(error));
            }
        };
        let live = match lease.live().sql() {
            Ok(live) => live,
            Err(error) => {
                record_script_run(
                    &self.store,
                    &operation_pin,
                    ScriptRunRecord {
                        sql: &payload.sql,
                        kind: script_kind,
                        action: "script:execute",
                        status: "error",
                        row_count: None,
                        error: Some(error.to_string()),
                        origin: &history_origin,
                    },
                )
                .await;
                let _ = self
                    .operation
                    .fail(
                        operation_id,
                        &serde_json::json!({"reason": "sql_backend_unavailable"}),
                    )
                    .await;
                return Err(DesktopScriptRunError::Execution(Box::new(
                    DesktopScriptExecutionFailure {
                        error,
                        _lease: lease,
                    },
                )));
            }
        };
        let transaction = execute_script_transaction(
            &live.write_pool,
            &statements,
            operation.grant(),
            operation_id,
        );
        let (outcomes, committed) = match executor::cancel::guard(
            Some(operation_id),
            executor::cancel::QUERY_TIMEOUT,
            transaction,
        )
        .await
        {
            Ok(result) => result,
            Err(error) => {
                let interrupted = matches!(
                    &error,
                    AppError::Safety(reason)
                        if reason == "query cancelled"
                            || reason.starts_with("query timed out after ")
                );
                let error = if interrupted {
                    AppError::OutcomeUnknown(format!(
                        "script execution was interrupted before rollback or commit could be confirmed: {error}"
                    ))
                } else {
                    error
                };
                record_script_run(
                    &self.store,
                    &operation_pin,
                    ScriptRunRecord {
                        sql: &payload.sql,
                        kind: script_kind,
                        action: "script:execute",
                        status: "error",
                        row_count: None,
                        error: Some(error.to_string()),
                        origin: &history_origin,
                    },
                )
                .await;
                let _ = if matches!(&error, AppError::OutcomeUnknown(_)) {
                    self.operation
                        .mark_outcome_unknown(
                            operation_id,
                            &serde_json::json!({"reason": "target_outcome_unconfirmed"}),
                        )
                        .await
                } else {
                    self.operation
                        .fail(operation_id, &serde_json::json!({"reason": error.kind()}))
                        .await
                };
                return Err(DesktopScriptRunError::Execution(Box::new(
                    DesktopScriptExecutionFailure {
                        error,
                        _lease: lease,
                    },
                )));
            }
        };

        if !committed
            && matches!(engine, crate::model::Engine::Mysql)
            && kinds
                .iter()
                .any(|kind| matches!(kind, QueryKind::Ddl | QueryKind::Privilege))
        {
            let error = AppError::OutcomeUnknown(
                "MySQL may implicitly commit DDL or privilege statements before a later script statement fails"
                    .into(),
            );
            record_script_run(
                &self.store,
                &operation_pin,
                ScriptRunRecord {
                    sql: &payload.sql,
                    kind: script_kind,
                    action: "script:execute",
                    status: "outcome_unknown",
                    row_count: None,
                    error: Some(error.to_string()),
                    origin: &history_origin,
                },
            )
            .await;
            let _ = self
                .operation
                .mark_outcome_unknown(
                    operation_id,
                    &serde_json::json!({"reason": "mysql_implicit_commit_unconfirmed"}),
                )
                .await;
            return Err(DesktopScriptRunError::Execution(Box::new(
                DesktopScriptExecutionFailure {
                    error,
                    _lease: lease,
                },
            )));
        }

        if committed && has_ddl {
            let _ = self
                .store
                .clear_schema_cache(operation_pin.connection_id)
                .await;
        }
        let total = outcomes
            .iter()
            .filter_map(|statement| statement.affected)
            .sum();
        let first_error = outcomes
            .iter()
            .find_map(|statement| statement.error.clone());
        record_script_run(
            &self.store,
            &operation_pin,
            ScriptRunRecord {
                sql: &payload.sql,
                kind: script_kind,
                action: "script:execute",
                status: if committed { "ok" } else { "error" },
                row_count: Some(total),
                error: first_error,
                origin: &history_origin,
            },
        )
        .await;
        let lifecycle = if committed {
            self.operation
                .succeed(
                    operation_id,
                    &serde_json::json!({
                        "rowCount": total,
                        "statementCount": statements.len(),
                    }),
                )
                .await
        } else {
            self.operation
                .fail(
                    operation_id,
                    &serde_json::json!({"reason": "script_transaction_rolled_back"}),
                )
                .await
        };
        if let Err(error) = lifecycle {
            if committed {
                let _ = self
                    .operation
                    .mark_outcome_unknown(
                        operation_id,
                        &serde_json::json!({"reason": "local_receipt_failed"}),
                    )
                    .await;
            }
            return Err(DesktopScriptRunError::Execution(Box::new(
                DesktopScriptExecutionFailure {
                    error,
                    _lease: lease,
                },
            )));
        }
        Ok(DesktopScriptRunReceipt {
            outcome: ScriptOutcome {
                statements: outcomes,
                committed,
                all_reads: false,
            },
            _lease: lease,
        })
    }
}

fn statement_ok(sql: &str, affected: u64) -> ScriptStatement {
    ScriptStatement {
        sql: sql.to_string(),
        result: None,
        affected: Some(affected as i64),
        error: None,
    }
}

fn statement_error(sql: &str, message: String) -> ScriptStatement {
    ScriptStatement {
        sql: sql.to_string(),
        result: None,
        affected: None,
        error: Some(message),
    }
}

fn statement_skipped(sql: &str) -> ScriptStatement {
    statement_error(sql, "skipped — transaction rolled back".into())
}

fn script_has_write(kinds: &[QueryKind]) -> bool {
    kinds.iter().any(|kind| !matches!(kind, QueryKind::Read))
}

fn script_operation_risk(classifications: &[crate::model::Classification]) -> OperationRiskLevel {
    if classifications.iter().any(|classification| {
        classification.no_where && !matches!(classification.kind, QueryKind::Read)
    }) {
        return OperationRiskLevel::Critical;
    }
    classifications
        .iter()
        .fold(OperationRiskLevel::Low, |risk, classification| {
            match (risk, classification.risk) {
                (OperationRiskLevel::High, _) | (_, crate::model::RiskLevel::High) => {
                    OperationRiskLevel::High
                }
                (OperationRiskLevel::Medium, _) | (_, crate::model::RiskLevel::Medium) => {
                    OperationRiskLevel::Medium
                }
                _ => OperationRiskLevel::Low,
            }
        })
}

/// Execute every statement in one write-pool transaction. MySQL may implicitly
/// commit DDL, so mixed MySQL DDL scripts retain the existing best-effort caveat.
async fn execute_script_transaction(
    pool: &DbPool,
    statements: &[String],
    grant: &ExecutionGrant,
    operation_id: Uuid,
) -> AppResult<(Vec<ScriptStatement>, bool)> {
    if grant.operation_id() != operation_id {
        return Err(AppError::Blocked {
            reason: "script transaction scope does not match its approved operation".into(),
        });
    }
    let _exact_payload = (grant.payload_sha256(), grant.connection_id());
    macro_rules! run_transaction {
        ($pool:expr) => {{
            let mut outcomes = Vec::with_capacity(statements.len());
            match $pool.begin().await {
                Ok(mut transaction) => {
                    let mut succeeded = true;
                    for statement in statements {
                        match sqlx::query(AssertSqlSafe(statement.as_str()))
                            .execute(&mut *transaction)
                            .await
                        {
                            Ok(result) => {
                                outcomes.push(statement_ok(statement, result.rows_affected()))
                            }
                            Err(error) => {
                                outcomes.push(statement_error(statement, error.to_string()));
                                succeeded = false;
                                break;
                            }
                        }
                    }
                    if !succeeded {
                        if let Err(error) = transaction.rollback().await {
                            return Err(AppError::OutcomeUnknown(format!(
                                "script rollback acknowledgement failed: {error}"
                            )));
                        }
                        while outcomes.len() < statements.len() {
                            outcomes.push(statement_skipped(&statements[outcomes.len()]));
                        }
                        (outcomes, false)
                    } else if let Err(error) = transaction.commit().await {
                        return Err(AppError::OutcomeUnknown(format!(
                            "script commit acknowledgement failed: {error}"
                        )));
                    } else {
                        (outcomes, true)
                    }
                }
                Err(error) => (
                    statements
                        .iter()
                        .map(|statement| {
                            statement_error(
                                statement,
                                format!("could not begin transaction: {error}"),
                            )
                        })
                        .collect(),
                    false,
                ),
            }
        }};
    }
    Ok(match pool {
        DbPool::Postgres(pool) => run_transaction!(pool),
        DbPool::Mysql(pool) => run_transaction!(pool),
        DbPool::Sqlite(pool) => run_transaction!(pool),
    })
}

struct ScriptRunRecord<'a> {
    sql: &'a str,
    kind: QueryKind,
    action: &'a str,
    status: &'a str,
    row_count: Option<i64>,
    error: Option<String>,
    origin: &'a str,
}

async fn record_script_run(store: &Store, pin: &PinnedConnection, record: ScriptRunRecord<'_>) {
    if let Err(error) = audit::record(
        store,
        RecordArgs {
            connection_id: pin.connection_id,
            engine: pin.profile.engine,
            agent_prompt: None,
            sql: record.sql.to_string(),
            kind: record.kind,
            action: record.action.to_string(),
            approved_by: None,
            affected_estimate: record.row_count,
            error: record.error.clone(),
        },
    )
    .await
    {
        tracing::error!(
            connection_id = %pin.connection_id,
            action = record.action,
            %error,
            "script audit record failed"
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
                duration_ms: None,
                error: record.error,
                executed_at: Utc::now(),
                origin: record.origin.to_string(),
            },
        )
        .await
    {
        tracing::error!(
            connection_id = %pin.connection_id,
            %error,
            "script history insert failed"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;
    use std::str::FromStr;
    use std::time::Duration;

    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use tempfile::TempDir;

    use super::*;
    use crate::model::{
        ConnectionProfile, Engine, Provider, WorkspaceConnectionAccess, WorkspaceCredentialMode,
    };
    use crate::store::TEST_SCHEMA;

    struct ScriptHarness {
        directory: TempDir,
        store: Store,
        connections: ConnectionManager,
        service: ScriptService,
        operation_service: crate::services::OperationService,
        approval: crate::operations::LocalApprovalAuthority,
        connection_id: Uuid,
        profile: ConnectionProfile,
        target_path: std::path::PathBuf,
    }

    impl ScriptHarness {
        async fn new() -> Self {
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
            let directory = TempDir::new().unwrap();
            let target_path = directory.path().join("script-target.sqlite");
            initialize_target(&target_path).await;
            let connection_id = Uuid::new_v4();
            let profile = ConnectionProfile {
                id: connection_id,
                name: "script-test".into(),
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
            };
            store.upsert_connection(&profile).await.unwrap();
            let connections = ConnectionManager::new(store.clone());
            let (operation, approval) = OperationRuntime::new(&store);
            let operation_service = crate::services::OperationService::new(
                store.clone(),
                connections.clone(),
                operation.clone(),
            );
            let service = ScriptService::new(store.clone(), connections.clone(), operation);
            Self {
                directory,
                store,
                connections,
                service,
                operation_service,
                approval,
                connection_id,
                profile,
                target_path,
            }
        }

        async fn configure(&self, allow_writes: bool, auto_run_reads: bool) {
            let mut profile = self.profile.clone();
            profile.allow_writes = allow_writes;
            self.store.upsert_connection(&profile).await.unwrap();
            let mut settings = self.store.get_safety(self.connection_id).await.unwrap();
            settings.allow_writes = allow_writes;
            settings.auto_run_reads = auto_run_reads;
            self.store
                .set_safety(self.connection_id, &settings)
                .await
                .unwrap();
        }

        async fn user_names(&self) -> Vec<String> {
            let options = SqliteConnectOptions::new()
                .filename(&self.target_path)
                .read_only(true);
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(options)
                .await
                .unwrap();
            let names = sqlx::query_scalar("SELECT name FROM users ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
            pool.close().await;
            names
        }

        async fn audit_actions(&self) -> Vec<String> {
            let (mut entries, valid, first_bad) = audit::snapshot(&self.store, self.connection_id)
                .await
                .unwrap();
            assert!(valid);
            assert_eq!(first_bad, None);
            entries.reverse();
            entries.into_iter().map(|entry| entry.action).collect()
        }

        async fn propose(
            &self,
            sql: &str,
            origin: Option<&str>,
        ) -> Result<DesktopScriptProposalReceipt, DesktopScriptRunError> {
            self.service
                .propose_desktop(DesktopScriptProposalRequest {
                    connection_id: self.connection_id,
                    sql: sql.into(),
                    origin: origin.map(str::to_string),
                })
                .await
        }

        async fn approve(&self, proposal: &DesktopScriptProposalReceipt) {
            self.operation_service
                .approve_local(
                    &self.approval,
                    crate::services::OperationDecisionRequest {
                        operation_id: proposal.operation_id,
                        expected_payload_hash: proposal.payload_hash.clone(),
                        reason: None,
                    },
                )
                .await
                .unwrap();
        }

        async fn close(self) {
            let mutation = self
                .connections
                .begin_connection_mutation(self.connection_id, ConnectionAccess::Read)
                .await
                .unwrap();
            mutation.retire_connection(self.connection_id).await;
            let Self {
                directory,
                store,
                connections,
                service,
                operation_service,
                ..
            } = self;
            drop(service);
            drop(operation_service);
            drop(connections);
            store.pool().close().await;
            drop(store);
            directory
                .close()
                .expect("temporary script directory must be removable after pool shutdown");
        }
    }

    async fn initialize_target(path: &Path) {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
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

    #[test]
    fn write_path_only_when_a_statement_writes() {
        assert!(!script_has_write(&[QueryKind::Read, QueryKind::Read]));
        assert!(script_has_write(&[QueryKind::Read, QueryKind::Write]));
        assert!(script_has_write(&[QueryKind::Ddl]));
        assert!(script_has_write(&[QueryKind::Privilege]));
    }

    #[tokio::test]
    async fn read_script_preserves_wire_history_and_lease() {
        let harness = ScriptHarness::new().await;
        let proposal = harness
            .propose(
                "SELECT id FROM users ORDER BY id; SELECT name FROM users ORDER BY id",
                Some("sql"),
            )
            .await
            .unwrap();
        assert!(!proposal.approval_required);
        assert_eq!(proposal.state, OperationState::Ready);
        let receipt = harness
            .service
            .run_desktop(proposal.operation_id)
            .await
            .unwrap();
        assert!(receipt.outcome.all_reads);
        assert!(!receipt.outcome.committed);
        assert_eq!(receipt.outcome.statements.len(), 2);
        assert_eq!(
            serde_json::to_value(&receipt).unwrap(),
            serde_json::to_value(&receipt.outcome).unwrap(),
            "script receipt must preserve the literal legacy ScriptOutcome wire"
        );
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                harness.connections.begin_scope_mutation(),
            )
            .await
            .is_err(),
            "script receipt must retain authority through adapter serialization"
        );
        let history = harness
            .store
            .list_history(harness.connection_id)
            .await
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].origin, "sql");
        assert_eq!(history[0].status, "ok");
        assert_eq!(history[0].row_count, Some(4));
        assert_eq!(harness.audit_actions().await, ["script:execute"]);
        drop(receipt);
        let mutation = tokio::time::timeout(
            Duration::from_secs(5),
            harness.connections.begin_scope_mutation(),
        )
        .await
        .expect("scope mutation must proceed after script receipt drop");
        drop(mutation);
        harness.close().await;
    }

    #[tokio::test]
    async fn read_script_remains_a_plan_run_flow_when_auto_run_is_off() {
        let harness = ScriptHarness::new().await;
        harness.configure(false, false).await;
        let proposal = harness.propose("SELECT id FROM users", None).await.unwrap();
        assert!(!proposal.approval_required);
        let receipt = harness
            .service
            .run_desktop(proposal.operation_id)
            .await
            .unwrap();
        assert!(receipt.outcome.all_reads);
        drop(receipt);
        assert_eq!(harness.audit_actions().await, ["script:execute"]);
        let history = harness
            .store
            .list_history(harness.connection_id)
            .await
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].status, "ok");
        assert_eq!(history[0].origin, "manual");
        harness.close().await;
    }

    #[tokio::test]
    async fn write_script_gates_preserve_exact_errors_and_never_touch_target() {
        let harness = ScriptHarness::new().await;
        let writes_disabled = match harness
            .propose("UPDATE users SET name = 'Grace' WHERE id = 1", None)
            .await
        {
            Err(error) => error.into_error(),
            Ok(_) => panic!("writes-disabled script must be rejected"),
        };
        assert_eq!(
            serde_json::to_value(&writes_disabled).unwrap(),
            serde_json::json!({
                "kind": "blocked",
                "message": "blocked: writes are disabled for this connection"
            })
        );

        harness.configure(true, true).await;
        let proposal = harness
            .propose("UPDATE users SET name = 'Grace' WHERE id = 1", Some("sql"))
            .await
            .unwrap();
        assert!(proposal.approval_required);
        let approval_required = match harness.service.run_desktop(proposal.operation_id).await {
            Err(error) => error.into_error(),
            Ok(_) => panic!("unapproved write script must be rejected"),
        };
        assert!(matches!(approval_required, AppError::Blocked { .. }));
        assert_eq!(harness.user_names().await, ["Ada", "Linus"]);
        assert!(harness.audit_actions().await.is_empty());
        assert!(harness
            .store
            .list_history(harness.connection_id)
            .await
            .unwrap()
            .is_empty());
        harness.close().await;
    }

    #[tokio::test]
    async fn arbitrary_privilege_script_is_blocked_before_operation_persistence() {
        let harness = ScriptHarness::new().await;
        harness.configure(true, true).await;
        let error = match harness
            .propose("GRANT SELECT ON users TO analyst", None)
            .await
        {
            Err(error) => error.into_error(),
            Ok(_) => panic!("arbitrary privilege scripts must be blocked"),
        };
        assert!(matches!(
            error,
            AppError::Blocked { ref reason } if reason.contains("arbitrary privilege SQL")
        ));
        assert!(harness.audit_actions().await.is_empty());
        harness.close().await;
    }

    #[tokio::test]
    async fn write_script_is_atomic_and_closes_attempt_ledger() {
        let harness = ScriptHarness::new().await;
        harness.configure(true, true).await;
        let proposal = harness
            .propose(
                "UPDATE users SET name = 'Grace' WHERE id = 1;\
                 UPDATE users SET name = 'Ken' WHERE id = 2",
                Some("data-view"),
            )
            .await
            .unwrap();
        harness.approve(&proposal).await;
        let receipt = harness
            .service
            .run_desktop(proposal.operation_id)
            .await
            .unwrap();
        assert!(receipt.outcome.committed);
        assert!(!receipt.outcome.all_reads);
        assert_eq!(receipt.outcome.statements.len(), 2);
        drop(receipt);
        assert_eq!(harness.user_names().await, ["Grace", "Ken"]);
        assert_eq!(
            harness.audit_actions().await,
            ["script:execute:attempt", "script:execute"]
        );
        let history = harness
            .store
            .list_history(harness.connection_id)
            .await
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].origin, "data-view");
        assert_eq!(history[0].status, "ok");
        assert_eq!(history[0].row_count, Some(2));
        harness.close().await;
    }

    #[tokio::test]
    async fn script_commit_without_acknowledgement_is_not_reported_as_rolled_back() {
        let harness = ScriptHarness::new().await;
        harness.configure(true, true).await;
        let lease = harness
            .connections
            .acquire(harness.connection_id, ConnectionAccess::Write)
            .await
            .unwrap();
        let DbPool::Sqlite(pool) = &lease.live().sql().unwrap().write_pool else {
            panic!("script harness must use SQLite");
        };
        sqlx::raw_sql(
            "CREATE TABLE parents (id INTEGER PRIMARY KEY);
             CREATE TABLE deferred_children (
               id INTEGER PRIMARY KEY,
               parent_id INTEGER NOT NULL,
               FOREIGN KEY(parent_id) REFERENCES parents(id)
                 DEFERRABLE INITIALLY DEFERRED
             );",
        )
        .execute(pool)
        .await
        .unwrap();
        drop(lease);

        let proposal = harness
            .propose(
                "INSERT INTO deferred_children (id, parent_id) VALUES (1, 999)",
                None,
            )
            .await
            .unwrap();
        harness.approve(&proposal).await;
        let error = match harness.service.run_desktop(proposal.operation_id).await {
            Err(error) => error.into_error(),
            Ok(_) => panic!("deferred foreign-key commit must not report success"),
        };
        assert!(
            matches!(error, AppError::OutcomeUnknown(_)),
            "uncertain script commit must remain visible, got {error}"
        );
        assert_eq!(
            harness
                .service
                .operation
                .get(proposal.operation_id)
                .await
                .unwrap()
                .state,
            OperationState::OutcomeUnknown
        );
        harness.close().await;
    }

    #[tokio::test]
    async fn committed_ddl_script_invalidates_schema_cache() {
        let harness = ScriptHarness::new().await;
        harness.configure(true, true).await;
        harness
            .store
            .set_schema_cache(harness.connection_id, r#"{"tables":[]}"#)
            .await
            .unwrap();
        let proposal = harness
            .propose(
                "CREATE TABLE widgets (id INTEGER PRIMARY KEY);\
                 INSERT INTO widgets (id) VALUES (1)",
                None,
            )
            .await
            .unwrap();
        harness.approve(&proposal).await;
        let receipt = harness
            .service
            .run_desktop(proposal.operation_id)
            .await
            .unwrap();
        assert!(receipt.outcome.committed);
        drop(receipt);
        assert_eq!(
            harness
                .store
                .get_schema_cache(harness.connection_id)
                .await
                .unwrap(),
            None
        );
        let (audit, valid, first_bad) = audit::snapshot(&harness.store, harness.connection_id)
            .await
            .unwrap();
        assert!(valid);
        assert_eq!(first_bad, None);
        assert!(audit.iter().all(|entry| entry.kind == QueryKind::Ddl));
        harness.close().await;
    }

    #[tokio::test]
    async fn failed_write_script_rolls_back_and_returns_statement_outcomes() {
        let harness = ScriptHarness::new().await;
        harness.configure(true, true).await;
        let proposal = harness
            .propose(
                "UPDATE users SET name = 'Grace' WHERE id = 1;\
                 UPDATE missing_users SET name = 'Ken' WHERE id = 2;\
                 UPDATE users SET name = 'Dennis' WHERE id = 2",
                None,
            )
            .await
            .unwrap();
        harness.approve(&proposal).await;
        let receipt = harness
            .service
            .run_desktop(proposal.operation_id)
            .await
            .unwrap();
        assert!(!receipt.outcome.committed);
        assert_eq!(receipt.outcome.statements.len(), 3);
        assert!(receipt.outcome.statements[0].error.is_none());
        assert!(receipt.outcome.statements[1]
            .error
            .as_deref()
            .is_some_and(|message| message.contains("missing_users")));
        assert_eq!(
            receipt.outcome.statements[2].error.as_deref(),
            Some("skipped — transaction rolled back")
        );
        drop(receipt);
        assert_eq!(harness.user_names().await, ["Ada", "Linus"]);
        assert_eq!(
            harness.audit_actions().await,
            ["script:execute:attempt", "script:execute"]
        );
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
        harness.close().await;
    }

    #[tokio::test]
    async fn write_script_fails_closed_when_attempt_audit_is_unavailable() {
        let harness = ScriptHarness::new().await;
        harness.configure(true, true).await;
        sqlx::raw_sql(
            "CREATE TRIGGER fail_script_attempt
             BEFORE INSERT ON audit_log
             BEGIN
               SELECT RAISE(FAIL, 'forced script attempt audit failure');
             END;",
        )
        .execute(harness.store.pool())
        .await
        .unwrap();
        let proposal = harness
            .propose("UPDATE users SET name = 'Grace' WHERE id = 1", None)
            .await
            .unwrap();
        harness.approve(&proposal).await;
        let error = match harness.service.run_desktop(proposal.operation_id).await {
            Err(error) => error.into_error(),
            Ok(_) => {
                panic!("script must fail before target touch when attempt audit is unavailable")
            }
        };
        assert!(matches!(
            error,
            AppError::Config(message)
                if message.starts_with("audit pre-record failed — refusing to run script:")
                    && message.contains("forced script attempt audit failure")
        ));
        assert_eq!(harness.user_names().await, ["Ada", "Linus"]);
        assert!(harness.audit_actions().await.is_empty());
        assert!(harness
            .store
            .list_history(harness.connection_id)
            .await
            .unwrap()
            .is_empty());
        harness.close().await;
    }
}
