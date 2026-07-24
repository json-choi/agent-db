//! Connection application service and the deliberately narrow agent-facing summary DTO.
//! Full profiles remain available to the existing UI path; agent summaries are allowlisted.

use std::fmt;
use std::sync::Arc;

use dopedb_protocol::ConnectionSelector;
use serde::Serialize;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::connection::{self, ConnectionAccess, ConnectionManager};
use crate::driver::{self, DriverDescriptor};
use crate::error::{AppError, AppResult};
use crate::model::{ConnectionProfile, Engine, WorkspaceConnectionAccess, WorkspaceCredentialMode};
use crate::store::Store;

use super::connection_credentials::{ConnectionCredentialVault, MAX_CONNECTION_CREDENTIAL_BYTES};
use super::TerminalAuthority;

pub(crate) struct ConnectionUpsertRequest {
    pub(crate) profile: ConnectionProfile,
    pub(crate) password: Option<Zeroizing<String>>,
}

pub(crate) struct ConnectionProfileTestRequest {
    pub(crate) profile: ConnectionProfile,
    pub(crate) password: Option<Zeroizing<String>>,
}

/// A connection projection safe to serialize for an agent transport.
///
/// This allowlist intentionally has no provider, driver, network host/port, user,
/// credential reference, workspace/account authority, or provider-specific parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentConnectionSummary {
    pub(crate) id: Uuid,
    pub(crate) name: String,
    pub(crate) engine: Engine,
    pub(crate) database: String,
    pub(crate) environment: Option<String>,
    pub(crate) readonly: bool,
    pub(crate) allow_writes: bool,
}

impl From<&ConnectionProfile> for AgentConnectionSummary {
    fn from(profile: &ConnectionProfile) -> Self {
        Self {
            id: profile.id,
            name: profile.name.clone(),
            engine: profile.engine,
            database: profile.database.clone(),
            environment: profile.env.clone(),
            readonly: profile.readonly_default,
            allow_writes: profile.allow_writes,
        }
    }
}

/// Domain-level legacy selector failures. Transport adapters map these variants to
/// their own error envelope without pulling rmcp or Tauri types into this service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LegacyConnectionResolutionError {
    NoConnections,
    NoMatch { selector: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CliConnectionResolutionError {
    NoMatch,
    Ambiguous {
        candidates: Vec<AgentConnectionSummary>,
    },
}

impl fmt::Display for CliConnectionResolutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoMatch => formatter.write_str("no connection matches the exact selector"),
            Self::Ambiguous { .. } => {
                formatter.write_str("the exact connection name matches more than one connection")
            }
        }
    }
}

impl std::error::Error for CliConnectionResolutionError {}

impl fmt::Display for LegacyConnectionResolutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoConnections => formatter.write_str("no connections are available"),
            Self::NoMatch { selector } => {
                write!(formatter, "no connection matches selector '{selector}'")
            }
        }
    }
}

impl std::error::Error for LegacyConnectionResolutionError {}

/// The outer [`AppResult`] of [`ConnectionService::resolve_legacy_mcp`] represents
/// store/authority failures; this inner result represents only selector semantics.
pub(crate) type LegacyConnectionResolution =
    Result<AgentConnectionSummary, LegacyConnectionResolutionError>;

#[derive(Clone)]
pub(crate) struct ConnectionService {
    store: Store,
    connections: ConnectionManager,
    credentials: Arc<dyn ConnectionCredentialVault>,
}

impl ConnectionService {
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

    pub(crate) fn list_drivers(&self) -> Vec<DriverDescriptor> {
        driver::list()
    }

    pub(crate) fn install_driver(&self, id: &str) -> AppResult<DriverDescriptor> {
        driver::install(id)
    }

    /// Preserve the existing UI contract, including its full non-plaintext profile.
    pub(crate) async fn list_profiles(&self) -> AppResult<Vec<ConnectionProfile>> {
        self.store.list_connections().await
    }

