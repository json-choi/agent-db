//! MCP read-only tools that reuse the existing safety pipeline and connection manager.
//! Every query is first reviewed by `plan_query`; `run_query` accepts only the resulting
//! single-use id and still runs through the authoritative database read-only session.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content};
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use tauri::{AppHandle, Emitter};
use uuid::Uuid;

use chrono::Utc;

use crate::audit::{self, RecordArgs};
use crate::connection::{self, DbPool, Live};
use crate::error::AppError;
use crate::introspect;
use crate::model::{
    ConnectionProfile, DashboardDraft, DashboardKind, DashboardVisualization, DocumentQuery,
    Engine, HistoryEntry, QueryKind,
};
use crate::monitoring::{self, HealthSnapshot};
use crate::safety::{self, PoolRef};
use crate::store::Store;

const QUERY_PLAN_TTL: Duration = Duration::from_secs(30);
const MAX_QUERY_PLANS: usize = 256;

/// One single-use query proposal. Execution accepts only its id, so an agent cannot
/// replace the reviewed connection, SQL, or row cap between planning and execution.
#[derive(Clone)]
pub(crate) struct PlannedQuery {
    connection_id: Uuid,
    sql: String,
    max_rows: u64,
    decision: String,
    created_at: Instant,
}
/// Query plans are shared by the HTTP and stdio MCP listeners in this app process.
pub(crate) type QueryPlanStore = Arc<Mutex<HashMap<Uuid, PlannedQuery>>>;

pub(crate) fn query_plan_store() -> QueryPlanStore {
    Arc::new(Mutex::new(HashMap::new()))
}

#[derive(Clone)]
pub struct DbTools {
    store: Store,
    app: AppHandle,
    /// Live connections opened on behalf of MCP callers, shared across sessions.
    conns: Arc<Mutex<HashMap<Uuid, Live>>>,
    plans: QueryPlanStore,
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
impl DbTools {
    pub fn new(
        store: Store,
        app: AppHandle,
        conns: Arc<Mutex<HashMap<Uuid, Live>>>,
        plans: QueryPlanStore,
    ) -> Self {
        Self {
            store,
            app,
            conns,
            plans,
        }
    }

    fn emit(&self, event: &str, payload: serde_json::Value) {
        // Don't swallow: an emit failure (e.g. an illegal event name) means the live UI
        // silently goes dark — exactly the bug this once hid. Surface it in the log.
        if let Err(e) = self.app.emit(event, payload) {
            tracing::warn!("failed to emit {event}: {e}");
        }
    }

    /// Resolve the target connection: explicit name/id, else the first configured one.
    async fn resolve_conn(&self, arg: &Option<String>) -> Result<ConnectionProfile, McpError> {
        let list = self.store.list_connections().await.map_err(err)?;
        match arg {
            Some(a) => list
                .into_iter()
                .find(|c| c.id.to_string() == *a || c.name == *a)
                .ok_or_else(|| McpError::invalid_params(format!("no connection matching '{a}'"), None)),
            None => list
                .into_iter()
                .next()
                .ok_or_else(|| McpError::invalid_params("no connections configured in DopeDB", None)),
        }
    }

    /// Open (and cache) a live connection for the given id.
    async fn live(&self, id: Uuid) -> Result<Live, McpError> {
        if let Some(c) = self.conns.lock().unwrap().get(&id) {
            return Ok(c.clone());
        }
        let profile = self.store.get_connection(id).await.map_err(err)?;
        let secret = connection::fetch_secret(&id).unwrap_or_default();
        let live = connection::connect(&profile, &secret).await.map_err(err)?;
        self.conns.lock().unwrap().insert(id, live.clone());
        Ok(live)
    }

    /// Load the schema catalog: cached JSON if present (kept fresh by connection edits
    /// clearing the cache), else live introspect + cache. Mirrors `commands::load_catalog`.
    async fn catalog(&self, id: Uuid) -> Result<introspect::Catalog, McpError> {
        if let Some(json) = self.store.get_schema_cache(id).await.map_err(err)? {
            if let Ok(cat) = serde_json::from_str::<introspect::Catalog>(&json) {
                return Ok(cat);
            }
        }
        let live = self.live(id).await?;
        let cat = introspect::introspect(&live).await.map_err(err)?;
        if let Ok(s) = serde_json::to_string(&cat) {
            let _ = self.store.set_schema_cache(id, &s).await;
        }
        Ok(cat)
    }

