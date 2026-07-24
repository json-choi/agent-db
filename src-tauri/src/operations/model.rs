//! Internal durable Operation models. These types are intentionally not Tauri or
//! broker response DTOs: adapters receive separately redacted projections and can
//! never construct an execution grant from a deserialized request.

use chrono::{DateTime, Utc};
use dopedb_protocol::{
    OperationActorKind, OperationEventKind, OperationKind, OperationRiskLevel, OperationState,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Bounded provenance attached to the immutable actor identity.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct OperationActorProvenance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_protocol_version: Option<u16>,
    pub origin_surface: String,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct OperationActor {
    pub kind: OperationActorKind,
    pub id: String,
    pub provenance: OperationActorProvenance,
}

/// Untrusted adapter input used only to create a first, persisted `planned` record.
/// The repository derives canonical bytes and hashes; callers never supply a digest.
#[derive(Clone)]
pub(crate) struct NewOperation {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub account_scope: String,
    pub connection_id: Uuid,
    pub connection_revision: i64,
    pub terminal_session_id: Option<Uuid>,
    pub actor: OperationActor,
    pub kind: OperationKind,
    pub payload_schema_version: u32,
    pub payload: Value,
    pub schema_fingerprint: Option<String>,
    pub risk_level: OperationRiskLevel,
    pub preview: Value,
    pub policy_snapshot: Value,
    pub policy_revision: String,
    pub single_use: bool,
    pub idempotency_key: String,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OperationApprovalDecision {
    Approved,
    Rejected,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct OperationApprover {
    pub kind: OperationActorKind,
    pub id: String,
}

pub(crate) struct OperationApprovalCommand {
    pub operation_id: Uuid,
    pub runtime_id: Uuid,
    pub expected_payload_hash: String,
    pub approver: OperationApprover,
    pub decision: OperationApprovalDecision,
    pub reason: Option<String>,
    pub current_policy_revision: String,
    pub now: DateTime<Utc>,
}

#[derive(Clone)]
pub(crate) struct OperationApprovalRecord {
    pub id: Uuid,
    pub operation_id: Uuid,
    pub payload_hash: String,
    pub approver: OperationApprover,
    pub decision: OperationApprovalDecision,
    pub reason: Option<String>,
    pub policy_revision: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
}

/// Complete internal projection loaded from local SQLite. The payload is deliberately
/// not serializable as an adapter response.
#[derive(Clone)]
pub(crate) struct OperationRecord {
    pub id: Uuid,
    pub runtime_id: Uuid,
    pub workspace_id: Uuid,
    pub account_scope: String,
    pub connection_id: Uuid,
    pub connection_revision: i64,
    pub terminal_session_id: Option<Uuid>,
    pub actor: OperationActor,
    pub kind: OperationKind,
    pub payload_schema_version: u32,
    pub payload: Value,
    pub payload_hash: String,
    pub schema_fingerprint: Option<String>,
    pub risk_level: OperationRiskLevel,
    pub preview: Value,
    pub policy_snapshot: Value,
    pub policy_revision: String,
    pub state: OperationState,
    pub single_use: bool,
    pub idempotency_key: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone)]
pub(crate) struct OperationEventRecord {
    pub id: Uuid,
    pub operation_id: Uuid,
    pub sequence: i64,
    pub kind: OperationEventKind,
    pub state: OperationState,
    pub details: Value,
    pub created_at: DateTime<Utc>,
    pub prev_hash: Option<String>,
    pub hash: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RestartRecoveryReport {
    pub expired: Vec<Uuid>,
    pub failed: Vec<Uuid>,
    pub outcome_unknown: Vec<Uuid>,
    pub checkpoint_validation_required: Vec<Uuid>,
}

pub(super) const fn actor_kind_str(value: OperationActorKind) -> &'static str {
    match value {
        OperationActorKind::LocalUser => "local_user",
        OperationActorKind::WorkspaceUser => "workspace_user",
        OperationActorKind::Agent => "agent",
        OperationActorKind::Plugin => "plugin",
        OperationActorKind::System => "system",
    }
}

pub(super) fn parse_actor_kind(value: &str) -> Option<OperationActorKind> {
    match value {
        "local_user" => Some(OperationActorKind::LocalUser),
        "workspace_user" => Some(OperationActorKind::WorkspaceUser),
        "agent" => Some(OperationActorKind::Agent),
        "plugin" => Some(OperationActorKind::Plugin),
        "system" => Some(OperationActorKind::System),
        _ => None,
    }
}

pub(super) const fn approval_decision_str(value: OperationApprovalDecision) -> &'static str {
    match value {
        OperationApprovalDecision::Approved => "approved",
        OperationApprovalDecision::Rejected => "rejected",
    }
}

pub(super) fn parse_approval_decision(value: &str) -> Option<OperationApprovalDecision> {
    match value {
        "approved" => Some(OperationApprovalDecision::Approved),
        "rejected" => Some(OperationApprovalDecision::Rejected),
        _ => None,
    }
}

pub(super) const fn operation_kind_str(value: OperationKind) -> &'static str {
    match value {
        OperationKind::ReadQuery => "read_query",
        OperationKind::DocumentRead => "document_read",
        OperationKind::WriteSql => "write_sql",
        OperationKind::Ddl => "ddl",
        OperationKind::Privilege => "privilege",
        OperationKind::SqlScript => "sql_script",
        OperationKind::TableDataChange => "table_data_change",
        OperationKind::SchemaChange => "schema_change",
        OperationKind::Import => "import",
        OperationKind::Export => "export",
        OperationKind::Migration => "migration",
        OperationKind::DashboardCreate => "dashboard_create",
        OperationKind::PluginAction => "plugin_action",
        OperationKind::ProviderAction => "provider_action",
    }
}