    /// Persist one local connection and atomically rotate its OS credential pointer.
    /// The IPC-supplied `secret_ref` is never trusted, and failed profile commits
    /// remove newly written credential material before returning.
    pub(crate) async fn upsert(
        &self,
        request: ConnectionUpsertRequest,
    ) -> AppResult<ConnectionProfile> {
        let ConnectionUpsertRequest {
            mut profile,
            password,
        } = request;
        if profile.workspace_access != WorkspaceConnectionAccess::Local {
            return Err(AppError::Blocked {
                reason:
                    "shared templates are edited by workspace editors; bind credentials separately"
                        .into(),
            });
        }
        profile.schema_group = normalize_schema_group(profile.schema_group);
        // Reject stale or incompatible explicit driver choices before persisting the profile.
        driver::validate(&profile)?;
        let mutation = self.connections.begin_scope_mutation().await;
        // Scope validation precedes credential-store writes so a known UUID from another
        // workspace cannot replace that resource's member-local secret as a side effect.
        self.store.ensure_connection_write_scope(profile.id).await?;
        let connections = self.store.list_connections().await?;
        validate_schema_group_engine(&profile, &connections)?;
        let existing_secret_id = connections
            .iter()
            .find(|connection| connection.id == profile.id)
            .and_then(|connection| connection.secret_ref.as_deref())
            .map(Uuid::parse_str)
            .transpose()
            .map_err(|_| {
                AppError::Config("stored connection secret reference is invalid".into())
            })?;
        let password = password.filter(|password| !password.is_empty());
        if password
            .as_ref()
            .is_some_and(|value| value.len() > MAX_CONNECTION_CREDENTIAL_BYTES)
        {
            return Err(AppError::Config(
                "connection credential exceeds the size limit".into(),
            ));
        }
        // Never trust an IPC-supplied secret reference. Preserve the stored pointer when
        // no password is supplied, or write a new credential item and atomically swap the
        // pointer with the SQLite profile.
        profile.secret_ref = existing_secret_id.map(|id| id.to_string());
        let replacement_secret_id = password.as_ref().map(|_| Uuid::new_v4());
        if let Some(password) = password.as_deref() {
            let credential_id = replacement_secret_id.expect("replacement id accompanies password");
            self.credentials.store(&credential_id, password)?;
            profile.secret_ref = Some(credential_id.to_string());
        }
        match self.store.upsert_connection(&profile).await {
            Ok(profile) => {
                // Only retire the old material after SQLite commits. A failed edit keeps
                // both the prior pool and catalog valid.
                let _ = self.store.clear_schema_cache(profile.id).await;
                mutation.retire_connection(profile.id).await;
                if replacement_secret_id.is_some() {
                    if let Some(previous_id) = existing_secret_id {
                        self.delete_secret_best_effort(
                            previous_id,
                            "replace_connection_credentials",
                        );
                    }
                }
                Ok(profile)
            }
            Err(error) => {
                if let Some(credential_id) = replacement_secret_id {
                    self.delete_secret_best_effort(credential_id, "upsert_connection");
                }
                Err(error)
            }
        }
    }

    pub(crate) async fn set_schema_group(
        &self,
        ids: Vec<Uuid>,
        schema_group: Option<String>,
    ) -> AppResult<Vec<ConnectionProfile>> {
        let mut unique_ids = Vec::with_capacity(ids.len());
        for id in ids {
            if !unique_ids.contains(&id) {
                unique_ids.push(id);
            }
        }
        if unique_ids.is_empty() {
            return Ok(Vec::new());
        }

        let mutation = self.connections.begin_scope_mutation().await;
        let normalized = normalize_schema_group(schema_group);
        let mut connections = self.store.list_connections().await?;
        for id in &unique_ids {
            let profile = connections
                .iter_mut()
                .find(|profile| profile.id == *id)
                .ok_or_else(|| AppError::NotFound(format!("connection {id}")))?;
            if profile.workspace_access != WorkspaceConnectionAccess::Local {
                return Err(AppError::Blocked {
                    reason:
                        "shared template metadata must be changed through the workspace service"
                            .into(),
                });
            }
            profile.schema_group = normalized.clone();
        }

        let updated = unique_ids
            .iter()
            .map(|id| {
                connections
                    .iter()
                    .find(|profile| profile.id == *id)
                    .cloned()
                    .ok_or_else(|| AppError::NotFound(format!("connection {id}")))
            })
            .collect::<AppResult<Vec<_>>>()?;
        for profile in &updated {
            validate_schema_group_engine(profile, &connections)?;
        }

        self.store
            .set_connections_schema_group(&unique_ids, normalized)
            .await?;
        mutation.retire_connections(&unique_ids).await;
        Ok(updated)
    }

