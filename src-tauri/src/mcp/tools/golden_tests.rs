//! Executable Phase 0 snapshots for the MCP behavior that must survive service extraction.

use std::collections::HashMap;
use std::str::FromStr;
use std::time::{Duration, Instant};

use chrono::{DateTime, TimeZone, Utc};
use rmcp::model::CallToolResult;
use serde_json::{json, Value};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tempfile::TempDir;
use uuid::Uuid;

use crate::audit;
use crate::connection::{ConnectionAccess, ConnectionManager};
use crate::model::{
    ConnectionProfile, DocumentPage, DocumentQuery, Engine, HistoryEntry, Provider, QueryKind,
    RiskLevel, WorkspaceConnectionAccess, WorkspaceCredentialMode,
};
use crate::monitoring::HealthSnapshot;
use crate::services::planning_guidance;
use crate::store::{Store, TEST_SCHEMA};

use super::*;

const SQLITE_CONNECTION_ID: &str = "018f9999-8888-7777-8666-555544443333";
const MONGO_CONNECTION_ID: &str = "018f9999-8888-7777-8666-555544443334";
const SQLITE_CONNECTION_NAME: &str = "phase0-sqlite";

struct SqliteHarness {
    store: Store,
    tools: DbTools,
    connections: ConnectionManager,
    connection_id: Uuid,
    events: RecordedToolEvents,
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
        let target_path = directory.path().join("phase0-target.db");
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
            r#"
            CREATE TABLE teams (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL
            );
            CREATE TABLE users (
                id INTEGER PRIMARY KEY,
                team_id INTEGER NOT NULL REFERENCES teams(id),
                name TEXT NOT NULL
            );
            CREATE INDEX idx_users_name ON users(name);
            INSERT INTO teams (id, name) VALUES (1, 'Core');
            INSERT INTO users (id, team_id, name) VALUES
                (1, 1, 'Ada'),
                (2, 1, 'Linus');
            CREATE VIEW user_names AS SELECT id, name FROM users;
            "#,
        )
        .execute(&target_pool)
        .await
        .unwrap();
        target_pool.close().await;

        let connection_id = Uuid::parse_str(SQLITE_CONNECTION_ID).unwrap();
        let profile = profile(
            connection_id,
            SQLITE_CONNECTION_NAME,
            Engine::Sqlite,
            target_path.to_string_lossy().into_owned(),
        );
        store.upsert_connection(&profile).await.unwrap();
        let connections = ConnectionManager::new(store.clone());
        let (operation, _) = crate::operations::OperationRuntime::new(&store);
        let services = crate::services::ApplicationServices::new(
            store.clone(),
            connections.clone(),
            operation,
        );
        let (tools, events) = DbTools::new_for_test(services);

        Self {
            store,
            tools,
            connections,
            connection_id,
            events,
            directory,
        }
    }

    fn selector(&self) -> Option<String> {
        Some(SQLITE_CONNECTION_NAME.into())
    }

    async fn close(self) {
        let mutation = self
            .connections
            .begin_connection_mutation(self.connection_id, ConnectionAccess::Read)
            .await
            .unwrap();
        mutation.retire_connection(self.connection_id).await;

        let Self {
            store,
            tools,
            connections,
            events,
            directory,
            ..
        } = self;
        drop(tools);
        drop(connections);
        drop(events);
        store.pool().close().await;
        drop(store);
        directory
            .close()
            .expect("temporary SQLite directory must be removable after pool shutdown");
    }
}

fn profile(id: Uuid, name: &str, engine: Engine, database: String) -> ConnectionProfile {
    ConnectionProfile {
        id,
        name: name.into(),
        engine,
        provider: Provider::Generic,
        driver_id: match engine {
            Engine::Sqlite => Some("sqlx-sqlite".into()),
            Engine::Mongodb => Some("mongodb".into()),
            Engine::Postgres | Engine::Mysql => None,
        },
        host: if engine == Engine::Mongodb {
            "localhost".into()
        } else {
            String::new()
        },
        port: if engine == Engine::Mongodb { 27017 } else { 0 },
        database,
        username: String::new(),
        sslmode: "disable".into(),
        extra_params: HashMap::new(),
        readonly_default: true,
        allow_writes: false,
        secret_ref: None,
        env: None,
        schema_group: None,
        workspace_access: WorkspaceConnectionAccess::Local,
        credential_mode: WorkspaceCredentialMode::Local,
    }
}

