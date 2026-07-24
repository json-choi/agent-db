//! The `#[tauri::command]` boundary. Commands already migrated to
//! [`crate::services`] are thin adapters; the remaining legacy commands stay here
//! only until their service boundary is extracted. Every command returns an
//! [`AppResult`] that serializes to `{ kind, message }` for the frontend.
//!
//! Safety invariants live in the service/operation path: writes, DDL, and privilege
//! changes are blocked unless policy authorizes the exact request. The executor
//! re-checks its gates as defense in depth, while the database's read-only session
//! remains the authoritative stop.

use tauri::State;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::error::{AppError, AppResult};
use crate::model::{
    ConnectionProfile, Dashboard, DashboardDraft, DocumentQuery, HistoryEntry,
    PlatformFeatureFlags, SafetySettings, Workspace, WorkspaceAuthState,
    WorkspaceDeviceAuthorization, WorkspaceFeatureState, WorkspaceLoginPoll,
};
use crate::services::{
    AgentService, AuditSnapshotReceipt, AuditVerdict, CatalogReadPolicy, ChatThreadCreateRequest,
    ConnectionProfileTestRequest, ConnectionUpsertRequest, DashboardRunError, DashboardRunReceipt,
    DashboardRunRequest, DesktopDocumentProposalReceipt, DesktopDocumentProposalRequest,
    DesktopDocumentReadError, DesktopScriptProposalReceipt, DesktopScriptProposalRequest,
    DesktopScriptRunError, DesktopScriptRunReceipt, DesktopSqlClassificationReceipt,
    DesktopSqlClassificationRequest, DesktopSqlInspectionError, DesktopSqlPreviewReceipt,
    DesktopSqlPreviewRequest, DesktopSqlProposalReceipt, DesktopSqlProposalRequest,
    DesktopSqlRunError, DesktopSqlRunReceipt, DocumentReadReceipt, MonitoringProposalReceipt,
    MonitoringProposalRequest, MonitoringServiceError, MonitoringStatusReceipt,
    OperationDecisionReceipt, OperationDecisionRequest, WorkspaceConnectionCopyRequest,
    WorkspaceCredentialBindingRequest,
};
use crate::state::AppState;

#[tauri::command]
pub async fn cli_installation_status(
    state: State<'_, AppState>,
) -> AppResult<crate::cli_install::CliInstallationStatus> {
    if !state
        .features
        .is_enabled(crate::features::FeatureFlag::CliV1)
    {
        return Err(AppError::Blocked {
            reason: "the CLI feature is disabled for this app runtime".into(),
        });
    }
    tokio::task::spawn_blocking(crate::cli_install::installation_status)
        .await
        .map_err(|_| AppError::Config("the CLI status worker stopped unexpectedly".into()))?
}

#[tauri::command]
pub async fn install_cli(
    state: State<'_, AppState>,
    update_path: bool,
    replace_existing: bool,
) -> AppResult<crate::cli_install::CliInstallReceipt> {
    if !state
        .features
        .is_enabled(crate::features::FeatureFlag::CliV1)
    {
        return Err(AppError::Blocked {
            reason: "the CLI feature is disabled for this app runtime".into(),
        });
    }
    tokio::task::spawn_blocking(move || crate::cli_install::install(update_path, replace_existing))
        .await
        .map_err(|_| AppError::Config("the CLI installer worker stopped unexpectedly".into()))?
}

// ── workspace context ────────────────────────────────────────────────────────

#[tauri::command]
pub fn workspace_feature_state(state: State<'_, AppState>) -> WorkspaceFeatureState {
    state.services.workspace.feature_state()
}

#[tauri::command]
pub fn platform_feature_flags(state: State<'_, AppState>) -> PlatformFeatureFlags {
    PlatformFeatureFlags {
        enabled: state
            .features
            .enabled_names()
            .into_iter()
            .map(str::to_string)
            .collect(),
    }
}

#[tauri::command]
pub async fn workspace_auth_state(state: State<'_, AppState>) -> AppResult<WorkspaceAuthState> {
    state.services.workspace.auth_state().await
}

/// Revalidate the active hosted session and memberships without making initial UI
/// rendering wait on the OS credential store or network. Cached public identity remains
/// stable during outages; sensitive resource commands still authorize online.
#[tauri::command]
pub async fn refresh_workspace_auth_state(
    state: State<'_, AppState>,
) -> AppResult<WorkspaceAuthState> {
    state.services.workspace.refresh_auth_state().await
}

