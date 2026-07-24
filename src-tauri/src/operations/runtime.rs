//! Process-wide Operation Runtime facade. It owns the runtime identity and is the
//! only production path that can turn an immutable stored plan into an opaque
//! execution grant.

use chrono::Utc;
use serde_json::Value;
use uuid::Uuid;

use super::execute::{self, ExecutionGrant};
use super::model::{
    NewOperation, OperationApprovalCommand, OperationApprovalDecision, OperationApprover,
    OperationRecord, RestartRecoveryReport,
};
use super::repository::OperationRepository;
use super::OperationState;
use crate::error::{AppError, AppResult};
use crate::store::Store;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OperationPlanDisposition {
    Ready,
    ApprovalRequired,
}

pub(crate) struct ExactApprovalRequest {
    pub operation_id: Uuid,
    pub expected_payload_hash: String,
    pub approver: OperationApprover,
    pub current_policy_revision: String,
    pub reason: Option<String>,
}

pub(crate) struct ClaimedOperation {
    record: OperationRecord,
    grant: ExecutionGrant,
}

impl ClaimedOperation {
    pub(crate) fn record(&self) -> &OperationRecord {
        &self.record
    }

    pub(crate) fn grant(&self) -> &ExecutionGrant {
        &self.grant
    }

    pub(crate) fn into_record(self) -> OperationRecord {
        self.record
    }
}

/// Opaque capability held by the desktop composition root, never by MCP/CLI/Agent
/// adapters. It intentionally implements no serialization, cloning, or defaulting.
pub(crate) struct LocalApprovalAuthority {
    runtime_id: Uuid,
}

#[derive(Clone)]
pub(crate) struct OperationRuntime {
    runtime_id: Uuid,
    repository: OperationRepository,
}

impl OperationRuntime {
    pub(crate) fn new(store: &Store) -> (Self, LocalApprovalAuthority) {
        let runtime_id = Uuid::new_v4();
        (
            Self {
                runtime_id,
                repository: OperationRepository::new(store),
            },
            LocalApprovalAuthority { runtime_id },
        )
    }

    pub(crate) const fn runtime_id(&self) -> Uuid {
        self.runtime_id
    }

    pub(crate) async fn recover_previous_runtimes(&self) -> AppResult<RestartRecoveryReport> {
        self.repository
            .recover_previous_runtimes(self.runtime_id)
            .await
    }

    pub(crate) async fn plan(
        &self,
        operation: NewOperation,
        disposition: OperationPlanDisposition,
    ) -> AppResult<OperationRecord> {
        if operation.kind.may_mutate_target()
            && disposition != OperationPlanDisposition::ApprovalRequired
        {
            return Err(AppError::Blocked {
                reason: "target-mutating operations always require an exact approval".into(),
            });
        }
        let planned = self
            .repository
            .insert_planned(self.runtime_id, operation)
            .await?;
        if planned.state != OperationState::Planned {
            return Ok(planned);
        }
        let target = match disposition {
            OperationPlanDisposition::Ready => OperationState::Ready,
            OperationPlanDisposition::ApprovalRequired => OperationState::PendingApproval,
        };
        self.repository
            .transition(
                planned.id,
                self.runtime_id,
                target,
                &serde_json::json!({"disposition": disposition_str(disposition)}),
            )
            .await
    }

    pub(crate) async fn get(&self, operation_id: Uuid) -> AppResult<OperationRecord> {
        self.repository.get(operation_id).await
    }

    pub(crate) async fn approve_exact(
        &self,
        authority: &LocalApprovalAuthority,
        request: ExactApprovalRequest,
    ) -> AppResult<OperationRecord> {
        self.ensure_local_approval_authority(authority)?;
        self.decide_exact(request, OperationApprovalDecision::Approved)
            .await
    }

    pub(crate) async fn reject_exact(
        &self,
        authority: &LocalApprovalAuthority,
        request: ExactApprovalRequest,
    ) -> AppResult<OperationRecord> {
        self.ensure_local_approval_authority(authority)?;
        self.decide_exact(request, OperationApprovalDecision::Rejected)
            .await
    }

    async fn decide_exact(
        &self,
        request: ExactApprovalRequest,
        decision: OperationApprovalDecision,
    ) -> AppResult<OperationRecord> {
        self.repository
            .decide_approval(OperationApprovalCommand {
                operation_id: request.operation_id,
                runtime_id: self.runtime_id,
                expected_payload_hash: request.expected_payload_hash,
                approver: request.approver,
                decision,
                reason: request.reason,
                current_policy_revision: request.current_policy_revision,
                now: Utc::now(),
            })
            .await
    }