fn tool_json(result: CallToolResult) -> Value {
    assert_ne!(result.is_error, Some(true));
    assert_eq!(result.content.len(), 1);
    let text = result.content[0]
        .as_text()
        .expect("MCP fixture result must contain text");
    serde_json::from_str(&text.text).expect("MCP fixture text must contain JSON")
}

fn normalized_uuid(value: &mut Value, pointer: &str, marker: &str) -> String {
    let slot = value
        .pointer_mut(pointer)
        .unwrap_or_else(|| panic!("missing dynamic UUID at {pointer}"));
    let raw = slot
        .as_str()
        .unwrap_or_else(|| panic!("dynamic UUID at {pointer} is not a string"))
        .to_string();
    Uuid::parse_str(&raw).unwrap_or_else(|_| panic!("invalid dynamic UUID at {pointer}"));
    *slot = Value::String(marker.into());
    raw
}

fn normalize_timestamp(value: &mut Value, pointer: &str) -> String {
    let slot = value
        .pointer_mut(pointer)
        .unwrap_or_else(|| panic!("missing dynamic timestamp at {pointer}"));
    let raw = slot
        .as_str()
        .unwrap_or_else(|| panic!("dynamic timestamp at {pointer} is not a string"))
        .to_string();
    DateTime::parse_from_rfc3339(&raw)
        .unwrap_or_else(|_| panic!("invalid dynamic timestamp at {pointer}"));
    *slot = Value::String("<timestamp>".into());
    raw
}

fn normalize_string_matches(value: &mut Value, raw: &str, marker: &str) {
    match value {
        Value::Array(values) => {
            for value in values {
                normalize_string_matches(value, raw, marker);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                normalize_string_matches(value, raw, marker);
            }
        }
        Value::String(value) if value == raw => *value = marker.into(),
        _ => {}
    }
}

fn normalize_duration(value: &mut Value, pointer: &str) {
    let slot = value
        .pointer_mut(pointer)
        .unwrap_or_else(|| panic!("missing dynamic duration at {pointer}"));
    assert!(
        slot.as_u64().is_some() || slot.as_i64().is_some(),
        "dynamic duration at {pointer} is not an integer"
    );
    *slot = Value::String("<duration-ms>".into());
}

fn fixture_success() -> Value {
    serde_json::from_str(include_str!(
        "../../../tests/fixtures/mcp/read-behavior-success-v1.json"
    ))
    .unwrap()
}

fn fixture_errors() -> Value {
    serde_json::from_str(include_str!(
        "../../../tests/fixtures/mcp/read-behavior-errors-v1.json"
    ))
    .unwrap()
}

fn error_case(name: &str, error: McpError) -> Value {
    json!({
        "case": name,
        "error": serde_json::to_value(error).unwrap(),
    })
}

