//! Better Auth RFC 8628 device authorization for the desktop app. Network exchange
//! and credential persistence stay in Rust so Bearer sessions never cross into the
//! webview, logs, local SQLite, or frontend query caches.

use std::net::IpAddr;
use std::time::Duration;

use reqwest::{redirect::Policy, Client, Response, StatusCode, Url};
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::connection::keychain::{
    delete_legacy_workspace_session, delete_workspace_session, fetch_legacy_workspace_session,
    fetch_workspace_session, store_workspace_session,
};
use crate::error::{AppError, AppResult};
use crate::model::{
    ConnectionProfile, Provider, WorkspaceAuthUser, WorkspaceConnectionAccess,
    WorkspaceCredentialMode, WorkspaceDeviceAuthorization, WorkspaceLoginPoll,
    WorkspaceLoginPollStatus, WorkspaceRole,
};

const DEFAULT_CONTROL_PLANE_ORIGIN: &str = "https://app.dopedb.dev";
const DESKTOP_CLIENT_ID: &str = "dopedb-desktop";
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
const MANAGED_LEASE_CONTRACT: &str = "access-v1";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri_complete: String,
    expires_in: u64,
    interval: u64,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
}

#[derive(Debug, Deserialize)]
struct OAuthErrorResponse {
    error: Option<String>,
    error_description: Option<String>,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SessionResponse {
    user: WorkspaceAuthUser,
}

#[derive(Debug, Deserialize)]
struct WorkspacesResponse {
    workspaces: Vec<RemoteWorkspaceResponse>,
}

#[derive(Debug, Deserialize)]
struct RemoteWorkspaceResponse {
    id: String,
    name: String,
    role: Option<String>,
}

fn default_remote_credential_mode() -> String {
    "member_local".into()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteConnectionResponse {
    id: String,
    name: String,
    engine: String,
    provider: String,
    driver_id: Option<String>,
    host: String,
    port: u16,
    database: String,
    sslmode: String,
    readonly_default: bool,
    allow_writes: bool,
    env: Option<String>,
    schema_group: Option<String>,
    revision: i64,
    access_mode: String,
    #[serde(default = "default_remote_credential_mode")]
    credential_mode: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteConnectionsResponse {
    connections: Vec<RemoteConnectionResponse>,
}

#[derive(Debug, Deserialize)]
struct CreatedConnectionResponse {
    connection: RemoteConnectionResponse,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthorizedConnectionResponse {
    allowed: bool,
    action: String,
    access_mode: String,
    revision: i64,
}

#[derive(Debug, Deserialize)]
struct ManagedLeaseResponse {
    lease: RemoteManagedLease,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteManagedLease {
    id: String,
    provider: String,
    engine: String,
    host: String,
    port: u16,
    database: String,
    username: String,
    password: String,
    sslmode: String,
    tls_server_ca_pem: Option<String>,
    access_mode: String,
    expires_at: String,
}

pub(crate) struct ManagedConnectionLease {
    pub lease_id: Uuid,
    pub profile: ConnectionProfile,
    pub secret: Zeroizing<String>,
    pub valid_for: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RemoteConnectionAuthority {
    pub revision: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SharedConnectionRequest<'a> {
    name: &'a str,
    engine: &'a str,
    provider: &'a str,
    driver_id: &'a Option<String>,
    host: &'a str,
    port: u16,
    database: &'a str,
    sslmode: &'a str,
    readonly_default: bool,
    allow_writes: bool,
    env: &'a Option<String>,
    schema_group: &'a Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct RemoteWorkspace {
    pub id: Uuid,
    pub name: String,
    pub role: WorkspaceRole,
}

fn is_loopback_host(url: &Url) -> bool {
    url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    })
}

fn origin() -> AppResult<String> {
    let raw = std::env::var("DOPEDB_WORKSPACE_ORIGIN")
        .unwrap_or_else(|_| DEFAULT_CONTROL_PLANE_ORIGIN.to_string())
        .trim_end_matches('/')
        .to_string();
    let url = Url::parse(&raw)
        .map_err(|_| AppError::Config("workspace control-plane origin is invalid".into()))?;
    let local_debug_origin =
        cfg!(debug_assertions) && url.scheme() == "http" && is_loopback_host(&url);
    if (url.scheme() != "https" && !local_debug_origin)
        || url.username() != ""
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(AppError::Config(
            "workspace control-plane origin must be an HTTPS origin".into(),
        ));
    }
    Ok(raw)
}

/// Build the hosted workspace console URL from the same validated origin used by
/// the auth API. Keeping this in Rust prevents the webview from opening an
/// arbitrary origin while still honoring the localhost override in debug builds.
pub(crate) fn console_url(workspace_id: Option<Uuid>) -> AppResult<String> {
    let mut url = Url::parse(&origin()?)
        .map_err(|_| AppError::Config("workspace control-plane origin is invalid".into()))?;
    url.set_path("/settings");
    if let Some(workspace_id) = workspace_id {
        let workspace_id = workspace_id.to_string();
        url.query_pairs_mut()
            .append_pair("workspace", &workspace_id);
        url.set_fragment(Some(&format!("workspace-{workspace_id}")));
    } else {
        url.set_fragment(Some("workspaces"));
    }
    Ok(url.into())
}

fn client() -> AppResult<Client> {
    Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(Policy::none())
        .user_agent(concat!("DopeDB/", env!("CARGO_PKG_VERSION"), " desktop"))
        .build()
        .map_err(|error| AppError::Network(format!("could not create HTTP client: {error}")))
}

fn request_error(action: &str, error: reqwest::Error) -> AppError {
    AppError::Network(format!("{action} failed: {error}"))
}

fn valid_device_code(device_code: &str) -> bool {
    device_code.len() == 40 && device_code.chars().all(|char| char.is_ascii_alphanumeric())
}

async fn oauth_error(response: Response) -> AppError {
    let status = response.status();
    let body = response.json::<OAuthErrorResponse>().await.ok();
    let detail = body
        .as_ref()
        .and_then(|value| {
            value
                .error_description
                .as_deref()
                .or(value.message.as_deref())
        })
        .unwrap_or("the control plane rejected the request");
    AppError::Network(format!(
        "workspace authentication returned {status}: {detail}"
    ))
}

/// Start a single-use ten-minute device authorization request.
pub async fn begin_login() -> AppResult<WorkspaceDeviceAuthorization> {
    let origin = origin()?;
    let response = client()?
        .post(format!("{origin}/api/auth/device/code"))
        .json(&json!({ "client_id": DESKTOP_CLIENT_ID }))
        .send()
        .await
        .map_err(|error| request_error("starting workspace login", error))?;
    if !response.status().is_success() {
        return Err(oauth_error(response).await);
    }
    let value = response
        .json::<DeviceCodeResponse>()
        .await
        .map_err(|error| request_error("reading workspace login response", error))?;
    let expected_verification_prefix = format!("{origin}/auth/device?user_code=");
    if !valid_device_code(&value.device_code)
        || !value
            .verification_uri_complete
            .starts_with(&expected_verification_prefix)
        || !(1..=60).contains(&value.interval)
        || !(1..=3600).contains(&value.expires_in)
    {
        return Err(AppError::Network(
            "workspace login returned an invalid device authorization response".into(),
        ));
    }
    Ok(WorkspaceDeviceAuthorization {
        device_code: value.device_code,
        user_code: value.user_code,
        verification_uri_complete: value.verification_uri_complete,
        expires_in: value.expires_in,
        interval: value.interval,
    })
}

async fn session_for_token(token: &str) -> AppResult<Option<WorkspaceAuthUser>> {
    let origin = origin()?;
    let response = client()?
        .get(format!("{origin}/api/v1/session"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|error| request_error("checking workspace session", error))?;
    if response.status() == StatusCode::UNAUTHORIZED {
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err(oauth_error(response).await);
    }
    let session = response
        .json::<SessionResponse>()
        .await
        .map_err(|error| request_error("reading workspace session", error))?;
    if Uuid::parse_str(&session.user.id).is_err()
        || session.user.email.trim().is_empty()
        || session.user.email.len() > 320
        || session.user.display_name.trim().is_empty()
        || session.user.display_name.len() > 120
    {
        return Err(AppError::Network(
            "workspace session returned an invalid user identity".into(),
        ));
    }
    Ok(Some(session.user))
}

/// Validate one account-specific session already stored in the OS credential store.
pub async fn auth_user(user_id: &str) -> AppResult<Option<WorkspaceAuthUser>> {
    let Some(token) = fetch_workspace_session(user_id)?.map(Zeroizing::new) else {
        return Ok(None);
    };
    let user = session_for_token(token.as_str()).await?;
    if user.as_ref().map(|user| user.id.as_str()) != Some(user_id) {
        delete_workspace_session(user_id)?;
        return Ok(None);
    }
    Ok(user)
}

/// One-time upgrade from the old fixed credential item. The token is validated before
/// it is copied to its account-specific key, so corrupt legacy state is never retained.
pub async fn migrate_legacy_session() -> AppResult<Option<WorkspaceAuthUser>> {
    let Some(token) = fetch_legacy_workspace_session()?.map(Zeroizing::new) else {
        return Ok(None);
    };
    let user = session_for_token(token.as_str()).await?;
    if let Some(user) = user.as_ref() {
        store_workspace_session(&user.id, token.as_str())?;
    }
    delete_legacy_workspace_session()?;
    Ok(user)
}

/// Revoke the current Better Auth session when the control plane is reachable, then
/// always remove the native client's credential. Remote revocation is best-effort so
/// losing the network cannot trap someone in a locally signed-in desktop session.
pub async fn sign_out(user_id: &str) -> AppResult<()> {
    let token = fetch_workspace_session(user_id)?.map(Zeroizing::new);
    if let Some(token) = token.as_deref() {
        let remote_result = async {
            let origin = origin()?;
            let response = client()?
                .post(format!("{origin}/api/auth/sign-out"))
                .bearer_auth(token)
                .json(&json!({}))
                .send()
                .await
                .map_err(|error| request_error("revoking workspace session", error))?;
            if response.status().is_success() || response.status() == StatusCode::UNAUTHORIZED {
                Ok(())
            } else {
                Err(oauth_error(response).await)
            }
        }
        .await;
        if let Err(error) = remote_result {
            tracing::warn!(
                %error,
                "workspace session could not be revoked remotely; deleting local credential"
            );
        }
    }
    delete_workspace_session(user_id)
}

/// Fetch organization memberships for the stored Bearer session. Only identifiers
/// and display names enter the local store; Better Auth remains membership authority.
pub(crate) async fn remote_workspaces(user_id: &str) -> AppResult<Vec<RemoteWorkspace>> {
    let token = fetch_workspace_session(user_id)?
        .map(Zeroizing::new)
        .ok_or_else(|| {
            AppError::Config("workspace memberships require an authenticated session".into())
        })?;
    let origin = origin()?;
    let response = client()?
        .get(format!("{origin}/api/v1/workspaces"))
        .bearer_auth(token.as_str())
        .send()
        .await
        .map_err(|error| request_error("loading workspace memberships", error))?;
    if response.status() == StatusCode::UNAUTHORIZED {
        delete_workspace_session(user_id)?;
        return Err(AppError::Network(
            "workspace session is no longer active".into(),
        ));
    }
    if !response.status().is_success() {
        return Err(oauth_error(response).await);
    }
    let payload = response
        .json::<WorkspacesResponse>()
        .await
        .map_err(|error| request_error("reading workspace memberships", error))?;
    let mut workspaces = Vec::with_capacity(payload.workspaces.len());
    for workspace in payload.workspaces {
        let id = Uuid::parse_str(&workspace.id)
            .map_err(|_| AppError::Network("workspace membership returned an invalid id".into()))?;
        let name = workspace.name.trim().to_string();
        if name.is_empty() || name.len() > 120 {
            return Err(AppError::Network(
                "workspace membership returned an invalid name".into(),
            ));
        }
        let role = match workspace.role.as_deref().unwrap_or("viewer") {
            "viewer" => WorkspaceRole::Viewer,
            "analyst" => WorkspaceRole::Analyst,
            "editor" => WorkspaceRole::Editor,
            "admin" => WorkspaceRole::Admin,
            "owner" => WorkspaceRole::Owner,
            _ => {
                return Err(AppError::Network(
                    "workspace membership returned an invalid role".into(),
                ))
            }
        };
        workspaces.push(RemoteWorkspace { id, name, role });
    }
    Ok(workspaces)
}

fn remote_connection(value: RemoteConnectionResponse) -> AppResult<(ConnectionProfile, i64)> {
    let id = Uuid::parse_str(&value.id)
        .map_err(|_| AppError::Network("shared connection returned an invalid id".into()))?;
    if value.name.trim().is_empty() || value.name.len() > 120 || value.host.len() > 512 {
        return Err(AppError::Network(
            "shared connection returned invalid metadata".into(),
        ));
    }
    let access = crate::store::parse_workspace_access(value.access_mode)?;
    if matches!(access, WorkspaceConnectionAccess::Local) {
        return Err(AppError::Network(
            "shared connection returned invalid access".into(),
        ));
    }
    let credential_mode = crate::store::parse_credential_mode(value.credential_mode)?;
    if credential_mode == WorkspaceCredentialMode::Local {
        return Err(AppError::Network(
            "shared connection returned invalid credential mode".into(),
        ));
    }
    let revision = value.revision;
    if revision < 1 {
        return Err(AppError::Network(
            "shared connection returned invalid revision".into(),
        ));
    }
    Ok((
        ConnectionProfile {
            id,
            name: value.name,
            engine: crate::store::parse_engine(value.engine)?,
            provider: crate::store::parse_provider(value.provider)?,
            driver_id: value.driver_id,
            host: value.host,
            port: value.port,
            database: value.database,
            // Managed usernames and passwords are supplied only by the lease route.
            username: String::new(),
            sslmode: value.sslmode,
            extra_params: Default::default(),
            readonly_default: value.readonly_default,
            allow_writes: value.allow_writes && access.can_write(),
            secret_ref: None,
            env: value.env,
            schema_group: value.schema_group,
            workspace_access: access,
            credential_mode,
        },
        revision,
    ))
}

/// Fetch redacted shared templates for a workspace using the OS-stored session.
pub(crate) async fn remote_connections(
    user_id: &str,
    workspace_id: Uuid,
) -> AppResult<Option<Vec<(ConnectionProfile, i64)>>> {
    let token = fetch_workspace_session(user_id)?
        .map(Zeroizing::new)
        .ok_or_else(|| {
            AppError::Config("shared connections require an authenticated session".into())
        })?;
    let origin = origin()?;
    let response = client()?
        .get(format!(
            "{origin}/api/v1/workspaces/{workspace_id}/connections"
        ))
        .bearer_auth(token.as_str())
        .send()
        .await
        .map_err(|error| request_error("loading shared connections", error))?;
    // An updated desktop can briefly reach the previous control-plane deployment.
    // Preserve the local cache instead of interpreting a missing route as no data.
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if response.status() == StatusCode::UNAUTHORIZED {
        delete_workspace_session(user_id)?;
    }
    if !response.status().is_success() {
        return Err(oauth_error(response).await);
    }
    let connections = response
        .json::<RemoteConnectionsResponse>()
        .await
        .map_err(|error| request_error("reading shared connections", error))?
        .connections
        .into_iter()
        .map(remote_connection)
        .collect::<AppResult<Vec<_>>>()?;
    Ok(Some(connections))
}

/// Publish only the non-secret portion of a local connection. The request type has
/// no credential fields, making accidental serialization of `secret_ref` impossible.
pub(crate) async fn share_connection(
    user_id: &str,
    workspace_id: Uuid,
    profile: &ConnectionProfile,
) -> AppResult<(ConnectionProfile, i64)> {
    let token = fetch_workspace_session(user_id)?
        .map(Zeroizing::new)
        .ok_or_else(|| {
            AppError::Config("sharing a connection requires an authenticated session".into())
        })?;
    let request = SharedConnectionRequest {
        name: &profile.name,
        engine: crate::store::engine_str(profile.engine),
        provider: crate::store::provider_str(profile.provider),
        driver_id: &profile.driver_id,
        host: &profile.host,
        port: profile.port,
        database: &profile.database,
        sslmode: &profile.sslmode,
        readonly_default: profile.readonly_default,
        allow_writes: profile.allow_writes,
        env: &profile.env,
        schema_group: &profile.schema_group,
    };
    let origin = origin()?;
    let response = client()?
        .post(format!(
            "{origin}/api/v1/workspaces/{workspace_id}/connections"
        ))
        .bearer_auth(token.as_str())
        .json(&request)
        .send()
        .await
        .map_err(|error| request_error("sharing connection", error))?;
    if response.status() == StatusCode::UNAUTHORIZED {
        delete_workspace_session(user_id)?;
    }
    if !response.status().is_success() {
        return Err(oauth_error(response).await);
    }
    remote_connection(
        response
            .json::<CreatedConnectionResponse>()
            .await
            .map_err(|error| request_error("reading shared connection", error))?
            .connection,
    )
}

/// Roll back a newly shared template when a later local credential/cache step fails.
/// The server performs the same workspace and RBAC checks as every other mutation.
pub(crate) async fn delete_connection(
    user_id: &str,
    workspace_id: Uuid,
    connection_id: Uuid,
) -> AppResult<()> {
    let token = fetch_workspace_session(user_id)?
        .map(Zeroizing::new)
        .ok_or_else(|| {
            AppError::Config(
                "deleting a shared connection requires an authenticated session".into(),
            )
        })?;
    let origin = origin()?;
    let response = client()?
        .delete(format!(
            "{origin}/api/v1/workspaces/{workspace_id}/connections/{connection_id}"
        ))
        .bearer_auth(token.as_str())
        .send()
        .await
        .map_err(|error| request_error("deleting shared connection", error))?;
    if response.status() == StatusCode::UNAUTHORIZED {
        delete_workspace_session(user_id)?;
    }
    if response.status().is_success() || response.status() == StatusCode::NOT_FOUND {
        return Ok(());
    }
    Err(oauth_error(response).await)
}

/// Revalidate a shared connection action against the current Better Auth session,
/// membership, role, and resource scope immediately before local DB access.
pub(crate) async fn authorize_connection(
    user_id: &str,
    workspace_id: Uuid,
    connection_id: Uuid,
    write: bool,
) -> AppResult<RemoteConnectionAuthority> {
    let token = fetch_workspace_session(user_id)?
        .map(Zeroizing::new)
        .ok_or_else(|| {
            AppError::Config("shared connection access requires an authenticated session".into())
        })?;
    let origin = origin()?;
    let response = client()?
        .post(format!(
            "{origin}/api/v1/workspaces/{workspace_id}/connections/{connection_id}"
        ))
        .bearer_auth(token.as_str())
        .json(&json!({ "action": if write { "write" } else { "read" } }))
        .send()
        .await
        .map_err(|error| request_error("authorizing shared connection", error))?;
    if response.status() == StatusCode::UNAUTHORIZED {
        delete_workspace_session(user_id)?;
    }
    if !response.status().is_success() {
        return Err(oauth_error(response).await);
    }
    let authority = response
        .json::<AuthorizedConnectionResponse>()
        .await
        .map_err(|error| request_error("reading shared connection authorization", error))?;
    let expected_action = if write { "write" } else { "read" };
    let access = crate::store::parse_workspace_access(authority.access_mode)?;
    if !authority.allowed
        || authority.action != expected_action
        || authority.revision < 1
        || access == WorkspaceConnectionAccess::Local
        || (write && !access.can_write())
        || (!write && !access.can_read())
    {
        return Err(AppError::Network(
            "shared connection authorization returned invalid authority".into(),
        ));
    }
    Ok(RemoteConnectionAuthority {
        revision: authority.revision,
    })
}

/// Obtain one provider-issued database credential for a managed shared connection.
/// The response password is moved into a zeroizing buffer immediately and never
/// touches the local store; the driver may retain it only inside the lease-bound pool.
pub(crate) async fn issue_managed_connection_lease(
    user_id: &str,
    workspace_id: Uuid,
    profile: &ConnectionProfile,
    write: bool,
) -> AppResult<ManagedConnectionLease> {
    if profile.credential_mode != WorkspaceCredentialMode::Managed {
        return Err(AppError::Config(
            "managed credentials were requested for a local binding".into(),
        ));
    }
    let token = fetch_workspace_session(user_id)?
        .map(Zeroizing::new)
        .ok_or_else(|| {
            AppError::Config("managed database access requires an authenticated session".into())
        })?;
    let origin = origin()?;
    let requested_access = if write { "write" } else { "read" };
    let response = client()?
        .post(format!(
            "{origin}/api/v1/workspaces/{workspace_id}/connections/{}/lease",
            profile.id
        ))
        .bearer_auth(token.as_str())
        .header("x-dopedb-managed-lease-contract", MANAGED_LEASE_CONTRACT)
        .json(&json!({ "accessMode": requested_access }))
        .send()
        .await
        .map_err(|error| request_error("requesting managed database access", error))?;
    if response.status() == StatusCode::UNAUTHORIZED {
        delete_workspace_session(user_id)?;
    }
    if !response.status().is_success() {
        return Err(oauth_error(response).await);
    }
    let mut lease = response
        .json::<ManagedLeaseResponse>()
        .await
        .map_err(|error| request_error("reading managed database access", error))?
        .lease;
    let secret = Zeroizing::new(std::mem::take(&mut lease.password));
    let lease_id = Uuid::parse_str(&lease.id)
        .map_err(|_| AppError::Network("managed database access returned an invalid id".into()))?;
    let provider = crate::store::parse_provider(lease.provider)?;
    let engine = crate::store::parse_engine(lease.engine)?;
    let valid_provider_tls = match provider {
        Provider::Neon | Provider::PlanetScale => {
            lease.sslmode == "verify-full" && lease.tls_server_ca_pem.is_none()
        }
        Provider::GcpCloudSql => {
            matches!(lease.sslmode.as_str(), "verify-ca" | "verify-full")
                && lease.tls_server_ca_pem.as_ref().is_some_and(|pem| {
                    pem.len() <= 64 * 1024
                        && pem.starts_with("-----BEGIN CERTIFICATE-----")
                        && pem.trim_end().ends_with("-----END CERTIFICATE-----")
                        && !pem.contains('\0')
                })
        }
        Provider::Auto | Provider::Generic => false,
    };
    if engine != profile.engine
        || provider != profile.provider
        || lease.host.is_empty()
        || lease.host.len() > 512
        || lease.host.contains("://")
        || lease.host.chars().any(char::is_whitespace)
        || lease.port == 0
        || lease.database.is_empty()
        || lease.database.len() > 512
        || lease.username.is_empty()
        || lease.username.len() > 512
        || secret.is_empty()
        || secret.len() > (1 << 16)
        || !valid_provider_tls
        || lease.access_mode != requested_access
        || (write && !profile.workspace_access.can_write())
    {
        return Err(AppError::Network(
            "managed database access returned invalid connection material".into(),
        ));
    }
    let expires_at = chrono::DateTime::parse_from_rfc3339(&lease.expires_at)
        .map_err(|_| AppError::Network("managed database access returned invalid expiry".into()))?
        .with_timezone(&chrono::Utc);
    let valid_seconds = expires_at
        .signed_duration_since(chrono::Utc::now())
        .num_seconds();
    if !(30..=20 * 60).contains(&valid_seconds) {
        return Err(AppError::Network(
            "managed database access returned an unsafe expiry".into(),
        ));
    }
    let mut leased_profile = profile.clone();
    leased_profile.provider = provider;
    leased_profile.host = lease.host;
    leased_profile.port = lease.port;
    leased_profile.database = lease.database;
    leased_profile.username = lease.username;
    leased_profile.sslmode = lease.sslmode;
    leased_profile.extra_params.clear();
    if let Some(ca) = lease.tls_server_ca_pem {
        leased_profile
            .extra_params
            .insert("sslrootcert_pem".into(), ca);
    }
    leased_profile.secret_ref = None;
    tracing::debug!(
        connection_id = %profile.id,
        lease_id = %lease_id,
        valid_seconds,
        "opened short-lived managed database access"
    );
    Ok(ManagedConnectionLease {
        lease_id,
        profile: leased_profile,
        secret,
        valid_for: Duration::from_secs(valid_seconds as u64),
    })
}

/// Release a provider credential before its natural expiry when the owning desktop
/// pool is retired. Failure is surfaced to the caller for logging, but never blocks
/// local pool closure.
pub(crate) async fn release_managed_connection_lease(
    user_id: &str,
    workspace_id: Uuid,
    connection_id: Uuid,
    lease_id: Uuid,
) -> AppResult<()> {
    let Some(token) = fetch_workspace_session(user_id)?.map(Zeroizing::new) else {
        return Ok(());
    };
    let origin = origin()?;
    let response = client()?
        .delete(format!(
            "{origin}/api/v1/workspaces/{workspace_id}/connections/{connection_id}/lease"
        ))
        .bearer_auth(token.as_str())
        .json(&json!({ "leaseId": lease_id }))
        .send()
        .await
        .map_err(|error| request_error("releasing managed database access", error))?;
    if response.status() == StatusCode::UNAUTHORIZED {
        delete_workspace_session(user_id)?;
    }
    if !response.status().is_success() {
        return Err(oauth_error(response).await);
    }
    Ok(())
}

/// Poll once at the server-provided interval. A successful token is validated and
/// committed directly to the OS credential store before signed-in state is returned.
pub async fn poll_login(device_code: &str) -> AppResult<WorkspaceLoginPoll> {
    if !valid_device_code(device_code) {
        return Err(AppError::Config("invalid workspace device code".into()));
    }
    let origin = origin()?;
    let response = client()?
        .post(format!("{origin}/api/auth/device/token"))
        .json(&json!({
            "grant_type": DEVICE_GRANT,
            "device_code": device_code,
            "client_id": DESKTOP_CLIENT_ID,
        }))
        .send()
        .await
        .map_err(|error| request_error("polling workspace login", error))?;

    if response.status().is_success() {
        let token = Zeroizing::new(
            response
                .json::<TokenResponse>()
                .await
                .map_err(|error| request_error("reading workspace session token", error))?
                .access_token,
        );
        if token.len() < 20 || token.len() > 4096 || token.chars().any(char::is_whitespace) {
            return Err(AppError::Network(
                "workspace login returned an invalid session token".into(),
            ));
        }
        let user = session_for_token(token.as_str()).await?.ok_or_else(|| {
            AppError::Network("workspace login returned an inactive session".into())
        })?;
        store_workspace_session(&user.id, token.as_str())?;
        return Ok(WorkspaceLoginPoll {
            status: WorkspaceLoginPollStatus::SignedIn,
            user: Some(user),
        });
    }

    let status = response.status();
    let body = response
        .json::<OAuthErrorResponse>()
        .await
        .map_err(|error| request_error("reading workspace login status", error))?;
    let poll_status = match body.error.as_deref() {
        Some("authorization_pending") => WorkspaceLoginPollStatus::Pending,
        Some("slow_down") => WorkspaceLoginPollStatus::SlowDown,
        Some("access_denied") => WorkspaceLoginPollStatus::Denied,
        Some("expired_token") | Some("invalid_grant") => WorkspaceLoginPollStatus::Expired,
        _ => {
            let detail = body
                .error_description
                .or(body.message)
                .unwrap_or_else(|| "the control plane rejected the request".into());
            return Err(AppError::Network(format!(
                "workspace login returned {status}: {detail}"
            )));
        }
    };
    Ok(WorkspaceLoginPoll {
        status: poll_status,
        user: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_origin_is_https() {
        assert!(DEFAULT_CONTROL_PLANE_ORIGIN.starts_with("https://"));
        assert_eq!(origin().unwrap(), DEFAULT_CONTROL_PLANE_ORIGIN);
    }

    #[test]
    fn device_code_validation_rejects_untrusted_input() {
        assert!(!valid_device_code("../../not-a-device-code"));
        assert!(valid_device_code(
            "aB3dE5gH7jK9mN2pQ4rS6tU8vW0xY1zA3bC5dE7f"
        ));
    }

    #[test]
    fn loopback_detection_accepts_ipv4_ipv6_and_localhost() {
        for origin in [
            "http://localhost:3000",
            "http://127.0.0.1:3000",
            "http://[::1]:3000",
        ] {
            assert!(is_loopback_host(&Url::parse(origin).unwrap()));
        }
        assert!(!is_loopback_host(
            &Url::parse("https://app.dopedb.dev").unwrap()
        ));
    }

    #[test]
    fn console_url_targets_the_requested_workspace() {
        let workspace_id = Uuid::parse_str("019bf6c8-2d35-7ba1-89bf-b4698600478c").unwrap();
        let url = Url::parse(&console_url(Some(workspace_id)).unwrap()).unwrap();

        assert_eq!(url.path(), "/settings");
        assert_eq!(
            url.query_pairs()
                .find(|(key, _)| key == "workspace")
                .unwrap()
                .1,
            workspace_id.to_string()
        );
        assert_eq!(
            url.fragment(),
            Some("workspace-019bf6c8-2d35-7ba1-89bf-b4698600478c")
        );
    }

    #[test]
    fn console_url_without_a_team_targets_the_workspace_list() {
        let url = Url::parse(&console_url(None).unwrap()).unwrap();

        assert_eq!(url.path(), "/settings");
        assert!(url.query().is_none());
        assert_eq!(url.fragment(), Some("workspaces"));
    }
}
