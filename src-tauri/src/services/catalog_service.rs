//! Transport-neutral catalog loading policy over the existing scoped Catalog V2 adapter.

use uuid::Uuid;

use crate::connection::{ConnectionAccess, ConnectionManager};
use crate::error::AppResult;
use crate::introspect::{self, Catalog, CatalogReadMode};
use crate::store::Store;
use dopedb_protocol::CatalogSnapshot;

use super::TerminalAuthority;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CatalogReadPolicy {
    CacheFirst,
    LiveNoCache,
    Refresh,
}

impl From<CatalogReadPolicy> for CatalogReadMode {
    fn from(policy: CatalogReadPolicy) -> Self {
        match policy {
            CatalogReadPolicy::CacheFirst => Self::CacheFirst,
            CatalogReadPolicy::LiveNoCache => Self::LiveNoCache,
            CatalogReadPolicy::Refresh => Self::Refresh,
        }
    }
}

#[derive(Clone)]
pub(crate) struct CatalogService {
    store: Store,
    connections: ConnectionManager,
}

impl CatalogService {
    pub(super) fn new(store: Store, connections: ConnectionManager) -> Self {
        Self { store, connections }
    }

    pub(crate) async fn load(
        &self,
        connection_id: Uuid,
        policy: CatalogReadPolicy,
    ) -> AppResult<Catalog> {
        introspect::load_catalog(&self.store, &self.connections, connection_id, policy.into()).await
    }

    pub(crate) async fn load_snapshot(
        &self,
        connection_id: Uuid,
        policy: CatalogReadPolicy,
    ) -> AppResult<CatalogSnapshot> {
        introspect::load_catalog_snapshot(
            &self.store,
            &self.connections,
            connection_id,
            policy.into(),
        )
        .await
    }

    pub(crate) async fn load_terminal_snapshot(
        &self,
        authority: &TerminalAuthority,
        policy: CatalogReadPolicy,
    ) -> AppResult<CatalogSnapshot> {
        let authority_context = self
            .connections
            .pin(authority.connection_id, ConnectionAccess::Read)
            .await?;
        authority.ensure_pin(authority_context.pin())?;
        self.load_snapshot(authority.connection_id, policy).await
    }

    pub(crate) async fn table_ddl(
        &self,
        connection_id: Uuid,
        schema: Option<&str>,
        table: &str,
    ) -> AppResult<String> {
        let lease = self
            .connections
            .acquire(connection_id, ConnectionAccess::Read)
            .await?;
        introspect::table_ddl(lease.live(), schema, table).await
    }
}