#[tokio::test]
async fn sqlite_read_flow_matches_phase_zero_golden() {
    let harness = SqliteHarness::new().await;

    let mut list_connections = tool_json(harness.tools.list_connections().await.unwrap());
    assert_eq!(
        list_connections["connections"][0]["id"],
        SQLITE_CONNECTION_ID
    );
    let sqlite_database = list_connections["connections"][0]["database"]
        .as_str()
        .expect("SQLite connection must expose its database selector")
        .to_string();
    assert!(sqlite_database.ends_with("phase0-target.db"));
    list_connections["connections"][0]["database"] = Value::String("<sqlite-path>".into());

    let list_tables = tool_json(
        harness
            .tools
            .list_tables(Parameters(ConnArg { connection: None }))
            .await
            .unwrap(),
    );
    let describe_table = tool_json(
        harness
            .tools
            .describe_table(Parameters(DescribeTableArgs {
                connection: Some(harness.connection_id.to_string()),
                table: "users".into(),
            }))
            .await
            .unwrap(),
    );

    let mut plan_query = tool_json(
        harness
            .tools
            .plan_query(Parameters(PlanQueryArgs {
                connection: harness.selector(),
                sql: "SELECT id, name FROM users ORDER BY id".into(),
                max_rows: Some(1),
            }))
            .await
            .unwrap(),
    );
    let plan_id = normalized_uuid(&mut plan_query, "/planId", "<plan-id>");
    let _ = normalize_timestamp(&mut plan_query, "/health/capturedAt");

    let mut run_query = tool_json(
        harness
            .tools
            .run_query(Parameters(RunQueryArgs {
                plan_id: plan_id.clone(),
            }))
            .await
            .unwrap(),
    );
    assert_eq!(run_query["planId"], plan_id);
    let run_plan_id = normalized_uuid(&mut run_query, "/planId", "<plan-id>");
    assert_eq!(run_plan_id, plan_id);
    let query_run_id = normalized_uuid(&mut run_query, "/queryRunId", "<query-run-id>");

    let history = harness
        .store
        .list_history(harness.connection_id)
        .await
        .unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].id.to_string(), query_run_id);
    assert!(history[0].duration_ms.is_some());
    let history_projection = history
        .iter()
        .map(|entry| {
            json!({
                "sql": entry.sql,
                "kind": entry.kind,
                "status": entry.status,
                "rowCount": entry.row_count,
                "error": entry.error,
                "origin": entry.origin,
            })
        })
        .collect::<Vec<_>>();

    let (mut audit_entries, chain_ok, first_bad) =
        audit::snapshot(&harness.store, harness.connection_id)
            .await
            .unwrap();
    assert!(chain_ok);
    assert_eq!(first_bad, None);
    audit_entries.reverse();
    let audit_projection = audit_entries
        .iter()
        .map(|entry| {
            assert!(!entry.hash.is_empty());
            json!({
                "sql": entry.sql,
                "kind": entry.kind,
                "action": entry.action,
                "error": entry.error,
            })
        })
        .collect::<Vec<_>>();

    let dashboard_args: CreateDashboardArgs = serde_json::from_value(json!({
        "query_run_id": query_run_id,
        "title": "User directory",
        "description": "Phase 0 provenance fixture",
        "kind": "table",
        "x_column": null,
        "y_columns": [],
        "connection": "attacker-selected",
        "sql": "DELETE FROM users"
    }))
    .unwrap();
    let mut create_dashboard = tool_json(
        harness
            .tools
            .create_dashboard(Parameters(dashboard_args))
            .await
            .unwrap(),
    );
    assert_eq!(create_dashboard["queryRunId"], query_run_id);
    assert_eq!(
        create_dashboard["dashboard"]["sql"],
        "SELECT id, name FROM users ORDER BY id"
    );
    assert_eq!(
        create_dashboard["dashboard"]["connectionId"],
        SQLITE_CONNECTION_ID
    );
    let dashboard_id = normalized_uuid(&mut create_dashboard, "/dashboard/id", "<dashboard-id>");
    normalized_uuid(&mut create_dashboard, "/queryRunId", "<query-run-id>");
    let dashboard_created_at = normalize_timestamp(&mut create_dashboard, "/dashboard/createdAt");
    let dashboard_updated_at = normalize_timestamp(&mut create_dashboard, "/dashboard/updatedAt");

    let dashboards = harness
        .store
        .list_dashboards(harness.connection_id)
        .await
        .unwrap();
    assert_eq!(dashboards.len(), 1);
    assert_eq!(dashboards[0].sql, "SELECT id, name FROM users ORDER BY id");

    let mut events = Value::Array(harness.events.lock().unwrap().clone());
    let raw_events = events.to_string();
    for forbidden in [
        sqlite_database.as_str(),
        "\"host\"",
        "\"port\"",
        "\"username\"",
        "\"secretRef\"",
        "\"secret_ref\"",
    ] {
        assert!(
            !raw_events.contains(forbidden),
            "recorded MCP event leaked {forbidden}"
        );
    }
    normalize_string_matches(&mut events, &plan_id, "<plan-id>");
    normalize_string_matches(&mut events, &query_run_id, "<query-run-id>");
    normalize_string_matches(&mut events, &dashboard_id, "<dashboard-id>");
    normalize_string_matches(&mut events, &dashboard_created_at, "<timestamp>");
    normalize_string_matches(&mut events, &dashboard_updated_at, "<timestamp>");
    normalize_duration(&mut events, "/9/payload/durationMs");

    let actual = json!({
        "listConnections": list_connections,
        "listTables": list_tables,
        "describeTable": describe_table,
        "planQuery": plan_query,
        "runQuery": run_query,
        "history": history_projection,
        "audit": audit_projection,
        "createDashboard": create_dashboard,
        "events": events,
    });
    assert_eq!(actual, fixture_success()["sqlite"]);
    harness.close().await;
}

