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

use crate::connection::{self, ConnectionAccess};
use crate::error::{AppError, AppResult};
use crate::model::{
    ConnectionProfile, Dashboard, DashboardDraft, DocumentQuery, HistoryEntry,
    PlatformFeatureFlags, SafetySettings, Workspace, WorkspaceAuthState, WorkspaceAuthUser,
    WorkspaceConnectionAccess, WorkspaceCredentialMode, WorkspaceDeviceAuthorization,
    WorkspaceFeatureState, WorkspaceKind, WorkspaceLoginPoll,
};
use crate::services::{
    AuditSnapshotReceipt, AuditVerdict, CatalogReadPolicy, ConnectionProfileTestRequest,
    ConnectionUpsertRequest, DashboardRunError, DashboardRunReceipt, DashboardRunRequest,
    DesktopDocumentReadError, DesktopDocumentReadRequest, DesktopScriptRunError,
    DesktopScriptRunReceipt, DesktopScriptRunRequest, DesktopSqlClassificationReceipt,
    DesktopSqlClassificationRequest, DesktopSqlInspectionError, DesktopSqlPreviewReceipt,
    DesktopSqlPreviewRequest, DesktopSqlRunError, DesktopSqlRunReceipt, DesktopSqlRunRequest,
    DocumentReadReceipt, MonitoringChangeRequest, MonitoringServiceError, MonitoringStatusReceipt,
};
use crate::state::AppState;

// ── helpers ──────────────────────────────────────────────────────────────────

const MAX_CONNECTION_CREDENTIAL_BYTES: usize = 1 << 16;

fn delete_secret_best_effort(id: Uuid, action: &'static str) {
    if let Err(error) = connection::delete_secret(&id) {
        tracing::warn!(credential_id = %id, %error, action, "credential cleanup deferred");
    }
}

// ── workspace context ────────────────────────────────────────────────────────

#[tauri::command]
pub fn workspace_feature_state() -> WorkspaceFeatureState {
    let enabled = std::env::var("DOPEDB_WORKSPACES_ENABLED")
        .map(|value| {
            !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off"
            )
        })
        .unwrap_or(true);
    WorkspaceFeatureState { enabled }
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
    ensure_active_workspace_account(&state).await?;
    workspace_auth_state_from_store(&state).await
}

/// Revalidate the active hosted session and memberships without making initial UI
/// rendering wait on the OS credential store or network. Cached public identity remains
/// stable during outages; sensitive resource commands still authorize online.
#[tauri::command]
pub async fn refresh_workspace_auth_state(
    state: State<'_, AppState>,
) -> AppResult<WorkspaceAuthState> {
    if state.store.workspace_accounts().await?.is_empty() {
        if let Some(user) = crate::workspace_auth::migrate_legacy_session().await? {
            if let Err(error) = sync_account_memberships(&state, &user).await {
                tracing::warn!(%error, "legacy workspace membership sync deferred");
            }
            state
                .connections
                .activate_workspace_account(&user.id)
                .await?;
        }
    }

    ensure_active_workspace_account(&state).await?;
    if let Some(user_id) = state.store.active_workspace_account_id().await? {
        match crate::workspace_auth::auth_user(&user_id).await {
            Ok(Some(user)) => {
                if let Err(error) = sync_account_memberships(&state, &user).await {
                    tracing::warn!(%error, "workspace membership sync deferred after session validation");
                }
            }
            Ok(None) => {
                state.connections.remove_workspace_account(&user_id).await?;
                ensure_active_workspace_account(&state).await?;
            }
            Err(error) => {
                // Keep the last verified identity visible during an outage. Every
                // shared-resource action still performs its own online authorization.
                tracing::warn!(%error, "workspace session validation deferred");
            }
        }
    }
    workspace_auth_state_from_store(&state).await
}

