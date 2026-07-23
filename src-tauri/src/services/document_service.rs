//! Transport-neutral, typed document reads for MongoDB.
//!
//! The service owns the authority pin, read-only classification, row cap, execution,
//! audit, and history lifecycle. Adapters receive only allowlisted display/result
//! DTOs; connection profiles and credential references never cross this boundary.

use std::fmt;
use std::time::Duration;

use chrono::Utc;
use uuid::Uuid;

use crate::audit::{self, RecordArgs};
use crate::connection::{
    ConnectionAccess, ConnectionContext, ConnectionLease, ConnectionManager,
    ConnectionOperationScope,
};
use crate::error::AppError;
use crate::executor;
#[cfg(test)]
use crate::model::Engine;
use crate::model::{DocumentPage, DocumentQuery, HistoryEntry, QueryKind, SafetySettings};
use crate::safety::{self, GateDecision};
use crate::store::{PinnedConnection, Store};

use super::query_service::{AgentQueryInvocationOrigin, MAX_AGENT_ROWS};

const MAX_DESKTOP_ROWS: u64 = 100_000;

/// Agent-facing input after the adapter has resolved its legacy connection selector.
#[derive(Debug, Clone)]
pub(crate) struct AgentDocumentReadRequest {
    connection_id: Uuid,
    query: DocumentQuery,
    /// Frozen canonical form used by both the pre-execution tool-call event and
    /// every later audit/history/result record.
    query_text: String,
    max_rows: Option<u64>,
    origin: AgentQueryInvocationOrigin,
}

impl AgentDocumentReadRequest {
    pub(crate) fn try_new(
        connection_id: Uuid,
        query: DocumentQuery,
        max_rows: Option<u64>,
        origin: AgentQueryInvocationOrigin,
    ) -> Result<Self, AppError> {
        let query_text = serde_json::to_string(&query)?;
        Ok(Self {
            connection_id,
            query,
            query_text,
            max_rows,
            origin,
        })
    }

    /// Canonical text for the adapter's compatibility `agent:tool_call`. Returning
    /// it from the immutable request prevents the event from describing a different
    /// request than the service later executes and audits.
    pub(crate) fn query_text(&self) -> &str {
        &self.query_text
    }
}

/// Desktop-facing input preserving the current approval, cancellation, and history
/// attribution contract.
#[derive(Debug, Clone)]
pub(crate) struct DesktopDocumentReadRequest {
    pub(crate) connection_id: Uuid,
    pub(crate) query: DocumentQuery,
    pub(crate) approved: bool,
    pub(crate) query_id: Option<Uuid>,
    pub(crate) origin: Option<String>,
}

/// Explicitly allowlisted fields needed to render a document tool event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DocumentReadEventContext {
    pub(crate) connection_id: Uuid,
    pub(crate) connection_name: String,
    /// Canonical JSON stored in the legacy audit/history `sql` column.
    pub(crate) query_text: String,
}

/// Successful typed document read. It intentionally contains no host, username,
/// credential reference, workspace binding, or full connection profile.
#[derive(Debug, Clone)]
pub(crate) struct DocumentReadResult {
    pub(crate) context: DocumentReadEventContext,
    pub(crate) query: DocumentQuery,
    pub(crate) page: DocumentPage,
}

/// Successful result whose lease keeps the exact workspace/account scope pinned
/// while the adapter builds and emits its response.
pub(crate) struct DocumentReadReceipt {
    result: DocumentReadResult,
    _lease: ConnectionLease,
}

impl DocumentReadReceipt {
    pub(crate) fn result(&self) -> &DocumentReadResult {
        &self.result
    }
}

/// Preserve the desktop command's exact `DocumentPage` JSON wire shape while the
/// receipt (and therefore its scope lease) remains alive through Tauri serialization.
impl serde::Serialize for DocumentReadReceipt {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serde::Serialize::serialize(&self.result.page, serializer)
    }
}