#[tokio::test]
async fn query_plan_is_shared_and_single_use_across_mcp_handlers() {
    let harness = SqliteHarness::new().await;
    let (other_handler, _) = DbTools::new_for_test(harness.tools.services.clone());
    let plan = tool_json(
        harness
            .tools
            .plan_query(Parameters(PlanQueryArgs {
                connection: harness.selector(),
                sql: "SELECT id FROM users ORDER BY id".into(),
                max_rows: Some(1),
            }))
            .await
            .unwrap(),
    );
    let plan_id = plan["planId"].as_str().unwrap().to_string();

    other_handler
        .run_query(Parameters(RunQueryArgs {
            plan_id: plan_id.clone(),
        }))
        .await
        .unwrap();
    let reused = harness
        .tools
        .run_query(Parameters(RunQueryArgs { plan_id }))
        .await
        .unwrap_err();
    let wire = serde_json::to_value(reused).unwrap();
    assert_eq!(wire["code"], -32602);
    assert_eq!(
        wire["message"],
        "plan_id is unknown, expired, or already used; call plan_query again"
    );

    drop(other_handler);
    harness.close().await;
}

#[tokio::test]
async fn execution_failure_adapter_emits_the_exact_error_result() {
    let harness = SqliteHarness::new().await;
    let context = harness
        .connections
        .pin(harness.connection_id, ConnectionAccess::Read)
        .await
        .unwrap();
    let plan_id = Uuid::new_v4();
    harness.tools.services.query.seed_plan_for_test(
        plan_id,
        context.pin(),
        "SELECT no_such_function()".into(),
        1,
        "ready".into(),
        Instant::now(),
    );
    drop(context);

    let error = harness
        .tools
        .run_query(Parameters(RunQueryArgs {
            plan_id: plan_id.to_string(),
        }))
        .await
        .unwrap_err();
    let wire = serde_json::to_value(error).unwrap();
    assert_eq!(wire["code"], -32603);
    let message = wire["message"].as_str().unwrap();
    assert!(message.contains("no_such_function"));

    {
        let events = harness.events.lock().unwrap();
        assert_eq!(
            events.as_slice(),
            [
                json!({
                    "event": "agent:tool_call",
                    "payload": {
                        "tool": "run_query",
                        "connection": SQLITE_CONNECTION_NAME,
                        "connectionId": harness.connection_id,
                        "planId": plan_id,
                        "sql": "SELECT no_such_function()",
                    },
                }),
                json!({
                    "event": "agent:result",
                    "payload": {
                        "tool": "run_query",
                        "connection": SQLITE_CONNECTION_NAME,
                        "connectionId": harness.connection_id,
                        "planId": plan_id,
                        "sql": "SELECT no_such_function()",
                        "error": message,
                    },
                }),
            ]
        );
    }

    harness.close().await;
}

#[tokio::test]
async fn consent_history_failure_adapter_never_emits_success_rows() {
    let harness = SqliteHarness::new().await;
    let context = harness
        .connections
        .pin(harness.connection_id, ConnectionAccess::Read)
        .await
        .unwrap();
    let plan_id = Uuid::new_v4();
    harness.tools.services.query.seed_plan_for_test(
        plan_id,
        context.pin(),
        "SELECT id, name FROM users ORDER BY id".into(),
        2,
        "ready".into(),
        Instant::now(),
    );
    drop(context);
    sqlx::raw_sql(
        "CREATE TRIGGER fail_success_query_history_adapter
         BEFORE INSERT ON query_history
         WHEN NEW.status = 'ok'
         BEGIN
           SELECT RAISE(FAIL, 'forced adapter consent history failure');
         END;",
    )
    .execute(harness.store.pool())
    .await
    .unwrap();

    let error = harness
        .tools
        .run_query(Parameters(RunQueryArgs {
            plan_id: plan_id.to_string(),
        }))
        .await
        .unwrap_err();
    let wire = serde_json::to_value(error).unwrap();
    assert_eq!(wire["code"], -32603);
    let internal_message = wire["message"].as_str().unwrap();
    assert!(internal_message.contains("forced adapter consent history failure"));
    let event_message = format!(
        "query succeeded but its consent handle could not be persisted: {internal_message}"
    );

    {
        let events = harness.events.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0],
            json!({
                "event": "agent:tool_call",
                "payload": {
                    "tool": "run_query",
                    "connection": SQLITE_CONNECTION_NAME,
                    "connectionId": harness.connection_id,
                    "planId": plan_id,
                    "sql": "SELECT id, name FROM users ORDER BY id",
                },
            })
        );
        assert_eq!(
            events[1],
            json!({
                "event": "agent:result",
                "payload": {
                    "tool": "run_query",
                    "connection": SQLITE_CONNECTION_NAME,
                    "connectionId": harness.connection_id,
                    "planId": plan_id,
                    "sql": "SELECT id, name FROM users ORDER BY id",
                    "error": event_message,
                },
            })
        );
        let serialized = events[1].to_string();
        for forbidden in ["queryRunId", "\"rows\"", "Ada", "Linus"] {
            assert!(
                !serialized.contains(forbidden),
                "consent failure event leaked {forbidden}"
            );
        }
    }
    assert!(harness
        .store
        .list_history(harness.connection_id)
        .await
        .unwrap()
        .is_empty());

    harness.close().await;
}