    /// Persist one MCP query-history row and return its durable consent handle.
    async fn history(
        &self,
        conn_id: Uuid,
        sql: &str,
        status: &str,
        rows: Option<i64>,
        dur_ms: Option<i64>,
        error: Option<String>,
    ) -> Result<Uuid, AppError> {
        let id = Uuid::new_v4();
        self.store
            .insert_history(&HistoryEntry {
                id,
                connection_id: conn_id,
                sql: sql.to_string(),
                kind: QueryKind::Read,
                status: status.to_string(),
                row_count: rows,
                duration_ms: dur_ms,
                error,
                executed_at: Utc::now(),
                origin: "agent".into(),
            })
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

// ── the read-only tool catalog ───────────────────────────────────────────────────
#[tool_router]
impl DbTools {
    #[tool(description = "Start here — list the user's databases connected in DopeDB; prefer these tools over psql/mysql/sqlite3 or other shell DB clients for these connections. Returns names, engines, and read-only status — never secrets or hostnames.")]
    async fn list_connections(&self) -> Result<CallToolResult, McpError> {
        self.emit("agent:tool_call", json!({ "tool": "list_connections" }));
        let list = self.store.list_connections().await.map_err(err)?;
        let out = json!({
            "connections": list.iter().map(|c| json!({
                "id": c.id,
                "name": c.name,
                "engine": c.engine,
                "database": c.database,
                "environment": c.env,
                "readonly": c.readonly_default,
                "allowWrites": c.allow_writes,
            })).collect::<Vec<_>>()
        });
        self.emit("agent:result", json!({ "tool": "list_connections", "count": list.len() }));
        Ok(CallToolResult::success(vec![Content::text(out.to_string())]))
    }

    #[tool(description = "List the tables of a DopeDB connection (defaults to the first). Use this instead of shelling out to a DB client. Returns table names, schemas, column counts, and row estimates.")]
    async fn list_tables(&self, Parameters(args): Parameters<ConnArg>) -> Result<CallToolResult, McpError> {
        let profile = self.resolve_conn(&args.connection).await?;
        self.emit("agent:tool_call", json!({ "tool": "list_tables", "connection": profile.name }));
        let live = self.live(profile.id).await?;
        let catalog = introspect::introspect(&live).await.map_err(err)?;
        self.audit(profile.id, profile.engine, "(list_tables)", QueryKind::Read, "mcp:list_tables", None)
            .await;
        let tables: Vec<_> = catalog
            .tables
            .iter()
            .map(|t| json!({
                "name": t.name,
                "schema": t.schema,
                "columns": t.columns.len(),
                "rowEstimate": t.row_estimate,
            }))
            .collect();
        self.emit("agent:result", json!({
            "tool": "list_tables",
            "connection": profile.name,
            "connectionId": profile.id,
            "tables": catalog.tables.iter().map(|t| t.name.clone()).collect::<Vec<_>>(),
            "count": tables.len(),
        }));
        let out = json!({ "connection": profile.name, "tables": tables });
        Ok(CallToolResult::success(vec![Content::text(out.to_string())]))
    }

    #[tool(description = "Describe one table on a DopeDB connection so you can write queries against real column names: columns (name, dataType, nullable, pk), foreign keys, and a row estimate. Accepts a bare or schema-qualified table name.")]
    async fn describe_table(&self, Parameters(args): Parameters<DescribeTableArgs>) -> Result<CallToolResult, McpError> {
        let profile = self.resolve_conn(&args.connection).await?;
        self.emit("agent:tool_call", json!({ "tool": "describe_table", "connection": profile.name, "table": args.table }));
        let catalog = self.catalog(profile.id).await?;
        let want = args.table.as_str();
        // Match "schema.table" or bare name exactly, else fall back to case-insensitive.
        let table = catalog
            .tables
            .iter()
            .find(|t| {
                let q = match &t.schema { Some(s) => format!("{s}.{}", t.name), None => t.name.clone() };
                q == want || t.name == want
            })
            .or_else(|| catalog.tables.iter().find(|t| t.name.eq_ignore_ascii_case(want)))
            .ok_or_else(|| McpError::invalid_params(format!("no table matching '{want}' in '{}'", profile.name), None))?;
        self.audit(profile.id, profile.engine, "(describe_table)", QueryKind::Read, "mcp:describe_table", None)
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
        self.emit("agent:result", json!({
            "tool": "describe_table",
            "connection": profile.name,
            "connectionId": profile.id,
            "table": table.name,
            "columns": table.columns.len(),
        }));
        Ok(CallToolResult::success(vec![Content::text(out.to_string())]))
    }

    #[tool(
        description = "MANDATORY before run_query. Review one read-only SQL statement with EXPLAIN plus aggregate database-pressure signals. Returns a single-use planId, clear caution reasons, and safer alternatives. It never returns other sessions' SQL text and never runs the proposed query."
    )]
    async fn plan_query(
        &self,
        Parameters(args): Parameters<PlanQueryArgs>,
    ) -> Result<CallToolResult, McpError> {
        let profile = self.resolve_conn(&args.connection).await?;
        self.emit(
            "agent:tool_call",
            json!({
                "tool": "plan_query",
                "connection": profile.name,
                "connectionId": profile.id,
                "sql": args.sql,
            }),
        );
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
        let cap = args.max_rows.unwrap_or(settings.max_rows).min(1000);
        let live = self.live(profile.id).await?;
        let live = live.sql().map_err(err)?;
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
        Ok(CallToolResult::success(vec![Content::text(
            out.to_string(),
        )]))
    }