#[tauri::command]
pub async fn workspace_sign_out(
    state: State<'_, AppState>,
    user_id: Option<String>,
) -> AppResult<WorkspaceAuthState> {
    state.services.workspace.sign_out(user_id).await
}

#[tauri::command]
pub async fn workspace_sign_out_all(state: State<'_, AppState>) -> AppResult<WorkspaceAuthState> {
    state.services.workspace.sign_out_all().await
}

#[tauri::command]
pub async fn begin_workspace_login(
    state: State<'_, AppState>,
) -> AppResult<WorkspaceDeviceAuthorization> {
    state.services.workspace.begin_login().await
}

#[tauri::command]
pub async fn poll_workspace_login(
    state: State<'_, AppState>,
    device_code: String,
) -> AppResult<WorkspaceLoginPoll> {
    state.services.workspace.poll_login(&device_code).await
}

#[tauri::command]
pub fn workspace_console_url(
    state: State<'_, AppState>,
    workspace_id: Option<Uuid>,
) -> AppResult<String> {
    state.services.workspace.console_url(workspace_id)
}

#[tauri::command]
pub async fn list_workspaces(state: State<'_, AppState>) -> AppResult<Vec<Workspace>> {
    state.services.workspace.list().await
}

/// Explicitly refresh hosted memberships without changing the cached authentication
/// presentation. The desktop calls this after returning from the web settings page.
#[tauri::command]
pub async fn refresh_workspace_memberships(
    state: State<'_, AppState>,
) -> AppResult<Vec<Workspace>> {
    state.services.workspace.refresh_memberships().await
}

#[tauri::command]
pub async fn get_active_workspace(state: State<'_, AppState>) -> AppResult<Workspace> {
    state.services.workspace.active().await
}

#[tauri::command]
pub async fn set_active_workspace(
    state: State<'_, AppState>,
    id: Uuid,
    account_user_id: Option<String>,
) -> AppResult<Workspace> {
    state.services.workspace.activate(id, account_user_id).await
}

#[tauri::command]
pub async fn set_active_workspace_account(
    state: State<'_, AppState>,
    user_id: String,
) -> AppResult<Workspace> {
    state.services.workspace.activate_account(user_id).await
}

/// Copy a local connection into a team workspace. Only its redacted template crosses
/// the network; the caller's credential is duplicated locally under the remote UUID.
#[tauri::command]
pub async fn copy_connection_to_workspace(
    state: State<'_, AppState>,
    connection_id: Uuid,
    workspace_id: Uuid,
    account_user_id: String,
) -> AppResult<ConnectionProfile> {
    state
        .services
        .workspace
        .copy_connection(WorkspaceConnectionCopyRequest {
            connection_id,
            workspace_id,
            account_user_id,
        })
        .await
}

/// Bind this member's own DB credential to a shared template. It is stored only in
/// the OS credential store and is never sent to the control plane.
#[tauri::command]
pub async fn bind_workspace_connection_credentials(
    state: State<'_, AppState>,
    id: Uuid,
    username: String,
    password: String,
) -> AppResult<ConnectionProfile> {
    state
        .services
        .workspace
        .bind_connection_credentials(WorkspaceCredentialBindingRequest {
            connection_id: id,
            username,
            password: Zeroizing::new(password),
        })
        .await
}

// ── connection CRUD ──────────────────────────────────────────────────────────

#[tauri::command]
pub fn list_drivers(state: State<'_, AppState>) -> Vec<crate::driver::DriverDescriptor> {
    state.services.connections.list_drivers()
}

#[tauri::command]
pub fn install_driver(
    state: State<'_, AppState>,
    id: String,
) -> AppResult<crate::driver::DriverDescriptor> {
    state.services.connections.install_driver(&id)
}

#[tauri::command]
pub async fn list_connections(state: State<'_, AppState>) -> AppResult<Vec<ConnectionProfile>> {
    state.services.connections.list_profiles().await
}

#[tauri::command]
pub async fn upsert_connection(
    state: State<'_, AppState>,
    profile: ConnectionProfile,
    password: Option<String>,
) -> AppResult<ConnectionProfile> {
    state
        .services
        .connections
        .upsert(ConnectionUpsertRequest {
            profile,
            password: password.map(Zeroizing::new),
        })
        .await
}

