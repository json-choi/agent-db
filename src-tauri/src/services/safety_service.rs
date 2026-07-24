//! Scope-aware per-connection safety settings.

use uuid::Uuid;

use crate::connection::{ConnectionAccess, ConnectionManager};
use crate::error::AppResult;
use crate::model::SafetySettings;
use crate::store::Store;

#[derive(Clone)]
pub(crate) struct SafetyService {
    store: Store,
    connections: ConnectionManager,
}

impl SafetyService {
    pub(super) fn new(store: Store, connections: ConnectionManager) -> Self {
        Self { store, connections }
    }

    pub(crate) async fn get(&self, connection_id: Uuid) -> AppResult<SafetySettings> {
        self.store.get_safety(connection_id).await
    }

    /// Normalize untrusted UI limits and persist them under an online connection
    /// authorization guard. Read-only workspace roles can never enable writes.
    pub(crate) async fn update(
        &self,
        connection_id: Uuid,
        mut settings: SafetySettings,
    ) -> AppResult<()> {
        let profile = self.store.get_connection(connection_id).await?;
        if !profile.workspace_access.can_write() {
            settings.allow_writes = false;
        }
        let _mutation = self
            .connections
            .begin_connection_mutation(
                connection_id,
                if settings.allow_writes {
                    ConnectionAccess::Write
                } else {
                    ConnectionAccess::Read
                },
            )
            .await?;
        settings.max_rows = settings.max_rows.clamp(1, 100_000);
        settings.exec_preview_row_limit = settings.exec_preview_row_limit.clamp(0, 1_000_000);
        self.store.set_safety(connection_id, &settings).await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::str::FromStr;

    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    use super::*;
    use crate::model::{
        ConnectionProfile, Engine, Provider, WorkspaceConnectionAccess, WorkspaceCredentialMode,
    };
    use crate::store::TEST_SCHEMA;

    async fn harness() -> (SafetyService, Store, Uuid) {
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
        let connection_id = Uuid::new_v4();
        store
            .upsert_connection(&ConnectionProfile {
                id: connection_id,
                name: "safety-test".into(),
                engine: Engine::Sqlite,
                provider: Provider::Generic,
                driver_id: Some("sqlx-sqlite".into()),
                host: String::new(),
                port: 0,
                database: ":memory:".into(),
                username: String::new(),
                sslmode: "disable".into(),
                extra_params: HashMap::new(),
                readonly_default: true,
                allow_writes: true,
                secret_ref: None,
                env: Some("test".into()),
                schema_group: None,
                workspace_access: WorkspaceConnectionAccess::Local,
                credential_mode: WorkspaceCredentialMode::Local,
            })
            .await
            .unwrap();
        let connections = ConnectionManager::new(store.clone());
        (
            SafetyService::new(store.clone(), connections),
            store,
            connection_id,
        )
    }

    #[tokio::test]
    async fn update_clamps_both_untrusted_row_limits() {
        let (service, store, connection_id) = harness().await;
        let mut settings = service.get(connection_id).await.unwrap();
        settings.allow_writes = true;
        settings.max_rows = u64::MAX;
        settings.exec_preview_row_limit = -1;
        service.update(connection_id, settings).await.unwrap();
        let stored = service.get(connection_id).await.unwrap();
        assert!(stored.allow_writes);
        assert_eq!(stored.max_rows, 100_000);
        assert_eq!(stored.exec_preview_row_limit, 0);

        let mut settings = stored;
        settings.max_rows = 0;
        settings.exec_preview_row_limit = i64::MAX;
        service.update(connection_id, settings).await.unwrap();
        let stored = service.get(connection_id).await.unwrap();
        assert_eq!(stored.max_rows, 1);
        assert_eq!(stored.exec_preview_row_limit, 1_000_000);
        store.pool().close().await;
    }

    #[tokio::test]
    async fn read_only_workspace_role_cannot_persist_write_enablement() {
        let (service, store, connection_id) = harness().await;
        let mut baseline = service.get(connection_id).await.unwrap();
        baseline.allow_writes = false;
        service.update(connection_id, baseline).await.unwrap();
        sqlx::query(
            "UPDATE connections
             SET workspace_access = 'read', revision = revision + 1
             WHERE id = ?1",
        )
        .bind(connection_id.to_string())
        .execute(store.pool())
        .await
        .unwrap();
        let mut settings = service.get(connection_id).await.unwrap();
        settings.allow_writes = true;
        let error = service.update(connection_id, settings).await.unwrap_err();
        assert!(matches!(
            error,
            crate::error::AppError::Blocked { reason }
                if reason == "workspace role cannot execute this connection"
        ));
        assert!(!service.get(connection_id).await.unwrap().allow_writes);
        store.pool().close().await;
    }
}
