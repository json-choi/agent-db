//! Scope-pinned authorization, single-flight pool creation, and managed-lease
//! retirement shared by UI, introspection, and agent transports.
//!
//! A connection UUID alone is never a cache identity. Every entry is keyed by the
//! exact workspace/account selection plus connection and binding revisions. A
//! `ConnectionLease` retains the scope read gate for the operation lifetime so the
//! current adapters cannot switch scope and then write history/cache into a different
//! account while their scoped-write APIs are being extracted.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use dashmap::DashMap;
use futures::future::join_all;
use tokio::sync::{Mutex, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};
use tokio::time::Instant;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::error::{AppError, AppResult};
use crate::model::{
    ConnectionProfile, Workspace, WorkspaceAuthUser, WorkspaceCredentialMode, WorkspaceRole,
};
use crate::store::{AccountScope, PinnedConnection, Store};

use super::Live;

const MANAGED_RELEASE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ConnectionAccess {
    Read,
    Write,
}

struct ConnectionAuthorization {
    user_id: Option<String>,
    workspace_id: Option<Uuid>,
}

struct OpenedLive {
    pub live: Live,
    retire_at: Option<Instant>,
    managed_lease: Option<ManagedLeaseHandle>,
}

#[derive(Clone)]
struct ManagedLeaseHandle {
    user_id: String,
    workspace_id: Uuid,
    connection_id: Uuid,
    lease_id: Uuid,
}

impl ManagedLeaseHandle {
    async fn release(self) {
        if let Err(error) = crate::workspace_auth::release_managed_connection_lease(
            &self.user_id,
            self.workspace_id,
            self.connection_id,
            self.lease_id,
        )
        .await
        {
            tracing::warn!(
                connection_id = %self.connection_id,
                %error,
                "managed database access release deferred until provider expiry"
            );
        }
    }
}