#[tauri::command]
pub async fn set_connections_schema_group(
    state: State<'_, AppState>,
    ids: Vec<Uuid>,
    schema_group: Option<String>,
) -> AppResult<Vec<ConnectionProfile>> {
    state
        .services
        .connections
        .set_schema_group(ids, schema_group)
        .await
}

#[tauri::command]
pub async fn delete_connection(state: State<'_, AppState>, id: Uuid) -> AppResult<()> {
    state.services.connections.delete(id).await
}

#[tauri::command]
pub async fn test_connection(state: State<'_, AppState>, id: Uuid) -> AppResult<()> {
    state.services.connections.test(id).await
}

#[tauri::command]
pub async fn test_connection_profile(
    state: State<'_, AppState>,
    profile: ConnectionProfile,
    password: Option<String>,
) -> AppResult<()> {
    state
        .services
        .connections
        .test_profile(ConnectionProfileTestRequest {
            profile,
            password: password.map(Zeroizing::new),
        })
        .await
}

// ── saved dashboards ─────────────────────────────────────────────────────────

#[tauri::command]
pub async fn list_dashboards(
    state: State<'_, AppState>,
    connection_id: Uuid,
) -> AppResult<Vec<Dashboard>> {
    state.services.dashboard.list(connection_id).await
}

#[tauri::command]
pub async fn save_dashboard(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    draft: DashboardDraft,
) -> AppResult<Dashboard> {
    use tauri::Emitter;

    let saved = state.services.dashboard.save(draft).await?;
    if let Err(e) = app.emit("dashboard:created", &saved) {
        tracing::warn!("failed to emit dashboard:created: {e}");
    }
    Ok(saved)
}

#[tauri::command]
pub async fn delete_dashboard(state: State<'_, AppState>, id: Uuid) -> AppResult<()> {
    state.services.dashboard.delete(id).await
}

/// Rerun one saved dashboard through the authoritative L2 read-only session.
/// Connection auto-run/write settings never select a writable executor here; the
/// current connection engine is used to revalidate the stored SQL on every run.
#[tauri::command]
pub async fn run_dashboard(
    state: State<'_, AppState>,
    id: Uuid,
    query_id: Option<Uuid>,
) -> AppResult<DashboardRunReceipt> {
    state
        .services
        .dashboard
        .run(DashboardRunRequest {
            dashboard_id: id,
            query_id,
        })
        .await
        .map_err(DashboardRunError::into_error)
}

// ── schema ───────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn get_schema(state: State<'_, AppState>, id: Uuid) -> AppResult<String> {
    let catalog = state
        .services
        .catalog
        .load(id, CatalogReadPolicy::CacheFirst)
        .await?;
    Ok(serde_json::to_string(&catalog)?)
}

/// Force a live re-introspection (bypassing the cache) and update the cache. Use this
/// when the table list is stale — the cache is otherwise written once and never expires.
#[tauri::command]
pub async fn refresh_schema(state: State<'_, AppState>, id: Uuid) -> AppResult<String> {
    let catalog = state
        .services
        .catalog
        .load(id, CatalogReadPolicy::Refresh)
        .await?;
    Ok(serde_json::to_string(&catalog)?)
}

#[tauri::command]
pub async fn get_table_ddl(
    state: State<'_, AppState>,
    id: Uuid,
    schema: Option<String>,
    table: String,
) -> AppResult<String> {
    state
        .services
        .catalog
        .table_ddl(id, schema.as_deref(), &table)
        .await
}

// ── safety pipeline (L1 / L3) ────────────────────────────────────────────────

#[tauri::command]
pub async fn classify_sql(
    state: State<'_, AppState>,
    id: Uuid,
    sql: String,
) -> AppResult<DesktopSqlClassificationReceipt> {
    state
        .services
        .query
        .classify_desktop_sql(DesktopSqlClassificationRequest {
            connection_id: id,
            sql,
        })
        .await
        .map_err(DesktopSqlInspectionError::into_error)
}

#[tauri::command]
pub async fn preview_sql(
    state: State<'_, AppState>,
    id: Uuid,
    sql: String,
) -> AppResult<DesktopSqlPreviewReceipt> {
    state
        .services
        .query
        .preview_desktop_sql(DesktopSqlPreviewRequest {
            connection_id: id,
            sql,
        })
        .await
        .map_err(DesktopSqlInspectionError::into_error)
}

