//! Account-aware workspace authentication, membership synchronization, and selection.
//! Better Auth tokens remain behind [`crate::workspace_auth`] and never enter a DTO.

use std::sync::Arc;

use uuid::Uuid;
use zeroize::Zeroizing;

use crate::connection::{ConnectionAccess, ConnectionManager};
use crate::error::{AppError, AppResult};
use crate::model::{
    ConnectionProfile, Workspace, WorkspaceAuthState, WorkspaceAuthUser, WorkspaceConnectionAccess,
    WorkspaceCredentialMode, WorkspaceDeviceAuthorization, WorkspaceFeatureState, WorkspaceKind,
    WorkspaceLoginPoll, WorkspaceLoginPollStatus,
};
use crate::store::Store;
use crate::workspace_auth;

use super::connection_credentials::{ConnectionCredentialVault, MAX_CONNECTION_CREDENTIAL_BYTES};

pub(crate) struct WorkspaceConnectionCopyRequest {
    pub(crate) connection_id: Uuid,
    pub(crate) workspace_id: Uuid,
    pub(crate) account_user_id: String,
}

pub(crate) struct WorkspaceCredentialBindingRequest {
    pub(crate) connection_id: Uuid,
    pub(crate) username: String,
    pub(crate) password: Zeroizing<String>,
}

#[derive(Clone)]
pub(crate) struct WorkspaceService {
    store: Store,
    connections: ConnectionManager,
    credentials: Arc<dyn ConnectionCredentialVault>,
}

impl WorkspaceService {
    pub(super) fn new(
        store: Store,
        connections: ConnectionManager,
        credentials: Arc<dyn ConnectionCredentialVault>,
    ) -> Self {
        Self {
            store,
            connections,
            credentials,
        }
    }