    fn ensure_local_approval_authority(&self, authority: &LocalApprovalAuthority) -> AppResult<()> {
        if authority.runtime_id == self.runtime_id {
            Ok(())
        } else {
            Err(AppError::Blocked {
                reason: "approval authority belongs to a different application runtime".into(),
            })
        }
    }

    /// Claim by id only. The repository reloads the immutable payload and uses its
    /// own hash in the CAS; callers never resend SQL, connection, or approval.
    pub(crate) async fn claim(&self, operation_id: Uuid) -> AppResult<ClaimedOperation> {
        let record = self
            .repository
            .claim_execution(operation_id, self.runtime_id, Utc::now())
            .await?;
        let grant = execute::issue(&record)?;
        Ok(ClaimedOperation { record, grant })
    }

    pub(crate) async fn progress(&self, operation_id: Uuid, details: &Value) -> AppResult<()> {
        self.repository
            .append_progress(operation_id, self.runtime_id, details)
            .await
            .map(|_| ())
    }

    pub(crate) async fn succeed(
        &self,
        operation_id: Uuid,
        details: &Value,
    ) -> AppResult<OperationRecord> {
        self.finish(operation_id, OperationState::Succeeded, details)
            .await
    }

    pub(crate) async fn fail(
        &self,
        operation_id: Uuid,
        details: &Value,
    ) -> AppResult<OperationRecord> {
        self.finish(operation_id, OperationState::Failed, details)
            .await
    }

    pub(crate) async fn confirm_cancelled(
        &self,
        operation_id: Uuid,
        details: &Value,
    ) -> AppResult<OperationRecord> {
        self.finish(operation_id, OperationState::Cancelled, details)
            .await
    }

    pub(crate) async fn mark_outcome_unknown(
        &self,
        operation_id: Uuid,
        details: &Value,
    ) -> AppResult<OperationRecord> {
        self.finish(operation_id, OperationState::OutcomeUnknown, details)
            .await
    }

    async fn finish(
        &self,
        operation_id: Uuid,
        target: OperationState,
        details: &Value,
    ) -> AppResult<OperationRecord> {
        self.repository
            .transition(operation_id, self.runtime_id, target, details)
            .await
    }

    #[cfg(test)]
    pub(crate) async fn approvals(
        &self,
        operation_id: Uuid,
    ) -> AppResult<Vec<super::model::OperationApprovalRecord>> {
        self.repository.approvals(operation_id).await
    }

    #[cfg(test)]
    pub(crate) async fn verify_event_chain(&self, operation_id: Uuid) -> AppResult<bool> {
        self.repository.verify_event_chain(operation_id).await
    }
}