#[tokio::test]
async fn document_on_sql_emits_only_the_legacy_tool_call() {
    let harness = SqliteHarness::new().await;
    let query = DocumentQuery::Count {
        collection: "users".into(),
        filter: None,
    };
    let query_text = serde_json::to_string(&query).unwrap();

    let error = harness
        .tools
        .run_document_query(Parameters(RunDocumentQueryArgs {
            connection: harness.selector(),
            query,
            max_rows: None,
        }))
        .await
        .unwrap_err();
    let wire = serde_json::to_value(error).unwrap();
    assert_eq!(wire["code"], -32602);
    assert_eq!(
        wire["message"],
        "run_document_query only works on MongoDB connections — use plan_query/run_query for SQL engines"
    );

    {
        let events = harness.events.lock().unwrap();
        assert_eq!(
            events.as_slice(),
            [json!({
                "event": "agent:tool_call",
                "payload": {
                    "tool": "run_document_query",
                    "connection": SQLITE_CONNECTION_NAME,
                    "connectionId": harness.connection_id,
                    "sql": query_text,
                },
            })]
        );
    }
    assert!(harness
        .store
        .list_history(harness.connection_id)
        .await
        .unwrap()
        .is_empty());

    harness.close().await;
}

#[tokio::test]
async fn missing_document_selector_emits_nothing_and_writes_no_ledger() {
    let harness = SqliteHarness::new().await;

    let error = harness
        .tools
        .run_document_query(Parameters(RunDocumentQueryArgs {
            connection: Some("missing".into()),
            query: DocumentQuery::Count {
                collection: "users".into(),
                filter: None,
            },
            max_rows: None,
        }))
        .await
        .unwrap_err();
    let wire = serde_json::to_value(error).unwrap();
    assert_eq!(wire["code"], -32602);
    assert_eq!(wire["message"], "no connection matching 'missing'");
    assert!(harness.events.lock().unwrap().is_empty());
    let audit_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(harness.store.pool())
        .await
        .unwrap();
    let history_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM query_history")
        .fetch_one(harness.store.pool())
        .await
        .unwrap();
    assert_eq!(audit_count, 0);
    assert_eq!(history_count, 0);

    harness.close().await;
}

#[tokio::test]
async fn document_pin_failure_keeps_only_the_tool_call_event() {
    let harness = SqliteHarness::new().await;
    sqlx::query("UPDATE connections SET workspace_access = 'view' WHERE id = ?")
        .bind(harness.connection_id.to_string())
        .execute(harness.store.pool())
        .await
        .unwrap();
    let query = DocumentQuery::Count {
        collection: "users".into(),
        filter: None,
    };
    let query_text = serde_json::to_string(&query).unwrap();

    let error = harness
        .tools
        .run_document_query(Parameters(RunDocumentQueryArgs {
            connection: harness.selector(),
            query,
            max_rows: None,
        }))
        .await
        .unwrap_err();
    let wire = serde_json::to_value(error).unwrap();
    assert_eq!(wire["code"], -32603);
    assert_eq!(
        wire["message"],
        "blocked: workspace role cannot execute this connection"
    );
    {
        let events = harness.events.lock().unwrap();
        assert_eq!(
            events.as_slice(),
            [json!({
                "event": "agent:tool_call",
                "payload": {
                    "tool": "run_document_query",
                    "connection": SQLITE_CONNECTION_NAME,
                    "connectionId": harness.connection_id,
                    "sql": query_text,
                },
            })]
        );
    }
    let (audit_entries, chain_ok, first_bad) =
        audit::snapshot(&harness.store, harness.connection_id)
            .await
            .unwrap();
    assert!(chain_ok);
    assert_eq!(first_bad, None);
    assert!(audit_entries.is_empty());
    assert!(harness
        .store
        .list_history(harness.connection_id)
        .await
        .unwrap()
        .is_empty());
    sqlx::query("UPDATE connections SET workspace_access = 'local' WHERE id = ?")
        .bind(harness.connection_id.to_string())
        .execute(harness.store.pool())
        .await
        .unwrap();

    harness.close().await;
}