/// Agent-path failures with stable distinctions for MCP/CLI error mapping.
#[derive(Debug)]
pub(crate) enum AgentDocumentReadError {
    /// `run_document_query` was used with a SQL-family connection.
    NonDocumentConnection,
    /// Typed classification rejected an unsafe operator/stage.
    Rejected(Box<RejectedAgentDocumentRead>),
    /// Pinning, safety settings, connection, or backend selection failed.
    Application(AppError),
    /// MongoDB accepted the read shape but execution failed.
    Execution(Box<AgentDocumentExecutionFailure>),
}

/// A rejected typed request that retains its authority scope until the adapter has
/// emitted the compatibility `agent:result` error. The audit entry is already
/// durable/best-effort when this token is returned, preserving MCP's audit-before-
/// result ordering.
pub(crate) struct RejectedAgentDocumentRead {
    context: DocumentReadEventContext,
    message: String,
    _authority: ConnectionContext,
}

impl fmt::Debug for RejectedAgentDocumentRead {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RejectedAgentDocumentRead")
            .field("connection_id", &self.context.connection_id)
            .field("connection_name", &self.context.connection_name)
            .field("message", &self.message)
            .finish_non_exhaustive()
    }
}

impl RejectedAgentDocumentRead {
    pub(crate) fn event_context(&self) -> &DocumentReadEventContext {
        &self.context
    }

    pub(crate) fn message(&self) -> &str {
        &self.message
    }

    /// Consume only after the adapter emitted its error result.
    pub(crate) fn into_message(self) -> String {
        self.message
    }
}

/// Execution failure retaining the live lease through the adapter's error event.
pub(crate) struct AgentDocumentExecutionFailure {
    context: DocumentReadEventContext,
    error: AppError,
    _lease: ConnectionLease,
}

impl fmt::Debug for AgentDocumentExecutionFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentDocumentExecutionFailure")
            .field("connection_id", &self.context.connection_id)
            .field("connection_name", &self.context.connection_name)
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl AgentDocumentExecutionFailure {
    pub(crate) fn event_context(&self) -> &DocumentReadEventContext {
        &self.context
    }

    pub(crate) fn error(&self) -> &AppError {
        &self.error
    }

    /// Consume only after the adapter emitted its error result.
    pub(crate) fn into_error(self) -> AppError {
        self.error
    }
}

/// Desktop-path failures preserve the command's existing structured `AppError`
/// contract while keeping guards alive until the thin adapter performs the mapping.
#[derive(Debug)]
pub(crate) enum DesktopDocumentReadError {
    NonDocumentConnection,
    Blocked(DesktopDocumentBlocked),
    Application(AppError),
    Execution(Box<DesktopDocumentExecutionFailure>),
}

impl DesktopDocumentReadError {
    pub(crate) fn into_error(self) -> AppError {
        match self {
            Self::NonDocumentConnection => AppError::Config(
                "document queries are only available on MongoDB connections".into(),
            ),
            Self::Blocked(blocked) => blocked.into_error(),
            Self::Application(error) => error,
            Self::Execution(failure) => failure.into_error(),
        }
    }
}

/// A blocked desktop request retains the pre-connection operation scope until the
/// command maps it back to `AppError::Blocked`.
pub(crate) struct DesktopDocumentBlocked {
    reason: String,
    _scope: ConnectionOperationScope,
}

impl fmt::Debug for DesktopDocumentBlocked {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DesktopDocumentBlocked")
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}

impl DesktopDocumentBlocked {
    fn into_error(self) -> AppError {
        AppError::Blocked {
            reason: self.reason,
        }
    }
}

/// A failed desktop execution retains the live lease until command error mapping.
pub(crate) struct DesktopDocumentExecutionFailure {
    error: AppError,
    _lease: ConnectionLease,
}

impl fmt::Debug for DesktopDocumentExecutionFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DesktopDocumentExecutionFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl DesktopDocumentExecutionFailure {
    fn into_error(self) -> AppError {
        self.error
    }
}

/// Scope-aware typed document service shared by desktop, MCP, and future CLI
/// adapters.
#[derive(Clone)]
pub(crate) struct DocumentService {
    store: Store,
    connections: ConnectionManager,
}