    pub(crate) async fn delete(&self, id: Uuid) -> AppResult<()> {
        let mutation = self
            .connections
            .begin_connection_mutation(id, ConnectionAccess::Read)
            .await?;
        let profile = mutation.pin().profile.clone();
        if profile.workspace_access != WorkspaceConnectionAccess::Local {
            return Err(AppError::Blocked {
                reason: "shared connections can only be removed by a workspace administrator"
                    .into(),
            });
        }
        self.store.delete_connection(id).await?;
        if let Some(secret_ref) = profile.secret_ref.as_deref() {
            match Uuid::parse_str(secret_ref) {
                Ok(credential_id) => {
                    self.delete_secret_best_effort(credential_id, "delete_connection");
                }
                Err(error) => {
                    tracing::warn!(connection_id = %id, %error, "ignored invalid credential reference while deleting connection");
                }
            }
        }
        mutation.retire_connection(id).await;
        Ok(())
    }

    pub(crate) async fn test(&self, id: Uuid) -> AppResult<()> {
        let profile = self.store.get_connection(id).await?;
        if !profile.workspace_access.can_read() {
            return Err(AppError::Blocked {
                reason: "your workspace role cannot test this shared connection".into(),
            });
        }
        self.connections
            .pin(id, ConnectionAccess::Read)
            .await?
            .test_fresh()
            .await
    }

    /// Dial an ad-hoc (possibly unsaved) profile without storing a row, credential,
    /// or reusable pool. This is the connection form's literal reachability check.
    pub(crate) async fn test_profile(
        &self,
        request: ConnectionProfileTestRequest,
    ) -> AppResult<()> {
        let ConnectionProfileTestRequest { profile, password } = request;
        if profile.workspace_access != WorkspaceConnectionAccess::Local
            || profile.credential_mode != WorkspaceCredentialMode::Local
        {
            return Err(AppError::Blocked {
                reason: "shared connections must be tested through workspace authorization".into(),
            });
        }
        let secret = password.unwrap_or_default();
        if secret.len() > MAX_CONNECTION_CREDENTIAL_BYTES {
            return Err(AppError::Config(
                "connection credential exceeds the size limit".into(),
            ));
        }
        let live = connection::connect(&profile, secret.as_str()).await?;
        live.test().await
    }

    fn delete_secret_best_effort(&self, id: Uuid, action: &'static str) {
        if let Err(error) = self.credentials.delete(&id) {
            tracing::warn!(credential_id = %id, %error, action, "credential cleanup deferred");
        }
    }

    /// Return only the explicit agent allowlist; secret-bearing profile fields never
    /// become members of the serialized DTO.
    pub(crate) async fn list_agent_summaries(&self) -> AppResult<Vec<AgentConnectionSummary>> {
        Ok(self
            .list_profiles()
            .await?
            .iter()
            .map(AgentConnectionSummary::from)
            .collect())
    }

    pub(crate) async fn terminal_summary(
        &self,
        authority: &TerminalAuthority,
    ) -> AppResult<AgentConnectionSummary> {
        let context = self
            .connections
            .pin(authority.connection_id, ConnectionAccess::Read)
            .await?;
        authority.ensure_pin(context.pin())?;
        Ok(AgentConnectionSummary::from(&context.pin().profile))
    }

