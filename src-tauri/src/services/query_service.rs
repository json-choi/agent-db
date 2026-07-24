//! Transport-neutral agent SQL planning and DB-enforced read-only execution.
//! Plans are immutable, short-lived, single-use capabilities shared by every clone
//! of one application-service composition root; transports only map DTOs and events.

use std::fmt;
use std::time::Duration;

#[cfg(test)]
use std::time::Instant;

use chrono::{Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::audit::{self, RecordArgs};
use crate::connection::{
    ConnectionAccess, ConnectionContext, ConnectionLease, ConnectionManager,
    ConnectionOperationScope, DbPool,
};
use crate::error::AppError;
use crate::executor;
use crate::model::{
    Classification, ConnectionProfile, Engine, ExecOutcome, HistoryEntry, PreviewMode,
    PreviewReport, QueryKind, QueryResult, SafetySettings,
};
use crate::monitoring::{self, HealthSnapshot};
use crate::operations::{
    canonical_hash, ClaimedOperation, NewOperation, OperationActorKind, OperationKind,
    OperationPlanDisposition, OperationRiskLevel, OperationRuntime, OperationState,
};
use crate::safety::{self, PoolRef};
use crate::store::{PinnedConnection, Store};

use super::operation_service::{
    actor_for_pin, agent_actor_for_pin, capture_policy, ensure_operation_scope,
    required_confirmation,
};

/// Lifetime of an agent query plan. A plan is valid at exactly this boundary and
/// expired only when its monotonic age is greater than this value.
pub(crate) const QUERY_PLAN_TTL: Duration = Duration::from_secs(30);

/// Hard agent result cap, independent of a connection's more permissive setting.
pub(crate) const MAX_AGENT_ROWS: u64 = 1000;

/// Desktop SQL-classification input after the Tauri adapter has decoded its wire
/// arguments. The service resolves the connection engine from one authority pin;
/// adapters never pass a separately fetched profile or engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DesktopSqlClassificationRequest {
    pub(crate) connection_id: Uuid,
    pub(crate) sql: String,
}

/// Desktop impact-preview input. Connection selection and permissions remain
/// service-owned so a transport cannot mix profiles fetched from different scopes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DesktopSqlPreviewRequest {
    pub(crate) connection_id: Uuid,
    pub(crate) sql: String,
}

/// Desktop SQL proposal input. SQL is accepted only at this planning boundary and
/// is persisted as an immutable payload before any execution capability exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DesktopSqlProposalRequest {
    pub(crate) connection_id: Uuid,
    pub(crate) sql: String,
    pub(crate) origin: Option<String>,
}

/// Exact immutable proposal rendered by the desktop before approval or execution.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopSqlProposalReceipt {
    pub(crate) operation_id: Uuid,
    pub(crate) payload_hash: String,
    pub(crate) state: OperationState,
    pub(crate) approval_required: bool,
    pub(crate) auto_run: bool,
    pub(crate) confirmation_phrase: Option<String>,
    pub(crate) expires_at: chrono::DateTime<Utc>,
    pub(crate) classification: Classification,
    pub(crate) preview: PreviewReport,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredDesktopSqlPayload {
    sql: String,
    history_origin: String,
}

/// Classification result retaining the active workspace/account scope through
/// adapter serialization. Its serialized form is exactly the legacy
/// [`Classification`] payload.
pub(crate) struct DesktopSqlClassificationReceipt {
    classification: Classification,
    _scope: ConnectionOperationScope,
}

impl serde::Serialize for DesktopSqlClassificationReceipt {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serde::Serialize::serialize(&self.classification, serializer)
    }
}

/// Authority retained by an impact preview. Pre-connection skipped reports keep
/// the operation scope itself; reports that touched the database keep the exact
/// connection lease used to produce them.
enum DesktopSqlPreviewAuthority {
    Scope { _scope: ConnectionOperationScope },
    Lease { _lease: Box<ConnectionLease> },
}

/// Preview result retaining its authority boundary through adapter serialization.
/// Its serialized form is exactly the legacy [`PreviewReport`] payload.
pub(crate) struct DesktopSqlPreviewReceipt {
    report: PreviewReport,
    pin: PinnedConnection,
    _authority: DesktopSqlPreviewAuthority,
}

impl serde::Serialize for DesktopSqlPreviewReceipt {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serde::Serialize::serialize(&self.report, serializer)
    }
}

/// Successful desktop execution retaining the exact connection lease until Tauri
/// has serialized the legacy [`ExecOutcome`] response.
pub(crate) struct DesktopSqlRunReceipt {
    outcome: ExecOutcome,
    _lease: ConnectionLease,
}

impl serde::Serialize for DesktopSqlRunReceipt {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serde::Serialize::serialize(&self.outcome, serializer)
    }
}

/// Desktop execution failures preserve the existing structured `AppError` wire
/// while retaining authority for blocked and post-connect failures until the thin
/// adapter maps the error.
#[derive(Debug)]
pub(crate) enum DesktopSqlRunError {
    Blocked(DesktopSqlRunBlocked),
    Application(AppError),
    Execution(Box<DesktopSqlExecutionFailure>),
}

impl DesktopSqlRunError {
    pub(crate) fn into_error(self) -> AppError {
        match self {
            Self::Blocked(blocked) => blocked.into_error(),
            Self::Application(error) => error,
            Self::Execution(failure) => failure.into_error(),
        }
    }
}

/// A policy rejection that holds the operation scope through adapter error
/// mapping, preventing a concurrent workspace switch from relabeling the result.
pub(crate) struct DesktopSqlRunBlocked {
    reason: String,
    _scope: ConnectionOperationScope,
}

impl fmt::Debug for DesktopSqlRunBlocked {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DesktopSqlRunBlocked")
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}

impl DesktopSqlRunBlocked {
    fn into_error(self) -> AppError {
        AppError::Blocked {
            reason: self.reason,
        }
    }
}

/// A post-connect failure retaining the live lease until adapter error mapping.
pub(crate) struct DesktopSqlExecutionFailure {
    error: AppError,
    _lease: ConnectionLease,
}

impl fmt::Debug for DesktopSqlExecutionFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DesktopSqlExecutionFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl DesktopSqlExecutionFailure {
    fn into_error(self) -> AppError {
        self.error
    }
}

/// Desktop classification/preview failures retain the structured `AppError`
/// contract currently returned by Tauri.
#[derive(Debug)]
pub(crate) enum DesktopSqlInspectionError {
    Application(AppError),
}

impl DesktopSqlInspectionError {
    pub(crate) fn into_error(self) -> AppError {
        match self {
            Self::Application(error) => error,
        }
    }
}

/// Trusted local adapter family that initiated a query capability. The value is
/// frozen into the plan so a later runner cannot relabel its audit provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentQueryInvocationOrigin {
    Mcp,
    // Constructed by the CLI adapter in the next Phase 1 slice.
    #[allow(dead_code)]
    Cli,
}

impl AgentQueryInvocationOrigin {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Mcp => "mcp",
            Self::Cli => "cli",
        }
    }

    fn plan_audit_action(self) -> &'static str {
        match self {
            Self::Mcp => "mcp:plan_query",
            Self::Cli => "cli:plan_query",
        }
    }

    fn run_audit_action(self) -> &'static str {
        match self {
            Self::Mcp => "mcp:run_query",
            Self::Cli => "cli:run_query",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredAgentReadPayload {
    sql: String,
    max_rows: u64,
    decision: String,
    origin: AgentQueryInvocationOrigin,
}

/// Inputs whose meaning is frozen into one plan. Connection selection has already
/// happened in the adapter so legacy selector behavior can remain transport-owned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentQueryPlanRequest {
    pub(crate) connection_id: Uuid,
    pub(crate) sql: String,
    pub(crate) max_rows: Option<u64>,
    pub(crate) origin: AgentQueryInvocationOrigin,
}

/// Aggregate-only planning result. The allowlist deliberately excludes the full
/// profile, network endpoint, username, credential reference, and binding details.
#[derive(Debug, Clone)]
pub(crate) struct AgentQueryPlan {
    pub(crate) connection_id: Uuid,
    pub(crate) connection_name: String,
    pub(crate) environment: Option<String>,
    pub(crate) sql: String,
    pub(crate) plan_id: Uuid,
    pub(crate) decision: String,
    pub(crate) notices: Vec<String>,
    pub(crate) suggestions: Vec<String>,
    pub(crate) estimated_rows: Option<i64>,
    pub(crate) health: HealthSnapshot,
}

/// Guard-bearing successful planning receipt. Keeping this value alive while the
/// adapter emits its result prevents rows/health from an old scope being published
/// into the UI after a concurrent workspace switch.
pub(crate) struct AgentQueryPlanReceipt {
    plan: AgentQueryPlan,
    _lease: ConnectionLease,
}

impl AgentQueryPlanReceipt {
    pub(crate) fn plan(&self) -> &AgentQueryPlan {
        &self.plan
    }
}

/// Planning failures with stable distinctions for an adapter's public error map.
#[derive(Debug)]
pub(crate) enum AgentQueryPlanError {
    /// SQL planning does not apply to a document-family connection.
    DocumentConnection,
    /// Classification did not yield exactly one read-only statement.
    NotSingleRead(Box<RejectedAgentQueryPlan>),
    /// Store, authorization, connection, preview, or monitoring setup failed.
    Application(AppError),
}

/// Guard-bearing non-read rejection. The adapter emits its compatibility result
/// first, then calls `audit_after_result`, preserving the Phase 0 event/audit order.
pub(crate) struct RejectedAgentQueryPlan {
    store: Store,
    context: ConnectionContext,
    connection_id: Uuid,
    connection_name: String,
    engine: Engine,
    sql: String,
    kind: QueryKind,
    origin: AgentQueryInvocationOrigin,
}

impl fmt::Debug for RejectedAgentQueryPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RejectedAgentQueryPlan")
            .finish_non_exhaustive()
    }
}

impl RejectedAgentQueryPlan {
    /// Return the id from the authority-pinned profile while this token holds the
    /// scope guard. Adapters must not reuse a pre-pin selector projection here.
    pub(crate) fn connection_id(&self) -> Uuid {
        self.connection_id
    }

    /// Return the allowlisted display name from the authority-pinned profile.
    pub(crate) fn connection_name(&self) -> &str {
        &self.connection_name
    }

    /// Record the rejection after the adapter has emitted `agent:result`. The
    /// retained connection context keeps the exact scope guard alive through both.
    pub(crate) async fn audit_after_result(self) {
        let Self {
            store,
            context,
            connection_id,
            connection_name: _,
            engine,
            sql,
            kind,
            origin,
        } = self;
        audit_best_effort(
            &store,
            connection_id,
            engine,
            &sql,
            kind,
            origin.plan_audit_action(),
            Some("plan_query accepts exactly one read-only SELECT statement".into()),
        )
        .await;
        drop(context);
    }
}

/// Explicitly allowlisted fields needed for the compatibility `agent:tool_call`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentQueryRunEventContext {
    pub(crate) connection_id: Uuid,
    pub(crate) connection_name: String,
    pub(crate) plan_id: Uuid,
    pub(crate) sql: String,
}

/// A successful DB-enforced read plus its required durable provenance handle.
#[derive(Debug, Clone)]
pub(crate) struct AgentQueryRun {
    pub(crate) connection_id: Uuid,
    pub(crate) connection_name: String,
    pub(crate) plan_id: Uuid,
    pub(crate) planning_decision: String,
    pub(crate) query_run_id: Uuid,
    pub(crate) sql: String,
    pub(crate) result: QueryResult,
}

/// Guard-bearing successful execution receipt. Transports borrow the allowlisted
/// result, construct and emit their payloads, and only then let this receipt drop.
pub(crate) struct AgentQueryRunReceipt {
    run: AgentQueryRun,
    _lease: ConnectionLease,
}

impl AgentQueryRunReceipt {
    pub(crate) fn run(&self) -> &AgentQueryRun {
        &self.run
    }
}

/// Failures before the adapter announces an execution tool call. A plan has already
/// been consumed for every variant, including expiry and authorization changes.
#[derive(Debug)]
pub(crate) enum AgentQueryRunPrepareError {
    UnknownOrAlreadyUsed,
    Expired,
    AuthorityChanged,
    StoredPlanInvalid,
    Application(AppError),
}

/// Failures after the adapter announces an execution tool call.
#[derive(Debug)]
pub(crate) enum AgentQueryRunError {
    /// The authority-bound target could not be connected.
    Connection(AppError),
    /// The DB-enforced read-only execution failed.
    Execution(AgentQueryExecutionFailure),
    /// The query succeeded, but the required durable query-run handle did not.
    ConsentHandlePersistence(AgentQueryConsentFailure),
}