impl DocumentService {
    pub(super) fn new(store: Store, connections: ConnectionManager) -> Self {
        Self { store, connections }
    }

    /// Execute one typed read for a local agent adapter.
    ///
    /// This keeps MCP's established behavior: non-read typed requests are audited
    /// but do not create history; connection/setup failures do not synthesize an
    /// `agent:result`; execution outcomes use best-effort audit/history; and success
    /// does not mint a SQL dashboard consent handle.
    pub(crate) async fn run_agent_read(
        &self,
        request: AgentDocumentReadRequest,
    ) -> Result<DocumentReadReceipt, AgentDocumentReadError> {
        let authority = self
            .connections
            .pin(request.connection_id, ConnectionAccess::Read)
            .await
            .map_err(AgentDocumentReadError::Application)?;
        let pin = authority.pin().clone();
        let engine = pin.profile.engine;
        if !engine.is_document() {
            return Err(AgentDocumentReadError::NonDocumentConnection);
        }

        let event_context = DocumentReadEventContext {
            connection_id: pin.connection_id,
            connection_name: pin.profile.name.clone(),
            query_text: request.query_text.clone(),
        };
        let classification = crate::mongo::query::classify(&request.query);
        if !matches!(classification.kind, QueryKind::Read) {
            let message = classification
                .notes
                .first()
                .cloned()
                .unwrap_or_else(|| agent_rejection_fallback(request.origin).into());
            audit_best_effort(
                &self.store,
                &pin,
                &event_context.query_text,
                classification.kind,
                agent_audit_action(request.origin),
                None,
                Some(message.clone()),
            )
            .await;
            return Err(AgentDocumentReadError::Rejected(Box::new(
                RejectedAgentDocumentRead {
                    context: event_context,
                    message,
                    _authority: authority,
                },
            )));
        }

        let settings = self
            .store
            .get_safety(pin.connection_id)
            .await
            .map_err(AgentDocumentReadError::Application)?;
        let max_rows = bounded_agent_rows(request.max_rows, settings.max_rows);
        let lease = authority
            .connect()
            .await
            .map_err(AgentDocumentReadError::Application)?;
        let mongo = match lease.live().mongo() {
            Ok(mongo) => mongo,
            Err(error) => return Err(AgentDocumentReadError::Application(error)),
        };
        let page = match crate::mongo::query::run(
            mongo,
            &request.query,
            max_rows,
            Duration::from_millis(safety::STATEMENT_TIMEOUT_MS),
        )
        .await
        {
            Ok(page) => page,
            Err(error) => {
                record_agent_execution(
                    &self.store,
                    &pin,
                    &event_context.query_text,
                    request.origin,
                    None,
                    None,
                    Some(error.to_string()),
                )
                .await;
                return Err(AgentDocumentReadError::Execution(Box::new(
                    AgentDocumentExecutionFailure {
                        context: event_context,
                        error,
                        _lease: lease,
                    },
                )));
            }
        };

        record_agent_execution(
            &self.store,
            &pin,
            &event_context.query_text,
            request.origin,
            Some(page.doc_count as i64),
            Some(page.duration_ms as i64),
            None,
        )
        .await;
        Ok(DocumentReadReceipt {
            result: DocumentReadResult {
                context: event_context,
                query: request.query,
                page,
            },
            _lease: lease,
        })
    }