const fn disposition_str(value: OperationPlanDisposition) -> &'static str {
    match value {
        OperationPlanDisposition::Ready => "ready",
        OperationPlanDisposition::ApprovalRequired => "approval_required",
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use chrono::Duration;
    use dopedb_protocol::{OperationActorKind, OperationKind, OperationRiskLevel};
    use serde_json::json;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    use super::*;
    use crate::operations::model::{OperationActor, OperationActorProvenance, OperationApprover};
    use crate::store::{Store, TEST_SCHEMA};

    async fn runtime() -> (OperationRuntime, LocalApprovalAuthority, Store) {
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
        let (runtime, authority) = OperationRuntime::new(&store);
        (runtime, authority, store)
    }

    fn operation(kind: OperationKind, idempotency_key: &str) -> NewOperation {
        NewOperation {
            id: Uuid::new_v4(),
            workspace_id: Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap(),
            account_scope: "personal".into(),
            connection_id: Uuid::new_v4(),
            connection_revision: 1,
            terminal_session_id: Some(Uuid::new_v4()),
            actor: OperationActor {
                kind: OperationActorKind::LocalUser,
                id: "local-owner".into(),
                provenance: OperationActorProvenance {
                    client_protocol_version: Some(1),
                    origin_surface: "sql_editor".into(),
                    ..OperationActorProvenance::default()
                },
            },
            kind,
            payload_schema_version: 1,
            payload: json!({"sql": "UPDATE items SET active = false WHERE id = 1"}),
            schema_fingerprint: Some("a".repeat(64)),
            risk_level: OperationRiskLevel::Medium,
            preview: json!({"estimatedRows": 1}),
            policy_snapshot: json!({"allowWrites": true, "requireApproval": true}),
            policy_revision: "policy-v1".into(),
            single_use: true,
            idempotency_key: idempotency_key.into(),
            expires_at: Some(Utc::now() + Duration::minutes(5)),
        }
    }

    fn exact_request(record: &OperationRecord) -> ExactApprovalRequest {
        ExactApprovalRequest {
            operation_id: record.id,
            expected_payload_hash: record.payload_hash.clone(),
            approver: OperationApprover {
                kind: OperationActorKind::LocalUser,
                id: "local-owner".into(),
            },
            current_policy_revision: record.policy_revision.clone(),
            reason: None,
        }
    }

    #[tokio::test]
    async fn ready_read_claim_uses_only_the_stored_operation_id() {
        let (runtime, _, _) = runtime().await;
        let mut request = operation(OperationKind::ReadQuery, "read");
        request.payload = json!({"sql": "SELECT * FROM items"});
        let ready = runtime
            .plan(request, OperationPlanDisposition::Ready)
            .await
            .unwrap();
        assert_eq!(ready.state, OperationState::Ready);
        let claimed = runtime.claim(ready.id).await.unwrap();
        assert_eq!(claimed.record().state, OperationState::Executing);
        assert_eq!(claimed.grant().operation_id(), ready.id);
        assert_eq!(claimed.grant().connection_id(), ready.connection_id);
        assert_eq!(claimed.grant().payload_sha256(), ready.payload_hash);
        runtime
            .succeed(ready.id, &json!({"rowCount": 1}))
            .await
            .unwrap();
        assert_eq!(
            runtime.get(ready.id).await.unwrap().state,
            OperationState::Succeeded
        );
        assert!(runtime.verify_event_chain(ready.id).await.unwrap());
    }

    #[tokio::test]
    async fn mutation_cannot_be_planned_ready_or_claimed_before_exact_approval() {
        let (runtime, authority, store) = runtime().await;
        let direct = operation(OperationKind::WriteSql, "unsafe-ready");
        let direct_id = direct.id;
        assert!(runtime
            .plan(direct, OperationPlanDisposition::Ready)
            .await
            .is_err());
        assert!(runtime.get(direct_id).await.is_err());

        let pending = runtime
            .plan(
                operation(OperationKind::WriteSql, "approved-write"),
                OperationPlanDisposition::ApprovalRequired,
            )
            .await
            .unwrap();
        assert_eq!(pending.state, OperationState::PendingApproval);
        assert!(runtime.claim(pending.id).await.is_err());

        let mut wrong_hash = exact_request(&pending);
        wrong_hash.expected_payload_hash = "0".repeat(64);
        assert!(runtime.approve_exact(&authority, wrong_hash).await.is_err());
        assert!(runtime.approvals(pending.id).await.unwrap().is_empty());
        let (_, wrong_authority) = OperationRuntime::new(&store);
        assert!(runtime
            .approve_exact(&wrong_authority, exact_request(&pending))
            .await
            .is_err());

        let approved = runtime
            .approve_exact(&authority, exact_request(&pending))
            .await
            .unwrap();
        assert_eq!(approved.state, OperationState::Approved);
        assert_eq!(runtime.approvals(approved.id).await.unwrap().len(), 1);
        let claimed = runtime.claim(approved.id).await.unwrap();
        assert_eq!(claimed.record().payload, pending.payload);
        assert_eq!(claimed.grant().payload_sha256(), pending.payload_hash);
    }

    #[tokio::test]
    async fn runtime_restart_marks_claimed_write_outcome_unknown_without_retry() {
        let (first, authority, store) = runtime().await;
        let pending = first
            .plan(
                operation(OperationKind::WriteSql, "restart"),
                OperationPlanDisposition::ApprovalRequired,
            )
            .await
            .unwrap();
        let approved = first
            .approve_exact(&authority, exact_request(&pending))
            .await
            .unwrap();
        first.claim(approved.id).await.unwrap();

        let (second, _) = OperationRuntime::new(&store);
        assert_ne!(first.runtime_id(), second.runtime_id());
        let report = second.recover_previous_runtimes().await.unwrap();
        assert_eq!(report.outcome_unknown, vec![approved.id]);
        assert_eq!(
            second.get(approved.id).await.unwrap().state,
            OperationState::OutcomeUnknown
        );
        assert!(second.claim(approved.id).await.is_err());
    }

    #[tokio::test]
    async fn exact_rejection_is_terminal_and_never_issues_a_grant() {
        let (runtime, authority, _) = runtime().await;
        let pending = runtime
            .plan(
                operation(OperationKind::Ddl, "reject"),
                OperationPlanDisposition::ApprovalRequired,
            )
            .await
            .unwrap();
        let rejected = runtime
            .reject_exact(&authority, exact_request(&pending))
            .await
            .unwrap();
        assert_eq!(rejected.state, OperationState::Rejected);
        assert!(runtime.claim(rejected.id).await.is_err());
    }
}
