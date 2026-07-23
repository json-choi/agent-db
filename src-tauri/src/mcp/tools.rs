//! MCP read-only tools that reuse the existing safety pipeline and connection manager.
//! Every query is first reviewed by `plan_query`; `run_query` accepts only the resulting
//! single-use id and still runs through the authoritative database read-only session.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use tauri::{AppHandle, Emitter};
use uuid::Uuid;

use chrono::Utc;

use crate::audit::{self, RecordArgs};
use crate::connection::{ConnectionAccess, ConnectionContext, ConnectionManager, DbPool};
use crate::error::AppError;
use crate::model::{
    ConnectionProfile, DashboardKind, DocumentPage, DocumentQuery, Engine, HistoryEntry, QueryKind,
};
use crate::monitoring::{self, HealthSnapshot};
use crate::safety::{self, PoolRef};
use crate::services::{
    AgentConnectionSummary, AgentDashboardCommitError, AgentDashboardPrepareError,
    AgentDashboardPresentation, ApplicationServices, CatalogReadPolicy,
    LegacyConnectionResolutionError,
};
use crate::store::{AccountScope, PinnedConnection, Store};

const QUERY_PLAN_TTL: Duration = Duration::from_secs(30);
const MAX_QUERY_PLANS: usize = 256;
const MAX_AGENT_ROWS: u64 = 1000;

/// One single-use query proposal. Execution accepts only its id, so an agent cannot
/// replace the reviewed connection, SQL, or row cap between planning and execution.
#[derive(Clone)]
pub(crate) struct PlannedQuery {
    connection_id: Uuid,
    identity: PlannedConnectionIdentity,
    sql: String,
    max_rows: u64,
    decision: String,
    created_at: Instant,
}

/// Non-secret authority snapshot bound to a reviewed query. A connection UUID is
/// not sufficient because the same id can resolve differently after an account or
/// workspace switch. The scope generation also rejects A → B → A reuse.
#[derive(Clone, PartialEq, Eq)]
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

/// Query plans are shared by the HTTP and stdio MCP listeners in this app process.
pub(crate) type QueryPlanStore = Arc<Mutex<HashMap<Uuid, PlannedQuery>>>;

pub(crate) fn query_plan_store() -> QueryPlanStore {
    Arc::new(Mutex::new(HashMap::new()))
}

#[derive(Clone)]
pub(crate) struct DbTools {
    store: Store,
    events: ToolEventSink,
    /// Scope-aware connection manager shared with the UI and other agent transports.
    conns: ConnectionManager,
    /// Transport-neutral application services shared with the Tauri adapter.
    services: ApplicationServices,
    plans: QueryPlanStore,
}

/// Keeps MCP behavior independent from Tauri's concrete runtime so headless contract
/// tests can exercise the real tools without starting a desktop window.
type ToolEventCallback = dyn Fn(&str, serde_json::Value) + Send + Sync;

#[derive(Clone)]
struct ToolEventSink(Arc<ToolEventCallback>);

#[cfg(test)]
type RecordedToolEvents = Arc<Mutex<Vec<serde_json::Value>>>;

impl ToolEventSink {
    fn tauri(app: AppHandle) -> Self {
        Self(Arc::new(move |event, payload| {
            // Don't swallow: an emit failure (e.g. an illegal event name) means the
            // live UI silently goes dark. Surface it in the log.
            if let Err(error) = app.emit(event, payload) {
                tracing::warn!("failed to emit {event}: {error}");
            }
        }))
    }

    #[cfg(test)]
    fn recording() -> (Self, RecordedToolEvents) {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let target = Arc::clone(&recorded);
        let sink = Self(Arc::new(move |event, payload| {
            target.lock().unwrap().push(json!({
                "event": event,
                "payload": payload,
            }));
        }));
        (sink, recorded)
    }

    fn emit(&self, event: &str, payload: serde_json::Value) {
        (self.0)(event, payload);
    }
}