    #[tool(
        description = "Execute one plan_query proposal by its exact single-use planId. No SQL or connection can be supplied here. The stored statement runs in an enforced read-only, audited DB session and its result is displayed live in DopeDB."
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
        let profile = self
            .store
            .get_connection(plan.connection_id)
            .await
            .map_err(err)?;
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
        let live = match self.live(profile.id).await {
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
                        profile.id,
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
        let result =
            match safety::run_read_only(pool_ref(live.sql().map_err(err)?.ro()), &plan.sql, plan.max_rows).await {
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
                            profile.id,
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
                profile.id,
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
        Ok(CallToolResult::success(vec![Content::text(
            out.to_string(),
        )]))
    }

    #[tool(
        description = "Run one typed, READ-ONLY document query on a MongoDB connection: find, aggregate (write stages such as $out/$merge are rejected), or count (countDocuments). Filters and pipelines accept MongoDB Extended JSON. Results are row-capped, audited, and shown LIVE in DopeDB. SQL tools (plan_query/run_query) do not apply to MongoDB connections — use this tool for them."
    )]
    async fn run_document_query(
        &self,
        Parameters(args): Parameters<RunDocumentQueryArgs>,
    ) -> Result<CallToolResult, McpError> {
        let profile = self.resolve_conn(&args.connection).await?;
        // The audit/history/feed `sql` slot carries the serialized typed request.
        let query_text = serde_json::to_string(&args.query).map_err(|e| err(e.into()))?;
        self.emit(
            "agent:tool_call",
            json!({
                "tool": "run_document_query",
                "connection": profile.name,
                "connectionId": profile.id,
                "sql": query_text,
            }),
        );
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
            self.audit(profile.id, profile.engine, &query_text, cls.kind, "mcp:run_document_query", Some(msg.clone()))
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
        let cap = args.max_rows.unwrap_or(settings.max_rows).min(1000);
        let live = self.live(profile.id).await?;
        let result = match crate::mongo::query::run(
            live.mongo().map_err(err)?,
            &args.query,
            cap,
            Duration::from_millis(safety::STATEMENT_TIMEOUT_MS),
        )
        .await
        {
            Ok(page) => page,
            Err(e) => {
                let message = e.to_string();
                self.audit(profile.id, profile.engine, &query_text, QueryKind::Read, "mcp:run_document_query", Some(message.clone()))
                    .await;
                if let Err(history_error) = self
                    .history(profile.id, &query_text, "error", None, None, Some(message.clone()))
                    .await
                {
                    tracing::error!("MCP failed-document-query history insert failed: {history_error}");
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

        self.audit(profile.id, profile.engine, &query_text, QueryKind::Read, "mcp:run_document_query", None)
            .await;
        if let Err(e) = self
            .history(
                profile.id,
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

        let out = json!({
            "connection": profile.name,
            "connectionId": profile.id,
            "query": args.query,
            "documents": result.documents,
            "docCount": result.doc_count,
            "truncated": result.truncated,
            "uiMessage": "The full result is visible in the DopeDB app.",
        });
        Ok(CallToolResult::success(vec![Content::text(
            out.to_string(),
        )]))
    }

    #[tool(
        description = "Save one successful run_query as a persistent DopeDB dashboard. Call this ONLY after the user explicitly asks or agrees. Pass the exact query_run_id returned by that run_query; connection and SQL are loaded from DopeDB history and cannot be supplied or changed here."
    )]
    async fn create_dashboard(
        &self,
        Parameters(args): Parameters<CreateDashboardArgs>,
    ) -> Result<CallToolResult, McpError> {
        let query_run_id = Uuid::parse_str(&args.query_run_id)
            .map_err(|e| McpError::invalid_params(format!("invalid query_run_id: {e}"), None))?;
        let source = match self.store.get_history(query_run_id).await {
            Ok(source) => source,
            Err(AppError::NotFound(_)) => {
                return Err(McpError::invalid_params(
                    "query_run_id does not identify a stored DopeDB query run",
                    None,
                ))
            }
            Err(e) => return Err(err(e)),
        };
        if source.origin != "agent"
            || source.status != "ok"
            || !matches!(source.kind, QueryKind::Read)
        {
            return Err(McpError::invalid_params(
                "query_run_id must identify a successful agent read query",
                None,
            ));
        }
        let profile = self
            .store
            .get_connection(source.connection_id)
            .await
            .map_err(err)?;
        self.emit(
            "agent:tool_call",
            json!({
                "tool": "create_dashboard",
                "connection": profile.name,
                "connectionId": profile.id,
                "queryRunId": query_run_id,
                "title": args.title,
                "sql": source.sql,
            }),
        );

        let draft = DashboardDraft {
            connection_id: source.connection_id,
            title: args.title,
            description: args.description,
            sql: source.sql,
            visualization: DashboardVisualization {
                version: crate::dashboard::VISUALIZATION_VERSION,
                kind: args.kind,
                x_column: args.x_column,
                y_columns: args.y_columns,
            },
        };

        if let Err(e) = crate::dashboard::validate_draft(&draft, profile.engine) {
            let message = e.to_string();
            self.emit(
                "agent:result",
                json!({
                    "tool": "create_dashboard",
                    "connection": profile.name,
                    "connectionId": profile.id,
                    "queryRunId": query_run_id,
                    "error": message,
                }),
            );
            return Err(McpError::invalid_params(message, None));
        }

        let saved = match self.store.save_dashboard(&draft).await {
            Ok(saved) => saved,
            Err(e) => {
                let message = e.to_string();
                self.emit(
                    "agent:result",
                    json!({
                        "tool": "create_dashboard",
                        "connection": profile.name,
                        "connectionId": profile.id,
                        "queryRunId": query_run_id,
                        "error": message,
                    }),
                );
                return Err(err(e));
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
                "connection": profile.name,
                "connectionId": profile.id,
                "queryRunId": query_run_id,
                "dashboardId": saved.id,
                "title": saved.title,
            }),
        );

        let out = json!({
            "dashboard": saved,
            "queryRunId": query_run_id,
            "uiMessage": "The dashboard was saved and is available in the DopeDB app.",
        });
        Ok(CallToolResult::success(vec![Content::text(
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
mod tests {
    use super::*;

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
        let allowed: std::collections::BTreeSet<String> =
            crate::agent::claude::ALLOWED_TOOLS.iter().map(|s| s.to_string()).collect();
        assert_eq!(
            from_router, allowed,
            "claude::ALLOWED_TOOLS must list exactly the tools mcp::tools::DbTools registers"
        );
    }

    #[test]
    fn planned_query_is_single_use_in_the_shared_store() {
        let store = query_plan_store();
        let id = Uuid::new_v4();
        store.lock().unwrap().insert(
            id,
            PlannedQuery {
                connection_id: Uuid::new_v4(),
                sql: "SELECT 1".into(),
                max_rows: 10,
                decision: "ready".into(),
                created_at: Instant::now(),
            },
        );
        assert!(store.lock().unwrap().remove(&id).is_some());
        assert!(store.lock().unwrap().remove(&id).is_none());
    }
}