    /// Execute one typed read for the desktop command while preserving its L4
    /// approval, cancellation, row-limit, audit, and caller-supplied history origin.
    pub(crate) async fn run_desktop_read(
        &self,
        request: DesktopDocumentReadRequest,
    ) -> Result<DocumentReadReceipt, DesktopDocumentReadError> {
        let operation_scope = self.connections.begin_operation_scope().await;
        let pin = operation_scope
            .pin_connection(request.connection_id)
            .await
            .map_err(DesktopDocumentReadError::Application)?;
        let engine = pin.profile.engine;
        if !engine.is_document() {
            return Err(DesktopDocumentReadError::NonDocumentConnection);
        }
        let settings = self
            .store
            .get_safety(pin.connection_id)
            .await
            .map_err(DesktopDocumentReadError::Application)?;
        let classification = crate::mongo::query::classify(&request.query);
        let history_origin = request.origin.unwrap_or_else(|| "manual".into());
        let query_text = serde_json::to_string(&request.query)
            .map_err(AppError::from)
            .map_err(DesktopDocumentReadError::Application)?;

        if let Some(reason) = desktop_blocked_reason(&settings, &classification, request.approved) {
            record_desktop_outcome(
                &self.store,
                &pin,
                &query_text,
                classification.kind,
                "blocked",
                "blocked",
                None,
                None,
                Some(reason.clone()),
                &history_origin,
            )
            .await;
            return Err(DesktopDocumentReadError::Blocked(DesktopDocumentBlocked {
                reason,
                _scope: operation_scope,
            }));
        }

        let lease = match operation_scope
            .connect(pin.clone(), ConnectionAccess::Read)
            .await
        {
            Ok(lease) => lease,
            Err(error) => {
                record_desktop_outcome(
                    &self.store,
                    &pin,
                    &query_text,
                    classification.kind,
                    "error",
                    "error",
                    None,
                    None,
                    Some(error.to_string()),
                    &history_origin,
                )
                .await;
                return Err(DesktopDocumentReadError::Application(error));
            }
        };
        let mongo = match lease.live().mongo() {
            Ok(mongo) => mongo,
            Err(error) => return Err(DesktopDocumentReadError::Application(error)),
        };
        let max_rows = bounded_desktop_rows(settings.max_rows);
        let run = crate::mongo::query::run(
            mongo,
            &request.query,
            max_rows,
            executor::cancel::QUERY_TIMEOUT,
        );
        match executor::cancel::guard(request.query_id, executor::cancel::QUERY_TIMEOUT, run).await
        {
            Ok(page) => {
                record_desktop_outcome(
                    &self.store,
                    &pin,
                    &query_text,
                    QueryKind::Read,
                    "read",
                    "ok",
                    Some(page.doc_count as i64),
                    Some(page.duration_ms as i64),
                    None,
                    &history_origin,
                )
                .await;
                Ok(DocumentReadReceipt {
                    result: DocumentReadResult {
                        context: DocumentReadEventContext {
                            connection_id: pin.connection_id,
                            connection_name: pin.profile.name.clone(),
                            query_text,
                        },
                        query: request.query,
                        page,
                    },
                    _lease: lease,
                })
            }
            Err(error) => {
                record_desktop_outcome(
                    &self.store,
                    &pin,
                    &query_text,
                    QueryKind::Read,
                    "error",
                    "error",
                    None,
                    None,
                    Some(error.to_string()),
                    &history_origin,
                )
                .await;
                Err(DesktopDocumentReadError::Execution(Box::new(
                    DesktopDocumentExecutionFailure {
                        error,
                        _lease: lease,
                    },
                )))
            }
        }
    }
}

fn bounded_agent_rows(requested: Option<u64>, configured: u64) -> u64 {
    requested.unwrap_or(configured).min(MAX_AGENT_ROWS)
}

fn bounded_desktop_rows(configured: u64) -> u64 {
    configured.clamp(1, MAX_DESKTOP_ROWS)
}

fn agent_audit_action(origin: AgentQueryInvocationOrigin) -> &'static str {
    match origin {
        AgentQueryInvocationOrigin::Mcp => "mcp:run_document_query",
        AgentQueryInvocationOrigin::Cli => "cli:run_document_query",
    }
}

fn agent_rejection_fallback(origin: AgentQueryInvocationOrigin) -> &'static str {
    match origin {
        AgentQueryInvocationOrigin::Mcp => "document writes are not supported over MCP",
        AgentQueryInvocationOrigin::Cli => "document writes are not supported over CLI",
    }
}

