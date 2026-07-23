//! Transport-neutral catalog loading policy over the existing scoped Catalog V2 adapter.

use uuid::Uuid;

use crate::connection::{ConnectionAccess, ConnectionManager};
use crate::error::AppResult;
use crate::introspect::{self, Catalog, CatalogReadMode};
use crate::store::Store;

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