// ── execution (L4 gate → executor → audit) ───────────────────────────────────

#[tauri::command]
pub async fn propose_sql(
    state: State<'_, AppState>,
    id: Uuid,
    sql: String,
    origin: Option<String>,
) -> AppResult<DesktopSqlProposalReceipt> {
    state
        .services
        .query
        .propose_desktop_sql(DesktopSqlProposalRequest {
            connection_id: id,
            sql,
            origin,
        })
        .await
        .map_err(DesktopSqlInspectionError::into_error)
}

#[tauri::command]
pub async fn approve_operation(
    state: State<'_, AppState>,
    operation_id: Uuid,
    payload_hash: String,
    reason: Option<String>,
) -> AppResult<OperationDecisionReceipt> {
    state
        .services
        .operation
        .approve_local(
            &state.local_operation_approval,
            OperationDecisionRequest {
                operation_id,
                expected_payload_hash: payload_hash,
                reason,
            },
        )
        .await
}

#[tauri::command]
pub async fn reject_operation(
    state: State<'_, AppState>,
    operation_id: Uuid,
    payload_hash: String,
    reason: Option<String>,
) -> AppResult<OperationDecisionReceipt> {
    state
        .services
        .operation
        .reject_local(
            &state.local_operation_approval,
            OperationDecisionRequest {
                operation_id,
                expected_payload_hash: payload_hash,
                reason,
            },
        )
        .await
}

#[tauri::command]
pub async fn run_sql(
    state: State<'_, AppState>,
    operation_id: Uuid,
) -> AppResult<DesktopSqlRunReceipt> {
    state
        .services
        .query
        .run_desktop_sql(operation_id)
        .await
        .map_err(DesktopSqlRunError::into_error)
}

// ── typed document queries (MongoDB) ─────────────────────────────────────────

#[tauri::command]
pub async fn propose_document_query(
    state: State<'_, AppState>,
    id: Uuid,
    query: DocumentQuery,
    origin: Option<String>,
) -> AppResult<DesktopDocumentProposalReceipt> {
    state
        .services
        .document
        .propose_desktop_read(DesktopDocumentProposalRequest {
            connection_id: id,
            query,
            origin,
        })
        .await
        .map_err(DesktopDocumentReadError::into_error)
}

/// Typed document execution accepts only a durable single-use operation id. The
/// stored query is reclassified against the MongoDB stage allowlist before use.
#[tauri::command]
pub async fn run_document_query(
    state: State<'_, AppState>,
    operation_id: Uuid,
) -> AppResult<DocumentReadReceipt> {
    state
        .services
        .document
        .run_desktop_read(operation_id)
        .await
        .map_err(DesktopDocumentReadError::into_error)
}

// ── multi-statement script execution ─────────────────────────────────────────

#[tauri::command]
pub async fn propose_script(
    state: State<'_, AppState>,
    id: Uuid,
    sql: String,
    origin: Option<String>,
) -> AppResult<DesktopScriptProposalReceipt> {
    state
        .services
        .script
        .propose_desktop(DesktopScriptProposalRequest {
            connection_id: id,
            sql,
            origin,
        })
        .await
        .map_err(DesktopScriptRunError::into_error)
}

/// Execute a previously persisted script by operation id only.
#[tauri::command]
pub async fn run_script(
    state: State<'_, AppState>,
    operation_id: Uuid,
) -> AppResult<DesktopScriptRunReceipt> {
    state
        .services
        .script
        .run_desktop(operation_id)
        .await
        .map_err(DesktopScriptRunError::into_error)
}

// ── safety settings ──────────────────────────────────────────────────────────

#[tauri::command]
pub async fn get_safety(state: State<'_, AppState>, id: Uuid) -> AppResult<SafetySettings> {
    state.services.safety.get(id).await
}

#[tauri::command]
pub async fn set_safety(
    state: State<'_, AppState>,
    id: Uuid,
    settings: SafetySettings,
) -> AppResult<()> {
    state.services.safety.update(id, settings).await
}

// ── lightweight monitoring access ───────────────────────────────────────────

#[tauri::command]
pub async fn get_monitoring_status(
    state: State<'_, AppState>,
    id: Uuid,
) -> AppResult<MonitoringStatusReceipt> {
    state
        .services
        .monitoring
        .status(id)
        .await
        .map_err(MonitoringServiceError::into_error)
}