    /// Return only the connection pinned to this Terminal capability. A Terminal
    /// session is not a workspace-wide metadata grant, so listing must not disclose
    /// sibling connection names merely because the desktop user can see them.
    pub(crate) async fn list_terminal_summaries(
        &self,
        authority: &TerminalAuthority,
    ) -> AppResult<Vec<AgentConnectionSummary>> {
        let context = self
            .connections
            .pin(authority.connection_id, ConnectionAccess::Read)
            .await?;
        authority.ensure_pin(context.pin())?;
        Ok(vec![AgentConnectionSummary::from(&context.pin().profile)])
    }

    pub(crate) async fn test_terminal(&self, authority: &TerminalAuthority) -> AppResult<()> {
        let context = self
            .connections
            .pin(authority.connection_id, ConnectionAccess::Read)
            .await?;
        authority.ensure_pin(context.pin())?;
        context.test_fresh().await
    }

    /// Preserve the legacy MCP selector exactly: profiles are already name-ordered by
    /// [`Store::list_connections`]; `None` chooses the first, while `Some` finds the
    /// first exact UUID string or exact case-sensitive name.
    ///
    /// The outer result is an application/store failure. The inner result is a
    /// transport-neutral `NoConnections`/`NoMatch` selection outcome.
    pub(crate) async fn resolve_legacy_mcp(
        &self,
        selector: Option<&str>,
    ) -> AppResult<LegacyConnectionResolution> {
        let profiles = self.list_profiles().await?;
        Ok(resolve_legacy_profiles(&profiles, selector))
    }

    /// Resolve an exact CLI selector under the same immutable scope that owns the
    /// Terminal session. The returned summary remains secret-free.
    pub(crate) async fn resolve_terminal_cli(
        &self,
        authority: &TerminalAuthority,
        selector: &ConnectionSelector,
    ) -> AppResult<Result<AgentConnectionSummary, CliConnectionResolutionError>> {
        let context = self
            .connections
            .pin(authority.connection_id, ConnectionAccess::Read)
            .await?;
        authority.ensure_pin(context.pin())?;
        let summaries = self.list_agent_summaries().await?;
        Ok(resolve_cli_summaries(
            &summaries,
            selector,
            authority.connection_id,
        ))
    }
}

fn normalize_schema_group(schema_group: Option<String>) -> Option<String> {
    schema_group.and_then(|value| {
        let trimmed = value.trim().to_string();
        (!trimmed.is_empty()).then_some(trimmed)
    })
}

fn validate_schema_group_engine(
    profile: &ConnectionProfile,
    connections: &[ConnectionProfile],
) -> AppResult<()> {
    let Some(group) = profile.schema_group.as_deref() else {
        return Ok(());
    };
    let incompatible = connections.iter().any(|connection| {
        connection.id != profile.id
            && connection
                .schema_group
                .as_deref()
                .is_some_and(|candidate| candidate.trim().eq_ignore_ascii_case(group))
            && connection.engine != profile.engine
    });
    if incompatible {
        return Err(AppError::Config(format!(
            "schema group '{group}' already contains a different database engine"
        )));
    }
    Ok(())
}

fn resolve_legacy_profiles(
    profiles: &[ConnectionProfile],
    selector: Option<&str>,
) -> LegacyConnectionResolution {
    match selector {
        Some(selector) => profiles
            .iter()
            .find(|profile| profile.id.to_string() == selector || profile.name == selector)
            .map(AgentConnectionSummary::from)
            .ok_or_else(|| LegacyConnectionResolutionError::NoMatch {
                selector: selector.to_string(),
            }),
        None => profiles
            .first()
            .map(AgentConnectionSummary::from)
            .ok_or(LegacyConnectionResolutionError::NoConnections),
    }
}