/// An execution failure that retains the connection lease until the adapter emits
/// the corresponding error result.
pub(crate) struct AgentQueryExecutionFailure {
    error: AppError,
    _lease: ConnectionLease,
}

impl fmt::Debug for AgentQueryExecutionFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentQueryExecutionFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl AgentQueryExecutionFailure {
    pub(crate) fn error(&self) -> &AppError {
        &self.error
    }

    /// Consume after the adapter emitted its error while this token held the guard.
    pub(crate) fn into_error(self) -> AppError {
        self.error
    }
}

/// A successful database read whose required history receipt failed to persist.
/// The lease remains alive until the adapter emits the compatibility error.
pub(crate) struct AgentQueryConsentFailure {
    error: AppError,
    _lease: ConnectionLease,
}

impl fmt::Debug for AgentQueryConsentFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentQueryConsentFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl AgentQueryConsentFailure {
    pub(crate) fn error(&self) -> &AppError {
        &self.error
    }

    /// Consume after the adapter emitted its error while this token held the guard.
    pub(crate) fn into_error(self) -> AppError {
        self.error
    }
}

/// Opaque, scope-bound execution capability. The adapter may inspect only the safe
/// event context before consuming this value with [`PreparedAgentQueryRun::execute`].
pub(crate) struct PreparedAgentQueryRun {
    store: Store,
    context: ConnectionContext,
    operation_pin: PinnedConnection,
    operation: OperationRuntime,
    claimed: ClaimedOperation,
    event_context: AgentQueryRunEventContext,
    decision: String,
    max_rows: u64,
    origin: AgentQueryInvocationOrigin,
}

impl PreparedAgentQueryRun {
    /// Return only the existing compatibility event fields, never the connection
    /// profile or its credential material.
    pub(crate) fn event_context(&self) -> &AgentQueryRunEventContext {
        &self.event_context
    }

    /// Connect and run the capability-bound statement in the authoritative
    /// database read-only session. The 15-second server timeout and 17-second wall
    /// guard remain owned by `safety::run_read_only`.
    pub(crate) async fn execute(self) -> Result<AgentQueryRunReceipt, AgentQueryRunError> {
        let Self {
            store,
            context,
            operation_pin,
            operation,
            claimed,
            event_context,
            decision,
            max_rows,
            origin,
        } = self;
        let operation_id = claimed.record().id;
        let engine = operation_pin.profile.engine;

        let lease = match context.connect().await {
            Ok(lease) => lease,
            Err(error) => {
                record_run_failure(
                    &store,
                    &operation_pin,
                    &event_context.sql,
                    engine,
                    origin,
                    &error,
                )
                .await;
                let _ = operation
                    .fail(
                        operation_id,
                        &serde_json::json!({
                            "error": error.to_string(),
                            "reason": "target_connection_failed",
                        }),
                    )
                    .await;
                return Err(AgentQueryRunError::Connection(error));
            }
        };
        let live = match lease.live().sql() {
            Ok(live) => live,
            Err(error) => {
                record_run_failure(
                    &store,
                    &operation_pin,
                    &event_context.sql,
                    engine,
                    origin,
                    &error,
                )
                .await;
                let _ = operation
                    .fail(
                        operation_id,
                        &serde_json::json!({
                            "error": error.to_string(),
                            "reason": "target_pool_unavailable",
                        }),
                    )
                    .await;
                return Err(AgentQueryRunError::Execution(AgentQueryExecutionFailure {
                    error,
                    _lease: lease,
                }));
            }
        };
        let result =
            match safety::run_read_only(pool_ref(live.ro()), &event_context.sql, max_rows).await {
                Ok(result) => result,
                Err(error) => {
                    record_run_failure(
                        &store,
                        &operation_pin,
                        &event_context.sql,
                        engine,
                        origin,
                        &error,
                    )
                    .await;
                    let _ = operation
                        .fail(
                            operation_id,
                            &serde_json::json!({
                                "error": error.to_string(),
                                "reason": "read_execution_failed",
                            }),
                        )
                        .await;
                    return Err(AgentQueryRunError::Execution(AgentQueryExecutionFailure {
                        error,
                        _lease: lease,
                    }));
                }
            };

        audit_best_effort(
            &store,
            operation_pin.connection_id,
            engine,
            &event_context.sql,
            QueryKind::Read,
            origin.run_audit_action(),
            None,
        )
        .await;
        let query_run_id = match persist_history(
            &store,
            &operation_pin,
            &event_context.sql,
            "ok",
            Some(result.row_count as i64),
            Some(result.duration_ms as i64),
            None,
        )
        .await
        {
            Ok(query_run_id) => query_run_id,
            Err(error) => {
                let _ = operation
                    .fail(
                        operation_id,
                        &serde_json::json!({
                            "error": error.to_string(),
                            "reason": "history_receipt_failed",
                        }),
                    )
                    .await;
                return Err(AgentQueryRunError::ConsentHandlePersistence(
                    AgentQueryConsentFailure {
                        error,
                        _lease: lease,
                    },
                ));
            }
        };
        if let Err(error) = operation
            .succeed(
                operation_id,
                &serde_json::json!({
                    "durationMs": result.duration_ms,
                    "queryRunId": query_run_id,
                    "rowCount": result.row_count,
                }),
            )
            .await
        {
            let _ = operation
                .fail(
                    operation_id,
                    &serde_json::json!({
                        "error": error.to_string(),
                        "reason": "operation_receipt_failed",
                    }),
                )
                .await;
            return Err(AgentQueryRunError::ConsentHandlePersistence(
                AgentQueryConsentFailure {
                    error,
                    _lease: lease,
                },
            ));
        }

        Ok(AgentQueryRunReceipt {
            run: AgentQueryRun {
                connection_id: event_context.connection_id,
                connection_name: event_context.connection_name,
                plan_id: event_context.plan_id,
                planning_decision: decision,
                query_run_id,
                sql: event_context.sql,
                result,
            },
            _lease: lease,
        })
    }
}

/// Scope-aware query service. Every clone shares the durable Operation Runtime, so
/// desktop, HTTP, stdio, and future broker adapters see one single-use capability
/// namespace that also survives long enough for explicit restart recovery.
#[derive(Clone)]
pub(crate) struct QueryService {
    store: Store,
    connections: ConnectionManager,
    operation: OperationRuntime,
}

impl QueryService {
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

    /// Classify SQL against the engine from one scope-pinned connection. The
    /// returned receipt keeps that scope stable while the adapter serializes the
    /// legacy classification payload.
    pub(crate) async fn classify_desktop_sql(
        &self,
        request: DesktopSqlClassificationRequest,
    ) -> Result<DesktopSqlClassificationReceipt, DesktopSqlInspectionError> {
        let operation_scope = self.connections.begin_operation_scope().await;
        let pin = operation_scope
            .pin_connection_for_view(request.connection_id)
            .await
            .map_err(DesktopSqlInspectionError::Application)?;
        let classification = safety::classify(&request.sql, pin.profile.engine)
            .map_err(DesktopSqlInspectionError::Application)?;

        Ok(DesktopSqlClassificationReceipt {
            classification,
            _scope: operation_scope,
        })
    }

    /// Produce the desktop L3 impact preview from one authority snapshot.
    ///
    /// Pre-connection policy skips deliberately avoid opening a target pool.
    /// Database-backed previews consume the same operation scope that pinned the
    /// profile, closing the previous connection/profile re-acquisition window.
    pub(crate) async fn preview_desktop_sql(
        &self,
        request: DesktopSqlPreviewRequest,
    ) -> Result<DesktopSqlPreviewReceipt, DesktopSqlInspectionError> {
        let operation_scope = self.connections.begin_operation_scope().await;
        let pin = operation_scope
            .pin_connection_for_view(request.connection_id)
            .await
            .map_err(DesktopSqlInspectionError::Application)?;
        let settings = self
            .store
            .get_safety(pin.connection_id)
            .await
            .map_err(DesktopSqlInspectionError::Application)?;
        let classification = safety::classify(&request.sql, pin.profile.engine)
            .map_err(DesktopSqlInspectionError::Application)?;
        let is_non_read = !matches!(classification.kind, QueryKind::Read);

        if !is_non_read && !pin.profile.workspace_access.can_read() {
            return Err(DesktopSqlInspectionError::Application(AppError::Blocked {
                reason: "workspace role cannot execute this connection".into(),
            }));
        }
        if is_non_read && !pin.profile.workspace_access.can_write() {
            return Ok(DesktopSqlPreviewReceipt {
                report: skipped_preview_report(
                    "workspace role is read-only — write preview skipped",
                ),
                pin,
                _authority: DesktopSqlPreviewAuthority::Scope {
                    _scope: operation_scope,
                },
            });
        }
        if is_non_read && !settings.allow_writes {
            return Ok(DesktopSqlPreviewReceipt {
                report: skipped_preview_report(
                    "writes are disabled for this connection — impact preview skipped (no rows locked)",
                ),
                pin,
                _authority: DesktopSqlPreviewAuthority::Scope {
                    _scope: operation_scope,
                },
            });
        }
        if matches!(classification.kind, QueryKind::Ddl | QueryKind::Privilege) {
            return Ok(DesktopSqlPreviewReceipt {
                report: skipped_preview_report(
                    "DDL / privilege change — no row-count preview; review the statement directly.",
                ),
                pin,
                _authority: DesktopSqlPreviewAuthority::Scope {
                    _scope: operation_scope,
                },
            });
        }

        let access = desktop_preview_connection_access(&classification, &settings);
        let lease = operation_scope
            .connect(pin.clone(), access)
            .await
            .map_err(DesktopSqlInspectionError::Application)?;
        let live = lease
            .live()
            .sql()
            .map_err(DesktopSqlInspectionError::Application)?;

        let report = safety::preview(
            pool_ref(live.ro()),
            &request.sql,
            &classification,
            &settings,
        )
        .await
        .map_err(DesktopSqlInspectionError::Application)?;

        Ok(DesktopSqlPreviewReceipt {
            report,
            pin,
            _authority: DesktopSqlPreviewAuthority::Lease {
                _lease: Box::new(lease),
            },
        })
    }

    /// Persist one exact SQL proposal. Reads become single-use `ready` plans;
    /// every target-mutating statement becomes `pending_approval` even when the
    /// legacy connection setting disabled prompts.
    pub(crate) async fn propose_desktop_sql(
        &self,
        request: DesktopSqlProposalRequest,
    ) -> Result<DesktopSqlProposalReceipt, DesktopSqlInspectionError> {
        let preview_receipt = self
            .preview_desktop_sql(DesktopSqlPreviewRequest {
                connection_id: request.connection_id,
                sql: request.sql.clone(),
            })
            .await?;
        let pin = &preview_receipt.pin;
        let settings = self
            .store
            .get_safety(pin.connection_id)
            .await
            .map_err(DesktopSqlInspectionError::Application)?;
        let classification = safety::classify(&request.sql, pin.profile.engine)
            .map_err(DesktopSqlInspectionError::Application)?;
        let is_write = !matches!(classification.kind, QueryKind::Read);

        if is_write && !pin.profile.workspace_access.can_write() {
            return Err(DesktopSqlInspectionError::Application(AppError::Blocked {
                reason: "your workspace role grants read-only database access".into(),
            }));
        }
        if let safety::GateDecision::Block { reason } = safety::decide(&settings, &classification) {
            return Err(DesktopSqlInspectionError::Application(AppError::Blocked {
                reason,
            }));
        }

        let policy =
            capture_policy(pin, &settings).map_err(DesktopSqlInspectionError::Application)?;
        let history_origin = request.origin.unwrap_or_else(|| "manual".into());
        let payload = serde_json::to_value(StoredDesktopSqlPayload {
            sql: request.sql,
            history_origin: history_origin.clone(),
        })
        .map_err(AppError::from)
        .map_err(DesktopSqlInspectionError::Application)?;
        let operation_id = Uuid::new_v4();
        let expires_at = Utc::now()
            + if is_write {
                ChronoDuration::minutes(5)
            } else {
                ChronoDuration::from_std(QUERY_PLAN_TTL)
                    .expect("query plan TTL is representable by chrono")
            };
        let disposition = if is_write {
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
                    actor: actor_for_pin(pin, history_origin),
                    kind: operation_kind(classification.kind),
                    payload_schema_version: 1,
                    payload,
                    schema_fingerprint: None,
                    risk_level: operation_risk(&classification),
                    preview: serde_json::to_value(&preview_receipt.report)
                        .map_err(AppError::from)
                        .map_err(DesktopSqlInspectionError::Application)?,
                    policy_snapshot: policy.snapshot,
                    policy_revision: policy.revision,
                    single_use: true,
                    idempotency_key: operation_id.to_string(),
                    expires_at: Some(expires_at),
                },
                disposition,
            )
            .await
            .map_err(DesktopSqlInspectionError::Application)?;
        let confirmation_phrase = required_confirmation(&operation).map(str::to_owned);

