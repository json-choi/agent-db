//! Desktop-only exact approval orchestration. The service derives the approver and
//! current policy from the active scope; Tauri callers may provide only an operation
//! id, the hash rendered to the user, and an optional human reason.

use std::time::Duration;

use dopedb_protocol::{OperationState, OperationSummary};
use serde::Serialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::connection::ConnectionManager;
use crate::error::{AppError, AppResult};
use crate::model::SafetySettings;
use crate::operations::{
    canonical_hash, ExactApprovalRequest, LocalApprovalAuthority, OperationActor,
    OperationActorKind, OperationActorProvenance, OperationApprover, OperationRecord,
    OperationRiskLevel, OperationRuntime,
};
use crate::store::{AccountScope, PinnedConnection, Store};

use super::TerminalAuthority;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OperationDecisionRequest {
    pub(crate) operation_id: Uuid,
    pub(crate) expected_payload_hash: String,
    pub(crate) reason: Option<String>,
}

/// Redacted lifecycle projection returned after a local approval decision.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OperationDecisionReceipt {
    pub(crate) operation_id: Uuid,
    pub(crate) payload_hash: String,
    pub(crate) state: OperationState,
}

#[derive(Clone)]
pub(crate) struct OperationService {
    store: Store,
    connections: ConnectionManager,
    runtime: OperationRuntime,
}

impl OperationService {
    pub(super) fn new(
        store: Store,
        connections: ConnectionManager,
        runtime: OperationRuntime,
    ) -> Self {
        Self {
            store,
            connections,
            runtime,
        }
    }

    pub(crate) async fn recover_previous_runtimes(&self) -> AppResult<()> {
        self.runtime.recover_previous_runtimes().await.map(|_| ())
    }

    pub(crate) async fn approve_local(
        &self,
        authority: &LocalApprovalAuthority,
        request: OperationDecisionRequest,
    ) -> AppResult<OperationDecisionReceipt> {
        let exact = self.exact_request(request, true).await?;
        self.runtime
            .approve_exact(authority, exact)
            .await
            .map(OperationDecisionReceipt::from)
    }

    pub(crate) async fn reject_local(
        &self,
        authority: &LocalApprovalAuthority,
        request: OperationDecisionRequest,
    ) -> AppResult<OperationDecisionReceipt> {
        let exact = self.exact_request(request, false).await?;
        self.runtime
            .reject_exact(authority, exact)
            .await
            .map(OperationDecisionReceipt::from)
    }

    pub(crate) async fn show_terminal(
        &self,
        scope: &TerminalAuthority,
        operation_id: Uuid,
    ) -> AppResult<OperationSummary> {
        let record = self.runtime.get(operation_id).await?;
        ensure_terminal_scope(&record, scope)?;
        Ok(operation_summary(&record))
    }

    pub(crate) async fn wait_terminal(
        &self,
        scope: &TerminalAuthority,
        operation_id: Uuid,
        timeout: Duration,
    ) -> AppResult<OperationSummary> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let summary = self.show_terminal(scope, operation_id).await?;
            if summary.state.is_terminal() {
                return Ok(summary);
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(AppError::Safety("operation wait timed out".into()));
            }
            tokio::time::sleep(
                deadline
                    .saturating_duration_since(now)
                    .min(Duration::from_millis(50)),
            )
            .await;
        }
    }

    pub(crate) async fn cancel_terminal(
        &self,
        scope: &TerminalAuthority,
        operation_id: Uuid,
    ) -> AppResult<OperationSummary> {
        let record = self.runtime.get(operation_id).await?;
        ensure_terminal_scope(&record, scope)?;
        if record.state.is_terminal() {
            return Ok(operation_summary(&record));
        }
        if record.state == OperationState::Executing {
            crate::executor::cancel::cancel(operation_id);
            return Ok(operation_summary(&record));
        }
        let cancelled = self
            .runtime
            .cancel_before_execution(
                operation_id,
                &json!({
                    "origin": "cli",
                    "terminalSessionId": scope.terminal_session_id,
                }),
            )
            .await?;
        Ok(operation_summary(&cancelled))
    }

    async fn exact_request(
        &self,
        request: OperationDecisionRequest,
        validate_confirmation: bool,
    ) -> AppResult<ExactApprovalRequest> {
        let record = self.runtime.get(request.operation_id).await?;
        if validate_confirmation {
            if let Some(expected) = required_confirmation(&record) {
                if request.reason.as_deref() != Some(expected) {
                    return Err(AppError::Blocked {
                        reason: format!(
                            "type the exact confirmation phrase `{expected}` before approving this operation"
                        ),
                    });
                }
            }
        }
        let operation_scope = self.connections.begin_operation_scope().await;
        let pin = operation_scope
            .pin_connection_for_view(record.connection_id)
            .await?;
        ensure_operation_scope(&record, &pin)?;
        let settings = self.store.get_safety(pin.connection_id).await?;
        let policy = capture_policy(&pin, &settings)?;
        Ok(ExactApprovalRequest {
            operation_id: request.operation_id,
            expected_payload_hash: request.expected_payload_hash,
            approver: approver_for_pin(&pin),
            current_policy_revision: policy.revision,
            reason: request.reason,
        })
    }
}