pub(super) fn parse_operation_kind(value: &str) -> Option<OperationKind> {
    match value {
        "read_query" => Some(OperationKind::ReadQuery),
        "document_read" => Some(OperationKind::DocumentRead),
        "write_sql" => Some(OperationKind::WriteSql),
        "ddl" => Some(OperationKind::Ddl),
        "privilege" => Some(OperationKind::Privilege),
        "sql_script" => Some(OperationKind::SqlScript),
        "table_data_change" => Some(OperationKind::TableDataChange),
        "schema_change" => Some(OperationKind::SchemaChange),
        "import" => Some(OperationKind::Import),
        "export" => Some(OperationKind::Export),
        "migration" => Some(OperationKind::Migration),
        "dashboard_create" => Some(OperationKind::DashboardCreate),
        "plugin_action" => Some(OperationKind::PluginAction),
        "provider_action" => Some(OperationKind::ProviderAction),
        _ => None,
    }
}

pub(super) const fn risk_level_str(value: OperationRiskLevel) -> &'static str {
    match value {
        OperationRiskLevel::Low => "low",
        OperationRiskLevel::Medium => "medium",
        OperationRiskLevel::High => "high",
        OperationRiskLevel::Critical => "critical",
    }
}

pub(super) fn parse_risk_level(value: &str) -> Option<OperationRiskLevel> {
    match value {
        "low" => Some(OperationRiskLevel::Low),
        "medium" => Some(OperationRiskLevel::Medium),
        "high" => Some(OperationRiskLevel::High),
        "critical" => Some(OperationRiskLevel::Critical),
        _ => None,
    }
}

pub(super) const fn state_str(value: OperationState) -> &'static str {
    match value {
        OperationState::Planned => "planned",
        OperationState::PendingApproval => "pending_approval",
        OperationState::Ready => "ready",
        OperationState::Approved => "approved",
        OperationState::Rejected => "rejected",
        OperationState::Expired => "expired",
        OperationState::Cancelled => "cancelled",
        OperationState::Executing => "executing",
        OperationState::Succeeded => "succeeded",
        OperationState::Failed => "failed",
        OperationState::OutcomeUnknown => "outcome_unknown",
    }
}

pub(super) fn parse_state(value: &str) -> Option<OperationState> {
    match value {
        "planned" => Some(OperationState::Planned),
        "pending_approval" => Some(OperationState::PendingApproval),
        "ready" => Some(OperationState::Ready),
        "approved" => Some(OperationState::Approved),
        "rejected" => Some(OperationState::Rejected),
        "expired" => Some(OperationState::Expired),
        "cancelled" => Some(OperationState::Cancelled),
        "executing" => Some(OperationState::Executing),
        "succeeded" => Some(OperationState::Succeeded),
        "failed" => Some(OperationState::Failed),
        "outcome_unknown" => Some(OperationState::OutcomeUnknown),
        _ => None,
    }
}

pub(super) const fn event_kind_str(value: OperationEventKind) -> &'static str {
    match value {
        OperationEventKind::Proposed => "proposed",
        OperationEventKind::Planned => "planned",
        OperationEventKind::ApprovalRequested => "approval_requested",
        OperationEventKind::Approved => "approved",
        OperationEventKind::Rejected => "rejected",
        OperationEventKind::ExecutionStarted => "execution_started",
        OperationEventKind::Progress => "progress",
        OperationEventKind::Succeeded => "succeeded",
        OperationEventKind::Failed => "failed",
        OperationEventKind::Cancelled => "cancelled",
        OperationEventKind::OutcomeUnknown => "outcome_unknown",
        OperationEventKind::Expired => "expired",
    }
}

pub(super) fn parse_event_kind(value: &str) -> Option<OperationEventKind> {
    match value {
        "proposed" => Some(OperationEventKind::Proposed),
        "planned" => Some(OperationEventKind::Planned),
        "approval_requested" => Some(OperationEventKind::ApprovalRequested),
        "approved" => Some(OperationEventKind::Approved),
        "rejected" => Some(OperationEventKind::Rejected),
        "execution_started" => Some(OperationEventKind::ExecutionStarted),
        "progress" => Some(OperationEventKind::Progress),
        "succeeded" => Some(OperationEventKind::Succeeded),
        "failed" => Some(OperationEventKind::Failed),
        "cancelled" => Some(OperationEventKind::Cancelled),
        "outcome_unknown" => Some(OperationEventKind::OutcomeUnknown),
        "expired" => Some(OperationEventKind::Expired),
        _ => None,
    }
}