        Ok(DesktopSqlProposalReceipt {
            operation_id: operation.id,
            payload_hash: operation.payload_hash,
            state: operation.state,
            approval_required: is_write,
            auto_run: !is_write && settings.auto_run_reads,
            confirmation_phrase,
            expires_at,
            classification,
            preview: preview_receipt.report.clone(),
        })
    }

    /// Execute one immutable SQL operation by id only. The SQL, connection, policy,
    /// and approval are reloaded from the durable record before an opaque grant is
    /// issued; no transport can resend or alter them at execution time.
    pub(crate) async fn run_desktop_sql(
        &self,
        operation_id: Uuid,
    ) -> Result<DesktopSqlRunReceipt, DesktopSqlRunError> {
        let planned = self
            .operation
            .get(operation_id)
            .await
            .map_err(DesktopSqlRunError::Application)?;
        if planned.payload_schema_version != 1
            || !matches!(
                planned.kind,
                OperationKind::ReadQuery
                    | OperationKind::WriteSql
                    | OperationKind::Ddl
                    | OperationKind::Privilege
            )
        {
            return Err(DesktopSqlRunError::Application(AppError::Blocked {
                reason: "operation is not a supported desktop SQL proposal".into(),
            }));
        }
        let payload: StoredDesktopSqlPayload = serde_json::from_value(planned.payload.clone())
            .map_err(AppError::from)
            .map_err(DesktopSqlRunError::Application)?;
        let operation_scope = self.connections.begin_operation_scope().await;
        let operation_pin = operation_scope
            .pin_connection(planned.connection_id)
            .await
            .map_err(DesktopSqlRunError::Application)?;
        ensure_operation_scope(&planned, &operation_pin)
            .map_err(DesktopSqlRunError::Application)?;
        let settings = self
            .store
            .get_safety(operation_pin.connection_id)
            .await
            .map_err(DesktopSqlRunError::Application)?;
        let policy =
            capture_policy(&operation_pin, &settings).map_err(DesktopSqlRunError::Application)?;
        if policy.revision != planned.policy_revision {
            return Err(DesktopSqlRunError::Blocked(DesktopSqlRunBlocked {
                reason: "the connection or safety policy changed; create a new proposal".into(),
                _scope: operation_scope,
            }));
        }
        let classification = safety::classify(&payload.sql, operation_pin.profile.engine)
            .map_err(DesktopSqlRunError::Application)?;
        if operation_kind(classification.kind) != planned.kind {
            return Err(DesktopSqlRunError::Blocked(DesktopSqlRunBlocked {
                reason: "stored SQL classification no longer matches its immutable proposal".into(),
                _scope: operation_scope,
            }));
        }
        let engine = operation_pin.profile.engine;
        let history_origin = payload.history_origin;
        let is_write = !matches!(classification.kind, QueryKind::Read);

        let access_allowed = if is_write {
            operation_pin.profile.workspace_access.can_write()
        } else {
            operation_pin.profile.workspace_access.can_read()
        };
        if !access_allowed {
            return Err(DesktopSqlRunError::Blocked(DesktopSqlRunBlocked {
                reason: "your workspace role no longer grants this database access".into(),
                _scope: operation_scope,
            }));
        }

        if let safety::GateDecision::Block { reason } = safety::decide(&settings, &classification) {
            record_desktop_run(
                &self.store,
                &operation_pin,
                DesktopRunRecord {
                    sql: &payload.sql,
                    kind: classification.kind,
                    action: "blocked",
                    status: "blocked",
                    row_count: None,
                    duration_ms: None,
                    error: Some(reason.clone()),
                    origin: &history_origin,
                },
            )
            .await;
            return Err(DesktopSqlRunError::Blocked(DesktopSqlRunBlocked {
                reason,
                _scope: operation_scope,
            }));
        }

        let claimed = self
            .operation
            .claim(operation_id)
            .await
            .map_err(DesktopSqlRunError::Application)?;

        if is_write {
            if let Err(error) = audit::record(
                &self.store,
                RecordArgs {
                    connection_id: operation_pin.connection_id,
                    engine,
                    agent_prompt: None,
                    sql: payload.sql.clone(),
                    kind: classification.kind,
                    action: "execute:attempt".into(),
                    approved_by: Some(planned.actor.id.clone()),
                    affected_estimate: None,
                    error: None,
                },
            )
            .await
            {
                let refusal = AppError::Config(format!(
                    "audit pre-record failed — refusing to run write: {error}"
                ));
                let _ = self
                    .operation
                    .fail(
                        operation_id,
                        &serde_json::json!({"reason": "audit_pre_record_failed"}),
                    )
                    .await;
                return Err(DesktopSqlRunError::Application(refusal));
            }
        }

        let lease = match operation_scope
            .connect(
                operation_pin.clone(),
                if is_write {
                    ConnectionAccess::Write
                } else {
                    ConnectionAccess::Read
                },
            )
            .await
        {
            Ok(lease) => lease,
            Err(error) => {
                record_desktop_run(
                    &self.store,
                    &operation_pin,
                    DesktopRunRecord {
                        sql: &payload.sql,
                        kind: classification.kind,
                        action: "error",
                        status: "error",
                        row_count: None,
                        duration_ms: None,
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
                return Err(DesktopSqlRunError::Application(error));
            }
        };
        let live = match lease.live().sql() {
            Ok(live) => live,
            Err(error) => {
                record_desktop_run(
                    &self.store,
                    &operation_pin,
                    DesktopRunRecord {
                        sql: &payload.sql,
                        kind: classification.kind,
                        action: "error",
                        status: "error",
                        row_count: None,
                        duration_ms: None,
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
                return Err(DesktopSqlRunError::Execution(Box::new(
                    DesktopSqlExecutionFailure {
                        error,
                        _lease: lease,
                    },
                )));
            }
        };

        match executor::execute(
            live,
            engine,
            &classification,
            &payload.sql,
            &settings,
            if is_write {
                Some(claimed.grant())
            } else {
                None
            },
            Some(operation_id),
        )
        .await
        {
            Ok(outcome) => {
                let row_count = outcome
                    .result
                    .as_ref()
                    .map(|result| result.row_count as i64)
                    .or_else(|| outcome.affected.map(|affected| affected as i64));
                let duration_ms = outcome
                    .result
                    .as_ref()
                    .map(|result| result.duration_ms as i64);
                if let Err(error) = self
                    .operation
                    .succeed(
                        operation_id,
                        &serde_json::json!({
                            "committed": outcome.committed,
                            "durationMs": duration_ms,
                            "rowCount": row_count,
                        }),
                    )
                    .await
                {
                    let _ = if is_write {
                        self.operation
                            .mark_outcome_unknown(
                                operation_id,
                                &serde_json::json!({"reason": "local_receipt_failed"}),
                            )
                            .await
                    } else {
                        self.operation
                            .fail(
                                operation_id,
                                &serde_json::json!({"reason": "local_receipt_failed"}),
                            )
                            .await
                    };
                    return Err(DesktopSqlRunError::Execution(Box::new(
                        DesktopSqlExecutionFailure {
                            error,
                            _lease: lease,
                        },
                    )));
                }
                if matches!(classification.kind, QueryKind::Ddl) && outcome.committed {
                    let _ = self
                        .store
                        .clear_schema_cache(operation_pin.connection_id)
                        .await;
                }
                record_desktop_run(
                    &self.store,
                    &operation_pin,
                    DesktopRunRecord {
                        sql: &payload.sql,
                        kind: classification.kind,
                        action: if outcome.committed { "execute" } else { "read" },
                        status: "ok",
                        row_count,
                        duration_ms,
                        error: None,
                        origin: &history_origin,
                    },
                )
                .await;
                Ok(DesktopSqlRunReceipt {
                    outcome,
                    _lease: lease,
                })
            }
            Err(error) => {
                let cancelled = matches!(
                    &error,
                    AppError::Safety(reason) if reason == "query cancelled"
                );
                let timed_out = matches!(
                    &error,
                    AppError::Safety(reason) if reason.starts_with("query timed out after ")
                );
                let error = if is_write
                    && (cancelled || timed_out || matches!(&error, AppError::OutcomeUnknown(_)))
                {
                    match error {
                        unknown @ AppError::OutcomeUnknown(_) => unknown,
                        other => AppError::OutcomeUnknown(format!(
                            "write execution was interrupted before rollback or commit could be confirmed: {other}"
                        )),
                    }
                } else {
                    error
                };
                let _ = if matches!(&error, AppError::OutcomeUnknown(_)) {
                    self.operation
                        .mark_outcome_unknown(
                            operation_id,
                            &serde_json::json!({"reason": "target_outcome_unconfirmed"}),
                        )
                        .await
                } else if cancelled {
                    self.operation
                        .confirm_cancelled(
                            operation_id,
                            &serde_json::json!({"reason": "user_cancelled"}),
                        )
                        .await
                } else {
                    self.operation
                        .fail(operation_id, &serde_json::json!({"reason": error.kind()}))
                        .await
                };
                record_desktop_run(
                    &self.store,
                    &operation_pin,
                    DesktopRunRecord {
                        sql: &payload.sql,
                        kind: classification.kind,
                        action: "error",
                        status: "error",
                        row_count: None,
                        duration_ms: None,
                        error: Some(error.to_string()),
                        origin: &history_origin,
                    },
                )
                .await;
                Err(DesktopSqlRunError::Execution(Box::new(
                    DesktopSqlExecutionFailure {
                        error,
                        _lease: lease,
                    },
                )))
            }
        }
    }

    /// Classify, preview, and inspect aggregate health before minting one immutable
    /// single-use plan. A `caution` decision is guidance and never an execution block.
    pub(crate) async fn plan_agent_read(
        &self,
        request: AgentQueryPlanRequest,
    ) -> Result<AgentQueryPlanReceipt, AgentQueryPlanError> {
        let context = self
            .connections
            .pin(request.connection_id, ConnectionAccess::Read)
            .await
            .map_err(AgentQueryPlanError::Application)?;
        let profile = context.pin().profile.clone();
        if profile.engine.is_document() {
            return Err(AgentQueryPlanError::DocumentConnection);
        }

        let classification = safety::classify(&request.sql, profile.engine)
            .map_err(AgentQueryPlanError::Application)?;
        if !matches!(classification.kind, QueryKind::Read) || classification.statement_count != 1 {
            return Err(AgentQueryPlanError::NotSingleRead(Box::new(
                RejectedAgentQueryPlan {
                    store: self.store.clone(),
                    context,
                    connection_id: profile.id,
                    connection_name: profile.name,
                    engine: profile.engine,
                    sql: request.sql,
                    kind: classification.kind,
                    origin: request.origin,
                },
            )));
        }

        let settings = self
            .store
            .get_safety(profile.id)
            .await
            .map_err(AgentQueryPlanError::Application)?;
        let max_rows = bounded_max_rows(request.max_rows, settings.max_rows);
        let operation_pin = context.pin().clone();
        let policy =
            capture_agent_read_policy(&operation_pin).map_err(AgentQueryPlanError::Application)?;
        let lease = context
            .connect()
            .await
            .map_err(AgentQueryPlanError::Application)?;
        let live = lease
            .live()
            .sql()
            .map_err(AgentQueryPlanError::Application)?;
        let preview = safety::preview(
            pool_ref(live.ro()),
            &request.sql,
            &classification,
            &settings,
        )
        .await
        .map_err(AgentQueryPlanError::Application)?;
        let health = monitoring::snapshot(live, profile.engine).await;
        let (decision, notices, suggestions) = planning_guidance(
            &profile,
            &health,
            preview.estimated_rows,
            settings.exec_preview_row_limit,
        );
        let plan_id = Uuid::new_v4();
        audit_best_effort(
            &self.store,
            profile.id,
            profile.engine,
            &request.sql,
            QueryKind::Read,
            request.origin.plan_audit_action(),
            None,
        )
        .await;
        let payload = serde_json::to_value(StoredAgentReadPayload {
            sql: request.sql.clone(),
            max_rows,
            decision: decision.clone(),
            origin: request.origin,
        })
        .map_err(AppError::from)
        .map_err(AgentQueryPlanError::Application)?;
        let expires_at = Utc::now()
            + ChronoDuration::from_std(QUERY_PLAN_TTL)
                .expect("agent query plan TTL is representable by chrono");
        self.operation
            .plan(
                NewOperation {
                    id: plan_id,
                    workspace_id: operation_pin.scope.workspace_id,
                    account_scope: operation_pin.scope.account_scope.storage_key().into(),
                    connection_id: operation_pin.connection_id,
                    connection_revision: operation_pin.connection_revision,
                    terminal_session_id: None,
                    actor: agent_actor_for_pin(
                        &operation_pin,
                        request.origin.as_str().into(),
                        request.origin.as_str().into(),
                    ),
                    kind: OperationKind::ReadQuery,
                    payload_schema_version: 1,
                    payload,
                    schema_fingerprint: None,
                    risk_level: operation_risk(&classification),
                    preview: serde_json::json!({
                        "decision": decision,
                        "estimatedRows": preview.estimated_rows,
                        "health": health,
                        "notices": notices,
                        "suggestions": suggestions,
                    }),
                    policy_snapshot: policy.0,
                    policy_revision: policy.1,
                    single_use: true,
                    idempotency_key: plan_id.to_string(),
                    expires_at: Some(expires_at),
                },
                OperationPlanDisposition::Ready,
            )
            .await
            .map_err(AgentQueryPlanError::Application)?;

        Ok(AgentQueryPlanReceipt {
            plan: AgentQueryPlan {
                connection_id: profile.id,
                connection_name: profile.name,
                environment: profile.env,
                sql: request.sql,
                plan_id,
                decision,
                notices,
                suggestions,
                estimated_rows: preview.estimated_rows,
                health,
            },
            _lease: lease,
        })
    }

    /// Atomically claim a durable plan, then re-pin and compare every authority and
    /// policy field. The returned capability retains both the claim and connection
    /// scope through the adapter event and the eventual read-only execution.
    pub(crate) async fn prepare_agent_run(
        &self,
        plan_id: Uuid,
    ) -> Result<PreparedAgentQueryRun, AgentQueryRunPrepareError> {
        let planned = match self.operation.get(plan_id).await {
            Ok(planned) => planned,
            Err(AppError::NotFound(_)) => {
                return Err(AgentQueryRunPrepareError::UnknownOrAlreadyUsed);
            }
            Err(error) => return Err(AgentQueryRunPrepareError::Application(error)),
        };
        if planned.payload_schema_version != 1
            || planned.kind != OperationKind::ReadQuery
            || planned.actor.kind != OperationActorKind::Agent
        {
            return Err(AgentQueryRunPrepareError::StoredPlanInvalid);
        }
        let payload: StoredAgentReadPayload = serde_json::from_value(planned.payload.clone())
            .map_err(|_| AgentQueryRunPrepareError::StoredPlanInvalid)?;
        if planned.actor.id != payload.origin.as_str() {
            return Err(AgentQueryRunPrepareError::StoredPlanInvalid);
        }
        if planned.state == OperationState::Expired {
            return Err(AgentQueryRunPrepareError::UnknownOrAlreadyUsed);
        }
        let expired = planned
            .expires_at
            .is_some_and(|expires_at| expires_at <= Utc::now());
        let claimed = match self.operation.claim(plan_id).await {
            Ok(claimed) => claimed,
            Err(_) if expired => {
                return Err(AgentQueryRunPrepareError::Expired);
            }
            Err(_) if planned.state != OperationState::Ready => {
                return Err(AgentQueryRunPrepareError::UnknownOrAlreadyUsed);
            }
            Err(error) => return Err(AgentQueryRunPrepareError::Application(error)),
        };

        let context = match self
            .connections
            .pin(planned.connection_id, ConnectionAccess::Read)
            .await
        {
            Ok(context) => context,
            Err(error) => {
                let _ = self
                    .operation
                    .fail(
                        plan_id,
                        &serde_json::json!({
                            "error": error.to_string(),
                            "reason": "connection_scope_unavailable",
                        }),
                    )
                    .await;
                return Err(AgentQueryRunPrepareError::Application(error));
            }
        };
        let pin = context.pin().clone();
        if ensure_operation_scope(&planned, &pin).is_err() {
            let _ = self
                .operation
                .fail(
                    plan_id,
                    &serde_json::json!({"reason": "operation_authority_changed"}),
                )
                .await;
            return Err(AgentQueryRunPrepareError::AuthorityChanged);
        }
        let settings = match self.store.get_safety(pin.connection_id).await {
            Ok(settings) => settings,
            Err(error) => {
                let _ = self
                    .operation
                    .fail(
                        plan_id,
                        &serde_json::json!({
                            "error": error.to_string(),
                            "reason": "safety_policy_unavailable",
                        }),
                    )
                    .await;
                return Err(AgentQueryRunPrepareError::Application(error));
            }
        };
        let policy = match capture_agent_read_policy(&pin) {
            Ok(policy) => policy,
            Err(error) => {
                let _ = self
                    .operation
                    .fail(
                        plan_id,
                        &serde_json::json!({
                            "error": error.to_string(),
                            "reason": "policy_snapshot_failed",
                        }),
                    )
                    .await;
                return Err(AgentQueryRunPrepareError::Application(error));
            }
        };
        if policy.1 != planned.policy_revision {
            let _ = self
                .operation
                .fail(
                    plan_id,
                    &serde_json::json!({"reason": "operation_policy_changed"}),
                )
                .await;
            return Err(AgentQueryRunPrepareError::AuthorityChanged);
        }
        let classification = match safety::classify(&payload.sql, pin.profile.engine) {
            Ok(classification) => classification,
            Err(error) => {
                let _ = self
                    .operation
                    .fail(
                        plan_id,
                        &serde_json::json!({
                            "error": error.to_string(),
                            "reason": "stored_plan_reclassification_failed",
                        }),
                    )
                    .await;
                return Err(AgentQueryRunPrepareError::Application(error));
            }
        };
        if !matches!(classification.kind, QueryKind::Read) || classification.statement_count != 1 {
            let _ = self
                .operation
                .fail(
                    plan_id,
                    &serde_json::json!({"reason": "stored_plan_classification_changed"}),
                )
                .await;
            return Err(AgentQueryRunPrepareError::StoredPlanInvalid);
        }

        let event_context = AgentQueryRunEventContext {
            connection_id: pin.connection_id,
            connection_name: pin.profile.name.clone(),
            plan_id,
            sql: payload.sql,
        };
        Ok(PreparedAgentQueryRun {
            store: self.store.clone(),
            context,
            operation_pin: pin,
            operation: self.operation.clone(),
            claimed,
            event_context,
            decision: payload.decision,
            max_rows: payload.max_rows.min(settings.max_rows).min(MAX_AGENT_ROWS),
            origin: payload.origin,
        })
    }

    /// Seed a durable plan only in crate tests so compatibility fixtures can cover
    /// deterministic expiry without exposing a production mutation API.
    #[cfg(test)]
    pub(crate) async fn seed_plan_for_test(
        &self,
        plan_id: Uuid,
        pin: &PinnedConnection,
        sql: String,
        max_rows: u64,
        decision: String,
        created_at: Instant,
    ) {
        let policy = capture_agent_read_policy(pin).unwrap();
        let elapsed = Instant::now().saturating_duration_since(created_at);
        let remaining = QUERY_PLAN_TTL.saturating_sub(elapsed);
        let expires_at = Utc::now()
            + ChronoDuration::from_std(remaining)
                .expect("seeded query plan TTL is representable by chrono");
        let origin = AgentQueryInvocationOrigin::Mcp;
        self.operation
            .plan(
                NewOperation {
                    id: plan_id,
                    workspace_id: pin.scope.workspace_id,
                    account_scope: pin.scope.account_scope.storage_key().into(),
                    connection_id: pin.connection_id,
                    connection_revision: pin.connection_revision,
                    terminal_session_id: None,
                    actor: agent_actor_for_pin(pin, origin.as_str().into(), origin.as_str().into()),
                    kind: OperationKind::ReadQuery,
                    payload_schema_version: 1,
                    payload: serde_json::to_value(StoredAgentReadPayload {
                        sql,
                        max_rows: max_rows.min(MAX_AGENT_ROWS),
                        decision,
                        origin,
                    })
                    .unwrap(),
                    schema_fingerprint: None,
                    risk_level: OperationRiskLevel::Low,
                    preview: serde_json::json!({"seededForTest": true}),
                    policy_snapshot: policy.0,
                    policy_revision: policy.1,
                    single_use: true,
                    idempotency_key: plan_id.to_string(),
                    expires_at: Some(expires_at),
                },
                OperationPlanDisposition::Ready,
            )
            .await
            .unwrap();
    }
}

fn bounded_max_rows(requested: Option<u64>, configured: u64) -> u64 {
    requested.unwrap_or(configured).min(MAX_AGENT_ROWS)
}

/// Agent read plans freeze their own row cap, so later UI tuning can only narrow
/// execution and must not invalidate an otherwise identical plan. Authority,
/// credential, connection, binding, and workspace switches remain hash-bound.
fn capture_agent_read_policy(
    pin: &PinnedConnection,
) -> Result<(serde_json::Value, String), AppError> {
    let snapshot = serde_json::json!({
        "accountScope": pin.scope.account_scope.storage_key(),
        "bindingRevision": pin.binding_revision,
        "bindingUpdatedAt": pin.binding_updated_at,
        "connectionRevision": pin.connection_revision,
        "credentialMode": pin.profile.credential_mode,
        "environment": pin.profile.env,
        "scopeGeneration": pin.scope.generation,
        "workspaceAccess": pin.profile.workspace_access,
        "workspaceId": pin.scope.workspace_id,
    });
    let revision = canonical_hash(&snapshot)?;
    Ok((snapshot, revision))
}

fn operation_kind(kind: QueryKind) -> OperationKind {
    match kind {
        QueryKind::Read => OperationKind::ReadQuery,
        QueryKind::Write => OperationKind::WriteSql,
        QueryKind::Ddl => OperationKind::Ddl,
        QueryKind::Privilege => OperationKind::Privilege,
    }
}

fn operation_risk(classification: &Classification) -> OperationRiskLevel {
    if classification.no_where && !matches!(classification.kind, QueryKind::Read) {
        return OperationRiskLevel::Critical;
    }
    match classification.risk {
        crate::model::RiskLevel::Low => OperationRiskLevel::Low,
        crate::model::RiskLevel::Medium => OperationRiskLevel::Medium,
        crate::model::RiskLevel::High => OperationRiskLevel::High,
    }
}

fn desktop_preview_connection_access(
    _classification: &Classification,
    _settings: &SafetySettings,
) -> ConnectionAccess {
    ConnectionAccess::Read
}

fn skipped_preview_report(note: &str) -> PreviewReport {
    PreviewReport {
        mode: PreviewMode::Skipped,
        estimated_rows: None,
        exact_rows: None,
        plan: None,
        note: Some(note.into()),
    }
}

struct DesktopRunRecord<'a> {
    sql: &'a str,
    kind: QueryKind,
    action: &'a str,
    status: &'a str,
    row_count: Option<i64>,
    duration_ms: Option<i64>,
    error: Option<String>,
    origin: &'a str,
}

/// Append the established desktop audit and history pair. Logging remains
/// best-effort so provenance outages do not mask the target operation result.
async fn record_desktop_run(store: &Store, pin: &PinnedConnection, record: DesktopRunRecord<'_>) {
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
            "desktop SQL audit record failed"
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
                origin: record.origin.to_string(),
            },
        )
        .await
    {
        tracing::error!(
            connection_id = %pin.connection_id,
            %error,
            "desktop SQL history insert failed"
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

pub(crate) fn planning_guidance(
    profile: &ConnectionProfile,
    health: &HealthSnapshot,
    estimated_rows: Option<i64>,
    estimate_limit: i64,
) -> (String, Vec<String>, Vec<String>) {
    let mut caution = false;
    let mut notices = Vec::new();
    let mut suggestions = Vec::new();

    if profile.env.as_deref() == Some("prod") {
        caution = true;
        notices.push("This connection is labeled production.".into());
        suggestions
            .push("Prefer a read replica or a bounded time range for production analysis.".into());
    }
    if health.level != "normal" {
        caution = true;
    }
    notices.extend(health.reasons.clone());
    if health.coverage == "limited" {
        caution = true;
        suggestions.push(
            "Enable PostgreSQL pg_monitor in DopeDB settings for fuller aggregate load checks."
                .into(),
        );
    }
    match estimated_rows {
        Some(rows) if rows > estimate_limit.max(0) => {
            caution = true;
            notices.push(format!(
                "EXPLAIN estimates {rows} result/plan rows, above the configured {estimate_limit} review threshold."
            ));
            suggestions.push(
                "Add a selective time/filter condition or aggregate before joining large log tables."
                    .into(),
            );
        }
        None if profile.env.as_deref() == Some("prod") => {
            caution = true;
            notices.push(
                "EXPLAIN did not provide a usable row estimate for this production query.".into(),
            );
            suggestions.push("Review the plan and narrow the query before execution.".into());
        }
        _ => {}
    }
    if health.level == "busy" {
        suggestions.push("Wait for database pressure to fall before running this query.".into());
    }
    suggestions.sort();
    suggestions.dedup();
    (
        if caution { "caution" } else { "ready" }.into(),
        notices,
        suggestions,
    )
}

async fn audit_best_effort(
    store: &Store,
    connection_id: Uuid,
    engine: Engine,
    sql: &str,
    kind: QueryKind,
    action: &str,
    error: Option<String>,
) {
    if let Err(audit_error) = audit::record(
        store,
        RecordArgs {
            connection_id,
            engine,
            agent_prompt: None,
            sql: sql.to_string(),
            kind,
            action: action.to_string(),
            approved_by: None,
            affected_estimate: None,
            error,
        },
    )
    .await
    {
        tracing::error!("query audit insert failed: {audit_error}");
    }
}

async fn persist_history(
    store: &Store,
    pin: &PinnedConnection,
    sql: &str,
    status: &str,
    rows: Option<i64>,
    duration_ms: Option<i64>,
    error: Option<String>,
) -> Result<Uuid, AppError> {
    let id = Uuid::new_v4();
    store
        .insert_history_if_current(
            pin,
            &HistoryEntry {
                id,
                connection_id: pin.connection_id,
                sql: sql.to_string(),
                kind: QueryKind::Read,
                status: status.to_string(),
                row_count: rows,
                duration_ms,
                error,
                executed_at: Utc::now(),
                // Both current local agent adapters remain dashboard-eligible.
                // Finer actor attribution arrives with Operation Runtime.
                origin: "agent".into(),
            },
        )
        .await?;
    Ok(id)
}

async fn record_run_failure(
    store: &Store,
    pin: &PinnedConnection,
    sql: &str,
    engine: Engine,
    origin: AgentQueryInvocationOrigin,
    error: &AppError,
) {
    let message = error.to_string();
    audit_best_effort(
        store,
        pin.connection_id,
        engine,
        sql,
        QueryKind::Read,
        origin.run_audit_action(),
        Some(message.clone()),
    )
    .await;
    if let Err(history_error) =
        persist_history(store, pin, sql, "error", None, None, Some(message)).await
    {
        tracing::error!("query failure history insert failed: {history_error}");
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::str::FromStr;

    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use tempfile::TempDir;

    use super::*;
    use crate::model::{Provider, RiskLevel, WorkspaceConnectionAccess, WorkspaceCredentialMode};
    use crate::store::TEST_SCHEMA;

    fn profile(id: Uuid, database: String) -> ConnectionProfile {
        ConnectionProfile {
            id,
            name: "query-service-test".into(),
            engine: Engine::Sqlite,
            provider: Provider::Generic,
            driver_id: Some("sqlx-sqlite".into()),
            host: String::new(),
            port: 0,
            database,
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
        }
    }

    fn classification(kind: QueryKind, rollback_safe: bool) -> Classification {
        Classification {
            kind,
            risk: RiskLevel::Low,
            statement_count: 1,
            no_where: false,
            tables: vec![],
            notes: vec![],
            rollback_safe,
        }
    }

    #[test]
    fn desktop_preview_access_is_always_read_only_before_exact_approval() {
        let settings = SafetySettings {
            allow_writes: true,
            explain_preview: true,
            ..SafetySettings::default()
        };

        for classification in [
            classification(QueryKind::Read, false),
            classification(QueryKind::Write, true),
            classification(QueryKind::Write, false),
            classification(QueryKind::Ddl, false),
            classification(QueryKind::Privilege, false),
        ] {
            assert_eq!(
                desktop_preview_connection_access(&classification, &settings),
                ConnectionAccess::Read
            );
        }
    }

    #[test]
    fn desktop_static_preview_reports_keep_exact_legacy_messages() {
        let workspace =
            skipped_preview_report("workspace role is read-only — write preview skipped");
        assert_eq!(workspace.mode, PreviewMode::Skipped);
        assert_eq!(workspace.estimated_rows, None);
        assert_eq!(workspace.exact_rows, None);
        assert_eq!(workspace.plan, None);
        assert_eq!(
            workspace.note.as_deref(),
            Some("workspace role is read-only — write preview skipped")
        );

        let disabled = skipped_preview_report(
            "writes are disabled for this connection — impact preview skipped (no rows locked)",
        );
        assert_eq!(
            disabled.note.as_deref(),
            Some(
                "writes are disabled for this connection — impact preview skipped (no rows locked)"
            )
        );
    }

    #[test]
    fn row_cap_is_frozen_in_the_stored_plan() {
        assert_eq!(bounded_max_rows(Some(5_000), 25), MAX_AGENT_ROWS);
        assert_eq!(bounded_max_rows(None, 5_000), MAX_AGENT_ROWS);
        assert_eq!(bounded_max_rows(Some(7), 500), 7);
    }

    #[test]
    fn production_and_limited_monitoring_are_guidance_not_blocks() {
        let mut production = profile(Uuid::new_v4(), "test.db".into());
        production.env = Some("prod".into());
        let health = HealthSnapshot {
            level: "normal".into(),
            coverage: "limited".into(),
            total_connections: Some(1),
            max_connections: Some(100),
            connection_usage_percent: Some(1.0),
            active_queries: Some(1),
            long_running_queries: Some(0),
            lock_waits: Some(0),
            replication_lag_seconds: None,
            reasons: vec!["Monitoring coverage is limited without pg_monitor.".into()],
            captured_at: Utc::now(),
        };
        let (decision, notices, suggestions) =
            planning_guidance(&production, &health, Some(100_001), 50_000);

        assert_eq!(decision, "caution");
        assert!(notices
            .iter()
            .any(|notice| notice == "This connection is labeled production."));
        assert!(notices
            .iter()
            .any(|notice| notice.contains("EXPLAIN estimates 100001")));
        assert!(suggestions
            .iter()
            .any(|suggestion| suggestion.contains("pg_monitor")));
        assert!(suggestions.windows(2).all(|pair| pair[0] <= pair[1]));
    }

    struct SqliteHarness {
        service: QueryService,
        operation_service: crate::services::OperationService,
        approval: crate::operations::LocalApprovalAuthority,
        store: Store,
        connections: ConnectionManager,
        connection_id: Uuid,
        profile: ConnectionProfile,
        directory: TempDir,
    }

    impl SqliteHarness {
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

            let directory = tempfile::tempdir().unwrap();
            let target_path = directory.path().join("query-service-target.db");
            let target_options = SqliteConnectOptions::new()
                .filename(&target_path)
                .create_if_missing(true)
                .foreign_keys(true);
            let target_pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(target_options)
                .await
                .unwrap();
            sqlx::raw_sql(
                "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
                 INSERT INTO users (id, name) VALUES (1, 'Ada'), (2, 'Linus');",
            )
            .execute(&target_pool)
            .await
            .unwrap();
            target_pool.close().await;

            let connection_id = Uuid::new_v4();
            let profile = profile(connection_id, target_path.to_string_lossy().into_owned());
            store.upsert_connection(&profile).await.unwrap();
            let connections = ConnectionManager::new(store.clone());
            let (operation, approval) = OperationRuntime::new(&store);
            let operation_service = crate::services::OperationService::new(
                store.clone(),
                connections.clone(),
                operation.clone(),
            );
            let service = QueryService::new(store.clone(), connections.clone(), operation);
            Self {
                service,
                operation_service,
                approval,
                store,
                connections,
                connection_id,
                profile,
                directory,
            }
        }

        async fn close(self) {
            let mutation = self
                .connections
                .begin_connection_mutation(self.connection_id, ConnectionAccess::Read)
                .await
                .unwrap();
            mutation.retire_connection(self.connection_id).await;
            let Self {
                service,
                operation_service,
                store,
                connections,
                directory,
                ..
            } = self;
            drop(service);
            drop(operation_service);
            drop(connections);
            store.pool().close().await;
            drop(store);
            directory
                .close()
                .expect("temporary SQLite directory must be removable after pool shutdown");
        }

        async fn propose(
            &self,
            sql: &str,
            origin: Option<&str>,
        ) -> Result<DesktopSqlProposalReceipt, DesktopSqlInspectionError> {
            self.service
                .propose_desktop_sql(DesktopSqlProposalRequest {
                    connection_id: self.connection_id,
                    sql: sql.into(),
                    origin: origin.map(str::to_string),
                })
                .await
        }

        async fn approve(&self, proposal: &DesktopSqlProposalReceipt) {
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

        async fn user_name(&self, id: i64) -> String {
            let lease = self
                .connections
                .acquire(self.connection_id, ConnectionAccess::Read)
                .await
                .unwrap();
            let live = lease.live().sql().unwrap();
            match live.ro() {
                DbPool::Sqlite(pool) => sqlx::query_scalar("SELECT name FROM users WHERE id = ?")
                    .bind(id)
                    .fetch_one(pool)
                    .await
                    .unwrap(),
                _ => panic!("query-service harness must use SQLite"),
            }
        }

        async fn set_connection_access_for_test(&self, access: &str) {
            sqlx::query(
                "UPDATE connections
                 SET workspace_access = ?2, revision = revision + 1
                 WHERE id = ?1",
            )
            .bind(self.connection_id.to_string())
            .bind(access)
            .execute(self.store.pool())
            .await
            .unwrap();
        }

        async fn enable_writes(&self, require_approval: bool) {
            let mut writable = self.profile.clone();
            writable.allow_writes = true;
            self.store.upsert_connection(&writable).await.unwrap();

            let mut settings = self.store.get_safety(self.connection_id).await.unwrap();
            settings.allow_writes = true;
            settings.require_approval = require_approval;
            self.store
                .set_safety(self.connection_id, &settings)
                .await
                .unwrap();
        }

        async fn audit_actions_in_order(&self) -> Vec<String> {
            let (mut entries, valid, first_bad) = audit::snapshot(&self.store, self.connection_id)
                .await
                .unwrap();
            assert!(valid);
            assert_eq!(first_bad, None);
            entries.reverse();
            entries.into_iter().map(|entry| entry.action).collect()
        }
    }

    #[tokio::test]
    async fn desktop_classification_is_view_capable_and_holds_scope_through_serialization() {
        let harness = SqliteHarness::new().await;
        harness.set_connection_access_for_test("view").await;

        let receipt = harness
            .service
            .classify_desktop_sql(DesktopSqlClassificationRequest {
                connection_id: harness.connection_id,
                sql: "SELECT id FROM users".into(),
            })
            .await
            .unwrap();
        assert_eq!(receipt.classification.kind, QueryKind::Read);
        let serialized = serde_json::to_value(&receipt).unwrap();
        assert_eq!(
            serialized,
            serde_json::json!({
                "kind": "read",
                "risk": "low",
                "statementCount": 1,
                "noWhere": false,
                "tables": ["users"],
                "notes": [],
                "rollbackSafe": false
            }),
            "desktop classification must retain the literal legacy wire contract"
        );
        assert_eq!(
            serialized,
            serde_json::to_value(&receipt.classification).unwrap(),
            "receipt serialization must preserve the legacy Classification shape"
        );
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                harness.connections.begin_scope_mutation(),
            )
            .await
            .is_err(),
            "classification receipt must retain the scope guard through serialization"
        );
        drop(receipt);
        let mutation = tokio::time::timeout(
            Duration::from_secs(5),
            harness.connections.begin_scope_mutation(),
        )
        .await
        .expect("scope writer must proceed after classification receipt drop");
        drop(mutation);

        harness.set_connection_access_for_test("local").await;
        harness.close().await;
    }

    #[tokio::test]
    async fn desktop_preview_preserves_viewer_gate_order_and_exact_messages() {
        let harness = SqliteHarness::new().await;
        harness.set_connection_access_for_test("view").await;

        let write_receipt = harness
            .service
            .preview_desktop_sql(DesktopSqlPreviewRequest {
                connection_id: harness.connection_id,
                sql: "UPDATE users SET name = 'Grace' WHERE id = 1".into(),
            })
            .await
            .unwrap();
        assert_eq!(write_receipt.report.mode, PreviewMode::Skipped);
        assert_eq!(
            write_receipt.report.note.as_deref(),
            Some("workspace role is read-only — write preview skipped")
        );
        assert_eq!(
            serde_json::to_value(&write_receipt).unwrap(),
            serde_json::json!({
                "mode": "skipped",
                "estimatedRows": null,
                "exactRows": null,
                "plan": null,
                "note": "workspace role is read-only — write preview skipped"
            }),
            "desktop preview must retain the literal legacy wire contract"
        );
        drop(write_receipt);

        let read_error = match harness
            .service
            .preview_desktop_sql(DesktopSqlPreviewRequest {
                connection_id: harness.connection_id,
                sql: "SELECT id FROM users".into(),
            })
            .await
        {
            Err(error) => error.into_error(),
            Ok(_) => panic!("viewer read preview must fail at target authorization"),
        };
        assert_eq!(
            serde_json::to_value(&read_error).unwrap(),
            serde_json::json!({
                "kind": "blocked",
                "message": "blocked: workspace role cannot execute this connection"
            }),
            "desktop preview errors must retain the literal legacy AppError wire contract"
        );
        assert!(matches!(
            read_error,
            AppError::Blocked { reason }
                if reason == "workspace role cannot execute this connection"
        ));

        harness.set_connection_access_for_test("local").await;
        harness.close().await;
    }

    #[tokio::test]
    async fn desktop_preconnection_skips_do_not_open_the_target_database() {
        let harness = SqliteHarness::new().await;
        let invalid_database = harness
            .directory
            .path()
            .join("missing-parent")
            .join("target.db")
            .to_string_lossy()
            .into_owned();

        let mut writes_disabled = harness.profile.clone();
        writes_disabled.database = invalid_database.clone();
        writes_disabled.allow_writes = true;
        harness
            .store
            .upsert_connection(&writes_disabled)
            .await
            .unwrap();
        let mut settings = harness
            .store
            .get_safety(harness.connection_id)
            .await
            .unwrap();
        settings.allow_writes = false;
        harness
            .store
            .set_safety(harness.connection_id, &settings)
            .await
            .unwrap();

        let disabled_receipt = harness
            .service
            .preview_desktop_sql(DesktopSqlPreviewRequest {
                connection_id: harness.connection_id,
                sql: "DELETE FROM users WHERE id = 1".into(),
            })
            .await
            .unwrap();
        assert_eq!(
            disabled_receipt.report.note.as_deref(),
            Some(
                "writes are disabled for this connection — impact preview skipped (no rows locked)"
            )
        );
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                harness.connections.begin_scope_mutation(),
            )
            .await
            .is_err(),
            "pre-connection skipped receipt must retain its scope guard through serialization"
        );
        drop(disabled_receipt);
        let mutation = tokio::time::timeout(
            Duration::from_secs(5),
            harness.connections.begin_scope_mutation(),
        )
        .await
        .expect("scope writer must proceed after skipped preview receipt drop");
        drop(mutation);

        harness.set_connection_access_for_test("view").await;
        settings.allow_writes = true;
        harness
            .store
            .set_safety(harness.connection_id, &settings)
            .await
            .unwrap();
        let readonly_receipt = harness
            .service
            .preview_desktop_sql(DesktopSqlPreviewRequest {
                connection_id: harness.connection_id,
                sql: "DELETE FROM users WHERE id = 1".into(),
            })
            .await
            .unwrap();
        assert_eq!(
            readonly_receipt.report.note.as_deref(),
            Some("workspace role is read-only — write preview skipped")
        );
        drop(readonly_receipt);

        harness.set_connection_access_for_test("local").await;
        harness
            .store
            .upsert_connection(&harness.profile)
            .await
            .unwrap();
        harness.close().await;
    }

    #[tokio::test]
    async fn desktop_read_preview_preserves_wire_shape_and_lease_guard() {
        let harness = SqliteHarness::new().await;
        let receipt = harness
            .service
            .preview_desktop_sql(DesktopSqlPreviewRequest {
                connection_id: harness.connection_id,
                sql: "SELECT id, name FROM users ORDER BY id".into(),
            })
            .await
            .unwrap();
        assert_eq!(receipt.report.mode, PreviewMode::Explain);
        assert_eq!(
            serde_json::to_value(&receipt).unwrap(),
            serde_json::to_value(&receipt.report).unwrap(),
            "receipt serialization must preserve the legacy PreviewReport shape"
        );
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                harness.connections.begin_scope_mutation(),
            )
            .await
            .is_err(),
            "preview receipt must retain the live lease through serialization"
        );
        drop(receipt);
        let mutation = tokio::time::timeout(
            Duration::from_secs(5),
            harness.connections.begin_scope_mutation(),
        )
        .await
        .expect("scope writer must proceed after preview receipt drop");
        drop(mutation);
        harness.close().await;
    }

    #[tokio::test]
    async fn desktop_preview_receipt_serializes_safety_mutation_until_drop() {
        let harness = SqliteHarness::new().await;
        let receipt = harness
            .service
            .preview_desktop_sql(DesktopSqlPreviewRequest {
                connection_id: harness.connection_id,
                sql: "SELECT id FROM users".into(),
            })
            .await
            .unwrap();

        let mut updated = harness
            .store
            .get_safety(harness.connection_id)
            .await
            .unwrap();
        assert_eq!(updated.max_rows, 1000);
        updated.max_rows = 77;

        let store = harness.store.clone();
        let connections = harness.connections.clone();
        let connection_id = harness.connection_id;
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let mut updater = tokio::spawn(async move {
            started_tx
                .send(())
                .expect("test must observe safety mutation start");
            let _mutation = connections
                .begin_connection_mutation(connection_id, ConnectionAccess::Read)
                .await
                .unwrap();
            store.set_safety(connection_id, &updated).await.unwrap();
        });
        started_rx
            .await
            .expect("safety mutation task must reach the scope writer");

        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut updater)
                .await
                .is_err(),
            "safety mutation must wait while the preview receipt retains its authority"
        );
        assert_eq!(
            harness
                .store
                .get_safety(harness.connection_id)
                .await
                .unwrap()
                .max_rows,
            1000,
            "waiting mutation must not publish settings early"
        );

        drop(receipt);
        tokio::time::timeout(Duration::from_secs(5), updater)
            .await
            .expect("safety mutation must proceed after preview receipt drop")
            .expect("safety mutation task must succeed");
        assert_eq!(
            harness
                .store
                .get_safety(harness.connection_id)
                .await
                .unwrap()
                .max_rows,
            77
        );
        harness.close().await;
    }

    #[tokio::test]
    async fn desktop_write_preview_disabled_is_read_authorized_and_explain_only() {
        let harness = SqliteHarness::new().await;
        let mut settings = harness
            .store
            .get_safety(harness.connection_id)
            .await
            .unwrap();
        settings.allow_writes = true;
        settings.explain_preview = false;
        harness
            .store
            .set_safety(harness.connection_id, &settings)
            .await
            .unwrap();

        // The profile write gate remains false. Success therefore proves that this
        // branch requested Read access and never entered execute+rollback.
        assert!(!harness.profile.allow_writes);
        let receipt = harness
            .service
            .preview_desktop_sql(DesktopSqlPreviewRequest {
                connection_id: harness.connection_id,
                sql: "UPDATE users SET name = 'Grace' WHERE id = 1".into(),
            })
            .await
            .unwrap();
        assert_eq!(receipt.report.mode, PreviewMode::Explain);
        assert_eq!(receipt.report.exact_rows, None);
        drop(receipt);
        assert_eq!(harness.user_name(1).await, "Ada");
        harness.close().await;
    }

    #[tokio::test]
    async fn desktop_write_preview_never_executes_before_exact_approval() {
        let harness = SqliteHarness::new().await;
        let mut settings = harness
            .store
            .get_safety(harness.connection_id)
            .await
            .unwrap();
        settings.allow_writes = true;
        settings.explain_preview = true;
        harness
            .store
            .set_safety(harness.connection_id, &settings)
            .await
            .unwrap();

        let receipt = harness
            .service
            .preview_desktop_sql(DesktopSqlPreviewRequest {
                connection_id: harness.connection_id,
                sql: "UPDATE users SET name = 'Grace' WHERE id = 1".into(),
            })
            .await
            .unwrap();
        assert_eq!(receipt.report.mode, PreviewMode::Explain);
        assert_eq!(receipt.report.exact_rows, None);
        assert!(receipt
            .report
            .note
            .as_deref()
            .is_some_and(|note| note.contains("no target-mutating statement was executed")));
        drop(receipt);
        assert_eq!(harness.user_name(1).await, "Ada");
        harness.close().await;
    }

    #[tokio::test]
    async fn desktop_nonrollback_write_preview_uses_read_authority_only() {
        let harness = SqliteHarness::new().await;
        let mut settings = harness
            .store
            .get_safety(harness.connection_id)
            .await
            .unwrap();
        settings.allow_writes = true;
        settings.explain_preview = true;
        harness
            .store
            .set_safety(harness.connection_id, &settings)
            .await
            .unwrap();

        let receipt = harness
            .service
            .preview_desktop_sql(DesktopSqlPreviewRequest {
                connection_id: harness.connection_id,
                sql: "this is not sql".into(),
            })
            .await
            .unwrap();
        assert_eq!(receipt.report.mode, PreviewMode::Explain);
        assert_eq!(receipt.report.exact_rows, None);
        drop(receipt);
        assert_eq!(harness.user_name(1).await, "Ada");
        harness.close().await;
    }

    #[tokio::test]
    async fn desktop_ddl_preview_skips_before_opening_the_target() {
        let harness = SqliteHarness::new().await;
        let mut invalid = harness.profile.clone();
        invalid.database = harness
            .directory
            .path()
            .join("missing-parent")
            .join("target.db")
            .to_string_lossy()
            .into_owned();
        harness.store.upsert_connection(&invalid).await.unwrap();

        let mut settings = harness
            .store
            .get_safety(harness.connection_id)
            .await
            .unwrap();
        settings.allow_writes = true;
        harness
            .store
            .set_safety(harness.connection_id, &settings)
            .await
            .unwrap();

        let receipt = harness
            .service
            .preview_desktop_sql(DesktopSqlPreviewRequest {
                connection_id: harness.connection_id,
                sql: "CREATE TABLE preview_only (id INTEGER PRIMARY KEY)".into(),
            })
            .await
            .unwrap();
        assert_eq!(receipt.report.mode, PreviewMode::Skipped);
        assert_eq!(
            receipt.report.note.as_deref(),
            Some("DDL / privilege change — no row-count preview; review the statement directly.")
        );
        drop(receipt);
        harness.close().await;
    }

    #[tokio::test]
    async fn desktop_sql_read_preserves_wire_provenance_and_lease_guard() {
        let harness = SqliteHarness::new().await;
        let proposal = harness
            .propose("SELECT id, name FROM users ORDER BY id", Some("data-view"))
            .await
            .unwrap();
        assert!(!proposal.approval_required);
        assert_eq!(proposal.state, OperationState::Ready);
        let receipt = harness
            .service
            .run_desktop_sql(proposal.operation_id)
            .await
            .unwrap();
        let result = receipt
            .outcome
            .result
            .as_ref()
            .expect("read execution must return a result grid");
        assert_eq!(result.row_count, 2);
        assert_eq!(
            serde_json::to_value(&receipt).unwrap(),
            serde_json::json!({
                "result": {
                    "columns": ["id", "name"],
                    "rows": [[1, "Ada"], [2, "Linus"]],
                    "rowCount": 2,
                    "truncated": false,
                    "durationMs": result.duration_ms
                },
                "affected": null,
                "committed": false
            }),
            "desktop SQL receipt must preserve the literal legacy ExecOutcome wire"
        );
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                harness.connections.begin_scope_mutation(),
            )
            .await
            .is_err(),
            "desktop SQL receipt must retain the live lease through serialization"
        );

        let history = harness
            .store
            .list_history(harness.connection_id)
            .await
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].status, "ok");
        assert_eq!(history[0].kind, QueryKind::Read);
        assert_eq!(history[0].row_count, Some(2));
        assert_eq!(history[0].origin, "data-view");
        let (audit, valid, first_bad) = audit::snapshot(&harness.store, harness.connection_id)
            .await
            .unwrap();
        assert!(valid);
        assert_eq!(first_bad, None);
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].action, "read");
        assert_eq!(audit[0].affected_estimate, Some(2));

        drop(receipt);
        let mutation = tokio::time::timeout(
            Duration::from_secs(5),
            harness.connections.begin_scope_mutation(),
        )
        .await
        .expect("scope writer must proceed after desktop SQL receipt drop");
        drop(mutation);
        harness.close().await;
    }

    #[tokio::test]
    async fn desktop_sql_write_gate_rejects_before_persist_or_target_touch() {
        let harness = SqliteHarness::new().await;
        let error = match harness
            .propose("UPDATE users SET name = 'Grace' WHERE id = 1", None)
            .await
        {
            Err(error) => error.into_error(),
            _ => panic!("writes-disabled policy must reject before target touch"),
        };
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            serde_json::json!({
                "kind": "blocked",
                "message": "blocked: writing is disabled for this connection (writes are off by default). Enable writes in the connection's safety settings to propose it."
            })
        );
        assert_eq!(harness.user_name(1).await, "Ada");
        assert!(harness.audit_actions_in_order().await.is_empty());
        assert!(harness
            .store
            .list_history(harness.connection_id)
            .await
            .unwrap()
            .is_empty());
        harness.close().await;
    }

    #[tokio::test]
    async fn desktop_arbitrary_privilege_sql_is_blocked_before_persistence() {
        let harness = SqliteHarness::new().await;
        harness.enable_writes(false).await;
        let error = match harness
            .propose("GRANT SELECT ON users TO analyst", None)
            .await
        {
            Err(error) => error.into_error(),
            Ok(_) => panic!("arbitrary privilege SQL must not become an operation"),
        };
        assert!(matches!(
            error,
            AppError::Blocked { ref reason } if reason.contains("arbitrary privilege SQL")
        ));
        assert!(harness.audit_actions_in_order().await.is_empty());
        harness.close().await;
    }

    #[tokio::test]
    async fn desktop_sql_workspace_view_role_blocks_without_audit_or_target_touch() {
        let harness = SqliteHarness::new().await;
        harness.enable_writes(true).await;
        harness.set_connection_access_for_test("view").await;

        let error = match harness
            .propose("UPDATE users SET name = 'Grace' WHERE id = 1", None)
            .await
        {
            Err(error) => error.into_error(),
            Ok(_) => panic!("workspace read role must reject mutations"),
        };
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            serde_json::json!({
                "kind": "blocked",
                "message": "blocked: your workspace role grants read-only database access"
            })
        );
        harness.set_connection_access_for_test("local").await;
        assert_eq!(harness.user_name(1).await, "Ada");
        assert!(harness.audit_actions_in_order().await.is_empty());
        assert!(harness
            .store
            .list_history(harness.connection_id)
            .await
            .unwrap()
            .is_empty());

        harness.close().await;
    }

    #[tokio::test]
    async fn desktop_sql_exact_approval_commits_and_records_both_ledgers() {
        let harness = SqliteHarness::new().await;
        harness.enable_writes(true).await;
        let sql = "UPDATE users SET name = 'Grace' WHERE id = 1";
        let proposal = harness.propose(sql, Some("sql")).await.unwrap();
        assert!(proposal.approval_required);
        assert_eq!(proposal.state, OperationState::PendingApproval);

        let rejected = match harness.service.run_desktop_sql(proposal.operation_id).await {
            Err(error) => error.into_error(),
            Ok(_) => panic!("a write without its exact approval must remain blocked"),
        };
        assert!(matches!(rejected, AppError::Blocked { .. }));

        harness.approve(&proposal).await;
        let receipt = harness
            .service
            .run_desktop_sql(proposal.operation_id)
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(&receipt).unwrap(),
            serde_json::json!({
                "result": null,
                "affected": 1,
                "committed": true
            })
        );
        drop(receipt);
        assert_eq!(harness.user_name(1).await, "Grace");
        assert_eq!(
            harness.audit_actions_in_order().await,
            ["execute:attempt", "execute"]
        );
        let history = harness
            .store
            .list_history(harness.connection_id)
            .await
            .unwrap();
        assert_eq!(history.len(), 1);
        assert!(history.iter().any(|entry| {
            entry.status == "ok"
                && entry.origin == "sql"
                && entry.kind == QueryKind::Write
                && entry.row_count == Some(1)
        }));
        harness.close().await;
    }

    #[tokio::test]
    async fn unacknowledged_target_commit_becomes_outcome_unknown_without_retry() {
        let harness = SqliteHarness::new().await;
        harness.enable_writes(true).await;
        let lease = harness
            .connections
            .acquire(harness.connection_id, ConnectionAccess::Write)
            .await
            .unwrap();
        let DbPool::Sqlite(pool) = &lease.live().sql().unwrap().write_pool else {
            panic!("query-service harness must use SQLite");
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
        let error = match harness.service.run_desktop_sql(proposal.operation_id).await {
            Err(error) => error.into_error(),
            Ok(_) => panic!("deferred foreign-key commit must not report success"),
        };
        assert!(
            matches!(error, AppError::OutcomeUnknown(_)),
            "commit acknowledgement failure must be explicit, got {error}"
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
        assert!(harness
            .service
            .operation
            .claim(proposal.operation_id)
            .await
            .is_err());
        harness.close().await;
    }

    #[tokio::test]
    async fn critical_write_requires_the_exact_typed_confirmation() {
        let harness = SqliteHarness::new().await;
        harness.enable_writes(true).await;
        let proposal = harness.propose("DELETE FROM users", None).await.unwrap();
        assert_eq!(
            proposal.confirmation_phrase.as_deref(),
            Some(super::super::operation_service::CRITICAL_CONFIRMATION)
        );
        let missing = harness
            .operation_service
            .approve_local(
                &harness.approval,
                crate::services::OperationDecisionRequest {
                    operation_id: proposal.operation_id,
                    expected_payload_hash: proposal.payload_hash.clone(),
                    reason: None,
                },
            )
            .await;
        assert!(matches!(missing, Err(AppError::Blocked { .. })));
        harness
            .operation_service
            .approve_local(
                &harness.approval,
                crate::services::OperationDecisionRequest {
                    operation_id: proposal.operation_id,
                    expected_payload_hash: proposal.payload_hash.clone(),
                    reason: proposal.confirmation_phrase.clone(),
                },
            )
            .await
            .unwrap();
        let receipt = harness
            .service
            .run_desktop_sql(proposal.operation_id)
            .await
            .unwrap();
        drop(receipt);
        assert_eq!(
            harness
                .service
                .operation
                .get(proposal.operation_id)
                .await
                .unwrap()
                .state,
            OperationState::Succeeded
        );
        harness.close().await;
    }

    #[tokio::test]
    async fn production_write_requires_production_confirmation() {
        let harness = SqliteHarness::new().await;
        harness.enable_writes(true).await;
        let mut production = harness.profile.clone();
        production.allow_writes = true;
        production.env = Some("prod".into());
        harness.store.upsert_connection(&production).await.unwrap();

        let proposal = harness
            .propose("UPDATE users SET name = 'Grace' WHERE id = 1", None)
            .await
            .unwrap();
        assert_eq!(
            proposal.confirmation_phrase.as_deref(),
            Some(super::super::operation_service::PRODUCTION_CONFIRMATION)
        );
        let wrong = harness
            .operation_service
            .approve_local(
                &harness.approval,
                crate::services::OperationDecisionRequest {
                    operation_id: proposal.operation_id,
                    expected_payload_hash: proposal.payload_hash.clone(),
                    reason: Some("prod".into()),
                },
            )
            .await;
        assert!(matches!(wrong, Err(AppError::Blocked { .. })));
        harness
            .operation_service
            .approve_local(
                &harness.approval,
                crate::services::OperationDecisionRequest {
                    operation_id: proposal.operation_id,
                    expected_payload_hash: proposal.payload_hash.clone(),
                    reason: proposal.confirmation_phrase.clone(),
                },
            )
            .await
            .unwrap();
        let receipt = harness
            .service
            .run_desktop_sql(proposal.operation_id)
            .await
            .unwrap();
        drop(receipt);
        assert_eq!(harness.user_name(1).await, "Grace");
        harness.close().await;
    }

    #[tokio::test]
    async fn desktop_sql_write_always_requires_exact_approval_when_legacy_prompt_is_off() {
        let harness = SqliteHarness::new().await;
        harness.enable_writes(false).await;
        let proposal = harness
            .propose("UPDATE users SET name = 'Grace' WHERE id = 1", None)
            .await
            .unwrap();
        assert!(proposal.approval_required);
        assert!(harness
            .service
            .run_desktop_sql(proposal.operation_id)
            .await
            .is_err());
        harness.approve(&proposal).await;
        let receipt = harness
            .service
            .run_desktop_sql(proposal.operation_id)
            .await
            .unwrap();
        assert!(receipt.outcome.committed);
        drop(receipt);
        assert_eq!(harness.user_name(1).await, "Grace");
        assert_eq!(
            harness.audit_actions_in_order().await,
            ["execute:attempt", "execute"]
        );
        harness.close().await;
    }

    #[tokio::test]
    async fn desktop_sql_write_fails_closed_when_attempt_audit_is_unavailable() {
        let harness = SqliteHarness::new().await;
        harness.enable_writes(true).await;
        sqlx::raw_sql(
            "CREATE TRIGGER fail_desktop_write_attempt
             BEFORE INSERT ON audit_log
             BEGIN
               SELECT RAISE(FAIL, 'forced desktop attempt audit failure');
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
        let error = match harness.service.run_desktop_sql(proposal.operation_id).await {
            Err(error) => error.into_error(),
            Ok(_) => panic!("write must fail closed before target touch"),
        };
        assert!(matches!(
            error,
            AppError::Config(message)
                if message.starts_with("audit pre-record failed — refusing to run write:")
                    && message.contains("forced desktop attempt audit failure")
        ));
        assert_eq!(harness.user_name(1).await, "Ada");
        assert!(audit::snapshot(&harness.store, harness.connection_id)
            .await
            .unwrap()
            .0
            .is_empty());
        assert!(harness
            .store
            .list_history(harness.connection_id)
            .await
            .unwrap()
            .is_empty());
        harness.close().await;
    }

    #[tokio::test]
    async fn desktop_sql_execution_failure_closes_the_attempt_and_keeps_original_error() {
        let harness = SqliteHarness::new().await;
        harness.enable_writes(true).await;
        let proposal = harness
            .propose("UPDATE users SET name = 'Grace' WHERE id = 1", None)
            .await
            .unwrap();
        harness.approve(&proposal).await;
        let target = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&harness.profile.database)
                    .foreign_keys(true),
            )
            .await
            .unwrap();
        sqlx::raw_sql("DROP TABLE users")
            .execute(&target)
            .await
            .unwrap();
        target.close().await;
        let failure = match harness.service.run_desktop_sql(proposal.operation_id).await {
            Err(DesktopSqlRunError::Execution(failure)) => failure,
            _ => panic!("missing target table must fail during desktop execution"),
        };
        let original = failure.error.to_string();
        assert!(original.contains("users"));
        assert!(!original.contains("audit"));
        let mapped = failure.into_error();
        assert_eq!(mapped.to_string(), original);
        assert_eq!(
            harness.audit_actions_in_order().await,
            ["execute:attempt", "error"]
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
            .is_some_and(|message| message.contains("users")));
        harness.close().await;
    }

    #[tokio::test]
    async fn desktop_sql_committed_ddl_invalidates_the_legacy_schema_cache() {
        let harness = SqliteHarness::new().await;
        harness.enable_writes(true).await;
        harness
            .store
            .set_schema_cache(harness.connection_id, r#"{"tables":[]}"#)
            .await
            .unwrap();
        assert!(harness
            .store
            .get_schema_cache(harness.connection_id)
            .await
            .unwrap()
            .is_some());

        let proposal = harness
            .propose("CREATE TABLE widgets (id INTEGER PRIMARY KEY)", None)
            .await
            .unwrap();
        harness.approve(&proposal).await;
        let receipt = harness
            .service
            .run_desktop_sql(proposal.operation_id)
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
        harness.close().await;
    }

    #[tokio::test]
    async fn sqlite_plan_run_persists_required_provenance() {
        let harness = SqliteHarness::new().await;
        let mut settings = harness
            .store
            .get_safety(harness.connection_id)
            .await
            .unwrap();
        settings.max_rows = 1;
        harness
            .store
            .set_safety(harness.connection_id, &settings)
            .await
            .unwrap();

        let plan_receipt = harness
            .service
            .plan_agent_read(AgentQueryPlanRequest {
                connection_id: harness.connection_id,
                sql: "SELECT id, name FROM users ORDER BY id".into(),
                max_rows: None,
                origin: AgentQueryInvocationOrigin::Mcp,
            })
            .await
            .unwrap();
        let plan = plan_receipt.plan();
        assert_eq!(plan.connection_id, harness.connection_id);
        assert_eq!(plan.connection_name, harness.profile.name);
        assert_eq!(plan.environment.as_deref(), Some("test"));
        let plan_id = plan.plan_id;
        let planning_decision = plan.decision.clone();
        drop(plan_receipt);

        // The plan freezes the configured cap. Later safety edits cannot widen it.
        settings.max_rows = MAX_AGENT_ROWS;
        harness
            .store
            .set_safety(harness.connection_id, &settings)
            .await
            .unwrap();

        let prepared = harness.service.prepare_agent_run(plan_id).await.unwrap();
        assert_eq!(
            prepared.event_context(),
            &AgentQueryRunEventContext {
                connection_id: harness.connection_id,
                connection_name: harness.profile.name.clone(),
                plan_id,
                sql: "SELECT id, name FROM users ORDER BY id".into(),
            }
        );
        let run_receipt = prepared.execute().await.unwrap();
        let run = run_receipt.run();
        assert_eq!(run.query_run_id.get_version_num(), 4);
        assert_eq!(run.result.row_count, 1);
        assert!(run.result.truncated);
        assert_eq!(run.planning_decision, planning_decision);
        let query_run_id = run.query_run_id;
        drop(run_receipt);

        let history = harness
            .store
            .list_history(harness.connection_id)
            .await
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].id, query_run_id);
        assert_eq!(history[0].origin, "agent");
        assert_eq!(history[0].status, "ok");
        assert_eq!(history[0].row_count, Some(1));

        let (mut audit, chain_ok, first_bad) =
            audit::snapshot(&harness.store, harness.connection_id)
                .await
                .unwrap();
        assert!(chain_ok);
        assert_eq!(first_bad, None);
        audit.reverse();
        assert_eq!(
            audit
                .iter()
                .map(|entry| entry.action.as_str())
                .collect::<Vec<_>>(),
            ["mcp:plan_query", "mcp:run_query"]
        );
        harness.close().await;
    }

    #[tokio::test]
    async fn cli_origin_separates_audit_but_keeps_dashboard_eligible_history() {
        let harness = SqliteHarness::new().await;
        let plan_receipt = harness
            .service
            .plan_agent_read(AgentQueryPlanRequest {
                connection_id: harness.connection_id,
                sql: "SELECT id FROM users ORDER BY id".into(),
                max_rows: Some(1),
                origin: AgentQueryInvocationOrigin::Cli,
            })
            .await
            .unwrap();
        let plan_id = plan_receipt.plan().plan_id;
        drop(plan_receipt);
        let run_receipt = harness
            .service
            .prepare_agent_run(plan_id)
            .await
            .unwrap()
            .execute()
            .await
            .unwrap();
        let query_run_id = run_receipt.run().query_run_id;
        drop(run_receipt);

        let history = harness
            .store
            .list_history(harness.connection_id)
            .await
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].id, query_run_id);
        assert_eq!(history[0].origin, "agent");

        let (mut audit, chain_ok, first_bad) =
            audit::snapshot(&harness.store, harness.connection_id)
                .await
                .unwrap();
        assert!(chain_ok);
        assert_eq!(first_bad, None);
        audit.reverse();
        assert_eq!(
            audit
                .iter()
                .map(|entry| entry.action.as_str())
                .collect::<Vec<_>>(),
            ["cli:plan_query", "cli:run_query"]
        );
        harness.close().await;
    }

    #[tokio::test]
    async fn service_clones_claim_one_durable_operation() {
        let harness = SqliteHarness::new().await;
        let other_transport = harness.service.clone();
        let receipt = harness
            .service
            .plan_agent_read(AgentQueryPlanRequest {
                connection_id: harness.connection_id,
                sql: "SELECT 1".into(),
                max_rows: Some(1),
                origin: AgentQueryInvocationOrigin::Mcp,
            })
            .await
            .unwrap();
        let plan_id = receipt.plan().plan_id;
        drop(receipt);

        let prepared = other_transport.prepare_agent_run(plan_id).await.unwrap();
        assert!(matches!(
            harness.service.prepare_agent_run(plan_id).await,
            Err(AgentQueryRunPrepareError::UnknownOrAlreadyUsed)
        ));
        drop(prepared);
        harness.close().await;
    }

    #[tokio::test]
    async fn execution_failure_keeps_single_use_and_persists_best_effort_history() {
        let harness = SqliteHarness::new().await;
        let pin = harness
            .store
            .pin_connection_for_read(harness.connection_id)
            .await
            .unwrap();
        let plan_id = Uuid::new_v4();
        harness
            .service
            .seed_plan_for_test(
                plan_id,
                &pin,
                "SELECT no_such_function()".into(),
                1,
                "ready".into(),
                Instant::now(),
            )
            .await;
        let prepared = harness.service.prepare_agent_run(plan_id).await.unwrap();
        let failure = match prepared.execute().await {
            Err(AgentQueryRunError::Execution(failure)) => failure,
            _ => panic!("invalid SQLite function must fail during execution"),
        };
        assert!(!failure.error().to_string().is_empty());
        assert!(matches!(
            harness.service.prepare_agent_run(plan_id).await,
            Err(AgentQueryRunPrepareError::UnknownOrAlreadyUsed)
        ));
        let _ = failure.into_error();

        let history = harness
            .store
            .list_history(harness.connection_id)
            .await
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].status, "error");
        assert!(history[0].error.is_some());
        harness.close().await;
    }

    #[tokio::test]
    async fn successful_read_without_consent_history_returns_no_rows_and_consumes_plan() {
        let harness = SqliteHarness::new().await;
        let receipt = harness
            .service
            .plan_agent_read(AgentQueryPlanRequest {
                connection_id: harness.connection_id,
                sql: "SELECT id, name FROM users ORDER BY id".into(),
                max_rows: Some(2),
                origin: AgentQueryInvocationOrigin::Mcp,
            })
            .await
            .unwrap();
        let plan_id = receipt.plan().plan_id;
        drop(receipt);
        let prepared = harness.service.prepare_agent_run(plan_id).await.unwrap();

        sqlx::raw_sql(
            "CREATE TRIGGER fail_success_query_history
             BEFORE INSERT ON query_history
             WHEN NEW.status = 'ok'
             BEGIN
               SELECT RAISE(FAIL, 'forced success history failure');
             END;",
        )
        .execute(harness.store.pool())
        .await
        .unwrap();

        let failure = match prepared.execute().await {
            Err(AgentQueryRunError::ConsentHandlePersistence(failure)) => failure,
            _ => panic!("a successful read without durable provenance must fail closed"),
        };
        let debug = format!("{failure:?}");
        assert!(debug.contains("forced success history failure"));
        assert!(!debug.contains("Ada"));
        assert!(!debug.contains("Linus"));
        assert!(!debug.contains("\"rows\""));
        assert!(matches!(
            harness.service.prepare_agent_run(plan_id).await,
            Err(AgentQueryRunPrepareError::UnknownOrAlreadyUsed)
        ));
        let _ = failure.into_error();
        assert!(harness
            .store
            .list_history(harness.connection_id)
            .await
            .unwrap()
            .is_empty());
        harness.close().await;
    }

    #[tokio::test]
    async fn audit_and_failed_history_outages_do_not_mask_execution_error() {
        let harness = SqliteHarness::new().await;
        sqlx::raw_sql(
            "CREATE TRIGGER fail_query_audit
             BEFORE INSERT ON audit_log
             BEGIN
               SELECT RAISE(FAIL, 'forced audit failure');
             END;
             CREATE TRIGGER fail_error_query_history
             BEFORE INSERT ON query_history
             WHEN NEW.status = 'error'
             BEGIN
               SELECT RAISE(FAIL, 'forced failed-history failure');
             END;",
        )
        .execute(harness.store.pool())
        .await
        .unwrap();

        let pin = harness
            .store
            .pin_connection_for_read(harness.connection_id)
            .await
            .unwrap();
        let plan_id = Uuid::new_v4();
        harness
            .service
            .seed_plan_for_test(
                plan_id,
                &pin,
                "SELECT no_such_function()".into(),
                1,
                "ready".into(),
                Instant::now(),
            )
            .await;
        let prepared = harness.service.prepare_agent_run(plan_id).await.unwrap();
        let failure = match prepared.execute().await {
            Err(AgentQueryRunError::Execution(failure)) => failure,
            _ => panic!("the original target-database execution must remain the error"),
        };
        let original = failure.error().to_string();
        assert!(original.contains("no_such_function"));
        assert!(!original.contains("forced audit failure"));
        assert!(!original.contains("forced failed-history failure"));
        assert!(matches!(
            harness.service.prepare_agent_run(plan_id).await,
            Err(AgentQueryRunPrepareError::UnknownOrAlreadyUsed)
        ));
        let _ = failure.into_error();

        assert!(audit::snapshot(&harness.store, harness.connection_id)
            .await
            .unwrap()
            .0
            .is_empty());
        assert!(harness
            .store
            .list_history(harness.connection_id)
            .await
            .unwrap()
            .is_empty());
        harness.close().await;
    }

    #[tokio::test]
    async fn plan_and_run_receipts_hold_scope_writer_until_adapter_drop() {
        let harness = SqliteHarness::new().await;
        let plan_receipt = harness
            .service
            .plan_agent_read(AgentQueryPlanRequest {
                connection_id: harness.connection_id,
                sql: "SELECT id FROM users ORDER BY id".into(),
                max_rows: Some(1),
                origin: AgentQueryInvocationOrigin::Mcp,
            })
            .await
            .unwrap();
        let plan_id = plan_receipt.plan().plan_id;
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                harness.connections.begin_scope_mutation(),
            )
            .await
            .is_err(),
            "plan receipt must retain the scope read guard through adapter emission"
        );
        assert_eq!(plan_receipt.plan().plan_id, plan_id);
        drop(plan_receipt);
        let mutation = tokio::time::timeout(
            Duration::from_secs(5),
            harness.connections.begin_scope_mutation(),
        )
        .await
        .expect("scope writer must proceed after the plan receipt drops");
        drop(mutation);

        let prepared = harness.service.prepare_agent_run(plan_id).await.unwrap();
        let run_receipt = prepared.execute().await.unwrap();
        let query_run_id = run_receipt.run().query_run_id;
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                harness.connections.begin_scope_mutation(),
            )
            .await
            .is_err(),
            "run receipt must retain the scope read guard through adapter emission"
        );
        assert_eq!(run_receipt.run().query_run_id, query_run_id);
        drop(run_receipt);
        let mutation = tokio::time::timeout(
            Duration::from_secs(5),
            harness.connections.begin_scope_mutation(),
        )
        .await
        .expect("scope writer must proceed after the run receipt drops");
        drop(mutation);
        harness.close().await;
    }

    #[tokio::test]
    async fn non_read_rejection_audits_only_after_adapter_result_boundary() {
        let harness = SqliteHarness::new().await;
        let rejection = match harness
            .service
            .plan_agent_read(AgentQueryPlanRequest {
                connection_id: harness.connection_id,
                sql: "DELETE FROM users".into(),
                max_rows: None,
                origin: AgentQueryInvocationOrigin::Mcp,
            })
            .await
        {
            Err(AgentQueryPlanError::NotSingleRead(rejection)) => rejection,
            _ => panic!("write planning must return a guard-bearing rejection"),
        };
        assert_eq!(rejection.connection_id(), harness.connection_id);
        assert_eq!(rejection.connection_name(), harness.profile.name);
        let before = audit::snapshot(&harness.store, harness.connection_id)
            .await
            .unwrap()
            .0;
        assert!(before.is_empty());
        rejection.audit_after_result().await;
        let after = audit::snapshot(&harness.store, harness.connection_id)
            .await
            .unwrap()
            .0;
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].action, "mcp:plan_query");
        assert!(after[0].error.is_some());
        harness.close().await;
    }

    #[tokio::test]
    async fn authority_failure_consumes_the_plan_before_awaiting() {
        let harness = SqliteHarness::new().await;
        let current_pin = harness
            .store
            .pin_connection_for_read(harness.connection_id)
            .await
            .unwrap();
        let plan_id = Uuid::new_v4();
        harness
            .service
            .seed_plan_for_test(
                plan_id,
                &current_pin,
                "SELECT 1".into(),
                1,
                "ready".into(),
                Instant::now(),
            )
            .await;

        let mut revised_profile = harness.profile.clone();
        revised_profile.name = "query-service-revised".into();
        harness
            .store
            .upsert_connection(&revised_profile)
            .await
            .unwrap();
        assert!(matches!(
            harness.service.prepare_agent_run(plan_id).await,
            Err(AgentQueryRunPrepareError::AuthorityChanged)
        ));
        assert!(matches!(
            harness.service.prepare_agent_run(plan_id).await,
            Err(AgentQueryRunPrepareError::UnknownOrAlreadyUsed)
        ));
        harness.close().await;
    }

    #[tokio::test]
    async fn expired_failure_also_consumes_the_plan() {
        let harness = SqliteHarness::new().await;
        let current_pin = harness
            .store
            .pin_connection_for_read(harness.connection_id)
            .await
            .unwrap();
        let plan_id = Uuid::new_v4();
        harness
            .service
            .seed_plan_for_test(
                plan_id,
                &current_pin,
                "SELECT 1".into(),
                1,
                "ready".into(),
                Instant::now() - QUERY_PLAN_TTL - Duration::from_secs(1),
            )
            .await;

        assert!(matches!(
            harness.service.prepare_agent_run(plan_id).await,
            Err(AgentQueryRunPrepareError::Expired)
        ));
        assert!(matches!(
            harness.service.prepare_agent_run(plan_id).await,
            Err(AgentQueryRunPrepareError::UnknownOrAlreadyUsed)
        ));
        harness.close().await;
    }
}