#[tauri::command]
pub async fn workspace_sign_out(
    state: State<'_, AppState>,
    user_id: Option<String>,
) -> AppResult<WorkspaceAuthState> {
    let user_id = match user_id {
        Some(user_id) => user_id,
        None => state
            .store
            .active_workspace_account_id()
            .await?
            .ok_or_else(|| AppError::Config("no workspace account is signed in".into()))?,
    };
    state.connections.remove_workspace_account(&user_id).await?;
    // Pool retirement releases managed provider credentials while the Better Auth
    // token is still available; session revocation and local token deletion follow.
    crate::workspace_auth::sign_out(&user_id).await?;
    workspace_auth_state_from_store(&state).await
}

#[tauri::command]
pub async fn workspace_sign_out_all(state: State<'_, AppState>) -> AppResult<WorkspaceAuthState> {
    let accounts = state.store.workspace_accounts().await?;
    let mut first_error = None;
    for account in accounts {
        if let Err(error) = state
            .connections
            .remove_workspace_account(&account.user.id)
            .await
        {
            first_error.get_or_insert(error);
        }
        if let Err(error) = crate::workspace_auth::sign_out(&account.user.id).await {
            first_error.get_or_insert(error);
        }
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    workspace_auth_state_from_store(&state).await
}

#[tauri::command]
pub async fn begin_workspace_login() -> AppResult<WorkspaceDeviceAuthorization> {
    crate::workspace_auth::begin_login().await
}

#[tauri::command]
pub async fn poll_workspace_login(
    state: State<'_, AppState>,
    device_code: String,
) -> AppResult<WorkspaceLoginPoll> {
    let result = crate::workspace_auth::poll_login(&device_code).await?;
    if result.status == crate::model::WorkspaceLoginPollStatus::SignedIn {
        let user = result
            .user
            .as_ref()
            .ok_or_else(|| AppError::Network("workspace login did not return an account".into()))?;
        if let Err(error) = sync_account_memberships(&state, user).await {
            // The session token is already validated and stored. Do not report a
            // successful login as failed merely because the first membership refresh
            // encountered a transient control-plane or local-cache error.
            tracing::warn!(%error, "workspace membership sync deferred after sign-in");
        }
        state
            .connections
            .activate_workspace_account(&user.id)
            .await?;
    }
    Ok(result)
}

#[tauri::command]
pub fn workspace_console_url(workspace_id: Option<Uuid>) -> AppResult<String> {
    crate::workspace_auth::console_url(workspace_id)
}

async fn workspace_auth_state_from_store(state: &AppState) -> AppResult<WorkspaceAuthState> {
    let accounts = state.store.workspace_accounts().await?;
    let active_account_id = state.store.active_workspace_account_id().await?;
    let user = active_account_id.and_then(|active_id| {
        accounts
            .iter()
            .find(|account| account.user.id == active_id)
            .map(|account| account.user.clone())
    });
    Ok(WorkspaceAuthState {
        authenticated: user.is_some(),
        user,
        accounts,
    })
}

async fn ensure_active_workspace_account(state: &AppState) -> AppResult<()> {
    let active_account_id = state.store.active_workspace_account_id().await?;
    let accounts = state.store.workspace_accounts().await?;
    if active_account_id
        .as_ref()
        .is_some_and(|active_id| accounts.iter().any(|account| account.user.id == *active_id))
    {
        return Ok(());
    }
    if let Some(stale_id) = active_account_id {
        state
            .connections
            .remove_workspace_account(&stale_id)
            .await?;
    } else if let Some(account) = accounts.first() {
        state
            .connections
            .activate_workspace_account(&account.user.id)
            .await?;
    }
    Ok(())
}

async fn validated_workspace_user(state: &AppState, user_id: &str) -> AppResult<WorkspaceAuthUser> {
    match crate::workspace_auth::auth_user(user_id).await? {
        Some(user) => Ok(user),
        None => {
            state.connections.remove_workspace_account(user_id).await?;
            ensure_active_workspace_account(state).await?;
            Err(AppError::Network(
                "workspace session is no longer active".into(),
            ))
        }
    }
}

async fn sync_account_memberships(state: &AppState, user: &WorkspaceAuthUser) -> AppResult<()> {
    state.store.remember_workspace_account(user).await?;
    let remote = crate::workspace_auth::remote_workspaces(&user.id).await?;
    let workspaces = remote
        .into_iter()
        .map(|workspace| (workspace.id, workspace.name, workspace.role))
        .collect::<Vec<_>>();
    state
        .connections
        .sync_account_workspaces(user, &workspaces)
        .await?;
    let active = state.store.active_workspace().await?;
    if active.kind == WorkspaceKind::Team
        && state.store.active_workspace_account_id().await?.as_deref() == Some(user.id.as_str())
    {
        sync_workspace_connections(state, &user.id, active.id).await?;
    }
    Ok(())
}

async fn sync_workspace_connections(
    state: &AppState,
    account_user_id: &str,
    workspace_id: Uuid,
) -> AppResult<()> {
    match crate::workspace_auth::remote_connections(account_user_id, workspace_id).await {
        Ok(Some(connections)) => {
            let removed_credential_ids = state
                .connections
                .sync_remote_connections(workspace_id, account_user_id, &connections)
                .await?;
            for credential_id in removed_credential_ids {
                delete_secret_best_effort(credential_id, "remove_remote_connection");
            }
            Ok(())
        }
        Ok(None) => {
            tracing::info!(
                %workspace_id,
                "shared connection API is not deployed yet; keeping the local workspace cache"
            );
            Ok(())
        }
        Err(error) => {
            // Switching is local and remains usable during a control-plane outage.
            // Shared execution still requires a fresh online authorization, so this
            // stale cache cannot broaden database access.
            tracing::warn!(%workspace_id, %error, "workspace connection sync deferred");
            Ok(())
        }
    }
}

#[tauri::command]
pub async fn list_workspaces(state: State<'_, AppState>) -> AppResult<Vec<Workspace>> {
    state.store.list_workspaces().await
}

/// Explicitly refresh hosted memberships without changing the cached authentication
/// presentation. The desktop calls this after returning from the web settings page.
#[tauri::command]
pub async fn refresh_workspace_memberships(
    state: State<'_, AppState>,
) -> AppResult<Vec<Workspace>> {
    let accounts = state.store.workspace_accounts().await?;
    for account in accounts {
        match crate::workspace_auth::auth_user(&account.user.id).await {
            Ok(Some(user)) => sync_account_memberships(&state, &user).await?,
            Ok(None) => {
                state
                    .connections
                    .remove_workspace_account(&account.user.id)
                    .await?;
            }
            Err(error) => tracing::warn!(
                user_id = %account.user.id,
                %error,
                "workspace account refresh deferred"
            ),
        }
    }
    ensure_active_workspace_account(&state).await?;
    state.store.list_workspaces().await
}

#[tauri::command]
pub async fn get_active_workspace(state: State<'_, AppState>) -> AppResult<Workspace> {
    state.store.active_workspace().await
}

#[tauri::command]
pub async fn set_active_workspace(
    state: State<'_, AppState>,
    id: Uuid,
    account_user_id: Option<String>,
) -> AppResult<Workspace> {
    let target = state
        .store
        .list_workspaces()
        .await?
        .into_iter()
        .find(|workspace| workspace.id == id)
        .ok_or_else(|| AppError::NotFound(format!("workspace {id}")))?;
    if target.kind == WorkspaceKind::Team {
        let user_id = account_user_id.as_deref().ok_or_else(|| {
            AppError::Config("team workspace selection requires an account".into())
        })?;
        let user = validated_workspace_user(&state, user_id).await?;
        sync_account_memberships(&state, &user).await?;
    }
    let workspace = state
        .connections
        .activate_workspace(id, account_user_id.as_deref())
        .await?;
    if workspace.kind == WorkspaceKind::Team {
        let account_user_id = account_user_id.ok_or_else(|| {
            AppError::Config("team workspace selection requires an account".into())
        })?;
        if let Err(error) = sync_workspace_connections(&state, &account_user_id, workspace.id).await
        {
            tracing::warn!(workspace_id = %workspace.id, %error, "workspace connection sync deferred after switch");
        }
    }
    Ok(workspace)
}

#[tauri::command]
pub async fn set_active_workspace_account(
    state: State<'_, AppState>,
    user_id: String,
) -> AppResult<Workspace> {
    let user = validated_workspace_user(&state, &user_id).await?;
    sync_account_memberships(&state, &user).await?;
    let workspace = state
        .connections
        .activate_workspace_account(&user_id)
        .await?;
    if workspace.kind == WorkspaceKind::Team {
        if let Err(error) = sync_workspace_connections(&state, &user_id, workspace.id).await {
            tracing::warn!(workspace_id = %workspace.id, %error, "workspace connection sync deferred after account switch");
        }
    }
    Ok(workspace)
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
    let source = state.store.get_connection(connection_id).await?;
    if source.workspace_access != WorkspaceConnectionAccess::Local {
        return Err(AppError::Config(
            "only a local connection can be copied into a workspace".into(),
        ));
    }
    let target = state
        .store
        .list_workspaces()
        .await?
        .into_iter()
        .find(|workspace| workspace.id == workspace_id && workspace.kind == WorkspaceKind::Team)
        .ok_or_else(|| AppError::NotFound(format!("team workspace {workspace_id}")))?;
    let current_account = state.store.active_workspace_account_id().await?;
    if target.id == state.store.active_workspace_id().await?
        && current_account.as_deref() == Some(account_user_id.as_str())
    {
        return Err(AppError::Config("choose a different team workspace".into()));
    }
    let account = state
        .store
        .workspace_accounts()
        .await?
        .into_iter()
        .find(|account| {
            account.user.id == account_user_id
                && account
                    .memberships
                    .iter()
                    .any(|membership| membership.workspace_id == workspace_id)
        })
        .ok_or_else(|| {
            AppError::NotFound(format!(
                "workspace {workspace_id} for account {account_user_id}"
            ))
        })?;

    // Resolve every local prerequisite and snapshot the current remote collection
    // before creating the server resource. This avoids a remote template being left
    // behind merely because a later credential read or collection fetch failed.
    let copied_secret = if source.secret_ref.is_some() {
        Some(Zeroizing::new(connection::fetch_profile_secret(&source)?))
    } else {
        None
    };
    let mut remote = crate::workspace_auth::remote_connections(&account.user.id, workspace_id)
        .await?
        .ok_or_else(|| {
            AppError::Network(
                "the workspace service has not deployed shared connections yet".into(),
            )
        })?;
    let credential_id = copied_secret.as_ref().map(|_| Uuid::new_v4());
    if let (Some(credential_id), Some(secret)) = (credential_id, copied_secret.as_deref()) {
        connection::store_secret(&credential_id, secret)?;
    }
    let shared =
        crate::workspace_auth::share_connection(&account.user.id, workspace_id, &source).await;
    let (created, revision) = match shared {
        Ok(created) => created,
        Err(error) => {
            if let Some(credential_id) = credential_id {
                delete_secret_best_effort(credential_id, "share_connection");
            }
            return Err(error);
        }
    };
    remote.push((created.clone(), revision));
    let credential_ref = credential_id.map(|id| id.to_string());
    let local_result = async {
        let removed_credential_ids = state
            .connections
            .sync_remote_connections(workspace_id, &account.user.id, &remote)
            .await?;
        for credential_id in removed_credential_ids {
            delete_secret_best_effort(credential_id, "remove_remote_connection");
        }
        state
            .store
            .bind_connection_credentials(
                created.id,
                &account.user.id,
                &source.username,
                &source.extra_params,
                credential_ref.as_deref(),
            )
            .await
    }
    .await;
    match local_result {
        Ok(profile) => Ok(profile),
        Err(error) => {
            if let Some(credential_id) = credential_id {
                delete_secret_best_effort(credential_id, "persist_shared_connection");
            }
            match crate::workspace_auth::delete_connection(
                &account.user.id,
                workspace_id,
                created.id,
            )
            .await
            {
                Ok(()) => {
                    if let Err(cache_error) = state
                        .store
                        .purge_remote_connection_cache(workspace_id, created.id)
                        .await
                    {
                        tracing::warn!(
                            connection_id = %created.id,
                            %cache_error,
                            "rolled-back shared connection cache cleanup deferred"
                        );
                    }
                }
                Err(rollback_error) => tracing::warn!(
                    connection_id = %created.id,
                    %rollback_error,
                    "shared connection rollback deferred"
                ),
            }
            Err(error)
        }
    }
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
    let username = username.trim();
    if username.len() > 320 || username.chars().any(char::is_control) {
        return Err(AppError::Config("username is invalid".into()));
    }
    if password.is_empty() || password.len() > MAX_CONNECTION_CREDENTIAL_BYTES {
        return Err(AppError::Config(
            "connection credential is empty or exceeds the size limit".into(),
        ));
    }
    let password = Zeroizing::new(password);
    let mutation = state
        .connections
        .begin_connection_mutation(id, ConnectionAccess::Read)
        .await?;
    let profile = mutation.pin().profile.clone();
    if profile.workspace_access == WorkspaceConnectionAccess::Local {
        return Err(AppError::Config(
            "connection is not a shared workspace template".into(),
        ));
    }
    if profile.credential_mode != WorkspaceCredentialMode::MemberLocal {
        return Err(AppError::Blocked {
            reason: "this shared connection uses automatically managed credentials".into(),
        });
    }
    if !profile.workspace_access.can_read() {
        return Err(AppError::Blocked {
            reason: "your workspace role cannot execute this connection".into(),
        });
    }
    let account_user_id = mutation
        .pin()
        .scope
        .selected_account_id
        .clone()
        .ok_or_else(|| AppError::Config("no active workspace account".into()))?;
    let previous_credential_id = profile
        .secret_ref
        .as_deref()
        .map(Uuid::parse_str)
        .transpose()
        .map_err(|_| AppError::Config("connection secret reference is invalid".into()))?;
    // Copy-on-write prevents a password-only rotation from mutating credential
    // material behind an unchanged binding revision.
    let credential_id = Uuid::new_v4();
    connection::store_secret(&credential_id, password.as_str())?;
    let credential_ref = credential_id.to_string();
    match state
        .store
        .bind_connection_credentials(
            id,
            &account_user_id,
            username,
            &profile.extra_params,
            Some(&credential_ref),
        )
        .await
    {
        Ok(profile) => {
            mutation.retire_connection(id).await;
            if let Some(previous_credential_id) = previous_credential_id {
                delete_secret_best_effort(
                    previous_credential_id,
                    "replace_workspace_connection_credentials",
                );
            }
            Ok(profile)
        }
        Err(error) => {
            delete_secret_best_effort(credential_id, "bind_connection_credentials");
            Err(error)
        }
    }
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
pub async fn run_sql(
    state: State<'_, AppState>,
    id: Uuid,
    sql: String,
    approved: bool,
    // Optional so existing frontend invokes keep working. `query_id` wires the
    // executor cancel slot; `origin` tags the history row (agent/data-view vs manual).
    query_id: Option<Uuid>,
    origin: Option<String>,
) -> AppResult<DesktopSqlRunReceipt> {
    state
        .services
        .query
        .run_desktop_sql(DesktopSqlRunRequest {
            connection_id: id,
            sql,
            approved,
            query_id,
            origin,
        })
        .await
        .map_err(DesktopSqlRunError::into_error)
}

// ── typed document queries (MongoDB) ─────────────────────────────────────────

/// The document counterpart of `run_sql`: same L4 gate and audit/history trail,
/// but classification walks the TYPED request (aggregate-stage allowlist) instead
/// of SQL text, and there is no write path at all — a non-Read classification is
/// blocked regardless of `allow_writes`/`approved`.
#[tauri::command]
pub async fn run_document_query(
    state: State<'_, AppState>,
    id: Uuid,
    query: DocumentQuery,
    approved: bool,
    query_id: Option<Uuid>,
    origin: Option<String>,
) -> AppResult<DocumentReadReceipt> {
    state
        .services
        .document
        .run_desktop_read(DesktopDocumentReadRequest {
            connection_id: id,
            query,
            approved,
            query_id,
            origin,
        })
        .await
        .map_err(DesktopDocumentReadError::into_error)
}

// ── multi-statement script execution ─────────────────────────────────────────

/// Run a pasted multi-statement script. Splits into statements (comment-only skipped),
/// classifies EACH via L1, then:
/// - all reads → run sequentially on the read-only pool (honoring `auto_run_reads`,
///   `max_rows` per statement), stopping at the first error;
/// - any write/DDL → require `approved` AND `allow_writes`, then run ALL statements in
///   ONE write-pool transaction (rollback on the first error).
///
/// This is the escape hatch from `run_sql`'s single-statement L4 hard block: a seed or
/// multi-statement file that `run_sql` refuses can run here, still gated + audited.
#[tauri::command]
pub async fn run_script(
    state: State<'_, AppState>,
    id: Uuid,
    sql: String,
    approved: bool,
    query_id: Option<Uuid>,
    origin: Option<String>,
) -> AppResult<DesktopScriptRunReceipt> {
    state
        .services
        .script
        .run_desktop(DesktopScriptRunRequest {
            connection_id: id,
            sql,
            approved,
            query_id,
            origin,
        })
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

/// Grant/revoke one fixed PostgreSQL predefined role for CURRENT_USER. This narrow
/// privilege action is independent of arbitrary SQL writes, but still requires a
/// visible user confirmation and is recorded with an explicit approver.
#[tauri::command]
pub async fn set_postgres_monitoring(
    state: State<'_, AppState>,
    id: Uuid,
    enabled: bool,
    approved: bool,
) -> AppResult<MonitoringStatusReceipt> {
    state
        .services
        .monitoring
        .set_postgres_role(MonitoringChangeRequest {
            connection_id: id,
            enabled,
            approved,
        })
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
    crate::agent::detect_clis_async().await
}

/// Models `provider`'s CLI can run a turn against (Codex: its own live catalog;
/// Claude Code: a static fallback — see `agent::claude_models`). Async + internally
/// timeout-bounded, same as `detect_agent_clis`.
#[tauri::command]
pub async fn list_agent_models(
    provider: crate::agent::AgentProvider,
) -> AppResult<Vec<crate::agent::AgentModel>> {
    crate::agent::list_models_async(provider).await
}

/// Saved chat threads, most recently updated first.
#[tauri::command]
pub async fn list_chat_threads(
    state: State<'_, AppState>,
) -> AppResult<Vec<crate::agent::ChatThread>> {
    state.store.list_chat_threads().await
}

/// One thread's messages, oldest first.
#[tauri::command]
pub async fn get_chat_messages(
    state: State<'_, AppState>,
    thread_id: Uuid,
) -> AppResult<Vec<crate::agent::ChatMessageRecord>> {
    state.store.list_chat_messages(thread_id).await
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
        .store
        .create_chat_thread(provider, connection_id, model, effort)
        .await
}

/// Delete a thread; its messages cascade with it.
#[tauri::command]
pub async fn delete_chat_thread(state: State<'_, AppState>, thread_id: Uuid) -> AppResult<()> {
    state.store.delete_chat_thread(thread_id).await
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