// ── tool argument shapes (JSON Schema is derived for the MCP tool manifest) ──────
#[derive(Debug, Deserialize, JsonSchema)]
struct ConnArg {
    /// Connection name or id. Omit to use the first configured connection.
    #[serde(default)]
    connection: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct PlanQueryArgs {
    /// Connection name or id. Omit to use the first configured connection.
    #[serde(default)]
    connection: Option<String>,
    /// A single read-only SQL statement (SELECT / WITH … SELECT).
    sql: String,
    /// Max rows to return (capped at 1000).
    #[serde(default)]
    max_rows: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RunQueryArgs {
    /// Single-use planId returned by plan_query within the last 30 seconds.
    plan_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RunDocumentQueryArgs {
    /// Connection name or id. Omit to use the first configured connection.
    #[serde(default)]
    connection: Option<String>,
    /// One typed, read-only document request: find, aggregate, or count.
    query: DocumentQuery,
    /// Max documents to return (capped at 1000).
    #[serde(default)]
    max_rows: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct DescribeTableArgs {
    /// Connection name or id. Omit to use the first configured connection.
    #[serde(default)]
    connection: Option<String>,
    /// Table name, optionally schema-qualified ("public.users" or "users").
    table: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CreateDashboardArgs {
    /// Exact queryRunId returned by the successful run_query the user agreed to save.
    query_run_id: String,
    /// Short display title for the saved dashboard.
    title: String,
    /// Optional explanatory copy shown with the visualization.
    #[serde(default)]
    description: String,
    /// Renderer choice. Omit for automatic visualization selection.
    #[serde(default)]
    kind: DashboardKind,
    /// Column used for the category/time axis, when applicable.
    #[serde(default)]
    x_column: Option<String>,
    /// Numeric/value columns rendered as series.
    #[serde(default)]
    y_columns: Vec<String>,
}

// ── helpers (not tools) ──────────────────────────────────────────────────────────
fn connection_list_payload(connections: &[AgentConnectionSummary]) -> serde_json::Value {
    json!({
        "connections": connections.iter().map(|connection| json!({
            "id": connection.id,
            "name": connection.name,
            "engine": connection.engine,
            "database": connection.database,
            "environment": connection.environment,
            "readonly": connection.readonly,
            "allowWrites": connection.allow_writes,
        })).collect::<Vec<_>>()
    })
}

impl DbTools {
    pub(crate) fn new(
        store: Store,
        app: AppHandle,
        conns: ConnectionManager,
        services: ApplicationServices,
        plans: QueryPlanStore,
    ) -> Self {
        Self {
            store,
            events: ToolEventSink::tauri(app),
            conns,
            services,
            plans,
        }
    }

    #[cfg(test)]
    fn new_for_test(
        store: Store,
        conns: ConnectionManager,
        services: ApplicationServices,
        plans: QueryPlanStore,
    ) -> (Self, RecordedToolEvents) {
        let (events, recorded) = ToolEventSink::recording();
        (
            Self {
                store,
                events,
                conns,
                services,
                plans,
            },
            recorded,
        )
    }

    fn emit(&self, event: &str, payload: serde_json::Value) {
        self.events.emit(event, payload);
    }

    /// Resolve the target connection: explicit name/id, else the first configured one.
    async fn resolve_conn(&self, arg: &Option<String>) -> Result<AgentConnectionSummary, McpError> {
        self.services
            .connections
            .resolve_legacy_mcp(arg.as_deref())
            .await
            .map_err(err)?
            .map_err(|error| match error {
                LegacyConnectionResolutionError::NoConnections => {
                    McpError::invalid_params("no connections configured in DopeDB", None)
                }
                LegacyConnectionResolutionError::NoMatch { selector } => {
                    McpError::invalid_params(format!("no connection matching '{selector}'"), None)
                }
            })
    }

    /// Pin the active workspace/account authority without opening the target DB.
    async fn pin(&self, id: Uuid) -> Result<ConnectionContext, McpError> {
        self.conns
            .pin(id, ConnectionAccess::Read)
            .await
            .map_err(err)
    }

    /// Persist one MCP query-history row and return its durable consent handle.
    async fn history(
        &self,
        pin: &PinnedConnection,
        sql: &str,
        status: &str,
        rows: Option<i64>,
        dur_ms: Option<i64>,
        error: Option<String>,
    ) -> Result<Uuid, AppError> {
        let id = Uuid::new_v4();
        self.store
            .insert_history_if_current(
                pin,
                &HistoryEntry {
                    id,
                    connection_id: pin.connection_id,
                    sql: sql.to_string(),
                    kind: QueryKind::Read,
                    status: status.to_string(),
                    row_count: rows,
                    duration_ms: dur_ms,
                    error,
                    executed_at: Utc::now(),
                    origin: "agent".into(),
                },
            )
            .await?;
        Ok(id)
    }

    /// Best-effort audit record for an MCP tool call. Origin is encoded in `action`
    /// (`mcp:*`); logging failures never fail the tool call.
    async fn audit(
        &self,
        conn_id: Uuid,
        engine: Engine,
        sql: &str,
        kind: QueryKind,
        action: &str,
        error: Option<String>,
    ) {
        let _ = audit::record(
            &self.store,
            RecordArgs {
                connection_id: conn_id,
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
        .await;
    }
}

fn err(e: AppError) -> McpError {
    McpError::internal_error(e.to_string(), None)
}

fn pool_ref(db: &DbPool) -> PoolRef<'_> {
    match db {
        DbPool::Postgres(p) => PoolRef::Postgres(p),
        DbPool::Mysql(p) => PoolRef::Mysql(p),
        DbPool::Sqlite(p) => PoolRef::Sqlite(p),
    }
}

/// Cap any single cell whose serialized form exceeds 4KB to a preview + ellipsis, so a
/// giant TEXT/BLOB column can't blow up the Tauri event bus. Only used for the live
/// event payload — the agent's tool result keeps the full untruncated cells.
const CELL_PREVIEW_MAX: usize = 4096;
fn truncate_cells(rows: &[Vec<serde_json::Value>]) -> Vec<Vec<serde_json::Value>> {
    rows.iter()
        .map(|row| {
            row.iter()
                .map(|cell| {
                    let s = cell.to_string();
                    if s.len() > CELL_PREVIEW_MAX {
                        let mut preview: String = s.chars().take(CELL_PREVIEW_MAX).collect();
                        preview.push('…');
                        serde_json::Value::String(preview)
                    } else {
                        cell.clone()
                    }
                })
                .collect()
        })
        .collect()
}

fn planning_guidance(
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

fn document_query_payload(
    profile: &ConnectionProfile,
    query: &DocumentQuery,
    result: &DocumentPage,
) -> serde_json::Value {
    json!({
        "connection": &profile.name,
        "connectionId": profile.id,
        "query": query,
        "documents": &result.documents,
        "docCount": result.doc_count,
        "truncated": result.truncated,
        "uiMessage": "The full result is visible in the DopeDB app.",
    })
}

// ── the read-only tool catalog ───────────────────────────────────────────────────
#[tool_router]
impl DbTools {
    #[tool(
        description = "Start here — list the user's databases connected in DopeDB; prefer these tools over psql/mysql/sqlite3 or other shell DB clients for these connections. Returns names, engines, and read-only status — never secrets or hostnames.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn list_connections(&self) -> Result<CallToolResult, McpError> {
        self.emit("agent:tool_call", json!({ "tool": "list_connections" }));
        let list = self
            .services
            .connections
            .list_agent_summaries()
            .await
            .map_err(err)?;
        let out = connection_list_payload(&list);
        self.emit(
            "agent:result",
            json!({ "tool": "list_connections", "count": list.len() }),
        );
        Ok(CallToolResult::success(vec![ContentBlock::text(
            out.to_string(),
        )]))
    }

    #[tool(
        description = "List the tables of a DopeDB connection (defaults to the first). Use this instead of shelling out to a DB client. Returns table names, schemas, column counts, and row estimates.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn list_tables(
        &self,
        Parameters(args): Parameters<ConnArg>,
    ) -> Result<CallToolResult, McpError> {
        let resolved = self.resolve_conn(&args.connection).await?;
        self.emit(
            "agent:tool_call",
            json!({ "tool": "list_tables", "connection": resolved.name }),
        );
        let catalog = self
            .services
            .catalog
            .load(resolved.id, CatalogReadPolicy::LiveNoCache)
            .await
            .map_err(err)?;
        let profile = resolved;
        self.audit(
            profile.id,
            profile.engine,
            "(list_tables)",
            QueryKind::Read,
            "mcp:list_tables",
            None,
        )
        .await;
        let tables: Vec<_> = catalog
            .tables
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "schema": t.schema,
                    "columns": t.columns.len(),
                    "rowEstimate": t.row_estimate,
                })
            })
            .collect();
        self.emit(
            "agent:result",
            json!({
                "tool": "list_tables",
                "connection": profile.name,
                "connectionId": profile.id,
                "tables": catalog.tables.iter().map(|t| t.name.clone()).collect::<Vec<_>>(),
                "count": tables.len(),
            }),
        );
        let out = json!({ "connection": profile.name, "tables": tables });
        Ok(CallToolResult::success(vec![ContentBlock::text(
            out.to_string(),
        )]))
    }

    #[tool(
        description = "Describe one table on a DopeDB connection so you can write queries against real column names: columns (name, dataType, nullable, pk), foreign keys, and a row estimate. Accepts a bare or schema-qualified table name.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn describe_table(
        &self,
        Parameters(args): Parameters<DescribeTableArgs>,
    ) -> Result<CallToolResult, McpError> {
        let resolved = self.resolve_conn(&args.connection).await?;
        self.emit(
            "agent:tool_call",
            json!({ "tool": "describe_table", "connection": resolved.name, "table": args.table }),
        );
        let catalog = self
            .services
            .catalog
            .load(resolved.id, CatalogReadPolicy::CacheFirst)
            .await
            .map_err(err)?;
        let profile = resolved;
        let want = args.table.as_str();
        // Match "schema.table" or bare name exactly, else fall back to case-insensitive.
        let table = catalog
            .tables
            .iter()
            .find(|t| {
                let q = match &t.schema {
                    Some(s) => format!("{s}.{}", t.name),
                    None => t.name.clone(),
                };
                q == want || t.name == want
            })
            .or_else(|| {
                catalog
                    .tables
                    .iter()
                    .find(|t| t.name.eq_ignore_ascii_case(want))
            })
            .ok_or_else(|| {
                McpError::invalid_params(
                    format!("no table matching '{want}' in '{}'", profile.name),
                    None,
                )
            })?;
        self.audit(
            profile.id,
            profile.engine,
            "(describe_table)",
            QueryKind::Read,
            "mcp:describe_table",
            None,
        )
        .await;
        // Sampled document structure is a hint, not a schema guarantee.
        let note = profile.engine.is_document().then_some(
            "MongoDB columns are inferred from a bounded document sample — fields not seen in the sample may exist",
        );
        let out = json!({
            "connection": profile.name,
            "schema": table.schema,
            "table": table.name,
            "note": note,
            "rowEstimate": table.row_estimate,
            "columns": table.columns.iter().map(|c| json!({
                "name": c.name,
                "dataType": c.data_type,
                "nullable": c.nullable,
                "pk": c.pk,
            })).collect::<Vec<_>>(),
            "foreignKeys": table.foreign_keys.iter().map(|f| json!({
                "column": f.column,
                "referencesTable": f.references_table,
                "referencesColumn": f.references_column,
            })).collect::<Vec<_>>(),
        });
        self.emit(
            "agent:result",
            json!({
                "tool": "describe_table",
                "connection": profile.name,
                "connectionId": profile.id,
                "table": table.name,
                "columns": table.columns.len(),
            }),
        );
        Ok(CallToolResult::success(vec![ContentBlock::text(
            out.to_string(),
        )]))
    }