#[tokio::test]
async fn document_setup_failure_keeps_only_the_tool_call_event() {
    let harness = SqliteHarness::new().await;
    let mongo_id = Uuid::parse_str(MONGO_CONNECTION_ID).unwrap();
    let mut mongo = profile(mongo_id, "phase0-mongo", Engine::Mongodb, "app".into());
    mongo.driver_id = Some("missing-document-driver".into());
    harness.store.upsert_connection(&mongo).await.unwrap();
    let query = DocumentQuery::Count {
        collection: "events".into(),
        filter: None,
    };
    let query_text = serde_json::to_string(&query).unwrap();

    let error = harness
        .tools
        .run_document_query(Parameters(RunDocumentQueryArgs {
            connection: Some("phase0-mongo".into()),
            query,
            max_rows: None,
        }))
        .await
        .unwrap_err();
    let wire = serde_json::to_value(error).unwrap();
    assert_eq!(wire["code"], -32603);
    assert_eq!(
        wire["message"],
        "config error: unknown database driver \"missing-document-driver\""
    );
    {
        let events = harness.events.lock().unwrap();
        assert_eq!(
            events.as_slice(),
            [json!({
                "event": "agent:tool_call",
                "payload": {
                    "tool": "run_document_query",
                    "connection": "phase0-mongo",
                    "connectionId": mongo_id,
                    "sql": query_text,
                },
            })]
        );
    }
    let (audit_entries, chain_ok, first_bad) =
        audit::snapshot(&harness.store, mongo_id).await.unwrap();
    assert!(chain_ok);
    assert_eq!(first_bad, None);
    assert!(audit_entries.is_empty());
    assert!(harness
        .store
        .list_history(mongo_id)
        .await
        .unwrap()
        .is_empty());

    harness.close().await;
}

#[tokio::test]
async fn document_write_rejection_preserves_result_event_and_no_history() {
    let harness = SqliteHarness::new().await;
    let mongo_id = Uuid::parse_str(MONGO_CONNECTION_ID).unwrap();
    harness
        .store
        .upsert_connection(&profile(
            mongo_id,
            "phase0-mongo",
            Engine::Mongodb,
            "app".into(),
        ))
        .await
        .unwrap();
    let query = DocumentQuery::Aggregate {
        collection: "events".into(),
        pipeline: vec![json!({ "$out": "copied_events" })],
    };
    let query_text = serde_json::to_string(&query).unwrap();
    let message = "aggregate stage \"$out\" is not in the read-only allowlist";

    let error = harness
        .tools
        .run_document_query(Parameters(RunDocumentQueryArgs {
            connection: Some("phase0-mongo".into()),
            query,
            max_rows: None,
        }))
        .await
        .unwrap_err();
    let wire = serde_json::to_value(error).unwrap();
    assert_eq!(wire["code"], -32602);
    assert_eq!(wire["message"], message);

    {
        let events = harness.events.lock().unwrap();
        assert_eq!(
            events.as_slice(),
            [
                json!({
                    "event": "agent:tool_call",
                    "payload": {
                        "tool": "run_document_query",
                        "connection": "phase0-mongo",
                        "connectionId": mongo_id,
                        "sql": query_text,
                    },
                }),
                json!({
                    "event": "agent:result",
                    "payload": {
                        "tool": "run_document_query",
                        "connection": "phase0-mongo",
                        "connectionId": mongo_id,
                        "sql": query_text,
                        "error": message,
                    },
                }),
            ]
        );
    }

    let (audit_entries, chain_ok, first_bad) =
        audit::snapshot(&harness.store, mongo_id).await.unwrap();
    assert!(chain_ok);
    assert_eq!(first_bad, None);
    assert_eq!(audit_entries.len(), 1);
    assert_eq!(audit_entries[0].action, "mcp:run_document_query");
    assert_eq!(audit_entries[0].sql, query_text);
    assert_eq!(audit_entries[0].kind, QueryKind::Write);
    assert_eq!(audit_entries[0].error.as_deref(), Some(message));
    assert!(harness
        .store
        .list_history(mongo_id)
        .await
        .unwrap()
        .is_empty());

    harness.close().await;
}