fn agent_history_origin(_origin: AgentQueryInvocationOrigin) -> &'static str {
    // Keep both local agent transports dashboard-compatible until Operation Runtime
    // introduces a separate actor/source dimension.
    "agent"
}

fn desktop_blocked_reason(
    settings: &SafetySettings,
    classification: &crate::model::Classification,
    approved: bool,
) -> Option<String> {
    if !matches!(classification.kind, QueryKind::Read) {
        return Some(
            classification
                .notes
                .first()
                .cloned()
                .unwrap_or_else(|| "document writes are not supported".into()),
        );
    }
    match safety::decide(settings, classification) {
        GateDecision::Block { reason } => Some(reason),
        GateDecision::RequireApproval if !approved => {
            Some("this query requires explicit approval".into())
        }
        GateDecision::AutoRun | GateDecision::RequireApproval => None,
    }
}

/// MCP/CLI audit and history behavior. History is deliberately best-effort for
/// document reads and deliberately keeps the legacy `"agent"` origin.
async fn record_agent_execution(
    store: &Store,
    pin: &PinnedConnection,
    query_text: &str,
    origin: AgentQueryInvocationOrigin,
    rows: Option<i64>,
    duration_ms: Option<i64>,
    error: Option<String>,
) {
    audit_best_effort(
        store,
        pin,
        query_text,
        QueryKind::Read,
        agent_audit_action(origin),
        None,
        error.clone(),
    )
    .await;
    let status = if error.is_some() { "error" } else { "ok" };
    if let Err(history_error) = persist_history(
        store,
        pin,
        query_text,
        QueryKind::Read,
        status,
        rows,
        duration_ms,
        error,
        agent_history_origin(origin),
    )
    .await
    {
        tracing::error!("agent document-query history insert failed: {history_error}");
    }
}

#[allow(clippy::too_many_arguments)]
async fn record_desktop_outcome(
    store: &Store,
    pin: &PinnedConnection,
    query_text: &str,
    kind: QueryKind,
    action: &str,
    status: &str,
    rows: Option<i64>,
    duration_ms: Option<i64>,
    error: Option<String>,
    history_origin: &str,
) {
    audit_best_effort(store, pin, query_text, kind, action, rows, error.clone()).await;
    if let Err(history_error) = persist_history(
        store,
        pin,
        query_text,
        kind,
        status,
        rows,
        duration_ms,
        error,
        history_origin,
    )
    .await
    {
        tracing::error!("desktop document-query history insert failed: {history_error}");
    }
}

async fn audit_best_effort(
    store: &Store,
    pin: &PinnedConnection,
    query_text: &str,
    kind: QueryKind,
    action: &str,
    affected_estimate: Option<i64>,
    error: Option<String>,
) {
    if let Err(audit_error) = audit::record(
        store,
        RecordArgs {
            connection_id: pin.connection_id,
            engine: pin.profile.engine,
            agent_prompt: None,
            sql: query_text.to_string(),
            kind,
            action: action.to_string(),
            approved_by: None,
            affected_estimate,
            error,
        },
    )
    .await
    {
        tracing::error!("document-query audit insert failed: {audit_error}");
    }
}

