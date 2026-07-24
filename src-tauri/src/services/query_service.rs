//! Transport-neutral agent SQL planning and DB-enforced read-only execution.
//! Plans are immutable, short-lived, single-use capabilities shared by every clone
//! of one application-service composition root; transports only map DTOs and events.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
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
use crate::safety::{self, PoolRef};
use crate::store::{AccountScope, PinnedConnection, Store};

/// Lifetime of an agent query plan. A plan is valid at exactly this boundary and
/// expired only when its monotonic age is greater than this value.
pub(crate) const QUERY_PLAN_TTL: Duration = Duration::from_secs(30);

/// Process-local plan bound. Insertion first removes expired entries, then evicts
/// exactly the oldest live entry when this capacity is already full.
pub(crate) const MAX_QUERY_PLANS: usize = 256;

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

/// Desktop SQL execution input. `approved` remains only for Phase 1 wire
/// compatibility; Phase 2 replaces it with a stored exact Operation approval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DesktopSqlRunRequest {
    pub(crate) connection_id: Uuid,
    pub(crate) sql: String,
    pub(crate) approved: bool,
    pub(crate) query_id: Option<Uuid>,
    pub(crate) origin: Option<String>,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentQueryInvocationOrigin {
    Mcp,
    // Constructed by the CLI adapter in the next Phase 1 slice.
    #[allow(dead_code)]
    Cli,
}

impl AgentQueryInvocationOrigin {
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
            event_context,
            decision,
            max_rows,
            origin,
        } = self;
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
                return Err(AgentQueryRunError::ConsentHandlePersistence(
                    AgentQueryConsentFailure {
                        error,
                        _lease: lease,
                    },
                ))
            }
        };

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

/// Scope-aware query service. Clones share one mutex-protected plan registry, so
/// HTTP and stdio adapters wired from the same composition root see one single-use
/// capability namespace.
#[derive(Clone)]
pub(crate) struct QueryService {
    store: Store,
    connections: ConnectionManager,
    plans: Arc<Mutex<QueryPlanRegistry>>,
}