fn resolve_cli_summaries(
    summaries: &[AgentConnectionSummary],
    selector: &ConnectionSelector,
    current_connection_id: Uuid,
) -> Result<AgentConnectionSummary, CliConnectionResolutionError> {
    match selector {
        ConnectionSelector::Id(id) => summaries
            .iter()
            .find(|summary| summary.id == *id)
            .cloned()
            .ok_or(CliConnectionResolutionError::NoMatch),
        ConnectionSelector::Current => summaries
            .iter()
            .find(|summary| summary.id == current_connection_id)
            .cloned()
            .ok_or(CliConnectionResolutionError::NoMatch),
        ConnectionSelector::Name(name) => {
            let mut candidates = summaries
                .iter()
                .filter(|summary| summary.name == *name)
                .cloned()
                .collect::<Vec<_>>();
            candidates.sort_by_key(|summary| summary.id);
            match candidates.as_slice() {
                [only] => Ok(only.clone()),
                [] => Err(CliConnectionResolutionError::NoMatch),
                _ => Err(CliConnectionResolutionError::Ambiguous { candidates }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::str::FromStr;
    use std::sync::Mutex;

    use serde_json::json;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    use super::*;
    use crate::model::{Provider, WorkspaceConnectionAccess, WorkspaceCredentialMode};
    use crate::store::TEST_SCHEMA;

    const ALPHA_ID: &str = "018f9999-8888-7777-8666-555544443331";
    const BETA_ID: &str = "018f9999-8888-7777-8666-555544443332";

    #[derive(Default)]
    struct MemoryCredentials {
        items: Mutex<HashMap<Uuid, String>>,
    }

    impl MemoryCredentials {
        fn snapshot(&self) -> HashMap<Uuid, String> {
            self.items.lock().unwrap().clone()
        }
    }

    impl ConnectionCredentialVault for MemoryCredentials {
        fn fetch_profile(&self, profile: &ConnectionProfile) -> AppResult<Zeroizing<String>> {
            let secret_ref = profile
                .secret_ref
                .as_deref()
                .ok_or_else(|| AppError::NotFound("missing test credential reference".into()))?;
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

    async fn harness() -> (ConnectionService, Store, Arc<MemoryCredentials>) {
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
        let service = ConnectionService::new(store.clone(), connections, credentials.clone());
        (service, store, credentials)
    }

    fn local_profile(id: Uuid, name: &str, engine: Engine) -> ConnectionProfile {
        let (driver_id, port, database) = match engine {
            Engine::Postgres => ("sqlx-postgres", 5432, "postgres"),
            Engine::Mysql => ("sqlx-mysql", 3306, "mysql"),
            Engine::Sqlite => ("sqlx-sqlite", 0, ":memory:"),
            Engine::Mongodb => ("mongodb-rust", 27017, "admin"),
        };
        ConnectionProfile {
            id,
            name: name.into(),
            engine,
            provider: Provider::Generic,
            driver_id: Some(driver_id.into()),
            host: "localhost".into(),
            port,
            database: database.into(),
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

    fn profile(id: &str, name: &str) -> ConnectionProfile {
        ConnectionProfile {
            id: Uuid::parse_str(id).unwrap(),
            name: name.into(),
            engine: Engine::Postgres,
            provider: Provider::Neon,
            driver_id: Some("secret-driver".into()),
            host: "secret-host.example".into(),
            port: 5432,
            database: "analytics".into(),
            username: "secret-user".into(),
            sslmode: "require".into(),
            extra_params: HashMap::from([("secret-param".into(), "secret-value".into())]),
            readonly_default: true,
            allow_writes: false,
            secret_ref: Some("secret-reference".into()),
            env: Some("prod".into()),
            schema_group: Some("secret-schema-group".into()),
            workspace_access: WorkspaceConnectionAccess::Manage,
            credential_mode: WorkspaceCredentialMode::Managed,
        }
    }

    fn profiles() -> Vec<ConnectionProfile> {
        vec![profile(ALPHA_ID, "alpha"), profile(BETA_ID, "beta")]
    }

    #[test]
    fn normalizes_empty_and_padded_group_names() {
        assert_eq!(
            normalize_schema_group(Some("  Core  ".into())).as_deref(),
            Some("Core")
        );
        assert_eq!(normalize_schema_group(Some("   ".into())), None);
    }

    #[test]
    fn rejects_a_different_engine_in_the_same_case_insensitive_group() {
        let mut postgres = local_profile(Uuid::from_u128(1), "postgres", Engine::Postgres);
        postgres.schema_group = Some("Core".into());
        let mut mysql = local_profile(Uuid::from_u128(2), "mysql", Engine::Mysql);
        mysql.schema_group = Some(" core ".into());

        assert!(validate_schema_group_engine(&postgres, &[mysql]).is_err());
        let mut sibling = local_profile(Uuid::from_u128(3), "sibling", Engine::Postgres);
        sibling.schema_group = Some("CORE".into());
        assert!(validate_schema_group_engine(&postgres, &[sibling]).is_ok());
    }

    #[test]
    fn agent_summary_serializes_only_the_allowlisted_shape() {
        let value = serde_json::to_value(AgentConnectionSummary::from(&profiles()[0])).unwrap();

        assert_eq!(
            value,
            json!({
                "id": ALPHA_ID,
                "name": "alpha",
                "engine": "postgres",
                "database": "analytics",
                "environment": "prod",
                "readonly": true,
                "allowWrites": false,
            })
        );
        assert_eq!(value.as_object().map(serde_json::Map::len), Some(7));

        let serialized = value.to_string();
        for forbidden in [
            "secret-host",
            "secret-user",
            "secret-driver",
            "secret-param",
            "secret-value",
            "secret-reference",
            "secret-schema-group",
            "provider",
            "driverId",
            "host",
            "port",
            "username",
            "extraParams",
            "secretRef",
            "workspaceAccess",
            "credentialMode",
        ] {
            assert!(
                !serialized.contains(forbidden),
                "agent summary leaked {forbidden}"
            );
        }
    }

    #[test]
    fn legacy_none_selects_the_first_name_ordered_profile() {
        let resolved = resolve_legacy_profiles(&profiles(), None).unwrap();
        assert_eq!(resolved.id, Uuid::parse_str(ALPHA_ID).unwrap());
    }

    #[test]
    fn legacy_exact_name_and_id_selectors_preserve_find_semantics() {
        let by_name = resolve_legacy_profiles(&profiles(), Some("beta")).unwrap();
        let by_id = resolve_legacy_profiles(&profiles(), Some(ALPHA_ID)).unwrap();

        assert_eq!(by_name.id, Uuid::parse_str(BETA_ID).unwrap());
        assert_eq!(by_id.name, "alpha");
        assert_eq!(
            resolve_legacy_profiles(&profiles(), Some("Beta")),
            Err(LegacyConnectionResolutionError::NoMatch {
                selector: "Beta".into(),
            })
        );
    }

    #[test]
    fn legacy_missing_selectors_distinguish_none_from_no_match() {
        assert_eq!(
            resolve_legacy_profiles(&[], None),
            Err(LegacyConnectionResolutionError::NoConnections)
        );
        assert_eq!(
            resolve_legacy_profiles(&[], Some("missing")),
            Err(LegacyConnectionResolutionError::NoMatch {
                selector: "missing".into(),
            })
        );
        assert_eq!(
            resolve_legacy_profiles(&profiles(), Some("missing")),
            Err(LegacyConnectionResolutionError::NoMatch {
                selector: "missing".into(),
            })
        );
    }

    #[test]
    fn cli_selector_never_picks_the_first_duplicate_name() {
        let alpha = AgentConnectionSummary::from(&profile(ALPHA_ID, "duplicate"));
        let beta = AgentConnectionSummary::from(&profile(BETA_ID, "duplicate"));
        let summaries = vec![beta.clone(), alpha.clone()];

        assert_eq!(
            resolve_cli_summaries(
                &summaries,
                &ConnectionSelector::Current,
                Uuid::parse_str(ALPHA_ID).unwrap(),
            ),
            Ok(alpha.clone())
        );
        assert_eq!(
            resolve_cli_summaries(
                &summaries,
                &ConnectionSelector::Id(beta.id),
                Uuid::parse_str(ALPHA_ID).unwrap(),
            ),
            Ok(beta)
        );
        assert_eq!(
            resolve_cli_summaries(
                &summaries,
                &ConnectionSelector::Name("duplicate".into()),
                Uuid::parse_str(ALPHA_ID).unwrap(),
            ),
            Err(CliConnectionResolutionError::Ambiguous {
                candidates: vec![
                    AgentConnectionSummary::from(&profile(ALPHA_ID, "duplicate")),
                    AgentConnectionSummary::from(&profile(BETA_ID, "duplicate")),
                ],
            })
        );
    }

    #[tokio::test]
    async fn terminal_list_does_not_disclose_sibling_connections() {
        let (service, _store, _credentials) = harness().await;
        let pinned_id = Uuid::new_v4();
        let sibling_id = Uuid::new_v4();
        for profile in [
            local_profile(pinned_id, "pinned", Engine::Sqlite),
            local_profile(sibling_id, "sibling", Engine::Sqlite),
        ] {
            service
                .upsert(ConnectionUpsertRequest {
                    profile,
                    password: None,
                })
                .await
                .unwrap();
        }
        let context = service
            .connections
            .pin(pinned_id, ConnectionAccess::Read)
            .await
            .unwrap();
        let pin = context.pin();
        let authority = TerminalAuthority {
            terminal_session_id: Uuid::new_v4(),
            workspace_id: pin.scope.workspace_id,
            account_scope: pin.scope.account_scope.storage_key().into(),
            connection_id: pin.connection_id,
            connection_revision: pin.connection_revision,
            client_protocol_version: dopedb_protocol::PROTOCOL_MAX,
        };

        let listed = service.list_terminal_summaries(&authority).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, pinned_id);
        assert!(listed.iter().all(|summary| summary.id != sibling_id));
    }

    #[tokio::test]
    async fn upsert_normalizes_metadata_and_never_trusts_an_ipc_secret_reference() {
        let (service, store, credentials) = harness().await;
        let id = Uuid::new_v4();
        let mut draft = local_profile(id, "local", Engine::Sqlite);
        draft.schema_group = Some("  Core  ".into());
        draft.secret_ref = Some(Uuid::new_v4().to_string());

        let saved = service
            .upsert(ConnectionUpsertRequest {
                profile: draft,
                password: None,
            })
            .await
            .unwrap();

        assert_eq!(saved.schema_group.as_deref(), Some("Core"));
        assert_eq!(saved.secret_ref, None);
        assert!(credentials.snapshot().is_empty());
        let persisted = store.get_connection(id).await.unwrap();
        assert_eq!(persisted.schema_group.as_deref(), Some("Core"));
        assert_eq!(persisted.secret_ref, None);
        store.pool().close().await;
    }

    #[tokio::test]
    async fn credential_rotation_commits_the_new_pointer_before_retiring_old_material() {
        let (service, store, credentials) = harness().await;
        let id = Uuid::new_v4();
        let saved = service
            .upsert(ConnectionUpsertRequest {
                profile: local_profile(id, "local", Engine::Sqlite),
                password: Some(Zeroizing::new("old-secret".into())),
            })
            .await
            .unwrap();
        let old_id = Uuid::parse_str(saved.secret_ref.as_deref().unwrap()).unwrap();
        assert_eq!(
            credentials.snapshot().get(&old_id).map(String::as_str),
            Some("old-secret")
        );

        let mut edited = saved;
        edited.secret_ref = Some(Uuid::new_v4().to_string());
        let rotated = service
            .upsert(ConnectionUpsertRequest {
                profile: edited,
                password: Some(Zeroizing::new("new-secret".into())),
            })
            .await
            .unwrap();
        let new_id = Uuid::parse_str(rotated.secret_ref.as_deref().unwrap()).unwrap();
        let snapshot = credentials.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(
            snapshot.get(&new_id).map(String::as_str),
            Some("new-secret")
        );
        assert!(!snapshot.contains_key(&old_id));
        assert_eq!(
            store
                .get_connection(id)
                .await
                .unwrap()
                .secret_ref
                .as_deref(),
            Some(new_id.to_string().as_str())
        );

        service.delete(id).await.unwrap();
        assert!(credentials.snapshot().is_empty());
        assert!(matches!(
            store.get_connection(id).await,
            Err(AppError::NotFound(message)) if message == format!("connection {id}")
        ));
        store.pool().close().await;
    }

    #[tokio::test]
    async fn a_failed_profile_commit_removes_the_unpublished_replacement_secret() {
        let (service, store, credentials) = harness().await;
        let mut incompatible = local_profile(Uuid::new_v4(), "invalid", Engine::Sqlite);
        incompatible.credential_mode = WorkspaceCredentialMode::MemberLocal;

        let error = service
            .upsert(ConnectionUpsertRequest {
                profile: incompatible,
                password: Some(Zeroizing::new("must-be-removed".into())),
            })
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            AppError::Config(message)
                if message == "local connections must use local credential mode"
        ));
        assert!(credentials.snapshot().is_empty());
        assert!(service.list_profiles().await.unwrap().is_empty());
        store.pool().close().await;
    }

    #[tokio::test]
    async fn schema_group_batch_deduplicates_ids_and_rejects_mixed_engines_atomically() {
        let (service, store, _) = harness().await;
        let alpha_id = Uuid::new_v4();
        let beta_id = Uuid::new_v4();
        let mysql_id = Uuid::new_v4();
        for profile in [
            local_profile(alpha_id, "alpha", Engine::Sqlite),
            local_profile(beta_id, "beta", Engine::Sqlite),
            local_profile(mysql_id, "mysql", Engine::Mysql),
        ] {
            service
                .upsert(ConnectionUpsertRequest {
                    profile,
                    password: None,
                })
                .await
                .unwrap();
        }

        let updated = service
            .set_schema_group(vec![alpha_id, alpha_id, beta_id], Some("  Core  ".into()))
            .await
            .unwrap();
        assert_eq!(updated.len(), 2);
        assert!(updated
            .iter()
            .all(|profile| profile.schema_group.as_deref() == Some("Core")));

        let error = service
            .set_schema_group(vec![mysql_id], Some("core".into()))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AppError::Config(message)
                if message
                    == "schema group 'core' already contains a different database engine"
        ));
        assert_eq!(
            store.get_connection(mysql_id).await.unwrap().schema_group,
            None
        );
        store.pool().close().await;
    }

    #[tokio::test]
    async fn profile_test_rejects_shared_and_oversized_credentials_before_dialing() {
        let (service, store, _) = harness().await;
        let mut shared = local_profile(Uuid::new_v4(), "shared", Engine::Sqlite);
        shared.workspace_access = WorkspaceConnectionAccess::Read;
        shared.credential_mode = WorkspaceCredentialMode::MemberLocal;
        assert!(matches!(
            service
                .test_profile(ConnectionProfileTestRequest {
                    profile: shared,
                    password: None,
                })
                .await,
            Err(AppError::Blocked { reason })
                if reason == "shared connections must be tested through workspace authorization"
        ));

        let oversized = "x".repeat(MAX_CONNECTION_CREDENTIAL_BYTES + 1);
        assert!(matches!(
            service
                .test_profile(ConnectionProfileTestRequest {
                    profile: local_profile(Uuid::new_v4(), "local", Engine::Sqlite),
                    password: Some(Zeroizing::new(oversized)),
                })
                .await,
            Err(AppError::Config(message))
                if message == "connection credential exceeds the size limit"
        ));
        assert!(service.list_profiles().await.unwrap().is_empty());
        store.pool().close().await;
    }

    #[tokio::test]
    async fn saved_test_preserves_the_workspace_view_only_error() {
        let (service, store, _) = harness().await;
        let id = Uuid::new_v4();
        service
            .upsert(ConnectionUpsertRequest {
                profile: local_profile(id, "view-only", Engine::Sqlite),
                password: None,
            })
            .await
            .unwrap();
        sqlx::query(
            "UPDATE connections
             SET workspace_access = 'view', revision = revision + 1
             WHERE id = ?1",
        )
        .bind(id.to_string())
        .execute(store.pool())
        .await
        .unwrap();

        assert!(matches!(
            service.test(id).await,
            Err(AppError::Blocked { reason })
                if reason == "your workspace role cannot test this shared connection"
        ));
        store.pool().close().await;
    }
}