#[allow(clippy::too_many_arguments)]
async fn persist_history(
    store: &Store,
    pin: &PinnedConnection,
    query_text: &str,
    kind: QueryKind,
    status: &str,
    rows: Option<i64>,
    duration_ms: Option<i64>,
    error: Option<String>,
    origin: &str,
) -> Result<(), AppError> {
    store
        .insert_history_if_current(
            pin,
            &HistoryEntry {
                id: Uuid::new_v4(),
                connection_id: pin.connection_id,
                sql: query_text.to_string(),
                kind,
                status: status.to_string(),
                row_count: rows,
                duration_ms,
                error,
                executed_at: Utc::now(),
                origin: origin.to_string(),
            },
        )
        .await
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::str::FromStr;

    use serde_json::json;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    use super::*;
    use crate::model::{
        ConnectionProfile, Provider, WorkspaceConnectionAccess, WorkspaceCredentialMode,
    };
    use crate::store::TEST_SCHEMA;

    fn profile(id: Uuid, engine: Engine) -> ConnectionProfile {
        ConnectionProfile {
            id,
            name: "document-service-test".into(),
            engine,
            provider: Provider::Generic,
            driver_id: Some(
                match engine {
                    Engine::Mongodb => "mongodb-rust",
                    _ => "sqlx-sqlite",
                }
                .into(),
            ),
            host: "sensitive-host.invalid".into(),
            port: if engine == Engine::Mongodb { 27_017 } else { 0 },
            database: if engine == Engine::Sqlite {
                ":memory:".into()
            } else {
                "test".into()
            },
            username: "sensitive-user".into(),
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

    fn safe_find() -> DocumentQuery {
        DocumentQuery::Find {
            collection: "users".into(),
            filter: Some(json!({ "active": true })),
            projection: None,
            sort: None,
            skip: None,
            limit: None,
        }
    }

    fn blocked_aggregate() -> DocumentQuery {
        DocumentQuery::Aggregate {
            collection: "users".into(),
            pipeline: vec![json!({ "$out": "copied_users" })],
        }
    }

    struct Harness {
        service: DocumentService,
        store: Store,
        connections: ConnectionManager,
        connection_id: Uuid,
    }

    impl Harness {
        async fn new(engine: Engine) -> Self {
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
                .upsert_connection(&profile(connection_id, engine))
                .await
                .unwrap();
            let connections = ConnectionManager::new(store.clone());
            let service = DocumentService::new(store.clone(), connections.clone());
            Self {
                service,
                store,
                connections,
                connection_id,
            }
        }

        async fn close(self) {
            let Self {
                service,
                store,
                connections,
                ..
            } = self;
            drop(service);
            drop(connections);
            store.pool().close().await;
        }
    }

    #[test]
    fn row_caps_preserve_agent_and_desktop_contracts() {
        assert_eq!(bounded_agent_rows(Some(5_000), 25), MAX_AGENT_ROWS);
        assert_eq!(bounded_agent_rows(None, 5_000), MAX_AGENT_ROWS);
        assert_eq!(bounded_agent_rows(Some(0), 500), 0);
        assert_eq!(bounded_desktop_rows(0), 1);
        assert_eq!(bounded_desktop_rows(250), 250);
        assert_eq!(bounded_desktop_rows(u64::MAX), MAX_DESKTOP_ROWS);
    }

    #[test]
    fn desktop_gate_requires_approval_without_weakening_typed_read_only() {
        let classification = crate::mongo::query::classify(&safe_find());
        let settings = SafetySettings {
            auto_run_reads: false,
            ..SafetySettings::default()
        };
        assert_eq!(
            desktop_blocked_reason(&settings, &classification, false).as_deref(),
            Some("this query requires explicit approval")
        );
        assert!(desktop_blocked_reason(&settings, &classification, true).is_none());

        let rejected = crate::mongo::query::classify(&blocked_aggregate());
        assert!(desktop_blocked_reason(&settings, &rejected, true)
            .is_some_and(|reason| reason.contains("$out")));
    }

    #[tokio::test]
    async fn desktop_receipt_serializes_as_the_exact_legacy_document_page() {
        let harness = Harness::new(Engine::Sqlite).await;
        let authority = harness
            .connections
            .pin(harness.connection_id, ConnectionAccess::Read)
            .await
            .unwrap();
        let lease = authority.connect().await.unwrap();
        let receipt = DocumentReadReceipt {
            result: DocumentReadResult {
                context: DocumentReadEventContext {
                    connection_id: harness.connection_id,
                    connection_name: "must-not-serialize".into(),
                    query_text: "must-not-serialize".into(),
                },
                query: safe_find(),
                page: DocumentPage {
                    documents: vec![json!({ "name": "Ada" })],
                    doc_count: 1,
                    truncated: false,
                    duration_ms: 7,
                },
            },
            _lease: lease,
        };

        assert_eq!(
            serde_json::to_value(&receipt).unwrap(),
            json!({
                "documents": [{ "name": "Ada" }],
                "docCount": 1,
                "truncated": false,
                "durationMs": 7,
            })
        );
        drop(receipt);
        harness.close().await;
    }

    #[test]
    fn agent_origin_splits_audit_and_preserves_history_origin() {
        assert_eq!(
            agent_audit_action(AgentQueryInvocationOrigin::Mcp),
            "mcp:run_document_query"
        );
        assert_eq!(
            agent_audit_action(AgentQueryInvocationOrigin::Cli),
            "cli:run_document_query"
        );
        assert_eq!(
            agent_history_origin(AgentQueryInvocationOrigin::Mcp),
            "agent"
        );
        assert_eq!(
            agent_history_origin(AgentQueryInvocationOrigin::Cli),
            "agent"
        );
    }

    #[tokio::test]
    async fn rejected_agent_query_is_audited_without_history_or_profile_leak() {
        let harness = Harness::new(Engine::Mongodb).await;
        let rejected = match harness
            .service
            .run_agent_read(
                AgentDocumentReadRequest::try_new(
                    harness.connection_id,
                    blocked_aggregate(),
                    None,
                    AgentQueryInvocationOrigin::Mcp,
                )
                .unwrap(),
            )
            .await
        {
            Err(AgentDocumentReadError::Rejected(rejected)) => rejected,
            Err(other) => panic!("expected typed rejection, got {other:?}"),
            Ok(_) => panic!("unsafe aggregate unexpectedly executed"),
        };
        assert_eq!(
            rejected.event_context().connection_id,
            harness.connection_id
        );
        assert_eq!(
            rejected.event_context().connection_name,
            "document-service-test"
        );
        assert!(rejected.message().contains("$out"));
        let debug = format!("{rejected:?}");
        assert!(!debug.contains("sensitive-host.invalid"));
        assert!(!debug.contains("sensitive-user"));

        let (audit, chain_ok, first_bad) = audit::snapshot(&harness.store, harness.connection_id)
            .await
            .unwrap();
        assert!(chain_ok);
        assert_eq!(first_bad, None);
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].action, "mcp:run_document_query");
        assert_eq!(audit[0].kind, QueryKind::Write);
        assert!(harness
            .store
            .list_history(harness.connection_id)
            .await
            .unwrap()
            .is_empty());
        let _ = rejected.into_message();
        harness.close().await;
    }

    #[tokio::test]
    async fn rejected_token_holds_scope_until_adapter_finishes() {
        let harness = Harness::new(Engine::Mongodb).await;
        let rejected = match harness
            .service
            .run_agent_read(
                AgentDocumentReadRequest::try_new(
                    harness.connection_id,
                    blocked_aggregate(),
                    None,
                    AgentQueryInvocationOrigin::Mcp,
                )
                .unwrap(),
            )
            .await
        {
            Err(AgentDocumentReadError::Rejected(rejected)) => rejected,
            Err(other) => panic!("expected typed rejection, got {other:?}"),
            Ok(_) => panic!("unsafe aggregate unexpectedly executed"),
        };

        let mutation_manager = harness.connections.clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let mut waiter = tokio::spawn(async move {
            let _ = started_tx.send(());
            mutation_manager.begin_scope_mutation().await
        });
        started_rx.await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut waiter)
                .await
                .is_err(),
            "scope mutation must wait while the adapter owns the rejection token"
        );
        let _ = rejected.into_message();
        let mutation = tokio::time::timeout(Duration::from_secs(5), waiter)
            .await
            .expect("scope mutation should resume after the token drops")
            .unwrap();
        drop(mutation);
        harness.close().await;
    }

    #[tokio::test]
    async fn desktop_block_keeps_legacy_audit_history_and_manual_origin() {
        let harness = Harness::new(Engine::Mongodb).await;
        let error = match harness
            .service
            .run_desktop_read(DesktopDocumentReadRequest {
                connection_id: harness.connection_id,
                query: blocked_aggregate(),
                approved: true,
                query_id: None,
                origin: None,
            })
            .await
        {
            Err(error) => error.into_error(),
            Ok(_) => panic!("unsafe aggregate unexpectedly executed"),
        };
        assert!(matches!(error, AppError::Blocked { .. }));

        let history = harness
            .store
            .list_history(harness.connection_id)
            .await
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].origin, "manual");
        assert_eq!(history[0].status, "blocked");
        assert_eq!(history[0].kind, QueryKind::Write);

        let (audit, chain_ok, first_bad) = audit::snapshot(&harness.store, harness.connection_id)
            .await
            .unwrap();
        assert!(chain_ok);
        assert_eq!(first_bad, None);
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].action, "blocked");
        assert_eq!(audit[0].kind, QueryKind::Write);
        harness.close().await;
    }

    #[tokio::test]
    async fn cli_provenance_uses_cli_audit_but_agent_history() {
        let harness = Harness::new(Engine::Mongodb).await;
        let pin = harness
            .store
            .pin_connection_for_read(harness.connection_id)
            .await
            .unwrap();
        record_agent_execution(
            &harness.store,
            &pin,
            r#"{"op":"count","collection":"users"}"#,
            AgentQueryInvocationOrigin::Cli,
            Some(1),
            Some(2),
            None,
        )
        .await;

        let history = harness
            .store
            .list_history(harness.connection_id)
            .await
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].origin, "agent");
        assert_eq!(history[0].status, "ok");
        assert_eq!(history[0].row_count, Some(1));
        let (audit, chain_ok, first_bad) = audit::snapshot(&harness.store, harness.connection_id)
            .await
            .unwrap();
        assert!(chain_ok);
        assert_eq!(first_bad, None);
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].action, "cli:run_document_query");
        assert_eq!(audit[0].affected_estimate, None);
        harness.close().await;
    }

    #[tokio::test]
    async fn agent_execution_error_preserves_audit_and_history_contract() {
        let harness = Harness::new(Engine::Mongodb).await;
        let pin = harness
            .store
            .pin_connection_for_read(harness.connection_id)
            .await
            .unwrap();
        let query_text = r#"{"op":"find","collection":"users"}"#;
        record_agent_execution(
            &harness.store,
            &pin,
            query_text,
            AgentQueryInvocationOrigin::Mcp,
            None,
            None,
            Some("backend unavailable".into()),
        )
        .await;

        let history = harness
            .store
            .list_history(harness.connection_id)
            .await
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].sql, query_text);
        assert_eq!(history[0].kind, QueryKind::Read);
        assert_eq!(history[0].status, "error");
        assert_eq!(history[0].origin, "agent");
        assert_eq!(history[0].error.as_deref(), Some("backend unavailable"));

        let (audit, chain_ok, first_bad) = audit::snapshot(&harness.store, harness.connection_id)
            .await
            .unwrap();
        assert!(chain_ok);
        assert_eq!(first_bad, None);
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].action, "mcp:run_document_query");
        assert_eq!(audit[0].kind, QueryKind::Read);
        assert_eq!(audit[0].affected_estimate, None);
        assert_eq!(audit[0].error.as_deref(), Some("backend unavailable"));
        harness.close().await;
    }

    #[tokio::test]
    async fn sql_connection_is_rejected_without_exposing_its_profile() {
        let harness = Harness::new(Engine::Sqlite).await;
        let error = match harness
            .service
            .run_agent_read(
                AgentDocumentReadRequest::try_new(
                    harness.connection_id,
                    safe_find(),
                    None,
                    AgentQueryInvocationOrigin::Mcp,
                )
                .unwrap(),
            )
            .await
        {
            Err(error) => error,
            Ok(_) => panic!("SQL connection unexpectedly accepted a document query"),
        };
        assert!(matches!(
            error,
            AgentDocumentReadError::NonDocumentConnection
        ));
        let debug = format!("{error:?}");
        assert!(!debug.contains("sensitive-host.invalid"));
        assert!(!debug.contains("sensitive-user"));
        harness.close().await;
    }
}