fn ensure_terminal_scope(record: &OperationRecord, scope: &TerminalAuthority) -> AppResult<()> {
    let matches = record.terminal_session_id == Some(scope.terminal_session_id)
        && record.workspace_id == scope.workspace_id
        && record.account_scope == scope.account_scope
        && record.connection_id == scope.connection_id
        && record.connection_revision == scope.connection_revision;
    if matches {
        Ok(())
    } else {
        Err(AppError::Blocked {
            reason: "operation belongs to a different Terminal or connection scope".into(),
        })
    }
}

fn operation_summary(record: &OperationRecord) -> OperationSummary {
    OperationSummary {
        operation_id: record.id,
        connection_id: record.connection_id,
        kind: record.kind,
        state: record.state,
        risk_level: record.risk_level,
        payload_hash: record.payload_hash.clone(),
        expires_at: record.expires_at,
        started_at: record.started_at,
        finished_at: record.finished_at,
        created_at: record.created_at,
        updated_at: record.updated_at,
    }
}

pub(super) const CRITICAL_CONFIRMATION: &str = "RUN CRITICAL";
pub(super) const PRODUCTION_CONFIRMATION: &str = "PROD";

/// Derive the additional human confirmation from immutable risk and policy data.
/// Rejection never needs this phrase; approval does.
pub(super) fn required_confirmation(record: &OperationRecord) -> Option<&'static str> {
    if !record.kind.may_mutate_target() {
        return None;
    }
    if record.risk_level == OperationRiskLevel::Critical {
        return Some(CRITICAL_CONFIRMATION);
    }
    let production = record
        .policy_snapshot
        .get("environment")
        .and_then(Value::as_str)
        .is_some_and(|environment| environment.eq_ignore_ascii_case("prod"));
    production.then_some(PRODUCTION_CONFIRMATION)
}

impl From<OperationRecord> for OperationDecisionReceipt {
    fn from(record: OperationRecord) -> Self {
        Self {
            operation_id: record.id,
            payload_hash: record.payload_hash,
            state: record.state,
        }
    }
}

pub(super) struct CapturedOperationPolicy {
    pub(super) snapshot: Value,
    pub(super) revision: String,
}

pub(super) fn capture_policy(
    pin: &PinnedConnection,
    settings: &SafetySettings,
) -> AppResult<CapturedOperationPolicy> {
    let snapshot = json!({
        "accountScope": pin.scope.account_scope.storage_key(),
        "bindingRevision": pin.binding_revision,
        "bindingUpdatedAt": pin.binding_updated_at,
        "connectionRevision": pin.connection_revision,
        "credentialMode": pin.profile.credential_mode,
        "environment": pin.profile.env,
        "safety": settings,
        "scopeGeneration": pin.scope.generation,
        "workspaceAccess": pin.profile.workspace_access,
        "workspaceId": pin.scope.workspace_id,
    });
    let revision = canonical_hash(&snapshot)?;
    Ok(CapturedOperationPolicy { snapshot, revision })
}

pub(super) fn actor_for_pin(pin: &PinnedConnection, origin_surface: String) -> OperationActor {
    let (kind, id, local_account_id, workspace_account_id) = match &pin.scope.account_scope {
        AccountScope::Personal => (
            OperationActorKind::LocalUser,
            "local-user".to_string(),
            Some("local-user".to_string()),
            None,
        ),
        AccountScope::WorkspaceUser(id) => (
            OperationActorKind::WorkspaceUser,
            id.clone(),
            None,
            Some(id.clone()),
        ),
    };
    OperationActor {
        kind,
        id,
        provenance: OperationActorProvenance {
            local_account_id,
            workspace_account_id,
            origin_surface,
            ..OperationActorProvenance::default()
        },
    }
}

pub(super) fn agent_actor_for_pin(
    pin: &PinnedConnection,
    actor_id: String,
    origin_surface: String,
) -> OperationActor {
    let (local_account_id, workspace_account_id) = match &pin.scope.account_scope {
        AccountScope::Personal => (Some("local-user".into()), None),
        AccountScope::WorkspaceUser(id) => (None, Some(id.clone())),
    };
    OperationActor {
        kind: OperationActorKind::Agent,
        id: actor_id,
        provenance: OperationActorProvenance {
            local_account_id,
            workspace_account_id,
            origin_surface,
            ..OperationActorProvenance::default()
        },
    }
}

pub(super) fn approver_for_pin(pin: &PinnedConnection) -> OperationApprover {
    match &pin.scope.account_scope {
        AccountScope::Personal => OperationApprover {
            kind: OperationActorKind::LocalUser,
            id: "local-user".into(),
        },
        AccountScope::WorkspaceUser(id) => OperationApprover {
            kind: OperationActorKind::WorkspaceUser,
            id: id.clone(),
        },
    }
}

pub(super) fn ensure_operation_scope(
    record: &OperationRecord,
    pin: &PinnedConnection,
) -> AppResult<()> {
    let matches = record.workspace_id == pin.scope.workspace_id
        && record.account_scope == pin.scope.account_scope.storage_key()
        && record.connection_id == pin.connection_id
        && record.connection_revision == pin.connection_revision;
    if matches {
        Ok(())
    } else {
        Err(AppError::Blocked {
            reason: "operation scope or connection revision changed after the proposal was created"
                .into(),
        })
    }
}