/// Persist one immutable fixed-role proposal. The desktop must render its literal
/// SQL and hash before using the separate exact approval command.
#[tauri::command]
pub async fn propose_postgres_monitoring(
    state: State<'_, AppState>,
    id: Uuid,
    enabled: bool,
) -> AppResult<MonitoringProposalReceipt> {
    state
        .services
        .monitoring
        .propose_postgres_role(MonitoringProposalRequest {
            connection_id: id,
            enabled,
        })
        .await
        .map_err(MonitoringServiceError::into_error)
}

/// Consume one exactly approved fixed-role proposal by operation id only.
#[tauri::command]
pub async fn set_postgres_monitoring(
    state: State<'_, AppState>,
    operation_id: Uuid,
) -> AppResult<MonitoringStatusReceipt> {
    state
        .services
        .monitoring
        .run_postgres_role(operation_id)
        .await
        .map_err(MonitoringServiceError::into_error)
}

// ── logs ─────────────────────────────────────────────────────────────────────

/// Verify the hash-chain for a connection's audit log. Returns `{ ok, firstBadIndex }`
/// where `firstBadIndex` is the insertion-order position of the first tampered row.
#[tauri::command]
pub async fn audit_verify(
    state: State<'_, AppState>,
    connection_id: Uuid,
) -> AppResult<AuditVerdict> {
    state.services.activity.verify_audit(connection_id).await
}

/// Fetch the displayed audit rows and verify that exact ordered snapshot in one read.
#[tauri::command]
pub async fn audit_snapshot(
    state: State<'_, AppState>,
    connection_id: Uuid,
) -> AppResult<AuditSnapshotReceipt> {
    state.services.activity.audit_snapshot(connection_id).await
}

#[tauri::command]
pub async fn list_history(state: State<'_, AppState>, id: Uuid) -> AppResult<Vec<HistoryEntry>> {
    state.services.activity.history(id).await
}

// ── MCP server status ─────────────────────────────────────────────────────────

/// Port / URL / bearer token for the local MCP server, so the UI can render the
/// per-platform connection snippets.
#[tauri::command]
pub fn mcp_status(state: State<'_, AppState>) -> serde_json::Value {
    serde_json::json!({
        "port": crate::mcp::MCP_PORT,
        "url": crate::mcp::mcp_url(),
        "token": state.mcp_token,
        "bridgePort": crate::mcp::MCP_BRIDGE_PORT,
        "bridgePath": crate::mcp::bridge_binary_path(),
    })
}

/// Detect which AI platforms are installed (for one-click connect buttons).
#[tauri::command]
pub async fn mcp_platforms() -> Vec<crate::mcp::connect::PlatformInfo> {
    crate::mcp::connect::detect().await
}

/// One-click connect: write/merge the MCP config for the given platform so the user
/// doesn't hand-edit JSON/TOML. Their local token is filled in automatically.
#[tauri::command]
pub async fn connect_platform(state: State<'_, AppState>, platform: String) -> AppResult<String> {
    let token = state.mcp_token.clone();
    let url = crate::mcp::mcp_url();
    let bridge_path = crate::mcp::bridge_binary_path();
    tokio::task::spawn_blocking(move || {
        crate::mcp::connect::connect(&platform, &token, &url, &bridge_path)
    })
    .await
    .map_err(|error| AppError::Config(format!("platform connection task failed: {error}")))?
    .map_err(AppError::Config)
}

/// One-click disconnect: remove the dopedb entry from the platform's MCP config.
#[tauri::command]
pub async fn disconnect_platform(platform: String) -> AppResult<String> {
    tokio::task::spawn_blocking(move || crate::mcp::connect::disconnect(&platform))
        .await
        .map_err(|error| AppError::Config(format!("platform disconnection task failed: {error}")))?
        .map_err(AppError::Config)
}

/// Open a supported local AI app after the frontend has copied a SQL prompt.
#[tauri::command]
pub fn open_agent_app(platform: String) -> AppResult<String> {
    crate::mcp::connect::open_app(&platform).map_err(AppError::Config)
}

// ── in-app agent chat ───────────────────────────────────────────────────────────

