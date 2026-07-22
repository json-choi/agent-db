// Typed wrappers around Tauri `invoke` for every backend command. Names match the
// Rust `#[tauri::command]` fns in src-tauri/src/commands/mod.rs exactly; arg keys
// match the Rust parameter names.

import { invoke } from "@tauri-apps/api/core";
import type {
  AgentModel,
  AgentProvider,
  AuditSnapshot,
  Catalog,
  ChatMessageRecord,
  ChatThread,
  CliInfo,
  Classification,
  ConnectionProfile,
  Dashboard,
  DashboardDraft,
  DocumentPage,
  DocumentQuery,
  DriverDescriptor,
  ExecOutcome,
  HistoryEntry,
  MonitoringStatus,
  PreviewReport,
  ScriptOutcome,
  SafetySettings,
  PlatformInfo,
  QueryResult,
  Workspace,
  WorkspaceAuthState,
  WorkspaceDeviceAuthorization,
  WorkspaceFeatureState,
  WorkspaceLoginPoll,
} from "./types";

export function workspaceFeatureState(): Promise<WorkspaceFeatureState> {
  return invoke("workspace_feature_state");
}

export function workspaceAuthState(): Promise<WorkspaceAuthState> {
  return invoke("workspace_auth_state");
}

export function refreshWorkspaceAuthState(): Promise<WorkspaceAuthState> {
  return invoke("refresh_workspace_auth_state");
}

export function signOutWorkspace(userId?: string): Promise<WorkspaceAuthState> {
  return invoke("workspace_sign_out", { userId: userId ?? null });
}

export function signOutAllWorkspaces(): Promise<WorkspaceAuthState> {
  return invoke("workspace_sign_out_all");
}

export function beginWorkspaceLogin(): Promise<WorkspaceDeviceAuthorization> {
  return invoke("begin_workspace_login");
}

export function pollWorkspaceLogin(deviceCode: string): Promise<WorkspaceLoginPoll> {
  return invoke("poll_workspace_login", { deviceCode });
}

export function workspaceConsoleUrl(workspaceId?: string): Promise<string> {
  return invoke("workspace_console_url", { workspaceId: workspaceId ?? null });
}

export function listWorkspaces(): Promise<Workspace[]> {
  return invoke("list_workspaces");
}

export function refreshWorkspaceMemberships(): Promise<Workspace[]> {
  return invoke("refresh_workspace_memberships");
}

export function getActiveWorkspace(): Promise<Workspace> {
  return invoke("get_active_workspace");
}

export function setActiveWorkspace(
  id: string,
  accountUserId?: string,
): Promise<Workspace> {
  return invoke("set_active_workspace", { id, accountUserId: accountUserId ?? null });
}

export function setActiveWorkspaceAccount(userId: string): Promise<Workspace> {
  return invoke("set_active_workspace_account", { userId });
}

export function copyConnectionToWorkspace(
  connectionId: string,
  workspaceId: string,
  accountUserId: string,
): Promise<ConnectionProfile> {
  return invoke("copy_connection_to_workspace", {
    connectionId,
    workspaceId,
    accountUserId,
  });
}

export function bindWorkspaceConnectionCredentials(
  id: string,
  username: string,
  password: string,
): Promise<ConnectionProfile> {
  return invoke("bind_workspace_connection_credentials", { id, username, password });
}

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

// Run one typed, read-only document query on a MongoDB connection. Aggregate
// write stages are rejected backend-side; there is no document write path.
export function runDocumentQuery(
  id: string,
  query: DocumentQuery,
  approved: boolean,
  queryId?: string,
  origin?: string,
): Promise<DocumentPage> {
  return invoke("run_document_query", {
    id,
    query,
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

// In-app agent chat: install/auth status for the supported subscription CLIs.
export function detectAgentClis(): Promise<CliInfo[]> {
  return invoke("detect_agent_clis");
}

// The composer's model picker: codex's own catalog (parsed from `codex debug models`) or
// claude's static list. Rejects rather than resolving an empty list on a real failure.
export function listAgentModels(provider: AgentProvider): Promise<AgentModel[]> {
  return invoke("list_agent_models", { provider });
}

// Sidebar thread list, newest-updated first.
export function listChatThreads(): Promise<ChatThread[]> {
  return invoke("list_chat_threads");
}

// One thread's message history, oldest first.
export function getChatMessages(threadId: string): Promise<ChatMessageRecord[]> {
  return invoke("get_chat_messages", { threadId });
}

// Creates the DB row for a still-draft conversation. Called only on its first message,
// so an abandoned draft never leaves an empty thread in the sidebar. Every conversation
// is bound to the database selected in the global sidebar context.
export function createChatThread(
  provider: AgentProvider,
  connectionId: string,
  model?: string,
  effort?: string,
): Promise<ChatThread> {
  return invoke("create_chat_thread", {
    provider,
    connectionId,
    model: model ?? null,
    effort: effort ?? null,
  });
}

// Deletes a thread and (via ON DELETE CASCADE) its messages.
export function deleteChatThread(threadId: string): Promise<void> {
  return invoke("delete_chat_thread", { threadId });
}

// Runs one chat turn against an existing thread (its provider/cli_session_id come from the
// thread row itself). Progress streams as agent:chat_event/agent:chat_done; this promise
// itself only resolves once the spawned CLI process exits (or fails to start). model/effort
// override the provider's CLI default for this turn when set.
export function sendChatTurn(
  threadId: string,
  message: string,
  turnId: string,
  model?: string,
  effort?: string,
): Promise<void> {
  return invoke("send_chat_turn", {
    threadId,
    message,
    turnId,
    model: model ?? null,
    effort: effort ?? null,
  });
}
