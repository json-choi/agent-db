//! Transport-neutral audit verification and execution-history reads.

use serde::Serialize;
use uuid::Uuid;

use crate::audit::{self, RecordArgs};
use crate::error::AppResult;
use crate::model::{AuditEntry, Engine, HistoryEntry, QueryKind};
use crate::store::Store;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActivityRecordRequest {
    pub(crate) connection_id: Uuid,
    pub(crate) engine: Engine,
    pub(crate) subject: String,
    pub(crate) kind: QueryKind,
    pub(crate) action: String,
    pub(crate) error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AuditVerdict {
    pub(crate) ok: bool,
    pub(crate) first_bad_index: Option<i64>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AuditSnapshotReceipt {
    pub(crate) entries: Vec<AuditEntry>,
    pub(crate) verdict: AuditVerdict,
}

#[derive(Clone)]
pub(crate) struct ActivityService {
    store: Store,
}

impl ActivityService {
    pub(super) fn new(store: Store) -> Self {
        Self { store }
    }

    pub(crate) async fn verify_audit(&self, connection_id: Uuid) -> AppResult<AuditVerdict> {
        self.store.get_connection(connection_id).await?;
        let (ok, first_bad_index) = audit::verify_chain(&self.store, connection_id).await?;
        Ok(AuditVerdict {
            ok,
            first_bad_index,
        })
    }

    pub(crate) async fn audit_snapshot(
        &self,
        connection_id: Uuid,
    ) -> AppResult<AuditSnapshotReceipt> {
        self.store.get_connection(connection_id).await?;
        let (entries, ok, first_bad_index) = audit::snapshot(&self.store, connection_id).await?;
        Ok(AuditSnapshotReceipt {
            entries,
            verdict: AuditVerdict {
                ok,
                first_bad_index,
            },
        })
    }

    pub(crate) async fn history(&self, connection_id: Uuid) -> AppResult<Vec<HistoryEntry>> {
        self.store.list_history(connection_id).await
    }

    /// Record a non-execution adapter action without allowing an audit outage to
    /// change the already-completed read result.
    pub(crate) async fn record_best_effort(&self, request: ActivityRecordRequest) {
        let _ = audit::record(
            &self.store,
            RecordArgs {
                connection_id: request.connection_id,
                engine: request.engine,
                agent_prompt: None,
                sql: request.subject,
                kind: request.kind,
                action: request.action,
                approved_by: None,
                affected_estimate: None,
                error: request.error,
            },
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::str::FromStr;

    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    use super::*;
    use crate::error::AppError;
    use crate::model::{
        ConnectionProfile, Engine, Provider, QueryKind, WorkspaceConnectionAccess,
        WorkspaceCredentialMode,
    };
    use crate::store::TEST_SCHEMA;

    async fn harness() -> (ActivityService, Store, Uuid) {
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
                name: "activity-test".into(),
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
        (ActivityService::new(store.clone()), store, connection_id)
    }

    #[tokio::test]
    async fn typed_audit_receipts_preserve_the_legacy_wire() {
        let (service, store, connection_id) = harness().await;
        service
            .record_best_effort(ActivityRecordRequest {
                connection_id,
                engine: Engine::Sqlite,
                subject: "SELECT 1".into(),
                kind: QueryKind::Read,
                action: "read".into(),
                error: None,
            })
            .await;

        let verdict = service.verify_audit(connection_id).await.unwrap();
        assert_eq!(
            serde_json::to_value(&verdict).unwrap(),
            serde_json::json!({"ok": true, "firstBadIndex": null})
        );
        let snapshot = service.audit_snapshot(connection_id).await.unwrap();
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(snapshot.entries[0].action, "read");
        assert_eq!(
            serde_json::to_value(&snapshot).unwrap(),
            serde_json::json!({
                "entries": snapshot.entries,
                "verdict": {"ok": true, "firstBadIndex": null}
            })
        );
        store.pool().close().await;
    }

    #[tokio::test]
    async fn audit_reads_reject_an_out_of_scope_connection_before_chain_access() {
        let (service, store, _) = harness().await;
        let missing = Uuid::new_v4();
        assert!(matches!(
            service.verify_audit(missing).await,
            Err(AppError::NotFound(message)) if message == format!("connection {missing}")
        ));
        assert!(matches!(
            service.audit_snapshot(missing).await,
            Err(AppError::NotFound(message)) if message == format!("connection {missing}")
        ));
        store.pool().close().await;
    }
}
