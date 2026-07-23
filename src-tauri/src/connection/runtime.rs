//! Shared authorization and credential materialization for UI, introspection, and MCP.
//! Managed provider passwords are never persisted; the driver retains only the
//! in-process material required to reconnect until the lease-bound pool is evicted.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use uuid::Uuid;
use zeroize::Zeroizing;

use crate::error::{AppError, AppResult};
use crate::model::{ConnectionProfile, WorkspaceConnectionAccess, WorkspaceCredentialMode};
use crate::store::Store;

use super::Live;

static NEXT_CACHE_GENERATION: AtomicU64 = AtomicU64::new(1);
static CACHE_GENERATIONS: OnceLock<Mutex<HashMap<Uuid, u64>>> = OnceLock::new();

pub struct ConnectionAuthorization {
    user_id: Option<String>,
    workspace_id: Option<Uuid>,
}

pub struct OpenedLive {
    pub live: Live,
    evict_after: Option<Duration>,
}

/// Revalidate current membership before any cached or newly opened pool is used.
pub async fn authorize_profile(
    store: &Store,
    profile: &ConnectionProfile,
    write: bool,
) -> AppResult<ConnectionAuthorization> {
    if !profile.workspace_access.can_read()
        || (write && (!profile.workspace_access.can_write() || !profile.allow_writes))
    {
        return Err(AppError::Blocked {
            reason: "your workspace role does not permit this database action".into(),
        });
    }
    if profile.workspace_access == WorkspaceConnectionAccess::Local {
        return Ok(ConnectionAuthorization {
            user_id: None,
            workspace_id: None,
        });
    }
    let user_id = store.active_workspace_account_id().await?.ok_or_else(|| {
        AppError::Config("shared connection access requires an active workspace account".into())
    })?;
    let workspace_id = store.active_workspace_id().await?;
    crate::workspace_auth::authorize_connection(&user_id, workspace_id, profile.id, write).await?;
    Ok(ConnectionAuthorization {
        user_id: Some(user_id),
        workspace_id: Some(workspace_id),
    })
}

/// Open a pool using either an OS credential reference or a short-lived provider lease.
pub async fn connect_authorized(
    profile: &ConnectionProfile,
    authorization: &ConnectionAuthorization,
) -> AppResult<OpenedLive> {
    if profile.credential_mode == WorkspaceCredentialMode::Managed {
        let user_id = authorization.user_id.as_deref().ok_or_else(|| {
            AppError::Config("managed database access requires a workspace account".into())
        })?;
        let workspace_id = authorization.workspace_id.ok_or_else(|| {
            AppError::Config("managed database access requires a team workspace".into())
        })?;
        let lease =
            crate::workspace_auth::issue_managed_connection_lease(user_id, workspace_id, profile)
                .await?;
        let live = crate::driver::connect(&lease.profile, lease.secret.as_str()).await?;
        return Ok(OpenedLive {
            live,
            evict_after: Some(
                lease
                    .valid_for
                    .saturating_sub(Duration::from_secs(30))
                    .max(Duration::from_secs(1)),
            ),
        });
    }

    let secret = Zeroizing::new(super::fetch_profile_secret(profile)?);
    Ok(OpenedLive {
        live: crate::driver::connect(profile, secret.as_str()).await?,
        evict_after: None,
    })
}

/// Cache a live pool and ensure managed pools are dropped before their credential TTL.
pub fn cache_opened(
    connections: &Arc<Mutex<HashMap<Uuid, Live>>>,
    id: Uuid,
    opened: OpenedLive,
) -> Live {
    let handle = opened.live.clone();
    let generation = opened
        .evict_after
        .map(|_| NEXT_CACHE_GENERATION.fetch_add(1, Ordering::Relaxed));
    {
        let mut generations = CACHE_GENERATIONS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap();
        if let Some(generation) = generation {
            generations.insert(id, generation);
        } else {
            generations.remove(&id);
        }
        connections.lock().unwrap().insert(id, opened.live);
    }
    if let (Some(delay), Some(generation)) = (opened.evict_after, generation) {
        let connections = Arc::clone(connections);
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let mut generations = CACHE_GENERATIONS
                .get_or_init(|| Mutex::new(HashMap::new()))
                .lock()
                .unwrap();
            if generations.get(&id) == Some(&generation) {
                connections.lock().unwrap().remove(&id);
                generations.remove(&id);
            }
        });
    }
    handle
}