    #[tool(
        description = "MANDATORY before run_query. Review one read-only SQL statement with EXPLAIN plus aggregate database-pressure signals. Returns a single-use planId, clear caution reasons, and safer alternatives. It never returns other sessions' SQL text and never runs the proposed query.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn plan_query(
        &self,
        Parameters(args): Parameters<PlanQueryArgs>,
    ) -> Result<CallToolResult, McpError> {
        let resolved = self.resolve_conn(&args.connection).await?;
        self.emit(
            "agent:tool_call",
            json!({
                "tool": "plan_query",
                "connection": resolved.name,
                "connectionId": resolved.id,
                "sql": args.sql,
            }),
        );
        let context = self.pin(resolved.id).await?;
        let profile = context.pin().profile.clone();
        // MongoDB has no SQL surface — point the agent at the typed document tool
        // instead of letting the fail-safe classifier return a misleading verdict.
        if profile.engine.is_document() {
            return Err(McpError::invalid_params(
                "this is a MongoDB connection — SQL planning does not apply; call \
                 run_document_query with a typed find/aggregate/countDocuments request",
                None,
            ));
        }

        let cls = safety::classify(&args.sql, profile.engine).map_err(err)?;
        if !matches!(cls.kind, QueryKind::Read) || cls.statement_count != 1 {
            let msg = "plan_query accepts exactly one read-only SELECT statement";
            self.emit(
                "agent:result",
                json!({
                    "tool": "plan_query",
                    "connection": profile.name,
                    "connectionId": profile.id,
                    "error": msg,
                }),
            );
            self.audit(
                profile.id,
                profile.engine,
                &args.sql,
                cls.kind,
                "mcp:plan_query",
                Some(msg.to_string()),
            )
            .await;
            return Err(McpError::invalid_params(msg, None));
        }

        let settings = self.store.get_safety(profile.id).await.map_err(err)?;
        let cap = args
            .max_rows
            .unwrap_or(settings.max_rows)
            .min(MAX_AGENT_ROWS);
        let identity = PlannedConnectionIdentity::from(context.pin());
        let lease = context.connect().await.map_err(err)?;
        let live = lease.live().sql().map_err(err)?;
        let preview = safety::preview(pool_ref(live.ro()), &args.sql, &cls, &settings)
            .await
            .map_err(err)?;
        let health = monitoring::snapshot(live, profile.engine).await;
        let (decision, notices, suggestions) = planning_guidance(
            &profile,
            &health,
            preview.estimated_rows,
            settings.exec_preview_row_limit,
        );
        let plan_id = Uuid::new_v4();
        {
            let mut plans = self.plans.lock().unwrap();
            plans.retain(|_, plan| plan.created_at.elapsed() <= QUERY_PLAN_TTL);
            if plans.len() >= MAX_QUERY_PLANS {
                if let Some(oldest) = plans
                    .iter()
                    .min_by_key(|(_, plan)| plan.created_at)
                    .map(|(id, _)| *id)
                {
                    plans.remove(&oldest);
                }
            }
            plans.insert(
                plan_id,
                PlannedQuery {
                    connection_id: profile.id,
                    identity,
                    sql: args.sql.clone(),
                    max_rows: cap,
                    decision: decision.clone(),
                    created_at: Instant::now(),
                },
            );
        }
        self.audit(
            profile.id,
            profile.engine,
            &args.sql,
            QueryKind::Read,
            "mcp:plan_query",
            None,
        )
        .await;
        self.emit(
            "agent:result",
            json!({
                "tool": "plan_query",
                "connection": profile.name,
                "connectionId": profile.id,
                "sql": args.sql,
                "planId": plan_id,
                "decision": decision,
                "estimatedRows": preview.estimated_rows,
                "healthLevel": health.level,
                "monitoringCoverage": health.coverage,
                "noticeCount": notices.len(),
                "notices": notices,
                "suggestions": suggestions,
            }),
        );
        let out = json!({
            "connection": profile.name,
            "connectionId": profile.id,
            "environment": profile.env,
            "sql": args.sql,
            "planId": plan_id,
            "singleUse": true,
            "expiresInSeconds": QUERY_PLAN_TTL.as_secs(),
            "decision": decision,
            "notices": notices,
            "suggestions": suggestions,
            "estimatedRows": preview.estimated_rows,
            "health": health,
            "nextAction": "Review these notices, then call run_query with this exact planId. If you change the SQL, call plan_query again.",
        });
        Ok(CallToolResult::success(vec![ContentBlock::text(
            out.to_string(),
        )]))
    }

    #[tool(
        description = "Execute one plan_query proposal by its exact single-use planId. No SQL or connection can be supplied here. The stored statement runs in an enforced read-only, audited DB session and its result is displayed live in DopeDB.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn run_query(
        &self,
        Parameters(args): Parameters<RunQueryArgs>,
    ) -> Result<CallToolResult, McpError> {
        let plan_id = Uuid::parse_str(&args.plan_id)
            .map_err(|e| McpError::invalid_params(format!("invalid plan_id: {e}"), None))?;
        let plan = self.plans.lock().unwrap().remove(&plan_id).ok_or_else(|| {
            McpError::invalid_params(
                "plan_id is unknown, expired, or already used; call plan_query again",
                None,
            )
        })?;
        if plan.created_at.elapsed() > QUERY_PLAN_TTL {
            return Err(McpError::invalid_params(
                "plan_id expired; database health must be checked again with plan_query",
                None,
            ));
        }
        // Pin first, before looking up the profile or opening the DB. This compares
        // against the exact workspace/account snapshot reviewed by plan_query and
        // rejects both cross-account reuse and A → B → A reuse.
        let context = self.pin(plan.connection_id).await?;
        if PlannedConnectionIdentity::from(context.pin()) != plan.identity {
            return Err(McpError::invalid_params(
                "workspace, account, or connection access changed; call plan_query again",
                None,
            ));
        }
        let operation_pin = context.pin().clone();
        let profile = context.pin().profile.clone();
        let cls = safety::classify(&plan.sql, profile.engine).map_err(err)?;
        if !matches!(cls.kind, QueryKind::Read) || cls.statement_count != 1 {
            return Err(McpError::invalid_params(
                "stored plan no longer validates as one read-only statement",
                None,
            ));
        }
        self.emit(
            "agent:tool_call",
            json!({
                "tool": "run_query",
                "connection": profile.name,
                "connectionId": profile.id,
                "planId": plan_id,
                "sql": plan.sql,
            }),
        );
        let live = match context.connect().await.map_err(err) {
            Ok(live) => live,
            Err(e) => {
                let message = e.message.to_string();
                self.audit(
                    profile.id,
                    profile.engine,
                    &plan.sql,
                    QueryKind::Read,
                    "mcp:run_query",
                    Some(message.clone()),
                )
                .await;
                if let Err(history_error) = self
                    .history(
                        &operation_pin,
                        &plan.sql,
                        "error",
                        None,
                        None,
                        Some(message.clone()),
                    )
                    .await
                {
                    tracing::error!("MCP connection-error history insert failed: {history_error}");
                }
                self.emit(
                    "agent:result",
                    json!({
                        "tool": "run_query",
                        "connection": profile.name,
                        "connectionId": profile.id,
                        "planId": plan_id,
                        "sql": plan.sql,
                        "error": message,
                    }),
                );
                return Err(e);
            }
        };

        // L2 authoritative read-only session — a misclassified write is rejected at the DB.
        // (`sql()` cannot fail here: plan_query never issues planIds for MongoDB.)
        let result = match safety::run_read_only(
            pool_ref(live.live().sql().map_err(err)?.ro()),
            &plan.sql,
            plan.max_rows,
        )
        .await
        {
            Ok(result) => result,
            Err(e) => {
                let message = e.to_string();
                self.audit(
                    profile.id,
                    profile.engine,
                    &plan.sql,
                    QueryKind::Read,
                    "mcp:run_query",
                    Some(message.clone()),
                )
                .await;
                if let Err(history_error) = self
                    .history(
                        &operation_pin,
                        &plan.sql,
                        "error",
                        None,
                        None,
                        Some(message.clone()),
                    )
                    .await
                {
                    tracing::error!("MCP failed-query history insert failed: {history_error}");
                }
                self.emit(
                    "agent:result",
                    json!({
                        "tool": "run_query",
                        "connection": profile.name,
                        "connectionId": profile.id,
                        "planId": plan_id,
                        "sql": plan.sql,
                        "error": message,
                    }),
                );
                return Err(err(e));
            }
        };
        self.audit(
            profile.id,
            profile.engine,
            &plan.sql,
            QueryKind::Read,
            "mcp:run_query",
            None,
        )
        .await;
        let query_run_id = match self
            .history(
                &operation_pin,
                &plan.sql,
                "ok",
                Some(result.row_count as i64),
                Some(result.duration_ms as i64),
                None,
            )
            .await
        {
            Ok(id) => id,
            Err(e) => {
                let message =
                    format!("query succeeded but its consent handle could not be persisted: {e}");
                self.emit(
                    "agent:result",
                    json!({
                        "tool": "run_query",
                        "connection": profile.name,
                        "connectionId": profile.id,
                        "planId": plan_id,
                        "sql": plan.sql,
                        "error": message,
                    }),
                );
                return Err(err(e));
            }
        };

        self.emit(
            "agent:result",
            json!({
                "tool": "run_query",
                "connection": profile.name,
                "connectionId": profile.id,
                "planId": plan_id,
                "queryRunId": query_run_id,
                "sql": plan.sql,
                "columns": result.columns,
                // Per-cell truncation for the event bus only; the agent result below is full.
                "rows": truncate_cells(&result.rows),
                "rowCount": result.row_count,
                "truncated": result.truncated,
                "durationMs": result.duration_ms,
            }),
        );

        // Agent gets compact columns-once JSON.
        let out = json!({
            "connection": profile.name,
            "connectionId": profile.id,
            "planId": plan_id,
            "planningDecision": plan.decision,
            "queryRunId": query_run_id,
            "sql": plan.sql,
            "columns": result.columns,
            "rows": result.rows,
            "rowCount": result.row_count,
            "truncated": result.truncated,
            "uiMessage": "The full result is visible in the DopeDB app.",
            "dashboardSuggestion": "Ask whether the user wants to save this query as a dashboard. After explicit agreement, call create_dashboard with this exact queryRunId.",
        });
        Ok(CallToolResult::success(vec![ContentBlock::text(
            out.to_string(),
        )]))
    }

    #[tool(
        description = "Run one typed, READ-ONLY document query on a MongoDB connection: find, aggregate (write stages such as $out/$merge are rejected), or count (countDocuments). Filters and pipelines accept MongoDB Extended JSON. Results are row-capped, audited, and shown LIVE in DopeDB. SQL tools (plan_query/run_query) do not apply to MongoDB connections — use this tool for them.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn run_document_query(
        &self,
        Parameters(args): Parameters<RunDocumentQueryArgs>,
    ) -> Result<CallToolResult, McpError> {
        let resolved = self.resolve_conn(&args.connection).await?;
        // The audit/history/feed `sql` slot carries the serialized typed request.
        let query_text = serde_json::to_string(&args.query).map_err(|e| err(e.into()))?;
        self.emit(
            "agent:tool_call",
            json!({
                "tool": "run_document_query",
                "connection": resolved.name,
                "connectionId": resolved.id,
                "sql": query_text,
            }),
        );
        let context = self.pin(resolved.id).await?;
        let operation_pin = context.pin().clone();
        let profile = context.pin().profile.clone();
        if !profile.engine.is_document() {
            return Err(McpError::invalid_params(
                "run_document_query only works on MongoDB connections — use plan_query/run_query for SQL engines",
                None,
            ));
        }

        // Typed L1 equivalent: the request tree is validated against the read-only
        // stage allowlist; anything else fail-safes to a blocked write.
        let cls = crate::mongo::query::classify(&args.query);
        if !matches!(cls.kind, QueryKind::Read) {
            let msg = cls
                .notes
                .first()
                .cloned()
                .unwrap_or_else(|| "document writes are not supported over MCP".into());
            self.audit(
                profile.id,
                profile.engine,
                &query_text,
                cls.kind,
                "mcp:run_document_query",
                Some(msg.clone()),
            )
            .await;
            self.emit(
                "agent:result",
                json!({
                    "tool": "run_document_query",
                    "connection": profile.name,
                    "connectionId": profile.id,
                    "sql": query_text,
                    "error": msg,
                }),
            );
            return Err(McpError::invalid_params(msg, None));
        }

        let settings = self.store.get_safety(profile.id).await.map_err(err)?;
        let cap = args
            .max_rows
            .unwrap_or(settings.max_rows)
            .min(MAX_AGENT_ROWS);
        let live = context.connect().await.map_err(err)?;
        let result = match crate::mongo::query::run(
            live.live().mongo().map_err(err)?,
            &args.query,
            cap,
            Duration::from_millis(safety::STATEMENT_TIMEOUT_MS),
        )
        .await
        {
            Ok(page) => page,
            Err(e) => {
                let message = e.to_string();
                self.audit(
                    profile.id,
                    profile.engine,
                    &query_text,
                    QueryKind::Read,
                    "mcp:run_document_query",
                    Some(message.clone()),
                )
                .await;
                if let Err(history_error) = self
                    .history(
                        &operation_pin,
                        &query_text,
                        "error",
                        None,
                        None,
                        Some(message.clone()),
                    )
                    .await
                {
                    tracing::error!(
                        "MCP failed-document-query history insert failed: {history_error}"
                    );
                }
                self.emit(
                    "agent:result",
                    json!({
                        "tool": "run_document_query",
                        "connection": profile.name,
                        "connectionId": profile.id,
                        "sql": query_text,
                        "error": message,
                    }),
                );
                return Err(err(e));
            }
        };

        self.audit(
            profile.id,
            profile.engine,
            &query_text,
            QueryKind::Read,
            "mcp:run_document_query",
            None,
        )
        .await;
        if let Err(e) = self
            .history(
                &operation_pin,
                &query_text,
                "ok",
                Some(result.doc_count as i64),
                Some(result.duration_ms as i64),
                None,
            )
            .await
        {
            tracing::error!("MCP document-query history insert failed: {e}");
        }

        // The live feed renders a columns/rows grid — one document per row.
        let feed_rows: Vec<Vec<serde_json::Value>> =
            result.documents.iter().map(|d| vec![d.clone()]).collect();
        self.emit(
            "agent:result",
            json!({
                "tool": "run_document_query",
                "connection": profile.name,
                "connectionId": profile.id,
                "sql": query_text,
                "columns": ["document"],
                "rows": truncate_cells(&feed_rows),
                "rowCount": result.doc_count,
                "truncated": result.truncated,
                "durationMs": result.duration_ms,
            }),
        );

        let out = document_query_payload(&profile, &args.query, &result);
        Ok(CallToolResult::success(vec![ContentBlock::text(
            out.to_string(),
        )]))
    }

    #[tool(
        description = "Save one successful run_query as a persistent DopeDB dashboard. Call this ONLY after the user explicitly asks or agrees. Pass the exact query_run_id returned by that run_query; connection and SQL are loaded from DopeDB history and cannot be supplied or changed here.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn create_dashboard(
        &self,
        Parameters(args): Parameters<CreateDashboardArgs>,
    ) -> Result<CallToolResult, McpError> {
        let query_run_id = Uuid::parse_str(&args.query_run_id)
            .map_err(|e| McpError::invalid_params(format!("invalid query_run_id: {e}"), None))?;
        let prepared = match self
            .services
            .dashboard
            .prepare_agent_create(
                query_run_id,
                AgentDashboardPresentation {
                    title: args.title,
                    description: args.description,
                    kind: args.kind,
                    x_column: args.x_column,
                    y_columns: args.y_columns,
                },
            )
            .await
        {
            Ok(prepared) => prepared,
            Err(AgentDashboardPrepareError::QueryRunNotFound) => {
                return Err(McpError::invalid_params(
                    "query_run_id does not identify a stored DopeDB query run",
                    None,
                ))
            }
            Err(AgentDashboardPrepareError::QueryRunIneligible) => {
                return Err(McpError::invalid_params(
                    "query_run_id must identify a successful agent read query",
                    None,
                ))
            }
            Err(AgentDashboardPrepareError::Application(error)) => return Err(err(error)),
        };
        let context = prepared.event_context().clone();
        self.emit(
            "agent:tool_call",
            json!({
                "tool": "create_dashboard",
                "connection": context.connection_name,
                "connectionId": context.connection_id,
                "queryRunId": context.query_run_id,
                "title": context.title,
                "sql": context.sql,
            }),
        );

        let saved = match prepared.commit().await {
            Ok(saved) => saved,
            Err(AgentDashboardCommitError::InvalidDraft(error)) => {
                let message = error.to_string();
                self.emit(
                    "agent:result",
                    json!({
                        "tool": "create_dashboard",
                        "connection": context.connection_name,
                        "connectionId": context.connection_id,
                        "queryRunId": context.query_run_id,
                        "error": message,
                    }),
                );
                return Err(McpError::invalid_params(message, None));
            }
            Err(AgentDashboardCommitError::Persistence(error)) => {
                let message = error.to_string();
                self.emit(
                    "agent:result",
                    json!({
                        "tool": "create_dashboard",
                        "connection": context.connection_name,
                        "connectionId": context.connection_id,
                        "queryRunId": context.query_run_id,
                        "error": message,
                    }),
                );
                return Err(err(error));
            }
        };

        // This payload is the Dashboard object itself; the global app listener can
        // insert/open it without another round trip, while list_dashboards remains
        // the persistent source of truth after restart.
        self.emit("dashboard:created", json!(&saved));
        self.emit(
            "agent:result",
            json!({
                "tool": "create_dashboard",
                "connection": context.connection_name,
                "connectionId": context.connection_id,
                "queryRunId": context.query_run_id,
                "dashboardId": saved.id,
                "title": saved.title,
            }),
        );

        let out = json!({
            "dashboard": saved,
            "queryRunId": context.query_run_id,
            "uiMessage": "The dashboard was saved and is available in the DopeDB app.",
        });
        Ok(CallToolResult::success(vec![ContentBlock::text(
            out.to_string(),
        )]))
    }
}