#[test]
fn document_response_keeps_full_cells_while_event_preview_is_capped() {
    let connection_id = Uuid::parse_str(MONGO_CONNECTION_ID).unwrap();
    let query = DocumentQuery::Find {
        collection: "events".into(),
        filter: None,
        projection: None,
        sort: None,
        skip: None,
        limit: None,
    };
    let large = "x".repeat(CELL_PREVIEW_MAX + 100);
    let page = DocumentPage {
        documents: vec![json!({ "large": large })],
        doc_count: 1,
        truncated: false,
        duration_ms: 7,
    };

    let response = document_query_payload("phase0-mongo", connection_id, &query, &page);
    assert_eq!(
        response,
        json!({
            "connection": "phase0-mongo",
            "connectionId": connection_id,
            "query": query,
            "documents": [{ "large": large }],
            "docCount": 1,
            "truncated": false,
            "uiMessage": "The full result is visible in the DopeDB app.",
        })
    );

    let query_text = serde_json::to_string(&query).unwrap();
    let event = document_success_event_payload("phase0-mongo", connection_id, &query_text, &page);
    let mut preview = page.documents[0]
        .to_string()
        .chars()
        .take(CELL_PREVIEW_MAX)
        .collect::<String>();
    preview.push('…');
    assert_eq!(
        event,
        json!({
            "tool": "run_document_query",
            "connection": "phase0-mongo",
            "connectionId": connection_id,
            "sql": query_text,
            "columns": ["document"],
            "rows": [[preview]],
            "rowCount": 1,
            "truncated": false,
            "durationMs": 7,
        })
    );
}

#[test]
fn pure_payloads_match_phase_zero_golden() {
    let mut prod = profile(
        Uuid::parse_str(SQLITE_CONNECTION_ID).unwrap(),
        "analytics",
        Engine::Postgres,
        "app".into(),
    );
    prod.env = Some("prod".into());
    let health = HealthSnapshot {
        level: "busy".into(),
        coverage: "limited".into(),
        total_connections: Some(85),
        max_connections: Some(100),
        connection_usage_percent: Some(85.0),
        active_queries: Some(8),
        long_running_queries: Some(2),
        lock_waits: Some(1),
        replication_lag_seconds: None,
        reasons: vec!["Database pressure is elevated.".into()],
        captured_at: Utc.with_ymd_and_hms(2026, 7, 24, 0, 0, 0).unwrap(),
    };
    let (decision, notices, suggestions) = planning_guidance(&prod, &health, Some(60_001), 50_000);
    let monitoring = json!({
        "decision": decision,
        "notices": notices,
        "suggestions": suggestions,
    });

    let mongo_profile = profile(
        Uuid::parse_str(MONGO_CONNECTION_ID).unwrap(),
        "phase0-mongo",
        Engine::Mongodb,
        "app".into(),
    );
    let query = DocumentQuery::Find {
        collection: "events".into(),
        filter: Some(json!({ "active": true })),
        projection: Some(json!({ "_id": 1, "active": 1 })),
        sort: Some(json!({ "_id": 1 })),
        skip: Some(0),
        limit: Some(2),
    };
    let classification = crate::mongo::query::classify(&query);
    assert_eq!(classification.kind, QueryKind::Read);
    assert_eq!(classification.risk, RiskLevel::Low);
    let page = DocumentPage {
        documents: vec![
            json!({
                "_id": { "$oid": "018f11112222733384445555" },
                "active": true
            }),
            json!({
                "_id": { "$oid": "018f11112222733384445556" },
                "active": true,
                "large": "9007199254740993"
            }),
        ],
        doc_count: 2,
        truncated: false,
        duration_ms: 7,
    };
    let mongo = json!({
        "classification": {
            "kind": classification.kind,
            "risk": classification.risk,
            "tables": classification.tables,
        },
        "payload": document_query_payload(
            &mongo_profile.name,
            mongo_profile.id,
            &query,
            &page,
        ),
    });

    let fixture = fixture_success();
    assert_eq!(
        fixture["providerCoverage"]["sqlite"],
        "live_local_roundtrip"
    );
    assert_eq!(
        fixture["providerCoverage"]["mongodb"],
        "classifier_and_payload_only"
    );
    assert_eq!(monitoring, fixture["monitoringGuidance"]);
    assert_eq!(mongo, fixture["mongo"]);
}

