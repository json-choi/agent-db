// Typed wrappers around Tauri `invoke` for every backend command. Names match the
// Rust `#[tauri::command]` fns in src-tauri/src/commands/mod.rs exactly; arg keys
// match the Rust parameter names.

import { invoke } from "@tauri-apps/api/core";
import type {
  AuditEntry,
  Catalog,
  Classification,
  ConnectionProfile,
  ExecOutcome,
  HistoryEntry,
  PreviewReport,
  SafetySettings,
} from "./types";

export function listConnections(): Promise<ConnectionProfile[]> {
  return invoke("list_connections");
}

// NOTE(integrator): ConnectionProfile carries no plaintext secret. The optional
// `password` is passed alongside the profile so the backend can stash it in the
// Keychain and set `secretRef`. If upsert_connection does not accept a `password`
// arg, drop it here and add a dedicated store_secret command.
export function upsertConnection(
  profile: ConnectionProfile,
  password?: string,
): Promise<ConnectionProfile> {
  return invoke("upsert_connection", { profile, password });
}

export function deleteConnection(id: string): Promise<void> {
  return invoke("delete_connection", { id });
}

export function testConnection(id: string): Promise<void> {
  return invoke("test_connection", { id });
}

// Reachability check for an ad-hoc (possibly unsaved) profile. Persists nothing.
export function testConnectionProfile(
  profile: ConnectionProfile,
  password?: string,
): Promise<void> {
  return invoke("test_connection_profile", { profile, password });
}

export function getSchema(id: string): Promise<string> {
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
): Promise<ExecOutcome> {
  return invoke("run_sql", { id, sql, approved });
}

// Run a multi-statement script. All-reads run sequentially on the read-only pool;
// any write/DDL requires approved + allow_writes and runs in ONE transaction.
export function runScript(
  id: string,
  sql: string,
  approved: boolean,
  queryId?: string,
  origin?: string,
): Promise<import("./types").ScriptOutcome> {
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

export function listAudit(id: string): Promise<AuditEntry[]> {
  return invoke("list_audit", { id });
}

export function listHistory(id: string): Promise<HistoryEntry[]> {
  return invoke("list_history", { id });
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
export function mcpPlatforms(): Promise<import("./types").PlatformInfo[]> {
  return invoke("mcp_platforms");
}

// One-click: write/merge the MCP config for a platform. Returns a status message.
export function connectPlatform(platform: string): Promise<string> {
  return invoke("connect_platform", { platform });
}

// One-click disconnect: remove the agentdb entry from a platform's MCP config.
export function disconnectPlatform(platform: string): Promise<string> {
  return invoke("disconnect_platform", { platform });
}

// Native pickers (null = user cancelled the dialog).
export function pickFolder(): Promise<string | null> {
  return invoke("pick_folder");
}
export function pickFile(): Promise<string | null> {
  return invoke("pick_file");
}

// Analyze a folder of .sql migrations (change log + generated down SQL + optional drift).
export function analyzeMigrations(
  dir: string,
  connectionId?: string,
): Promise<import("./types").MigrationReport> {
  return invoke("analyze_migrations", { dir, connectionId: connectionId ?? null });
}

// Auto-detect the migrations subfolder inside a project root.
export function detectMigrationsDir(projectDir: string): Promise<string | null> {
  return invoke("detect_migrations_dir", { projectDir });
}

// Watch the migrations folder; the backend emits `migrations.changed` on any change.
export function startMigrationWatch(dir: string): Promise<void> {
  return invoke("start_migration_watch", { dir });
}

// Apply or roll back a single migration in one transaction. Re-analyzes fresh,
// gates on approved + allow_writes, enforces order, audits, and records history.
export function runMigrationScript(
  connectionId: string,
  dir: string,
  version: string,
  direction: "apply" | "rollback",
  approved: boolean,
): Promise<ExecOutcome> {
  return invoke("run_migration_script", { connectionId, dir, version, direction, approved });
}