    pub(crate) fn feature_state(&self) -> WorkspaceFeatureState {
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

    pub(crate) async fn auth_state(&self) -> AppResult<WorkspaceAuthState> {
        self.ensure_active_account().await?;
        self.auth_state_from_store().await
    }

    /// Revalidate the active hosted session and memberships without making initial UI
    /// rendering wait on the OS credential store or network. Cached public identity
    /// remains stable during outages; sensitive commands still authorize online.
    pub(crate) async fn refresh_auth_state(&self) -> AppResult<WorkspaceAuthState> {
        if self.store.workspace_accounts().await?.is_empty() {
            if let Some(user) = workspace_auth::migrate_legacy_session().await? {
                if let Err(error) = self.sync_account_memberships(&user).await {
                    tracing::warn!(%error, "legacy workspace membership sync deferred");
                }
                self.connections
                    .activate_workspace_account(&user.id)
                    .await?;
            }
        }

        self.ensure_active_account().await?;
        if let Some(user_id) = self.store.active_workspace_account_id().await? {
            match workspace_auth::auth_user(&user_id).await {
                Ok(Some(user)) => {
                    if let Err(error) = self.sync_account_memberships(&user).await {
                        tracing::warn!(%error, "workspace membership sync deferred after session validation");
                    }
                }
                Ok(None) => {
                    self.connections.remove_workspace_account(&user_id).await?;
                    self.ensure_active_account().await?;
                }
                Err(error) => {
                    // Keep the last verified identity visible during an outage. Every
                    // shared-resource action still performs its own online authorization.
                    tracing::warn!(%error, "workspace session validation deferred");
                }
            }
        }
        self.auth_state_from_store().await
    }

    pub(crate) async fn sign_out(&self, user_id: Option<String>) -> AppResult<WorkspaceAuthState> {
        let user_id = match user_id {
            Some(user_id) => user_id,
            None => self
                .store
                .active_workspace_account_id()
                .await?
                .ok_or_else(|| AppError::Config("no workspace account is signed in".into()))?,
        };
        self.connections.remove_workspace_account(&user_id).await?;
        // Pool retirement releases managed provider credentials while the Better Auth
        // token is still available; session revocation and local token deletion follow.
        workspace_auth::sign_out(&user_id).await?;
        self.auth_state_from_store().await
    }

    pub(crate) async fn sign_out_all(&self) -> AppResult<WorkspaceAuthState> {
        let accounts = self.store.workspace_accounts().await?;
        let mut first_error = None;
        for account in accounts {
            if let Err(error) = self
                .connections
                .remove_workspace_account(&account.user.id)
                .await
            {
                first_error.get_or_insert(error);
            }
            if let Err(error) = workspace_auth::sign_out(&account.user.id).await {
                first_error.get_or_insert(error);
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        self.auth_state_from_store().await
    }

    pub(crate) async fn begin_login(&self) -> AppResult<WorkspaceDeviceAuthorization> {
        workspace_auth::begin_login().await
    }

    pub(crate) async fn poll_login(&self, device_code: &str) -> AppResult<WorkspaceLoginPoll> {
        let result = workspace_auth::poll_login(device_code).await?;
        if result.status == WorkspaceLoginPollStatus::SignedIn {
            let user = result.user.as_ref().ok_or_else(|| {
                AppError::Network("workspace login did not return an account".into())
            })?;
            if let Err(error) = self.sync_account_memberships(user).await {
                // The session token is already validated and stored. Do not report a
                // successful login as failed merely because the first membership refresh
                // encountered a transient control-plane or local-cache error.
                tracing::warn!(%error, "workspace membership sync deferred after sign-in");
            }
            self.connections
                .activate_workspace_account(&user.id)
                .await?;
        }
        Ok(result)
    }

    pub(crate) fn console_url(&self, workspace_id: Option<Uuid>) -> AppResult<String> {
        workspace_auth::console_url(workspace_id)
    }

    pub(crate) async fn list(&self) -> AppResult<Vec<Workspace>> {
        self.store.list_workspaces().await
    }

    /// Explicitly refresh hosted memberships without changing the cached authentication
    /// presentation. The desktop calls this after returning from web settings.
    pub(crate) async fn refresh_memberships(&self) -> AppResult<Vec<Workspace>> {
        let accounts = self.store.workspace_accounts().await?;
        for account in accounts {
            match workspace_auth::auth_user(&account.user.id).await {
                Ok(Some(user)) => self.sync_account_memberships(&user).await?,
                Ok(None) => {
                    self.connections
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
        self.ensure_active_account().await?;
        self.store.list_workspaces().await
    }

    pub(crate) async fn active(&self) -> AppResult<Workspace> {
        self.store.active_workspace().await
    }

    pub(crate) async fn activate(
        &self,
        id: Uuid,
        account_user_id: Option<String>,
    ) -> AppResult<Workspace> {
        let target = self
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
            let user = self.validated_user(user_id).await?;
            self.sync_account_memberships(&user).await?;
        }
        let workspace = self
            .connections
            .activate_workspace(id, account_user_id.as_deref())
            .await?;
        if workspace.kind == WorkspaceKind::Team {
            let account_user_id = account_user_id.ok_or_else(|| {
                AppError::Config("team workspace selection requires an account".into())
            })?;
            if let Err(error) = self.sync_connections(&account_user_id, workspace.id).await {
                tracing::warn!(workspace_id = %workspace.id, %error, "workspace connection sync deferred after switch");
            }
        }
        Ok(workspace)
    }

    pub(crate) async fn activate_account(&self, user_id: String) -> AppResult<Workspace> {
        let user = self.validated_user(&user_id).await?;
        self.sync_account_memberships(&user).await?;
        let workspace = self
            .connections
            .activate_workspace_account(&user_id)
            .await?;
        if workspace.kind == WorkspaceKind::Team {
            if let Err(error) = self.sync_connections(&user_id, workspace.id).await {
                tracing::warn!(workspace_id = %workspace.id, %error, "workspace connection sync deferred after account switch");
            }
        }
        Ok(workspace)
    }

    /// Copy a local connection into a team workspace. Only its redacted template
    /// crosses the network; the caller's credential is duplicated locally under the
    /// remote resource UUID.
    pub(crate) async fn copy_connection(
        &self,
        request: WorkspaceConnectionCopyRequest,
    ) -> AppResult<ConnectionProfile> {
        let WorkspaceConnectionCopyRequest {
            connection_id,
            workspace_id,
            account_user_id,
        } = request;
        let source = self.store.get_connection(connection_id).await?;
        if source.workspace_access != WorkspaceConnectionAccess::Local {
            return Err(AppError::Config(
                "only a local connection can be copied into a workspace".into(),
            ));
        }
        let target = self
            .store
            .list_workspaces()
            .await?
            .into_iter()
            .find(|workspace| workspace.id == workspace_id && workspace.kind == WorkspaceKind::Team)
            .ok_or_else(|| AppError::NotFound(format!("team workspace {workspace_id}")))?;
        let current_account = self.store.active_workspace_account_id().await?;
        if target.id == self.store.active_workspace_id().await?
            && current_account.as_deref() == Some(account_user_id.as_str())
        {
            return Err(AppError::Config("choose a different team workspace".into()));
        }
        let account = self
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
            Some(self.credentials.fetch_profile(&source)?)
        } else {
            None
        };
        let mut remote = workspace_auth::remote_connections(&account.user.id, workspace_id)
            .await?
            .ok_or_else(|| {
                AppError::Network(
                    "the workspace service has not deployed shared connections yet".into(),
                )
            })?;
        let credential_id = copied_secret.as_ref().map(|_| Uuid::new_v4());
        if let (Some(credential_id), Some(secret)) = (credential_id, copied_secret.as_deref()) {
            self.credentials.store(&credential_id, secret)?;
        }
        let shared =
            workspace_auth::share_connection(&account.user.id, workspace_id, &source).await;
        let (created, revision) = match shared {
            Ok(created) => created,
            Err(error) => {
                if let Some(credential_id) = credential_id {
                    self.delete_secret_best_effort(credential_id, "share_connection");
                }
                return Err(error);
            }
        };
        remote.push((created.clone(), revision));
        let credential_ref = credential_id.map(|id| id.to_string());
        let local_result = async {
            let removed_credential_ids = self
                .connections
                .sync_remote_connections(workspace_id, &account.user.id, &remote)
                .await?;
            for credential_id in removed_credential_ids {
                self.delete_secret_best_effort(credential_id, "remove_remote_connection");
            }
            self.store
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
                    self.delete_secret_best_effort(credential_id, "persist_shared_connection");
                }
                match workspace_auth::delete_connection(&account.user.id, workspace_id, created.id)
                    .await
                {
                    Ok(()) => {
                        if let Err(cache_error) = self
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

    /// Store one member's database credential only in the OS credential store and
    /// atomically publish the new binding revision for a shared template.
    pub(crate) async fn bind_connection_credentials(
        &self,
        request: WorkspaceCredentialBindingRequest,
    ) -> AppResult<ConnectionProfile> {
        let WorkspaceCredentialBindingRequest {
            connection_id,
            username,
            password,
        } = request;
        let username = username.trim();
        if username.len() > 320 || username.chars().any(char::is_control) {
            return Err(AppError::Config("username is invalid".into()));
        }
        if password.is_empty() || password.len() > MAX_CONNECTION_CREDENTIAL_BYTES {
            return Err(AppError::Config(
                "connection credential is empty or exceeds the size limit".into(),
            ));
        }
        let mutation = self
            .connections
            .begin_connection_mutation(connection_id, ConnectionAccess::Read)
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
        self.credentials.store(&credential_id, password.as_str())?;
        let credential_ref = credential_id.to_string();
        match self
            .store
            .bind_connection_credentials(
                connection_id,
                &account_user_id,
                username,
                &profile.extra_params,
                Some(&credential_ref),
            )
            .await
        {
            Ok(profile) => {
                mutation.retire_connection(connection_id).await;
                if let Some(previous_credential_id) = previous_credential_id {
                    self.delete_secret_best_effort(
                        previous_credential_id,
                        "replace_workspace_connection_credentials",
                    );
                }
                Ok(profile)
            }
            Err(error) => {
                self.delete_secret_best_effort(credential_id, "bind_connection_credentials");
                Err(error)
            }
        }
    }

    async fn auth_state_from_store(&self) -> AppResult<WorkspaceAuthState> {
        let accounts = self.store.workspace_accounts().await?;
        let active_account_id = self.store.active_workspace_account_id().await?;
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

    async fn ensure_active_account(&self) -> AppResult<()> {
        let active_account_id = self.store.active_workspace_account_id().await?;
        let accounts = self.store.workspace_accounts().await?;
        if active_account_id
            .as_ref()
            .is_some_and(|active_id| accounts.iter().any(|account| account.user.id == *active_id))
        {
            return Ok(());
        }
        if let Some(stale_id) = active_account_id {
            self.connections.remove_workspace_account(&stale_id).await?;
        } else if let Some(account) = accounts.first() {
            self.connections
                .activate_workspace_account(&account.user.id)
                .await?;
        }
        Ok(())
    }

    async fn validated_user(&self, user_id: &str) -> AppResult<WorkspaceAuthUser> {
        match workspace_auth::auth_user(user_id).await? {
            Some(user) => Ok(user),
            None => {
                self.connections.remove_workspace_account(user_id).await?;
                self.ensure_active_account().await?;
                Err(AppError::Network(
                    "workspace session is no longer active".into(),
                ))
            }
        }
    }

    async fn sync_account_memberships(&self, user: &WorkspaceAuthUser) -> AppResult<()> {
        self.store.remember_workspace_account(user).await?;
        let remote = workspace_auth::remote_workspaces(&user.id).await?;
        let workspaces = remote
            .into_iter()
            .map(|workspace| (workspace.id, workspace.name, workspace.role))
            .collect::<Vec<_>>();
        self.connections
            .sync_account_workspaces(user, &workspaces)
            .await?;
        let active = self.store.active_workspace().await?;
        if active.kind == WorkspaceKind::Team
            && self.store.active_workspace_account_id().await?.as_deref() == Some(user.id.as_str())
        {
            self.sync_connections(&user.id, active.id).await?;
        }
        Ok(())
    }

    async fn sync_connections(&self, account_user_id: &str, workspace_id: Uuid) -> AppResult<()> {
        match workspace_auth::remote_connections(account_user_id, workspace_id).await {
            Ok(Some(connections)) => {
                let removed_credential_ids = self
                    .connections
                    .sync_remote_connections(workspace_id, account_user_id, &connections)
                    .await?;
                for credential_id in removed_credential_ids {
                    self.delete_secret_best_effort(credential_id, "remove_remote_connection");
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

    fn delete_secret_best_effort(&self, id: Uuid, action: &'static str) {
        if let Err(error) = self.credentials.delete(&id) {
            tracing::warn!(credential_id = %id, %error, action, "credential cleanup deferred");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::str::FromStr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use serde_json::json;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    use super::*;
    use crate::model::{Engine, Provider, WorkspaceAuthUser, WorkspaceRole};
    use crate::store::TEST_SCHEMA;

    #[derive(Default)]
    struct MemoryCredentials {
        items: Mutex<HashMap<Uuid, String>>,
        fetches: AtomicUsize,
    }

    impl MemoryCredentials {
        fn snapshot(&self) -> HashMap<Uuid, String> {
            self.items.lock().unwrap().clone()
        }

        fn fetch_count(&self) -> usize {
            self.fetches.load(Ordering::Relaxed)
        }
    }

    impl ConnectionCredentialVault for MemoryCredentials {
        fn fetch_profile(&self, profile: &ConnectionProfile) -> AppResult<Zeroizing<String>> {
            self.fetches.fetch_add(1, Ordering::Relaxed);
            let Some(secret_ref) = profile.secret_ref.as_deref() else {
                if profile.workspace_access == WorkspaceConnectionAccess::Local {
                    return Ok(Zeroizing::new(String::new()));
                }
                return Err(AppError::NotFound(format!(
                    "no credential binding for shared connection {}",
                    profile.id
                )));
            };
            let id = Uuid::parse_str(secret_ref)
                .map_err(|_| AppError::Config("invalid test credential reference".into()))?;
            self.items
                .lock()
                .unwrap()
                .get(&id)
                .cloned()
                .map(Zeroizing::new)
                .ok_or_else(|| AppError::NotFound(format!("test credential {id}")))
        }

        fn store(&self, id: &Uuid, secret: &str) -> AppResult<()> {
            self.items.lock().unwrap().insert(*id, secret.to_string());
            Ok(())
        }

        fn delete(&self, id: &Uuid) -> AppResult<()> {
            self.items.lock().unwrap().remove(id);
            Ok(())
        }
    }

    async fn harness() -> (
        WorkspaceService,
        Store,
        ConnectionManager,
        Arc<MemoryCredentials>,
    ) {
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
        let connections = ConnectionManager::new(store.clone());
        let credentials = Arc::new(MemoryCredentials::default());
        (
            WorkspaceService::new(store.clone(), connections.clone(), credentials.clone()),
            store,
            connections,
            credentials,
        )
    }

    fn local_profile(id: Uuid) -> ConnectionProfile {
        ConnectionProfile {
            id,
            name: "local".into(),
            engine: Engine::Sqlite,
            provider: Provider::Generic,
            driver_id: Some("sqlx-sqlite".into()),
            host: String::new(),
            port: 0,
            database: ":memory:".into(),
            username: "tester".into(),
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

    #[tokio::test]
    async fn fresh_auth_state_preserves_the_unauthenticated_wire_and_personal_scope() {
        let (service, store, _, _) = harness().await;

        assert_eq!(
            serde_json::to_value(service.auth_state().await.unwrap()).unwrap(),
            json!({
                "authenticated": false,
                "user": null,
                "accounts": [],
            })
        );
        let workspaces = service.list().await.unwrap();
        assert_eq!(workspaces.len(), 1);
        assert_eq!(service.active().await.unwrap().id, workspaces[0].id);
        assert_eq!(workspaces[0].kind, WorkspaceKind::Personal);
        store.pool().close().await;
    }

    #[tokio::test]
    async fn cached_accounts_select_their_own_team_membership_without_exposing_a_session() {
        let (service, store, connections, _) = harness().await;
        let team_id = Uuid::new_v4();
        let user = WorkspaceAuthUser {
            id: Uuid::new_v4().to_string(),
            email: "member@example.com".into(),
            display_name: "Member".into(),
        };
        store.remember_workspace_account(&user).await.unwrap();
        connections
            .sync_account_workspaces(&user, &[(team_id, "Team".into(), WorkspaceRole::Editor)])
            .await
            .unwrap();
        let active = connections
            .activate_workspace_account(&user.id)
            .await
            .unwrap();
        assert_eq!(active.id, team_id);

        let auth = service.auth_state().await.unwrap();
        assert!(auth.authenticated);
        assert_eq!(
            auth.user.as_ref().map(|user| user.id.as_str()),
            Some(user.id.as_str())
        );
        assert_eq!(auth.accounts.len(), 1);
        assert_eq!(auth.accounts[0].memberships.len(), 1);
        let serialized = serde_json::to_string(&auth).unwrap();
        assert!(!serialized.contains("access_token"));
        assert!(!serialized.contains("session"));
        store.pool().close().await;
    }

    #[tokio::test]
    async fn missing_workspace_selection_keeps_the_exact_not_found_contract() {
        let (service, store, _, _) = harness().await;
        let missing = Uuid::new_v4();
        assert!(matches!(
            service.activate(missing, None).await,
            Err(AppError::NotFound(message)) if message == format!("workspace {missing}")
        ));
        store.pool().close().await;
    }

    #[tokio::test]
    async fn copy_resolves_local_scope_before_reading_or_publishing_credentials() {
        let (service, store, _, credentials) = harness().await;
        let connection_id = Uuid::new_v4();
        let secret_id = Uuid::new_v4();
        credentials
            .store(&secret_id, "never-read-for-a-missing-team")
            .unwrap();
        let mut profile = local_profile(connection_id);
        profile.secret_ref = Some(secret_id.to_string());
        store.upsert_connection(&profile).await.unwrap();
        let missing_team = Uuid::new_v4();

        assert!(matches!(
            service
                .copy_connection(WorkspaceConnectionCopyRequest {
                    connection_id,
                    workspace_id: missing_team,
                    account_user_id: Uuid::new_v4().to_string(),
                })
                .await,
            Err(AppError::NotFound(message))
                if message == format!("team workspace {missing_team}")
        ));
        assert_eq!(
            credentials.snapshot().get(&secret_id).map(String::as_str),
            Some("never-read-for-a-missing-team")
        );
        assert_eq!(credentials.fetch_count(), 0);
        store.pool().close().await;
    }

    #[tokio::test]
    async fn binding_validates_secrets_before_scope_mutation_and_rejects_local_profiles() {
        let (service, store, _, credentials) = harness().await;
        let connection_id = Uuid::new_v4();
        store
            .upsert_connection(&local_profile(connection_id))
            .await
            .unwrap();

        assert!(matches!(
            service
                .bind_connection_credentials(WorkspaceCredentialBindingRequest {
                    connection_id,
                    username: "bad\nname".into(),
                    password: Zeroizing::new("secret".into()),
                })
                .await,
            Err(AppError::Config(message)) if message == "username is invalid"
        ));
        assert!(matches!(
            service
                .bind_connection_credentials(WorkspaceCredentialBindingRequest {
                    connection_id,
                    username: "member".into(),
                    password: Zeroizing::new(String::new()),
                })
                .await,
            Err(AppError::Config(message))
                if message == "connection credential is empty or exceeds the size limit"
        ));
        assert!(matches!(
            service
                .bind_connection_credentials(WorkspaceCredentialBindingRequest {
                    connection_id,
                    username: " member ".into(),
                    password: Zeroizing::new("secret".into()),
                })
                .await,
            Err(AppError::Config(message))
                if message == "connection is not a shared workspace template"
        ));
        assert!(credentials.snapshot().is_empty());
        store.pool().close().await;
    }
}
