//! Persisted Agent chat metadata and local CLI discovery.
//! Streaming turn execution remains in the legacy Agent transport until Terminal replaces it.

use uuid::Uuid;

use crate::agent::{self, AgentModel, AgentProvider, ChatMessageRecord, ChatThread, CliInfo};
use crate::error::AppResult;
use crate::store::Store;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChatThreadCreateRequest {
    pub(crate) provider: AgentProvider,
    pub(crate) connection_id: Uuid,
    pub(crate) model: Option<String>,
    pub(crate) effort: Option<String>,
}

#[derive(Clone)]
pub(crate) struct AgentService {
    store: Store,
}

impl AgentService {
    pub(super) fn new(store: Store) -> Self {
        Self { store }
    }

    pub(crate) async fn detect_clis() -> Vec<CliInfo> {
        agent::detect_clis_async().await
    }

    pub(crate) async fn list_models(provider: AgentProvider) -> AppResult<Vec<AgentModel>> {
        agent::list_models_async(provider).await
    }

    pub(crate) async fn list_threads(&self) -> AppResult<Vec<ChatThread>> {
        self.store.list_chat_threads().await
    }

    pub(crate) async fn messages(&self, thread_id: Uuid) -> AppResult<Vec<ChatMessageRecord>> {
        self.store.list_chat_messages(thread_id).await
    }

    pub(crate) async fn create_thread(
        &self,
        request: ChatThreadCreateRequest,
    ) -> AppResult<ChatThread> {
        self.store
            .create_chat_thread(
                request.provider,
                request.connection_id,
                request.model,
                request.effort,
            )
            .await
    }

    pub(crate) async fn delete_thread(&self, thread_id: Uuid) -> AppResult<()> {
        self.store.delete_chat_thread(thread_id).await
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

    async fn harness() -> (AgentService, Store, Uuid) {
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
                name: "agent-test".into(),
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
                allow_writes: false,
                secret_ref: None,
                env: Some("test".into()),
                schema_group: None,
                workspace_access: WorkspaceConnectionAccess::Local,
                credential_mode: WorkspaceCredentialMode::Local,
            })
            .await
            .unwrap();
        (AgentService::new(store.clone()), store, connection_id)
    }

    #[tokio::test]
    async fn chat_metadata_round_trips_through_the_transport_neutral_service() {
        let (service, store, connection_id) = harness().await;
        let thread = service
            .create_thread(ChatThreadCreateRequest {
                provider: AgentProvider::Codex,
                connection_id,
                model: Some("gpt-test".into()),
                effort: Some("high".into()),
            })
            .await
            .unwrap();
        store
            .insert_chat_message_with_id(thread.id, Uuid::new_v4(), "user", "show users", None)
            .await
            .unwrap();

        let threads = service.list_threads().await.unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].id, thread.id);
        assert_eq!(threads[0].connection_id, Some(connection_id));
        assert_eq!(threads[0].model.as_deref(), Some("gpt-test"));
        let messages = service.messages(thread.id).await.unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].text, "show users");

        service.delete_thread(thread.id).await.unwrap();
        assert!(service.list_threads().await.unwrap().is_empty());
        assert!(service.messages(thread.id).await.unwrap().is_empty());
        store.pool().close().await;
    }

    #[tokio::test]
    async fn creating_a_thread_with_missing_connection_preserves_scope_failure() {
        let (service, store, _) = harness().await;
        let missing = Uuid::new_v4();
        let error = service
            .create_thread(ChatThreadCreateRequest {
                provider: AgentProvider::Claude,
                connection_id: missing,
                model: None,
                effort: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            crate::error::AppError::NotFound(message)
                if message == format!("connection {missing}")
        ));
        store.pool().close().await;
    }
}