#[tokio::test]
async fn mcp_owned_errors_match_phase_zero_golden() {
    let harness = SqliteHarness::new().await;
    let mut actual = Vec::new();

    let error = harness
        .tools
        .list_tables(Parameters(ConnArg {
            connection: Some("missing".into()),
        }))
        .await
        .unwrap_err();
    actual.push(error_case("missingConnection", error));

    let error = harness
        .tools
        .describe_table(Parameters(DescribeTableArgs {
            connection: harness.selector(),
            table: "missing".into(),
        }))
        .await
        .unwrap_err();
    actual.push(error_case("missingTable", error));

    let error = harness
        .tools
        .plan_query(Parameters(PlanQueryArgs {
            connection: harness.selector(),
            sql: "DELETE FROM users".into(),
            max_rows: None,
        }))
        .await
        .unwrap_err();
    actual.push(error_case("writePlan", error));

    let plan = tool_json(
        harness
            .tools
            .plan_query(Parameters(PlanQueryArgs {
                connection: harness.selector(),
                sql: "SELECT id FROM users ORDER BY id".into(),
                max_rows: Some(1),
            }))
            .await
            .unwrap(),
    );
    let plan_id = plan["planId"].as_str().unwrap().to_string();
    harness
        .tools
        .run_query(Parameters(RunQueryArgs {
            plan_id: plan_id.clone(),
        }))
        .await
        .unwrap();
    let error = harness
        .tools
        .run_query(Parameters(RunQueryArgs { plan_id }))
        .await
        .unwrap_err();
    actual.push(error_case("usedPlan", error));

    let context = harness
        .connections
        .pin(harness.connection_id, ConnectionAccess::Read)
        .await
        .unwrap();
    let expired_id = Uuid::parse_str("018f1111-2222-7333-8444-555566667779").unwrap();
    harness.tools.services.query.seed_plan_for_test(
        expired_id,
        context.pin(),
        "SELECT 1".into(),
        1,
        "ready".into(),
        Instant::now() - QUERY_PLAN_TTL - Duration::from_secs(1),
    );
    drop(context);
    let error = harness
        .tools
        .run_query(Parameters(RunQueryArgs {
            plan_id: expired_id.to_string(),
        }))
        .await
        .unwrap_err();
    actual.push(error_case("expiredPlan", error));

    let mongo_id = Uuid::parse_str(MONGO_CONNECTION_ID).unwrap();
    harness
        .store
        .upsert_connection(&profile(
            mongo_id,
            "phase0-mongo",
            Engine::Mongodb,
            "app".into(),
        ))
        .await
        .unwrap();

    let error = harness
        .tools
        .plan_query(Parameters(PlanQueryArgs {
            connection: Some("phase0-mongo".into()),
            sql: "SELECT 1".into(),
            max_rows: None,
        }))
        .await
        .unwrap_err();
    actual.push(error_case("sqlOnMongo", error));

    let error = harness
        .tools
        .run_document_query(Parameters(RunDocumentQueryArgs {
            connection: harness.selector(),
            query: DocumentQuery::Count {
                collection: "users".into(),
                filter: None,
            },
            max_rows: None,
        }))
        .await
        .unwrap_err();
    actual.push(error_case("documentOnSql", error));

    let error = harness
        .tools
        .run_document_query(Parameters(RunDocumentQueryArgs {
            connection: Some("phase0-mongo".into()),
            query: DocumentQuery::Aggregate {
                collection: "events".into(),
                pipeline: vec![json!({ "$out": "copied_events" })],
            },
            max_rows: None,
        }))
        .await
        .unwrap_err();
    actual.push(error_case("mongoWriteStage", error));

    let error = harness
        .tools
        .create_dashboard(Parameters(CreateDashboardArgs {
            query_run_id: "018f1111-2222-7333-8444-555566667780".into(),
            title: "Missing".into(),
            description: String::new(),
            kind: DashboardKind::Table,
            x_column: None,
            y_columns: Vec::new(),
        }))
        .await
        .unwrap_err();
    actual.push(error_case("missingQueryRun", error));

    let context = harness
        .connections
        .pin(harness.connection_id, ConnectionAccess::Read)
        .await
        .unwrap();
    let pin = context.pin().clone();
    drop(context);
    let manual_run_id = Uuid::parse_str("018f1111-2222-7333-8444-555566667781").unwrap();
    harness
        .store
        .insert_history_if_current(
            &pin,
            &HistoryEntry {
                id: manual_run_id,
                connection_id: harness.connection_id,
                sql: "SELECT 1".into(),
                kind: QueryKind::Read,
                status: "ok".into(),
                row_count: Some(1),
                duration_ms: Some(1),
                error: None,
                executed_at: Utc::now(),
                origin: "manual".into(),
            },
        )
        .await
        .unwrap();
    let error = harness
        .tools
        .create_dashboard(Parameters(CreateDashboardArgs {
            query_run_id: manual_run_id.to_string(),
            title: "Wrong provenance".into(),
            description: String::new(),
            kind: DashboardKind::Table,
            x_column: None,
            y_columns: Vec::new(),
        }))
        .await
        .unwrap_err();
    actual.push(error_case("manualQueryRun", error));

    assert_eq!(Value::Array(actual), fixture_errors()["errors"]);
    harness.close().await;
}