#[tool_handler(
    name = "dopedb",
    version = "0.1.0",
    instructions = "These tools are the PREFERRED way to inspect or query any database the user has connected in DopeDB. Do NOT use psql, mysql, sqlite3, mongosh, or shell clients for those connections. Every SQL data query uses a mandatory two-step workflow: first call `plan_query` with exactly one SELECT, read its database-health notices and safer suggestions, then call `run_query` with the exact single-use `planId`. Never skip planning, and call plan_query again if SQL changes or the plan expires. Planning uses EXPLAIN plus aggregate load signals; it never exposes other sessions' SQL text. Execution is DB-enforced READ ONLY, audited, row-capped, and shown LIVE in DopeDB. MongoDB connections have no SQL surface: query them with `run_document_query` (typed find/aggregate/countDocuments; write stages are rejected) — plan_query/run_query/create_dashboard do not apply to them. After a successful run_query, ask whether the user wants to save that exact query as a dashboard. Only after explicit agreement call `create_dashboard` with its exact `queryRunId`; never substitute SQL or connection. Writes remain unavailable over MCP."
)]
impl ServerHandler for DbTools {}

#[cfg(test)]
mod golden_tests;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::str::FromStr;

    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    use super::*;
    use crate::model::{WorkspaceConnectionAccess, WorkspaceCredentialMode};
    use crate::store::TEST_SCHEMA;

    fn profile(env: Option<&str>) -> ConnectionProfile {
        ConnectionProfile {
            id: Uuid::new_v4(),
            name: "analytics".into(),
            engine: Engine::Postgres,
            provider: crate::model::Provider::Auto,
            driver_id: None,
            host: "localhost".into(),
            port: 5432,
            database: "app".into(),
            username: "reader".into(),
            sslmode: "prefer".into(),
            extra_params: HashMap::new(),
            readonly_default: true,
            allow_writes: false,
            secret_ref: None,
            env: env.map(str::to_string),
            schema_group: None,
            workspace_access: crate::model::WorkspaceConnectionAccess::Local,
            credential_mode: crate::model::WorkspaceCredentialMode::Local,
        }
    }

    fn quiet_health(coverage: &str) -> HealthSnapshot {
        HealthSnapshot {
            level: "normal".into(),
            coverage: coverage.into(),
            total_connections: Some(1),
            max_connections: Some(100),
            connection_usage_percent: Some(1.0),
            active_queries: Some(1),
            long_running_queries: Some(0),
            lock_waits: Some(0),
            replication_lag_seconds: None,
            reasons: vec!["No aggregate database-pressure warning was detected.".into()],
            captured_at: Utc::now(),
        }
    }

    async fn dashboard_harness() -> (DbTools, RecordedToolEvents, Store, Uuid, Uuid) {
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
        let mut connection = profile(Some("test"));
        connection.id = connection_id;
        connection.engine = Engine::Sqlite;
        connection.provider = crate::model::Provider::Generic;
        connection.driver_id = Some("sqlx-sqlite".into());
        connection.host.clear();
        connection.port = 0;
        connection.database = ":memory:".into();
        connection.sslmode = "disable".into();
        connection.workspace_access = WorkspaceConnectionAccess::Local;
        connection.credential_mode = WorkspaceCredentialMode::Local;
        store.upsert_connection(&connection).await.unwrap();

        let query_run_id = Uuid::new_v4();
        let pin = store.pin_connection_for_read(connection_id).await.unwrap();
        store
            .insert_history_if_current(
                &pin,
                &HistoryEntry {
                    id: query_run_id,
                    connection_id,
                    sql: "SELECT id, name FROM users ORDER BY id".into(),
                    kind: QueryKind::Read,
                    status: "ok".into(),
                    row_count: Some(1),
                    duration_ms: Some(1),
                    error: None,
                    executed_at: Utc::now(),
                    origin: "agent".into(),
                },
            )
            .await
            .unwrap();

        let connections = ConnectionManager::new(store.clone());
        let services = ApplicationServices::new(store.clone(), connections.clone());
        let (tools, events) =
            DbTools::new_for_test(store.clone(), connections, services, query_plan_store());
        (tools, events, store, connection_id, query_run_id)
    }

    fn assert_dashboard_failure_events(
        events: &RecordedToolEvents,
        connection_id: Uuid,
        query_run_id: Uuid,
        title: &str,
        error: &str,
    ) {
        let events = events.lock().unwrap();
        assert_eq!(
            events.as_slice(),
            [
                json!({
                    "event": "agent:tool_call",
                    "payload": {
                        "tool": "create_dashboard",
                        "connection": "analytics",
                        "connectionId": connection_id,
                        "queryRunId": query_run_id,
                        "title": title,
                        "sql": "SELECT id, name FROM users ORDER BY id",
                    },
                }),
                json!({
                    "event": "agent:result",
                    "payload": {
                        "tool": "create_dashboard",
                        "connection": "analytics",
                        "connectionId": connection_id,
                        "queryRunId": query_run_id,
                        "error": error,
                    },
                }),
            ]
        );
    }

    #[tokio::test]
    async fn create_dashboard_invalid_draft_preserves_error_event_contract() {
        let (tools, events, store, connection_id, query_run_id) = dashboard_harness().await;
        let error = tools
            .create_dashboard(Parameters(CreateDashboardArgs {
                query_run_id: query_run_id.to_string(),
                title: " ".into(),
                description: String::new(),
                kind: DashboardKind::Table,
                x_column: None,
                y_columns: Vec::new(),
            }))
            .await
            .unwrap_err();
        let wire = serde_json::to_value(error).unwrap();
        assert_eq!(wire["code"], -32602);
        assert_eq!(
            wire["message"],
            "config error: dashboard title cannot be empty"
        );
        assert_dashboard_failure_events(
            &events,
            connection_id,
            query_run_id,
            " ",
            wire["message"].as_str().unwrap(),
        );
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM dashboards")
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn create_dashboard_persistence_failure_preserves_internal_error_context() {
        let (tools, events, store, connection_id, query_run_id) = dashboard_harness().await;
        sqlx::raw_sql(
            "CREATE TRIGGER reject_dashboard_insert
             BEFORE INSERT ON dashboards
             BEGIN
               SELECT RAISE(ABORT, 'forced dashboard persistence failure');
             END;",
        )
        .execute(store.pool())
        .await
        .unwrap();

        let error = tools
            .create_dashboard(Parameters(CreateDashboardArgs {
                query_run_id: query_run_id.to_string(),
                title: "Persistence failure".into(),
                description: String::new(),
                kind: DashboardKind::Table,
                x_column: None,
                y_columns: Vec::new(),
            }))
            .await
            .unwrap_err();
        let wire = serde_json::to_value(error).unwrap();
        assert_eq!(wire["code"], -32603);
        let message = wire["message"].as_str().unwrap();
        assert!(message.contains("forced dashboard persistence failure"));
        assert_dashboard_failure_events(
            &events,
            connection_id,
            query_run_id,
            "Persistence failure",
            message,
        );
    }

    #[test]
    fn truncate_only_oversized_cells() {
        let big = serde_json::Value::String("x".repeat(CELL_PREVIEW_MAX + 100));
        let small = serde_json::json!(42);
        let rows = vec![vec![big, small.clone()]];
        let out = truncate_cells(&rows);
        assert_eq!(out[0][1], small); // small cell untouched
        let s = out[0][0].as_str().unwrap();
        assert!(s.ends_with('…')); // oversized cell became a marked preview
        assert!(s.chars().count() <= CELL_PREVIEW_MAX + 1);
    }

    #[test]
    fn production_and_limited_coverage_force_agent_caution() {
        let (decision, notices, suggestions) = planning_guidance(
            &profile(Some("prod")),
            &quiet_health("limited"),
            Some(10),
            50_000,
        );
        assert_eq!(decision, "caution");
        assert!(notices.iter().any(|n| n.contains("production")));
        assert!(suggestions.iter().any(|n| n.contains("pg_monitor")));
    }

    /// Regression guard for the exact bug that shipped once: `claude::ALLOWED_TOOLS`
    /// silently drifting behind the actual MCP tool catalog (missing
    /// `run_document_query` after MongoDB support landed) so Claude Code chat rejects
    /// a tool call the server itself advertises. Derives the expected set from
    /// `DbTools`'s own `tool_router` rather than hand-maintaining a second list here.
    #[test]
    fn claude_allowed_tools_matches_the_mcp_tool_catalog() {
        let from_router: std::collections::BTreeSet<String> = DbTools::tool_router()
            .list_all()
            .into_iter()
            .map(|t| format!("mcp__dopedb__{}", t.name))
            .collect();
        let allowed: std::collections::BTreeSet<String> = crate::agent::claude::ALLOWED_TOOLS
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            from_router, allowed,
            "claude::ALLOWED_TOOLS must list exactly the tools mcp::tools::DbTools registers"
        );
    }

    #[test]
    fn tool_annotations_keep_database_reads_non_interactive() {
        let read_only: std::collections::BTreeSet<&str> = [
            "list_connections",
            "list_tables",
            "describe_table",
            "plan_query",
            "run_query",
            "run_document_query",
        ]
        .into_iter()
        .collect();

        for tool in DbTools::tool_router().list_all() {
            let annotations = tool
                .annotations
                .as_ref()
                .unwrap_or_else(|| panic!("{} must declare MCP safety annotations", tool.name));
            assert_eq!(annotations.open_world_hint, Some(false), "{}", tool.name);
            if read_only.contains(tool.name.as_ref()) {
                assert_eq!(annotations.read_only_hint, Some(true), "{}", tool.name);
                assert_eq!(annotations.destructive_hint, Some(false), "{}", tool.name);
            } else {
                assert_eq!(tool.name.as_ref(), "create_dashboard");
                assert_eq!(annotations.read_only_hint, Some(false));
                assert_eq!(annotations.destructive_hint, Some(false));
            }
        }
    }

    #[test]
    fn current_mcp_read_contract_matches_the_phase_zero_golden_fixture() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/mcp/read-contract-v1.json"
        ))
        .unwrap();
        assert_eq!(fixture["schemaVersion"], 1);
        assert_eq!(fixture["coverage"], "phase0_partial_behavioral");
        assert_eq!(
            fixture["behaviorFixtures"].as_array().map(Vec::len),
            Some(2)
        );
        assert_eq!(
            fixture["providerCoverage"]["sqlite"],
            "live_local_roundtrip"
        );
        assert_eq!(
            fixture["providerCoverage"]["mongodb"],
            "classifier_and_payload_only"
        );
        assert_eq!(
            fixture["providerCoverage"]["externalLiveRoundTrips"],
            "excluded"
        );
        assert_eq!(fixture["queryPlan"]["ttlSeconds"], QUERY_PLAN_TTL.as_secs());
        assert_eq!(fixture["queryPlan"]["maxRows"], MAX_AGENT_ROWS);
        assert_eq!(fixture["queryPlan"]["singleUse"], true);

        let implicit: ConnArg = serde_json::from_value(json!({})).unwrap();
        assert!(implicit.connection.is_none());
        assert_eq!(
            fixture["connectionResolution"]["implicitFirstConnection"],
            true
        );

        let actual: BTreeMap<String, (bool, bool, bool)> = DbTools::tool_router()
            .list_all()
            .into_iter()
            .filter_map(|tool| {
                let annotations = tool.annotations?;
                (annotations.read_only_hint == Some(true)).then(|| {
                    (
                        tool.name.to_string(),
                        (
                            annotations.read_only_hint.unwrap_or(false),
                            annotations.destructive_hint.unwrap_or(true),
                            annotations.open_world_hint.unwrap_or(true),
                        ),
                    )
                })
            })
            .collect();
        let expected: BTreeMap<String, (bool, bool, bool)> = fixture["readTools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| {
                (
                    tool["name"].as_str().unwrap().to_string(),
                    (
                        tool["readOnly"].as_bool().unwrap(),
                        tool["destructive"].as_bool().unwrap(),
                        tool["openWorld"].as_bool().unwrap(),
                    ),
                )
            })
            .collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn connection_list_payload_matches_the_redacted_golden_fixture() {
        let mut connection = profile(Some("prod"));
        connection.id = Uuid::parse_str("018f9999-8888-7777-8666-555544443333").unwrap();
        connection.host = "ep-secret.example".into();
        connection.username = "workspace_user".into();
        connection.secret_ref = Some("credential-item-id".into());
        connection
            .extra_params
            .insert("channel_binding".into(), "require".into());

        let output = connection_list_payload(&[AgentConnectionSummary::from(&connection)]);
        let expected: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/mcp/list-connections-success.json"
        ))
        .unwrap();
        assert_eq!(output, expected);

        let serialized = output.to_string();
        for forbidden in [
            "ep-secret.example",
            "workspace_user",
            "credential-item-id",
            "channel_binding",
            "provider",
            "host",
            "port",
            "username",
        ] {
            assert!(
                !serialized.contains(forbidden),
                "redacted response leaked {forbidden}"
            );
        }
    }

    #[test]
    fn planned_query_is_single_use_in_the_shared_store() {
        let store = query_plan_store();
        let id = Uuid::new_v4();
        let identity = PlannedConnectionIdentity {
            workspace_id: Uuid::new_v4(),
            account_scope: AccountScope::Personal,
            scope_generation: 1,
            connection_revision: 1,
            binding_revision: 0,
            binding_updated_at: String::new(),
        };
        store.lock().unwrap().insert(
            id,
            PlannedQuery {
                connection_id: Uuid::new_v4(),
                identity,
                sql: "SELECT 1".into(),
                max_rows: 10,
                decision: "ready".into(),
                created_at: Instant::now(),
            },
        );
        assert!(store.lock().unwrap().remove(&id).is_some());
        assert!(store.lock().unwrap().remove(&id).is_none());
    }

    #[test]
    fn planned_query_identity_rejects_scope_and_revision_changes() {
        let workspace_id = Uuid::new_v4();
        let identity = PlannedConnectionIdentity {
            workspace_id,
            account_scope: AccountScope::WorkspaceUser("account-a".into()),
            scope_generation: 7,
            connection_revision: 3,
            binding_revision: 2,
            binding_updated_at: "2026-07-24T00:00:00Z".into(),
        };

        let mut switched_account = identity.clone();
        switched_account.account_scope = AccountScope::WorkspaceUser("account-b".into());
        assert!(identity != switched_account);

        let mut reselected_scope = identity.clone();
        reselected_scope.scope_generation += 2;
        assert!(identity != reselected_scope);

        let mut changed_binding = identity.clone();
        changed_binding.binding_revision += 1;
        assert!(identity != changed_binding);
    }
}
