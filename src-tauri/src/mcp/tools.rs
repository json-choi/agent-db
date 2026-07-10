//! MCP tool handlers (Phase 0 spike). Read-only tools that REUSE the existing safety
//! pipeline — L1 classify + L2 read-only session (authoritative) — and the connection
//! manager, and emit Tauri events so the app window reacts live to each agent call.
//!
//! There is deliberately NO write tool here yet: writes (approval-gated via L4) land in
//! Phase 2. `run_query` runs through the read-only DB session, so even a misclassified
//! write is rejected by the database itself (PG 25006 / SQLITE_READONLY).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

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
use crate::connection::{self, DbPool, LiveConnection};
use crate::error::AppError;
use crate::introspect;
use crate::model::{
    ConnectionProfile, DashboardDraft, DashboardKind, DashboardVisualization, Engine, HistoryEntry,
    QueryKind,
};
use crate::safety::{self, PoolRef};
use crate::store::Store;

#[derive(Clone)]
pub struct DbTools {
    store: Store,
    app: AppHandle,
    /// Live connections opened on behalf of MCP callers, shared across sessions.
    conns: Arc<Mutex<HashMap<Uuid, LiveConnection>>>,
}

// ── tool argument shapes (JSON Schema is derived for the MCP tool manifest) ──────
#[derive(Debug, Deserialize, JsonSchema)]
struct ConnArg {
    /// Connection name or id. Omit to use the first configured connection.
    #[serde(default)]
    connection: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RunQueryArgs {
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
        conns: Arc<Mutex<HashMap<Uuid, LiveConnection>>>,
    ) -> Self {
        Self { store, app, conns }
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
    async fn live(&self, id: Uuid) -> Result<LiveConnection, McpError> {
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
        let out = json!({
            "connection": profile.name,
            "schema": table.schema,
            "table": table.name,
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

    #[tool(description = "Run a READ-ONLY SQL query (SELECT) on a DopeDB connection — prefer this over psql/shell clients. Executes in an enforced read-only, audited DB session, so writes are rejected by the database. Returns columns + rows, AND displays the result table live to the user in the DopeDB desktop app — running a query here is how the user SEES the answer.")]
    async fn run_query(&self, Parameters(args): Parameters<RunQueryArgs>) -> Result<CallToolResult, McpError> {
        let profile = self.resolve_conn(&args.connection).await?;
        self.emit("agent:tool_call", json!({ "tool": "run_query", "connection": profile.name, "sql": args.sql }));

        // L1 classify: reject obvious non-reads early (L2 is the authoritative stop).
        let cls = safety::classify(&args.sql, profile.engine).map_err(err)?;
        if !matches!(cls.kind, QueryKind::Read) {
            let msg = "run_query only runs read (SELECT) statements; writes go through an approval-gated tool (coming soon)";
            self.emit("agent:result", json!({ "tool": "run_query", "error": msg }));
            self.audit(
                profile.id,
                profile.engine,
                &args.sql,
                cls.kind,
                "mcp:run_query",
                Some(msg.to_string()),
            )
            .await;
            if let Err(e) = self
                .history(
                    profile.id,
                    &args.sql,
                    "blocked",
                    None,
                    None,
                    Some(msg.to_string()),
                )
                .await
            {
                tracing::error!("MCP blocked-query history insert failed: {e}");
            }
            return Err(McpError::invalid_params(msg, None));
        }

        let settings = self.store.get_safety(profile.id).await.map_err(err)?;
        let cap = args.max_rows.unwrap_or(settings.max_rows).min(1000);
        let live = match self.live(profile.id).await {
            Ok(live) => live,
            Err(e) => {
                let message = e.message.to_string();
                self.audit(
                    profile.id,
                    profile.engine,
                    &args.sql,
                    QueryKind::Read,
                    "mcp:run_query",
                    Some(message.clone()),
                )
                .await;
                if let Err(history_error) = self
                    .history(
                        profile.id,
                        &args.sql,
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
                        "sql": args.sql,
                        "error": message,
                    }),
                );
                return Err(e);
            }
        };

        // L2 authoritative read-only session — a misclassified write is rejected at the DB.
        let result = match safety::run_read_only(pool_ref(live.ro()), &args.sql, cap).await {
            Ok(result) => result,
            Err(e) => {
                let message = e.to_string();
                self.audit(
                    profile.id,
                    profile.engine,
                    &args.sql,
                    QueryKind::Read,
                    "mcp:run_query",
                    Some(message.clone()),
                )
                .await;
                if let Err(history_error) = self
                    .history(
                        profile.id,
                        &args.sql,
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
                        "sql": args.sql,
                        "error": message,
                    }),
                );
                return Err(err(e));
            }
        };
        self.audit(
            profile.id,
            profile.engine,
            &args.sql,
            QueryKind::Read,
            "mcp:run_query",
            None,
        )
        .await;
        let query_run_id = match self
            .history(
                profile.id,
                &args.sql,
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
                        "sql": args.sql,
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
                "queryRunId": query_run_id,
                "sql": args.sql,
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
            "queryRunId": query_run_id,
            "sql": args.sql,
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
    instructions = "These tools are the PREFERRED way to inspect or query any database the user has connected in DopeDB. When the user asks you to look at, browse, or query one of their managed databases, use these tools — do NOT reach for psql, mysql, sqlite3, or other shell/database clients for those connections. Reasons: (1) every query runs in an enforced READ-ONLY session, so it is safe; (2) calls are audited; and (3) results are shown LIVE to the user inside the DopeDB desktop app — running a query here is HOW THE USER SEES THE ANSWER, not just your chat reply. Workflow: call `list_connections` to find the user's databases, then `list_tables` and/or `describe_table` to get exact table and column names, then `run_query` with a single SELECT. After a successful run_query, tell the user the full result is visible in DopeDB and ask whether they want to save that exact query as a dashboard. Only after the user explicitly asks or agrees, call `create_dashboard` and pass the exact returned `queryRunId` as its `query_run_id` argument; never create one automatically and never substitute different SQL or a different connection. Saved dashboards persist locally and rerun through a dedicated read-only command. Writes are rejected by the read-only session (approval-gated writes are coming soon)."
)]
impl ServerHandler for DbTools {}

#[cfg(test)]
mod tests {
    use super::*;

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
}
