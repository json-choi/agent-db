// Typed wrappers around Tauri `invoke` for every backend command. Names match the
// Rust `#[tauri::command]` fns in src-tauri/src/commands/mod.rs exactly; arg keys
// match the Rust parameter names.

import { invoke } from "@tauri-apps/api/core";
import type {
  AuditSnapshot,
  Catalog,
  Classification,
  ConnectionProfile,
  Dashboard,
  DashboardDraft,
  DriverDescriptor,
  ExecOutcome,
  HistoryEntry,
  MonitoringStatus,
  PreviewReport,
  ScriptOutcome,
  SafetySettings,
  PlatformInfo,
  QueryResult,
} from "./types";

export function listConnections(): Promise<ConnectionProfile[]> {
  return invoke("list_connections");
}

export function listDrivers(): Promise<DriverDescriptor[]> {
  return invoke("list_drivers");
}

export function installDriver(id: string): Promise<DriverDescriptor> {
  return invoke("install_driver", { id });
}

// NOTE(integrator): ConnectionProfile carries no plaintext secret. The optional
// `password` is passed alongside the profile so the backend can stash it in the
// OS credential store and set `secretRef`. If upsert_connection does not accept a `password`
// arg, drop it here and add a dedicated store_secret command.
export function upsertConnection(
  profile: ConnectionProfile,
  password?: string,
): Promise<ConnectionProfile> {
  return invoke("upsert_connection", { profile, password });
}

export function setConnectionSchemaGroup(
  id: string,
  schemaGroup: string | null,
): Promise<ConnectionProfile> {
  return invoke("set_connection_schema_group", { id, schemaGroup });
}

export function setConnectionsSchemaGroup(
  ids: string[],
  schemaGroup: string | null,
): Promise<ConnectionProfile[]> {
  return invoke("set_connections_schema_group", { ids, schemaGroup });
}

export function deleteConnection(id: string): Promise<void> {
  return invoke("delete_connection", { id });
}

// Reachability check for an ad-hoc (possibly unsaved) profile. Persists nothing.
export function testConnectionProfile(
  profile: ConnectionProfile,
  password?: string,
): Promise<void> {
  return invoke("test_connection_profile", { profile, password });
}

function getSchema(id: string): Promise<string> {
  return invoke("get_schema", { id });
}

// Introspected schema, parsed. Backend returns the Catalog as a JSON string.
export async function getCatalog(id: string): Promise<Catalog> {
  return JSON.parse(await getSchema(id)) as Catalog;
}

// Force a live re-introspection (bypasses the one-shot schema cache) and return it.
export async function refreshCatalog(id: string): Promise<Catalog> {
  return JSON.parse(await invoke<string>("refresh_schema", { id })) as Catalog;
}

// The CREATE-TABLE DDL for one table (MySQL/SQLite native; Postgres synthesized).
export function getTableDdl(
  id: string,
  table: string,
  schema?: string | null,
): Promise<string> {
  return invoke("get_table_ddl", { id, schema: schema ?? null, table });
}

export function classifySql(id: string, sql: string): Promise<Classification> {
  return invoke("classify_sql", { id, sql });
}

export function previewSql(id: string, sql: string): Promise<PreviewReport> {
  return invoke("preview_sql", { id, sql });
}

export function runSql(
  id: string,
  sql: string,
  approved: boolean,
  queryId?: string,
  origin?: string,
): Promise<ExecOutcome> {
  return invoke("run_sql", {
    id,
    sql,
    approved,
    queryId: queryId ?? null,
    origin: origin ?? null,
  });
}

// Cancel an in-flight run_sql/run_script by its query id. Returns true if a running
// query was found and signalled.
export function cancelQuery(queryId: string): Promise<boolean> {
  return invoke("cancel_query", { queryId });
}

// Run a multi-statement script. All-reads run sequentially on the read-only pool;
// any write/DDL requires approved + allow_writes and runs in ONE transaction.
export function runScript(
  id: string,
  sql: string,
  approved: boolean,
  queryId?: string,
  origin?: string,
): Promise<ScriptOutcome> {
  return invoke("run_script", {
    id,
    sql,
    approved,
    queryId: queryId ?? null,
    origin: origin ?? null,
  });
}

export function getSafety(id: string): Promise<SafetySettings> {
  return invoke("get_safety", { id });
}

export function setSafety(id: string, settings: SafetySettings): Promise<void> {
  return invoke("set_safety", { id, settings });
}

export function getMonitoringStatus(id: string): Promise<MonitoringStatus> {
  return invoke("get_monitoring_status", { id });
}

export function setPostgresMonitoring(
  id: string,
  enabled: boolean,
  approved: boolean,
): Promise<MonitoringStatus> {
  return invoke("set_postgres_monitoring", { id, enabled, approved });
}

// Backend hash-chain verification (rowid order + real SHA-256 recompute). Authoritative —
// a client-side link-only check can't detect an in-place field edit. firstBadIndex is the
// insertion-order (oldest-first) position of the first tampered row, or null when ok.
export function auditVerify(id: string): Promise<{ ok: boolean; firstBadIndex: number | null }> {
  return invoke("audit_verify", { connectionId: id });
}

// Rows and verdict come from one ordered backend read, so the integrity result always
// describes the exact audit entries rendered by the Activity detail panel.
export function auditSnapshot(id: string): Promise<AuditSnapshot> {
  return invoke("audit_snapshot", { connectionId: id });
}

export function listHistory(id: string): Promise<HistoryEntry[]> {
  return invoke("list_history", { id });
}

export function listDashboards(connectionId: string): Promise<Dashboard[]> {
  return invoke("list_dashboards", { connectionId });
}

export function saveDashboard(draft: DashboardDraft): Promise<Dashboard> {
  return invoke("save_dashboard", { draft });
}

export function deleteDashboard(id: string): Promise<void> {
  return invoke("delete_dashboard", { id });
}

export function runDashboard(id: string, queryId?: string): Promise<QueryResult> {
  return invoke("run_dashboard", { id, queryId: queryId ?? null });
}

export interface McpStatus {
  port: number;
  url: string;
  token: string;
  bridgePort: number;
  bridgePath: string;
}

// Local MCP server status (URL + bearer token) for the connection snippets.
export function mcpStatus(): Promise<McpStatus> {
  return invoke("mcp_status");
}

// Live listener state (distinct from the static config above): whether the HTTP/bridge
// listeners actually bound, plus the last bind error. camelCase per mcp::mcp_runtime_status.
export interface McpRuntimeStatus {
  httpRunning: boolean;
  bridgeRunning: boolean;
  error: string | null;
}

export function mcpRuntimeStatus(): Promise<McpRuntimeStatus> {
  return invoke("mcp_runtime_status");
}

// Detect installed AI platforms for the one-click connect buttons.
export function mcpPlatforms(): Promise<PlatformInfo[]> {
  return invoke("mcp_platforms");
}

// One-click: write/merge the MCP config for a platform. Returns a status message.
export function connectPlatform(platform: string): Promise<string> {
  return invoke("connect_platform", { platform });
}

// One-click disconnect: remove the dopedb entry from a platform's MCP config.
export function disconnectPlatform(platform: string): Promise<string> {
  return invoke("disconnect_platform", { platform });
}

// Open a local AI app after the SQL tab copies the prompt/context.
export function openAgentApp(platform: string): Promise<string> {
  return invoke("open_agent_app", { platform });
}

// Native picker (null = user cancelled the dialog).
export function pickFile(): Promise<string | null> {
  return invoke("pick_file");
}