/// Claude Code / Codex CLI installed + subscription-login status. Distinct from
/// `mcp_platforms` (which asks whether dopedb is *registered* in a platform's MCP
/// config) — conflating the two would couple this chat gate to the Settings > MCP
/// screen's state. Async + internally timeout-bounded (see `agent::AGENT_PROBE_TIMEOUT`)
/// so a hung `claude`/`codex` subprocess can't freeze the app.
#[tauri::command]
pub async fn detect_agent_clis() -> Vec<crate::agent::CliInfo> {
    AgentService::detect_clis().await
}

/// Models `provider`'s CLI can run a turn against (Codex: its own live catalog;
/// Claude Code: a static fallback — see `agent::claude_models`). Async + internally
/// timeout-bounded, same as `detect_agent_clis`.
#[tauri::command]
pub async fn list_agent_models(
    provider: crate::agent::AgentProvider,
) -> AppResult<Vec<crate::agent::AgentModel>> {
    AgentService::list_models(provider).await
}

/// Saved chat threads, most recently updated first.
#[tauri::command]
pub async fn list_chat_threads(
    state: State<'_, AppState>,
) -> AppResult<Vec<crate::agent::ChatThread>> {
    state.services.agent.list_threads().await
}

/// One thread's messages, oldest first.
#[tauri::command]
pub async fn get_chat_messages(
    state: State<'_, AppState>,
    thread_id: Uuid,
) -> AppResult<Vec<crate::agent::ChatMessageRecord>> {
    state.services.agent.messages(thread_id).await
}

/// Create a new (empty) chat thread. The frontend calls this on a draft's first send,
/// immediately before `send_chat_turn` — an unsent draft never reaches the store.
/// `connection_id` binds the thread to the globally selected DopeDB connection; it is
/// fixed at creation and cannot change for this thread. Missing context fails closed.
#[tauri::command]
pub async fn create_chat_thread(
    state: State<'_, AppState>,
    provider: crate::agent::AgentProvider,
    connection_id: Uuid,
    model: Option<String>,
    effort: Option<String>,
) -> AppResult<crate::agent::ChatThread> {
    state
        .services
        .agent
        .create_thread(ChatThreadCreateRequest {
            provider,
            connection_id,
            model,
            effort,
        })
        .await
}

/// Delete a thread; its messages cascade with it.
#[tauri::command]
pub async fn delete_chat_thread(state: State<'_, AppState>, thread_id: Uuid) -> AppResult<()> {
    state.services.agent.delete_thread(thread_id).await
}

/// Run one chat turn against an existing thread. Progress streams as
/// `agent:chat_event`/`agent:chat_done`; this call itself only resolves once the CLI
/// process exits, so the frontend should treat its rejection as "failed to even
/// start" (bad binary, etc.) and rely on `agent:chat_done` for the actual turn outcome.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatTurnMessageIds {
    turn_id: Uuid,
    user_message_id: Uuid,
}

#[tauri::command]
pub async fn send_chat_turn(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    thread_id: Uuid,
    message: String,
    message_ids: ChatTurnMessageIds,
    model: Option<String>,
    effort: Option<String>,
) -> AppResult<()> {
    // In-app chat must use this process's listener. Development can run beside an
    // installed DopeDB, in which case the listener has an ephemeral fallback port.
    // Fail before spawning a paid/slow model turn if no local MCP is available.
    let mcp_url = {
        let runtime = state.mcp_runtime.lock().unwrap();
        if !runtime.http_running {
            let detail = runtime
                .last_error
                .as_deref()
                .unwrap_or("the local MCP listener has not started");
            return Err(AppError::Agent(format!(
                "DopeDB Agent is unavailable: {detail}"
            )));
        }
        runtime.http_url.clone().unwrap_or_else(crate::mcp::mcp_url)
    };
    crate::agent::send_turn(
        app,
        state.chat.clone(),
        state.store.clone(),
        state.connections.clone(),
        state.mcp_token.clone(),
        mcp_url,
        thread_id,
        message,
        message_ids.turn_id,
        message_ids.user_message_id,
        model,
        effort,
    )
    .await
}

// ── native picker ─────────────────────────────────────────────────────────────

/// Native file picker for a SQLite database path. None means the user cancelled.
#[tauri::command]
pub async fn pick_file(app: tauri::AppHandle) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    app.dialog()
        .file()
        .blocking_pick_file()
        .and_then(|path| path.into_path().ok())
        .map(|path| path.to_string_lossy().into_owned())
}