impl QueryService {
    pub(super) fn new(store: Store, connections: ConnectionManager) -> Self {
        Self {
            store,
            connections,
            plans: Arc::new(Mutex::new(QueryPlanRegistry::default())),
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
                _authority: DesktopSqlPreviewAuthority::Scope {
                    _scope: operation_scope,
                },
            });
        }

        let access = desktop_preview_connection_access(&classification, &settings);
        let lease = operation_scope
            .connect(pin, access)
            .await
            .map_err(DesktopSqlInspectionError::Application)?;
        let live = lease
            .live()
            .sql()
            .map_err(DesktopSqlInspectionError::Application)?;

        // explain_preview off means plan-only for a write. Reclassifying only this
        // L3 invocation to Read keeps safety::preview on its EXPLAIN branch.
        let report = if matches!(classification.kind, QueryKind::Write) && !settings.explain_preview
        {
            let explain_only = Classification {
                kind: QueryKind::Read,
                ..classification
            };
            safety::preview(pool_ref(live.ro()), &request.sql, &explain_only, &settings)
                .await
                .map_err(DesktopSqlInspectionError::Application)?
        } else {
            let db = if access == ConnectionAccess::Write {
                &live.write_pool
            } else {
                live.ro()
            };
            safety::preview(pool_ref(db), &request.sql, &classification, &settings)
                .await
                .map_err(DesktopSqlInspectionError::Application)?
        };

        Ok(DesktopSqlPreviewReceipt {
            report,
            _authority: DesktopSqlPreviewAuthority::Lease {
                _lease: Box::new(lease),
            },
        })
    }

    /// Execute one desktop SQL statement while preserving the Phase 1 Tauri wire,
    /// gate, cancellation, audit, history, and schema-cache behavior.
    ///
    /// The caller-provided approval boolean is intentionally quarantined here only
    /// until Operation Runtime replaces it with an exact stored approval.
    pub(crate) async fn run_desktop_sql(
        &self,
        request: DesktopSqlRunRequest,
    ) -> Result<DesktopSqlRunReceipt, DesktopSqlRunError> {
        let operation_scope = self.connections.begin_operation_scope().await;
        let operation_pin = operation_scope
            .pin_connection(request.connection_id)
            .await
            .map_err(DesktopSqlRunError::Application)?;
        let settings = self
            .store
            .get_safety(operation_pin.connection_id)
            .await
            .map_err(DesktopSqlRunError::Application)?;
        let classification = safety::classify(&request.sql, operation_pin.profile.engine)
            .map_err(DesktopSqlRunError::Application)?;
        let engine = operation_pin.profile.engine;
        let history_origin = request.origin.unwrap_or_else(|| "manual".into());
        let is_write = !matches!(classification.kind, QueryKind::Read);

        if is_write && !operation_pin.profile.workspace_access.can_write() {
            return Err(DesktopSqlRunError::Blocked(DesktopSqlRunBlocked {
                reason: "your workspace role grants read-only database access".into(),
                _scope: operation_scope,
            }));
        }

        let decision = safety::decide(&settings, &classification);
        let blocked_reason = match &decision {
            safety::GateDecision::Block { reason } => Some(reason.clone()),
            safety::GateDecision::RequireApproval if !request.approved => {
                Some("this statement modifies data and requires explicit approval".into())
            }
            _ => None,
        };
        if let Some(reason) = blocked_reason {
            record_desktop_run(
                &self.store,
                &operation_pin,
                DesktopRunRecord {
                    sql: &request.sql,
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

        // Legacy compatibility: an auto-run gate decision clears the executor's
        // defense-in-depth approval check even when the caller supplied false.
        let authorized = request.approved || matches!(decision, safety::GateDecision::AutoRun);

        // A committed mutation must have a durable attempt row before target touch.
        // This remains fail-closed until Operation Runtime owns the event ledger.
        if is_write {
            audit::record(
                &self.store,
                RecordArgs {
                    connection_id: operation_pin.connection_id,
                    engine,
                    agent_prompt: None,
                    sql: request.sql.clone(),
                    kind: classification.kind,
                    action: "execute:attempt".into(),
                    approved_by: None,
                    affected_estimate: None,
                    error: None,
                },
            )
            .await
            .map_err(|error| {
                DesktopSqlRunError::Application(AppError::Config(format!(
                    "audit pre-record failed — refusing to run write: {error}"
                )))
            })?;
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
                        sql: &request.sql,
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
                        sql: &request.sql,
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
            &request.sql,
            &settings,
            authorized,
            request.query_id,
        )
        .await
        {
            Ok(outcome) => {
                if matches!(classification.kind, QueryKind::Ddl) && outcome.committed {
                    let _ = self
                        .store
                        .clear_schema_cache(operation_pin.connection_id)
                        .await;
                }
                let row_count = outcome
                    .result
                    .as_ref()
                    .map(|result| result.row_count as i64)
                    .or_else(|| outcome.affected.map(|affected| affected as i64));
                let duration_ms = outcome
                    .result
                    .as_ref()
                    .map(|result| result.duration_ms as i64);
                record_desktop_run(
                    &self.store,
                    &operation_pin,
                    DesktopRunRecord {
                        sql: &request.sql,
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
                record_desktop_run(
                    &self.store,
                    &operation_pin,
                    DesktopRunRecord {
                        sql: &request.sql,
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
        let identity = PlannedConnectionIdentity::from(context.pin());
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
        // Start the TTL only after best-effort audit work. A contended local audit
        // must not hand the caller an already-aged or immediately expired plan.
        {
            let mut plans = self
                .plans
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let published_at = Instant::now();
            plans.insert_at(
                plan_id,
                StoredReadPlan {
                    connection_id: profile.id,
                    identity,
                    sql: request.sql.clone(),
                    max_rows,
                    decision: decision.clone(),
                    origin: request.origin,
                    created_at: published_at,
                    insertion_order: 0,
                },
                published_at,
            );
        }

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

    /// Consume a plan synchronously before the first await, then re-pin and compare
    /// every scope/material identity field. The returned capability retains that
    /// scope through the adapter's tool-call event and the eventual connection.
    pub(crate) async fn prepare_agent_run(
        &self,
        plan_id: Uuid,
    ) -> Result<PreparedAgentQueryRun, AgentQueryRunPrepareError> {
        // Deliberately do not move this lock operation below an await. Removal is
        // the linearization point that guarantees single-use across transports.
        let claimed = {
            let mut plans = self
                .plans
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let claimed_at = Instant::now();
            plans.claim_at(plan_id, claimed_at)
        };
        let plan = match claimed {
            Ok(plan) => plan,
            Err(PlanClaimError::Missing) => {
                return Err(AgentQueryRunPrepareError::UnknownOrAlreadyUsed)
            }
            Err(PlanClaimError::Expired) => return Err(AgentQueryRunPrepareError::Expired),
        };

        let context = self
            .connections
            .pin(plan.connection_id, ConnectionAccess::Read)
            .await
            .map_err(AgentQueryRunPrepareError::Application)?;
        if PlannedConnectionIdentity::from(context.pin()) != plan.identity {
            return Err(AgentQueryRunPrepareError::AuthorityChanged);
        }
        let classification = safety::classify(&plan.sql, context.pin().profile.engine)
            .map_err(AgentQueryRunPrepareError::Application)?;
        if !matches!(classification.kind, QueryKind::Read) || classification.statement_count != 1 {
            return Err(AgentQueryRunPrepareError::StoredPlanInvalid);
        }

        let operation_pin = context.pin().clone();
        let event_context = AgentQueryRunEventContext {
            connection_id: operation_pin.connection_id,
            connection_name: operation_pin.profile.name.clone(),
            plan_id,
            sql: plan.sql,
        };
        Ok(PreparedAgentQueryRun {
            store: self.store.clone(),
            context,
            operation_pin,
            event_context,
            decision: plan.decision,
            max_rows: plan.max_rows,
            origin: plan.origin,
        })
    }

    /// Seed a plan only in crate tests so compatibility adapters can preserve their
    /// deterministic expired-plan case without a production registry mutation API.
    #[cfg(test)]
    pub(crate) fn seed_plan_for_test(
        &self,
        plan_id: Uuid,
        pin: &PinnedConnection,
        sql: String,
        max_rows: u64,
        decision: String,
        created_at: Instant,
    ) {
        self.plans
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .seed(
                plan_id,
                StoredReadPlan {
                    connection_id: pin.connection_id,
                    identity: PlannedConnectionIdentity::from(pin),
                    sql,
                    max_rows: max_rows.min(MAX_AGENT_ROWS),
                    decision,
                    origin: AgentQueryInvocationOrigin::Mcp,
                    created_at,
                    insertion_order: 0,
                },
            );
    }
}

/// Non-secret authority snapshot frozen into a plan. The account scope plus
/// generation rejects both cross-account reuse and A → B → A re-selection.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PlannedConnectionIdentity {
    workspace_id: Uuid,
    account_scope: AccountScope,
    scope_generation: i64,
    connection_revision: i64,
    binding_revision: i64,
    binding_updated_at: String,
}

impl From<&PinnedConnection> for PlannedConnectionIdentity {
    fn from(pin: &PinnedConnection) -> Self {
        Self {
            workspace_id: pin.scope.workspace_id,
            account_scope: pin.scope.account_scope.clone(),
            scope_generation: pin.scope.generation,
            connection_revision: pin.connection_revision,
            binding_revision: pin.binding_revision,
            binding_updated_at: pin.binding_updated_at.clone(),
        }
    }
}

#[derive(Debug, Clone)]
struct StoredReadPlan {
    connection_id: Uuid,
    identity: PlannedConnectionIdentity,
    sql: String,
    max_rows: u64,
    decision: String,
    origin: AgentQueryInvocationOrigin,
    created_at: Instant,
    insertion_order: u64,
}

impl StoredReadPlan {
    fn is_expired_at(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.created_at) > QUERY_PLAN_TTL
    }
}

#[derive(Default)]
struct QueryPlanRegistry {
    plans: HashMap<Uuid, StoredReadPlan>,
    next_insertion_order: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlanClaimError {
    Missing,
    Expired,
}

impl QueryPlanRegistry {
    fn insert_at(&mut self, id: Uuid, mut plan: StoredReadPlan, now: Instant) {
        self.plans.retain(|_, plan| !plan.is_expired_at(now));
        if !self.plans.contains_key(&id) && self.plans.len() >= MAX_QUERY_PLANS {
            if let Some(oldest) = self
                .plans
                .iter()
                .min_by_key(|(_, plan)| (plan.created_at, plan.insertion_order))
                .map(|(id, _)| *id)
            {
                self.plans.remove(&oldest);
            }
        }
        plan.insertion_order = self.next_insertion_order;
        self.next_insertion_order = self.next_insertion_order.wrapping_add(1);
        self.plans.insert(id, plan);
    }

    fn claim_at(&mut self, id: Uuid, now: Instant) -> Result<StoredReadPlan, PlanClaimError> {
        let plan = self.plans.remove(&id).ok_or(PlanClaimError::Missing)?;
        if plan.is_expired_at(now) {
            Err(PlanClaimError::Expired)
        } else {
            Ok(plan)
        }
    }

    #[cfg(test)]
    fn seed(&mut self, id: Uuid, mut plan: StoredReadPlan) {
        plan.insertion_order = self.next_insertion_order;
        self.next_insertion_order = self.next_insertion_order.wrapping_add(1);
        self.plans.insert(id, plan);
    }
}

fn bounded_max_rows(requested: Option<u64>, configured: u64) -> u64 {
    requested.unwrap_or(configured).min(MAX_AGENT_ROWS)
}

fn desktop_preview_connection_access(
    classification: &Classification,
    settings: &SafetySettings,
) -> ConnectionAccess {
    if matches!(classification.kind, QueryKind::Write) && settings.explain_preview {
        ConnectionAccess::Write
    } else {
        ConnectionAccess::Read
    }
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
    use std::sync::{Arc, Barrier};
    use std::thread;

    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use tempfile::TempDir;

    use super::*;
    use crate::model::{
        Provider, RiskLevel, WorkspaceConnectionAccess, WorkspaceCredentialMode, WorkspaceKind,
    };
    use crate::store::{ActiveResourceScope, CatalogCachePolicy, TEST_SCHEMA};

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

    fn pin() -> PinnedConnection {
        let connection_id = Uuid::from_u128(2);
        PinnedConnection {
            scope: ActiveResourceScope {
                workspace_id: Uuid::from_u128(1),
                workspace_kind: WorkspaceKind::Team,
                selected_account_id: Some("account-a".into()),
                account_scope: AccountScope::WorkspaceUser("account-a".into()),
                generation: 7,
            },
            connection_id,
            connection_revision: 3,
            binding_revision: 2,
            binding_updated_at: "2026-07-24T00:00:00Z".into(),
            profile: profile(connection_id, "test.db".into()),
            requires_remote_rbac: true,
            catalog_cache_policy: CatalogCachePolicy::Persistent,
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

    fn stored_plan(created_at: Instant, identity: PlannedConnectionIdentity) -> StoredReadPlan {
        StoredReadPlan {
            connection_id: Uuid::from_u128(2),
            identity,
            sql: "SELECT 1".into(),
            max_rows: 1,
            decision: "ready".into(),
            origin: AgentQueryInvocationOrigin::Mcp,
            created_at,
            insertion_order: 0,
        }
    }

    #[test]
    fn desktop_preview_access_preserves_legacy_access_matrix() {
        let mut settings = SafetySettings {
            allow_writes: true,
            explain_preview: true,
            ..SafetySettings::default()
        };

        assert_eq!(
            desktop_preview_connection_access(&classification(QueryKind::Read, false), &settings),
            ConnectionAccess::Read
        );
        assert_eq!(
            desktop_preview_connection_access(&classification(QueryKind::Write, true), &settings),
            ConnectionAccess::Write
        );
        assert_eq!(
            desktop_preview_connection_access(&classification(QueryKind::Write, false), &settings),
            ConnectionAccess::Write
        );
        assert_eq!(
            desktop_preview_connection_access(&classification(QueryKind::Ddl, false), &settings),
            ConnectionAccess::Read
        );
        assert_eq!(
            desktop_preview_connection_access(
                &classification(QueryKind::Privilege, false),
                &settings
            ),
            ConnectionAccess::Read
        );

        settings.explain_preview = false;
        assert_eq!(
            desktop_preview_connection_access(&classification(QueryKind::Write, true), &settings),
            ConnectionAccess::Read
        );
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
    fn ttl_is_valid_at_exact_boundary_and_expired_immediately_after() {
        let created_at = Instant::now();
        let identity = PlannedConnectionIdentity::from(&pin());
        let exact_id = Uuid::new_v4();
        let expired_id = Uuid::new_v4();
        let mut registry = QueryPlanRegistry::default();
        registry.insert_at(
            exact_id,
            stored_plan(created_at, identity.clone()),
            created_at,
        );
        registry.insert_at(expired_id, stored_plan(created_at, identity), created_at);

        assert!(registry
            .claim_at(exact_id, created_at + QUERY_PLAN_TTL)
            .is_ok());
        assert!(matches!(
            registry.claim_at(
                expired_id,
                created_at + QUERY_PLAN_TTL + Duration::from_nanos(1)
            ),
            Err(PlanClaimError::Expired)
        ));
    }

    #[test]
    fn insert_prunes_expired_then_evicts_only_the_oldest_live_plan() {
        let base = Instant::now();
        let identity = PlannedConnectionIdentity::from(&pin());
        let expired_id = Uuid::new_v4();
        let oldest_live_id = Uuid::new_v4();
        let mut registry = QueryPlanRegistry::default();
        registry.seed(expired_id, stored_plan(base, identity.clone()));
        let now = base + QUERY_PLAN_TTL + Duration::from_nanos(1);
        registry.insert_at(oldest_live_id, stored_plan(now, identity.clone()), now);
        assert!(!registry.plans.contains_key(&expired_id));

        for offset in 1..MAX_QUERY_PLANS {
            let at = now + Duration::from_nanos(offset as u64);
            registry.insert_at(Uuid::new_v4(), stored_plan(at, identity.clone()), at);
        }
        assert_eq!(registry.plans.len(), MAX_QUERY_PLANS);

        let newest_id = Uuid::new_v4();
        let newest_at = now + Duration::from_secs(1);
        registry.insert_at(newest_id, stored_plan(newest_at, identity), newest_at);
        assert_eq!(registry.plans.len(), MAX_QUERY_PLANS);
        assert!(!registry.plans.contains_key(&oldest_live_id));
        assert!(registry.plans.contains_key(&newest_id));
    }

    #[test]
    fn concurrent_claim_has_exactly_one_winner() {
        const CLAIMANTS: usize = 16;
        let now = Instant::now();
        let id = Uuid::new_v4();
        let registry = Arc::new(Mutex::new(QueryPlanRegistry::default()));
        registry.lock().unwrap().seed(
            id,
            stored_plan(now, PlannedConnectionIdentity::from(&pin())),
        );
        let barrier = Arc::new(Barrier::new(CLAIMANTS + 1));
        let mut threads = Vec::new();
        for _ in 0..CLAIMANTS {
            let registry = Arc::clone(&registry);
            let barrier = Arc::clone(&barrier);
            threads.push(thread::spawn(move || {
                barrier.wait();
                registry.lock().unwrap().claim_at(id, now).is_ok()
            }));
        }
        barrier.wait();
        let winners = threads
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .filter(|won| *won)
            .count();
        assert_eq!(winners, 1);
    }

    #[test]
    fn identity_covers_every_scope_and_material_revision_field() {
        let original = PlannedConnectionIdentity::from(&pin());

        let mut workspace = original.clone();
        workspace.workspace_id = Uuid::new_v4();
        assert_ne!(original, workspace);

        let mut account = original.clone();
        account.account_scope = AccountScope::WorkspaceUser("account-b".into());
        assert_ne!(original, account);

        let mut generation = original.clone();
        generation.scope_generation += 1;
        assert_ne!(original, generation);

        let mut connection = original.clone();
        connection.connection_revision += 1;
        assert_ne!(original, connection);

        let mut binding_revision = original.clone();
        binding_revision.binding_revision += 1;
        assert_ne!(original, binding_revision);

        let mut binding_time = original.clone();
        binding_time.binding_updated_at = "2026-07-24T00:00:01Z".into();
        assert_ne!(original, binding_time);
    }

    #[test]
    fn row_cap_is_frozen_in_the_stored_plan() {
        assert_eq!(bounded_max_rows(Some(5_000), 25), MAX_AGENT_ROWS);
        assert_eq!(bounded_max_rows(None, 5_000), MAX_AGENT_ROWS);
        assert_eq!(bounded_max_rows(Some(7), 500), 7);

        let now = Instant::now();
        let id = Uuid::new_v4();
        let mut registry = QueryPlanRegistry::default();
        let mut plan = stored_plan(now, PlannedConnectionIdentity::from(&pin()));
        plan.max_rows = bounded_max_rows(Some(7), 500);
        registry.insert_at(id, plan, now);
        assert_eq!(registry.claim_at(id, now).unwrap().max_rows, 7);
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
            let service = QueryService::new(store.clone(), connections.clone());
            Self {
                service,
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
                store,
                connections,
                directory,
                ..
            } = self;
            drop(service);
            drop(connections);
            store.pool().close().await;
            drop(store);
            directory
                .close()
                .expect("temporary SQLite directory must be removable after pool shutdown");
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
    async fn desktop_rollback_preview_requires_write_and_rolls_back_exactly() {
        let harness = SqliteHarness::new().await;
        let mut writable = harness.profile.clone();
        writable.allow_writes = true;
        harness.store.upsert_connection(&writable).await.unwrap();
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
        assert_eq!(receipt.report.mode, PreviewMode::ExecRollback);
        assert_eq!(receipt.report.exact_rows, Some(1));
        drop(receipt);
        assert_eq!(harness.user_name(1).await, "Ada");
        harness.close().await;
    }

    #[tokio::test]
    async fn desktop_nonrollback_write_keeps_legacy_write_authorization() {
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

        let error = match harness
            .service
            .preview_desktop_sql(DesktopSqlPreviewRequest {
                connection_id: harness.connection_id,
                sql: "this is not sql".into(),
            })
            .await
        {
            Err(error) => error.into_error(),
            Ok(_) => panic!("legacy non-rollback-safe write preview must request Write access"),
        };
        assert!(matches!(
            error,
            AppError::Blocked { reason }
                if reason == "your workspace role does not permit this database action"
        ));
        harness.close().await;
    }

    #[tokio::test]
    async fn desktop_ddl_preview_connects_then_returns_exact_l3_skip() {
        let harness = SqliteHarness::new().await;
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
    async fn desktop_ddl_preview_preserves_legacy_target_touch_order() {
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

        let error = match harness
            .service
            .preview_desktop_sql(DesktopSqlPreviewRequest {
                connection_id: harness.connection_id,
                sql: "CREATE TABLE preview_only (id INTEGER PRIMARY KEY)".into(),
            })
            .await
        {
            Err(error) => error.into_error(),
            Ok(_) => panic!("legacy DDL preview must touch the target before the L3 skip"),
        };
        assert!(
            matches!(error, AppError::Db(_)),
            "missing target must surface the legacy database-open error, got {error}"
        );
        harness.close().await;
    }

    #[tokio::test]
    async fn desktop_sql_read_preserves_wire_provenance_and_lease_guard() {
        let harness = SqliteHarness::new().await;
        let receipt = harness
            .service
            .run_desktop_sql(DesktopSqlRunRequest {
                connection_id: harness.connection_id,
                sql: "SELECT id, name FROM users ORDER BY id".into(),
                approved: false,
                query_id: None,
                origin: Some("data-view".into()),
            })
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
    async fn desktop_sql_write_gate_preserves_exact_error_audit_and_scope() {
        let harness = SqliteHarness::new().await;
        let blocked = match harness
            .service
            .run_desktop_sql(DesktopSqlRunRequest {
                connection_id: harness.connection_id,
                sql: "UPDATE users SET name = 'Grace' WHERE id = 1".into(),
                approved: true,
                query_id: None,
                origin: None,
            })
            .await
        {
            Err(DesktopSqlRunError::Blocked(blocked)) => blocked,
            _ => panic!("writes-disabled policy must reject before target touch"),
        };
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                harness.connections.begin_scope_mutation(),
            )
            .await
            .is_err(),
            "blocked desktop SQL error must retain its scope through adapter mapping"
        );
        let error = blocked.into_error();
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            serde_json::json!({
                "kind": "blocked",
                "message": "blocked: writing is disabled for this connection (writes are off by default). Enable writes in the connection's safety settings to propose it."
            })
        );
        assert_eq!(harness.user_name(1).await, "Ada");
        assert_eq!(
            harness.audit_actions_in_order().await,
            ["blocked".to_string()]
        );
        let history = harness
            .store
            .list_history(harness.connection_id)
            .await
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].status, "blocked");
        assert_eq!(history[0].origin, "manual");
        assert_eq!(
            history[0].error.as_deref(),
            Some(
                "writing is disabled for this connection (writes are off by default). Enable writes in the connection's safety settings to propose it."
            )
        );
        harness.close().await;
    }

    #[tokio::test]
    async fn desktop_sql_workspace_view_role_blocks_without_audit_or_target_touch() {
        let harness = SqliteHarness::new().await;
        harness.enable_writes(true).await;
        harness.set_connection_access_for_test("view").await;

        let error = match harness
            .service
            .run_desktop_sql(DesktopSqlRunRequest {
                connection_id: harness.connection_id,
                sql: "UPDATE users SET name = 'Grace' WHERE id = 1".into(),
                approved: true,
                query_id: None,
                origin: None,
            })
            .await
        {
            Err(error) => error.into_error(),
            Ok(_) => panic!("workspace read role must reject mutations"),
        };
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            serde_json::json!({
                "kind": "blocked",
                "message": "blocked: workspace role cannot execute this connection"
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
    async fn desktop_sql_exact_approval_compatibility_commits_and_records_both_ledgers() {
        let harness = SqliteHarness::new().await;
        harness.enable_writes(true).await;
        let sql = "UPDATE users SET name = 'Grace' WHERE id = 1";

        let rejected = match harness
            .service
            .run_desktop_sql(DesktopSqlRunRequest {
                connection_id: harness.connection_id,
                sql: sql.into(),
                approved: false,
                query_id: None,
                origin: Some("sql".into()),
            })
            .await
        {
            Err(error) => error.into_error(),
            Ok(_) => panic!("legacy approval-required write must remain blocked"),
        };
        assert!(matches!(
            rejected,
            AppError::Blocked { reason }
                if reason == "this statement modifies data and requires explicit approval"
        ));

        let receipt = harness
            .service
            .run_desktop_sql(DesktopSqlRunRequest {
                connection_id: harness.connection_id,
                sql: sql.into(),
                approved: true,
                query_id: None,
                origin: Some("sql".into()),
            })
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
            ["blocked", "execute:attempt", "execute"]
        );
        let history = harness
            .store
            .list_history(harness.connection_id)
            .await
            .unwrap();
        assert_eq!(history.len(), 2);
        assert!(history
            .iter()
            .any(|entry| entry.status == "blocked" && entry.origin == "sql"));
        assert!(history.iter().any(|entry| {
            entry.status == "ok"
                && entry.origin == "sql"
                && entry.kind == QueryKind::Write
                && entry.row_count == Some(1)
        }));
        harness.close().await;
    }

    #[tokio::test]
    async fn desktop_sql_legacy_auto_run_write_still_authorizes_the_executor() {
        let harness = SqliteHarness::new().await;
        harness.enable_writes(false).await;
        let receipt = harness
            .service
            .run_desktop_sql(DesktopSqlRunRequest {
                connection_id: harness.connection_id,
                sql: "UPDATE users SET name = 'Grace' WHERE id = 1".into(),
                approved: false,
                query_id: None,
                origin: None,
            })
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

        let error = match harness
            .service
            .run_desktop_sql(DesktopSqlRunRequest {
                connection_id: harness.connection_id,
                sql: "UPDATE users SET name = 'Grace' WHERE id = 1".into(),
                approved: true,
                query_id: None,
                origin: None,
            })
            .await
        {
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
        let failure = match harness
            .service
            .run_desktop_sql(DesktopSqlRunRequest {
                connection_id: harness.connection_id,
                sql: "UPDATE missing_users SET name = 'Grace' WHERE id = 1".into(),
                approved: true,
                query_id: None,
                origin: None,
            })
            .await
        {
            Err(DesktopSqlRunError::Execution(failure)) => failure,
            _ => panic!("missing target table must fail during desktop execution"),
        };
        let original = failure.error.to_string();
        assert!(original.contains("missing_users"));
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
            .is_some_and(|message| message.contains("missing_users")));
        assert_eq!(harness.user_name(1).await, "Ada");
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

        let receipt = harness
            .service
            .run_desktop_sql(DesktopSqlRunRequest {
                connection_id: harness.connection_id,
                sql: "CREATE TABLE widgets (id INTEGER PRIMARY KEY)".into(),
                approved: true,
                query_id: None,
                origin: None,
            })
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
    async fn service_clones_claim_one_shared_registry() {
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
        harness.service.seed_plan_for_test(
            plan_id,
            &pin,
            "SELECT no_such_function()".into(),
            1,
            "ready".into(),
            Instant::now(),
        );
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
        harness.service.seed_plan_for_test(
            plan_id,
            &pin,
            "SELECT no_such_function()".into(),
            1,
            "ready".into(),
            Instant::now(),
        );
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
        harness.service.seed_plan_for_test(
            plan_id,
            &current_pin,
            "SELECT 1".into(),
            1,
            "ready".into(),
            Instant::now(),
        );

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
        harness.service.seed_plan_for_test(
            plan_id,
            &current_pin,
            "SELECT 1".into(),
            1,
            "ready".into(),
            Instant::now() - QUERY_PLAN_TTL - Duration::from_secs(1),
        );

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
