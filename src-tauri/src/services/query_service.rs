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
    ConnectionAccess, ConnectionContext, ConnectionLease, ConnectionManager, DbPool,
};
use crate::error::AppError;
use crate::model::{ConnectionProfile, Engine, HistoryEntry, QueryKind, QueryResult};
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
        Provider, WorkspaceConnectionAccess, WorkspaceCredentialMode, WorkspaceKind,
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
            Duration::from_secs(1),
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
            Duration::from_secs(1),
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