async fn release_managed_bounded(lease: ManagedLeaseHandle) {
    let connection_id = lease.connection_id;
    if tokio::time::timeout(MANAGED_RELEASE_TIMEOUT, lease.release())
        .await
        .is_err()
    {
        tracing::warn!(
            %connection_id,
            "managed database access release timed out; provider expiry remains authoritative"
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ConnectionCacheKey {
    workspace_id: Uuid,
    account_scope: AccountScope,
    scope_generation: i64,
    connection_id: Uuid,
    connection_revision: i64,
    binding_revision: i64,
    binding_updated_at: String,
    access: ConnectionAccess,
}

impl ConnectionCacheKey {
    fn new(pin: &PinnedConnection, access: ConnectionAccess) -> Self {
        let access = if pin.profile.credential_mode == WorkspaceCredentialMode::Managed {
            access
        } else {
            // Local/member-local Live values already contain distinct read-only and
            // read/write pools; splitting the outer cache would double sessions.
            ConnectionAccess::Read
        };
        Self {
            workspace_id: pin.scope.workspace_id,
            account_scope: pin.scope.account_scope.clone(),
            scope_generation: pin.scope.generation,
            connection_id: pin.connection_id,
            connection_revision: pin.connection_revision,
            binding_revision: pin.binding_revision,
            binding_updated_at: pin.binding_updated_at.clone(),
            access,
        }
    }
}

struct CacheEntry {
    live: Live,
    generation: u64,
    retire_at: Option<Instant>,
    managed_lease: StdMutex<Option<ManagedLeaseHandle>>,
}

impl CacheEntry {
    fn take_managed_lease(&self) -> Option<ManagedLeaseHandle> {
        self.managed_lease
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

impl Drop for CacheEntry {
    fn drop(&mut self) {
        let Some(lease) = self.take_managed_lease() else {
            return;
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(release_managed_bounded(lease));
        }
    }
}

#[derive(Default)]
struct ConnectionSlot {
    // Empty slots deliberately remain mapped. Removing a slot after releasing this
    // mutex can orphan a waiter that has already cloned the Arc and let a second slot
    // open a duplicate pool for the same authority key.
    entry: Option<Arc<CacheEntry>>,
}

struct ConnectionManagerInner {
    store: Store,
    scope_gate: Arc<RwLock<()>>,
    slots: DashMap<ConnectionCacheKey, Arc<Mutex<ConnectionSlot>>>,
    next_generation: AtomicU64,
}

/// Process-local owner of every database pool. Clones share the same slots and scope
/// gate, including the instances handed to the MCP listeners.
#[derive(Clone)]
pub(crate) struct ConnectionManager {
    inner: Arc<ConnectionManagerInner>,
}

/// An online-authorized connection identity without a database pool. Catalog
/// cache-first reads use this so RBAC is checked before a cache hit without opening
/// an unnecessary target connection.
pub(crate) struct ConnectionContext {
    manager: ConnectionManager,
    pin: PinnedConnection,
    access: ConnectionAccess,
    authorization: ConnectionAuthorization,
    scope_guard: Option<OwnedRwLockReadGuard<()>>,
}

/// A pool and its exact local authority snapshot. This type is intentionally not
/// Clone: adapters retain one lease for the complete operation.
pub(crate) struct ConnectionLease {
    pin: PinnedConnection,
    entry: Arc<CacheEntry>,
    _scope_guard: OwnedRwLockReadGuard<()>,
}

/// Local operation boundary used before a database pool is needed. It freezes the
/// active workspace/account while commands classify input, evaluate gates, and write
/// scoped artifacts, without issuing an unnecessary remote authorization request.
pub(crate) struct ConnectionOperationScope {
    manager: ConnectionManager,
    _scope_guard: OwnedRwLockReadGuard<()>,
}

/// Exclusive keychain/material mutation boundary. Existing operations drain before
/// this is created, and no new scope pins can begin until it is released.
pub(crate) struct ConnectionMutation {
    manager: ConnectionManager,
    pin: Option<PinnedConnection>,
    scope_guard: Option<OwnedRwLockWriteGuard<()>>,
}

impl ConnectionLease {
    pub(crate) fn live(&self) -> &Live {
        &self.entry.live
    }

    pub(crate) fn pin(&self) -> &PinnedConnection {
        &self.pin
    }
}

impl ConnectionOperationScope {
    pub(crate) async fn pin_connection(&self, id: Uuid) -> AppResult<PinnedConnection> {
        self.manager.inner.store.pin_connection_for_read(id).await
    }

    /// Upgrade this operation boundary into a live connection without reacquiring
    /// the writer-preferred scope lock. Re-entering `ConnectionManager::acquire`
    /// while this scope owns a read guard can deadlock behind a queued mutation.
    pub(crate) async fn connect(
        self,
        pin: PinnedConnection,
        access: ConnectionAccess,
    ) -> AppResult<ConnectionLease> {
        let authorization = authorize_pin(&pin, access).await?;
        if !self.manager.inner.store.is_pin_current(&pin).await? {
            return Err(scope_changed());
        }
        let Self {
            manager,
            _scope_guard,
        } = self;
        ConnectionContext {
            manager,
            pin,
            access,
            authorization,
            scope_guard: Some(_scope_guard),
        }
        .connect()
        .await
    }
}

impl ConnectionMutation {
    pub(crate) fn pin(&self) -> &PinnedConnection {
        self.pin
            .as_ref()
            .expect("connection mutation was created with an authority pin")
    }

    /// Publish a successful material change by detaching every cached pool for the
    /// resource before allowing new acquisitions.
    pub(crate) async fn retire_connection(self, connection_id: Uuid) {
        self.retire_connections(&[connection_id]).await;
    }

    /// Atomically publish a successful batch mutation while the exclusive scope
    /// gate keeps waiters from retaining a slot that is about to be detached.
    pub(crate) async fn retire_connections(mut self, connection_ids: &[Uuid]) {
        let keys = self
            .manager
            .inner
            .slots
            .iter()
            .filter(|entry| connection_ids.contains(&entry.key().connection_id))
            .map(|entry| entry.key().clone())
            .collect::<Vec<_>>();
        let retired = self.manager.detach_keys(keys).await;
        self.scope_guard.take();
        drop(self);
        retire_entries(retired).await;
    }
}

impl ConnectionContext {
    pub(crate) fn pin(&self) -> &PinnedConnection {
        &self.pin
    }

    pub(crate) async fn connect(mut self) -> AppResult<ConnectionLease> {
        let key = ConnectionCacheKey::new(&self.pin, self.access);
        let slot = self
            .manager
            .inner
            .slots
            .entry(key.clone())
            .or_insert_with(|| Arc::new(Mutex::new(ConnectionSlot::default())))
            .clone();

        loop {
            let mut state = slot.lock().await;
            if let Some(entry) = state.entry.as_ref() {
                let is_expired = cache_entry_expired(entry);
                if !is_expired {
                    let entry = Arc::clone(entry);
                    drop(state);
                    // `pin` authorized before potentially waiting on this slot. A
                    // revoke can occur while another task opens the pool, so authorize
                    // again at the exact cache-use boundary.
                    if self.pin.requires_remote_rbac {
                        authorize_pin(&self.pin, self.access).await?;
                    }
                    if !self.manager.inner.store.is_pin_current(&self.pin).await? {
                        return Err(scope_changed());
                    }
                    // Online authorization can outlive the retirement timer. Check
                    // again at the exact hand-off boundary and detach only this
                    // generation; never return a lease whose safety margin elapsed.
                    if cache_entry_expired(&entry) {
                        let retired = {
                            let mut state = slot.lock().await;
                            if state
                                .entry
                                .as_ref()
                                .is_some_and(|current| Arc::ptr_eq(current, &entry))
                            {
                                state.entry.take()
                            } else {
                                None
                            }
                        };
                        drop(entry);
                        if let Some(retired) = retired {
                            retire_entries(vec![retired]).await;
                        }
                        continue;
                    }
                    return Ok(ConnectionLease {
                        pin: self.pin,
                        entry,
                        _scope_guard: self
                            .scope_guard
                            .take()
                            .expect("connection context owns one scope guard"),
                    });
                }
            }

            let expired = state.entry.take();
            if expired.is_some() {
                drop(state);
                retire_entries(expired.into_iter().collect()).await;
                continue;
            }

            let opened =
                connect_authorized(&self.pin.profile, &self.authorization, self.access).await;
            let opened = match opened {
                Ok(opened) => opened,
                Err(error) => {
                    drop(state);
                    return Err(error);
                }
            };
            if self.pin.requires_remote_rbac {
                if let Err(error) = authorize_pin(&self.pin, self.access).await {
                    drop(state);
                    retire_opened(opened).await;
                    return Err(error);
                }
            }
            match self.manager.inner.store.is_pin_current(&self.pin).await {
                Ok(true) => {}
                Ok(false) => {
                    drop(state);
                    retire_opened(opened).await;
                    return Err(scope_changed());
                }
                Err(error) => {
                    drop(state);
                    retire_opened(opened).await;
                    return Err(error);
                }
            }

            let generation = self
                .manager
                .inner
                .next_generation
                .fetch_add(1, Ordering::Relaxed);
            if opened
                .retire_at
                .is_some_and(|retire_at| retire_at <= Instant::now())
            {
                drop(state);
                retire_opened(opened).await;
                return Err(AppError::Network(
                    "managed database access expired while opening the connection".into(),
                ));
            }
            let OpenedLive {
                live,
                retire_at,
                managed_lease,
            } = opened;
            let entry = Arc::new(CacheEntry {
                live,
                generation,
                retire_at,
                managed_lease: StdMutex::new(managed_lease),
            });
            state.entry = Some(Arc::clone(&entry));
            drop(state);
            if let Some(retire_at) = retire_at {
                schedule_expiry(
                    slot,
                    generation,
                    retire_at.saturating_duration_since(Instant::now()),
                );
            }
            return Ok(ConnectionLease {
                pin: self.pin,
                entry,
                _scope_guard: self
                    .scope_guard
                    .take()
                    .expect("connection context owns one scope guard"),
            });
        }
    }

    /// Open and close an uncached pool while retaining the exact scope pin for the
    /// complete reachability check. Connection-form tests intentionally do not warm
    /// the shared pool cache.
    pub(crate) async fn test_fresh(self) -> AppResult<()> {
        let opened =
            connect_authorized(&self.pin.profile, &self.authorization, self.access).await?;
        if self.pin.requires_remote_rbac {
            if let Err(error) = authorize_pin(&self.pin, self.access).await {
                retire_opened(opened).await;
                return Err(error);
            }
        }
        let pin_is_current = match self.manager.inner.store.is_pin_current(&self.pin).await {
            Ok(current) => current,
            Err(error) => {
                retire_opened(opened).await;
                return Err(error);
            }
        };
        if !pin_is_current
            || opened
                .retire_at
                .is_some_and(|retire_at| retire_at <= Instant::now())
        {
            retire_opened(opened).await;
            return Err(scope_changed());
        }
        let result = opened.live.test().await;
        retire_opened(opened).await;
        result
    }
}

impl ConnectionManager {
    pub(crate) fn new(store: Store) -> Self {
        Self {
            inner: Arc::new(ConnectionManagerInner {
                store,
                scope_gate: Arc::new(RwLock::new(())),
                slots: DashMap::new(),
                next_generation: AtomicU64::new(1),
            }),
        }
    }

    pub(crate) async fn pin(
        &self,
        id: Uuid,
        access: ConnectionAccess,
    ) -> AppResult<ConnectionContext> {
        let scope_guard = Arc::clone(&self.inner.scope_gate).read_owned().await;
        let pin = self.inner.store.pin_connection_for_read(id).await?;
        let authorization = authorize_pin(&pin, access).await?;
        if !self.inner.store.is_pin_current(&pin).await? {
            return Err(scope_changed());
        }
        Ok(ConnectionContext {
            manager: self.clone(),
            pin,
            access,
            authorization,
            scope_guard: Some(scope_guard),
        })
    }

    pub(crate) async fn acquire(
        &self,
        id: Uuid,
        access: ConnectionAccess,
    ) -> AppResult<ConnectionLease> {
        self.pin(id, access).await?.connect().await
    }

    pub(crate) async fn begin_operation_scope(&self) -> ConnectionOperationScope {
        ConnectionOperationScope {
            manager: self.clone(),
            _scope_guard: Arc::clone(&self.inner.scope_gate).read_owned().await,
        }
    }

    pub(crate) async fn begin_scope_mutation(&self) -> ConnectionMutation {
        ConnectionMutation {
            manager: self.clone(),
            pin: None,
            scope_guard: Some(Arc::clone(&self.inner.scope_gate).write_owned().await),
        }
    }

    pub(crate) async fn begin_connection_mutation(
        &self,
        id: Uuid,
        access: ConnectionAccess,
    ) -> AppResult<ConnectionMutation> {
        let scope_guard = Arc::clone(&self.inner.scope_gate).write_owned().await;
        let pin = self.inner.store.pin_connection_for_read(id).await?;
        authorize_pin(&pin, access).await?;
        if !self.inner.store.is_pin_current(&pin).await? {
            return Err(scope_changed());
        }
        Ok(ConnectionMutation {
            manager: self.clone(),
            pin: Some(pin),
            scope_guard: Some(scope_guard),
        })
    }

    pub(crate) async fn activate_workspace(
        &self,
        id: Uuid,
        account_user_id: Option<&str>,
    ) -> AppResult<Workspace> {
        let _gate = self.inner.scope_gate.write().await;
        let workspace = self
            .inner
            .store
            .activate_workspace(id, account_user_id)
            .await?;
        let retired = self.detach_all().await;
        drop(_gate);
        retire_entries(retired).await;
        Ok(workspace)
    }

    pub(crate) async fn activate_workspace_account(&self, user_id: &str) -> AppResult<Workspace> {
        let _gate = self.inner.scope_gate.write().await;
        let workspace = self.inner.store.activate_workspace_account(user_id).await?;
        let retired = self.detach_all().await;
        drop(_gate);
        retire_entries(retired).await;
        Ok(workspace)
    }

    pub(crate) async fn remove_workspace_account(&self, user_id: &str) -> AppResult<()> {
        let _gate = self.inner.scope_gate.write().await;
        self.inner.store.remove_workspace_account(user_id).await?;
        let retired = self.detach_all().await;
        drop(_gate);
        retire_entries(retired).await;
        Ok(())
    }

    pub(crate) async fn sync_account_workspaces(
        &self,
        user: &WorkspaceAuthUser,
        workspaces: &[(Uuid, String, WorkspaceRole)],
    ) -> AppResult<()> {
        let _gate = self.inner.scope_gate.write().await;
        self.inner
            .store
            .sync_account_workspaces(user, workspaces)
            .await?;
        let retired = self.detach_all().await;
        drop(_gate);
        retire_entries(retired).await;
        Ok(())
    }

    /// Reconcile control-plane connection templates while excluding concurrent
    /// scope-pinned operations. Any material or binding revision change gets a fresh
    /// pool on the next acquisition.
    pub(crate) async fn sync_remote_connections(
        &self,
        workspace_id: Uuid,
        account_user_id: &str,
        connections: &[(ConnectionProfile, i64)],
    ) -> AppResult<Vec<Uuid>> {
        let gate = self.inner.scope_gate.write().await;
        let removed_credential_ids = self
            .inner
            .store
            .sync_remote_connections(workspace_id, account_user_id, connections)
            .await?;
        let retired = self.detach_all().await;
        drop(gate);
        retire_entries(retired).await;
        Ok(removed_credential_ids)
    }

    async fn detach_all(&self) -> Vec<Arc<CacheEntry>> {
        let keys = self
            .inner
            .slots
            .iter()
            .map(|entry| entry.key().clone())
            .collect::<Vec<_>>();
        self.detach_keys(keys).await
    }

    async fn detach_keys(&self, keys: Vec<ConnectionCacheKey>) -> Vec<Arc<CacheEntry>> {
        let mut retired = Vec::new();
        for key in keys {
            if let Some((_, slot)) = self.inner.slots.remove(&key) {
                if let Some(entry) = slot.lock().await.entry.take() {
                    retired.push(entry);
                }
            }
        }
        retired
    }
}

fn schedule_expiry(slot: Arc<Mutex<ConnectionSlot>>, generation: u64, delay: Duration) {
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        let expired = {
            let mut state = slot.lock().await;
            if state
                .entry
                .as_ref()
                .is_some_and(|entry| entry.generation == generation)
            {
                state.entry.take()
            } else {
                None
            }
        };
        if expired.is_some() {
            retire_entries(expired.into_iter().collect()).await;
        }
    });
}

fn cache_entry_expired(entry: &CacheEntry) -> bool {
    entry
        .retire_at
        .is_some_and(|retire_at| retire_at <= Instant::now())
}

async fn retire_entries(entries: Vec<Arc<CacheEntry>>) {
    let retirements = entries.into_iter().filter_map(|entry| {
        Arc::try_unwrap(entry).ok().map(|entry| async move {
            entry.live.close().await;
            if let Some(managed_lease) = entry.take_managed_lease() {
                release_managed_bounded(managed_lease).await;
            }
        })
    });
    if tokio::time::timeout(
        MANAGED_RELEASE_TIMEOUT + Duration::from_secs(1),
        join_all(retirements),
    )
    .await
    .is_err()
    {
        tracing::warn!(
            "connection retirement timed out; remaining pools and provider leases are dropping"
        );
    }
}

async fn retire_opened(opened: OpenedLive) {
    opened.live.close().await;
    if let Some(managed_lease) = opened.managed_lease {
        release_managed_bounded(managed_lease).await;
    }
}

fn scope_changed() -> AppError {
    AppError::Blocked {
        reason: "workspace or connection access changed; retry the operation".into(),
    }
}

async fn authorize_pin(
    pin: &PinnedConnection,
    access: ConnectionAccess,
) -> AppResult<ConnectionAuthorization> {
    let write = access == ConnectionAccess::Write;
    if !pin.profile.workspace_access.can_read()
        || (write && (!pin.profile.workspace_access.can_write() || !pin.profile.allow_writes))
    {
        return Err(AppError::Blocked {
            reason: "your workspace role does not permit this database action".into(),
        });
    }
    if !pin.requires_remote_rbac {
        return Ok(ConnectionAuthorization {
            user_id: None,
            workspace_id: None,
        });
    }
    let user_id = pin.scope.selected_account_id.clone().ok_or_else(|| {
        AppError::Config("shared connection access requires an active workspace account".into())
    })?;
    let authority = crate::workspace_auth::authorize_connection(
        &user_id,
        pin.scope.workspace_id,
        pin.connection_id,
        write,
    )
    .await?;
    if authority.revision != pin.connection_revision {
        return Err(AppError::Blocked {
            reason: "the shared connection changed; refresh the workspace and retry".into(),
        });
    }
    Ok(ConnectionAuthorization {
        user_id: Some(user_id),
        workspace_id: Some(pin.scope.workspace_id),
    })
}

/// Open a pool using either an OS credential reference or a short-lived provider lease.
async fn connect_authorized(
    profile: &ConnectionProfile,
    authorization: &ConnectionAuthorization,
    access: ConnectionAccess,
) -> AppResult<OpenedLive> {
    if profile.credential_mode == WorkspaceCredentialMode::Managed {
        let user_id = authorization.user_id.as_deref().ok_or_else(|| {
            AppError::Config("managed database access requires a workspace account".into())
        })?;
        let workspace_id = authorization.workspace_id.ok_or_else(|| {
            AppError::Config("managed database access requires a team workspace".into())
        })?;
        let lease = crate::workspace_auth::issue_managed_connection_lease(
            user_id,
            workspace_id,
            profile,
            access == ConnectionAccess::Write,
        )
        .await?;
        // Anchor retirement immediately after the HTTPS response, before a slow TLS
        // or database handshake can consume part of the provider credential's life.
        let retire_at = Instant::now()
            + lease
                .valid_for
                .saturating_sub(Duration::from_secs(30))
                .max(Duration::from_secs(1));
        let managed_lease = ManagedLeaseHandle {
            user_id: user_id.to_string(),
            workspace_id,
            connection_id: profile.id,
            lease_id: lease.lease_id,
        };
        let live = match crate::driver::connect(&lease.profile, lease.secret.as_str()).await {
            Ok(live) => live,
            Err(error) => {
                release_managed_bounded(managed_lease).await;
                return Err(error);
            }
        };
        return Ok(OpenedLive {
            live,
            retire_at: Some(retire_at),
            managed_lease: Some(managed_lease),
        });
    }

    let secret = Zeroizing::new(super::fetch_profile_secret(profile)?);
    Ok(OpenedLive {
        live: crate::driver::connect(profile, secret.as_str()).await?,
        retire_at: None,
        managed_lease: None,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use sqlx::sqlite::SqlitePoolOptions;

    use crate::connection::pool::{DbPool, LiveConnection};
    use crate::model::{
        Engine, Provider, WorkspaceConnectionAccess, WorkspaceCredentialMode, WorkspaceKind,
    };
    use crate::store::{ActiveResourceScope, CatalogCachePolicy};

    use super::*;

    fn pin(credential_mode: WorkspaceCredentialMode) -> PinnedConnection {
        PinnedConnection {
            scope: ActiveResourceScope {
                workspace_id: Uuid::from_u128(1),
                workspace_kind: WorkspaceKind::Team,
                selected_account_id: Some("account-a".into()),
                account_scope: AccountScope::WorkspaceUser("account-a".into()),
                generation: 7,
            },
            connection_id: Uuid::from_u128(2),
            connection_revision: 3,
            binding_revision: 4,
            binding_updated_at: "2026-07-24T00:00:00Z".into(),
            profile: ConnectionProfile {
                id: Uuid::from_u128(2),
                name: "app".into(),
                engine: Engine::Postgres,
                provider: Provider::Neon,
                driver_id: None,
                host: "db.example".into(),
                port: 5432,
                database: "app".into(),
                username: "member".into(),
                sslmode: "verify-full".into(),
                extra_params: HashMap::new(),
                readonly_default: true,
                allow_writes: true,
                secret_ref: None,
                env: None,
                schema_group: None,
                workspace_access: WorkspaceConnectionAccess::Write,
                credential_mode,
            },
            requires_remote_rbac: true,
            catalog_cache_policy: CatalogCachePolicy::Persistent,
        }
    }

    #[test]
    fn managed_read_and_write_leases_never_share_a_cache_key() {
        let pin = pin(WorkspaceCredentialMode::Managed);

        assert_ne!(
            ConnectionCacheKey::new(&pin, ConnectionAccess::Read),
            ConnectionCacheKey::new(&pin, ConnectionAccess::Write)
        );
    }

    #[test]
    fn local_material_reuses_the_outer_cache_for_read_and_write() {
        let pin = pin(WorkspaceCredentialMode::MemberLocal);

        assert_eq!(
            ConnectionCacheKey::new(&pin, ConnectionAccess::Read),
            ConnectionCacheKey::new(&pin, ConnectionAccess::Write)
        );
    }

    #[tokio::test]
    async fn expiry_keeps_the_single_flight_slot_for_the_next_generation() {
        let store_pool = SqlitePoolOptions::new()
            .connect_lazy("sqlite::memory:")
            .unwrap();
        let manager = ConnectionManager::new(Store::from_pool_for_test(store_pool));
        let key = ConnectionCacheKey::new(
            &pin(WorkspaceCredentialMode::MemberLocal),
            ConnectionAccess::Read,
        );
        let slot = manager
            .inner
            .slots
            .entry(key.clone())
            .or_insert_with(|| Arc::new(Mutex::new(ConnectionSlot::default())))
            .clone();

        let first_pool = SqlitePoolOptions::new()
            .connect_lazy("sqlite::memory:")
            .unwrap();
        let first_entry = Arc::new(CacheEntry {
            live: Live::Sql(LiveConnection {
                read_pool: DbPool::Sqlite(first_pool.clone()),
                write_pool: DbPool::Sqlite(first_pool),
                skip_fk_metadata: false,
            }),
            generation: 1,
            retire_at: Some(Instant::now()),
            managed_lease: StdMutex::new(None),
        });
        slot.lock().await.entry = Some(first_entry);
        schedule_expiry(Arc::clone(&slot), 1, Duration::ZERO);

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if slot.lock().await.entry.is_none() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        let second_pool = SqlitePoolOptions::new()
            .connect_lazy("sqlite::memory:")
            .unwrap();
        slot.lock().await.entry = Some(Arc::new(CacheEntry {
            live: Live::Sql(LiveConnection {
                read_pool: DbPool::Sqlite(second_pool.clone()),
                write_pool: DbPool::Sqlite(second_pool),
                skip_fk_metadata: false,
            }),
            generation: 2,
            retire_at: None,
            managed_lease: StdMutex::new(None),
        }));
        tokio::task::yield_now().await;

        let mapped = manager.inner.slots.get(&key).unwrap().clone();
        assert!(Arc::ptr_eq(&mapped, &slot));
        assert_eq!(
            mapped
                .lock()
                .await
                .entry
                .as_ref()
                .map(|entry| entry.generation),
            Some(2)
        );
    }
}
