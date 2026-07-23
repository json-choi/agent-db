//! Stable operation lifecycle vocabulary shared with CLI status responses.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    ReadQuery,
    DocumentRead,
    WriteSql,
    Ddl,
    Privilege,
    SqlScript,
    TableDataChange,
    SchemaChange,
    Import,
    Export,
    Migration,
    DashboardCreate,
    PluginAction,
    ProviderAction,
}

impl OperationKind {
    pub const fn is_read(self) -> bool {
        matches!(self, Self::ReadQuery | Self::DocumentRead)
    }

    pub const fn is_resumable_job(self) -> bool {
        matches!(self, Self::Import | Self::Export)
    }

    /// Whether an interrupted execution may already have changed the target DB.
    pub const fn may_mutate_target(self) -> bool {
        matches!(
            self,
            Self::WriteSql
                | Self::Ddl
                | Self::Privilege
                | Self::SqlScript
                | Self::TableDataChange
                | Self::SchemaChange
                | Self::Import
                | Self::Migration
                | Self::PluginAction
                | Self::ProviderAction
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationActorKind {
    LocalUser,
    WorkspaceUser,
    Agent,
    Plugin,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    Planned,
    PendingApproval,
    Ready,
    Approved,
    Rejected,
    Expired,
    Cancelled,
    Executing,
    Succeeded,
    Failed,
    OutcomeUnknown,
}

impl OperationState {
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Rejected
                | Self::Expired
                | Self::Cancelled
                | Self::Succeeded
                | Self::Failed
                | Self::OutcomeUnknown
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationEventKind {
    Proposed,
    Planned,
    ApprovalRequested,
    Approved,
    Rejected,
    ExecutionStarted,
    Progress,
    Succeeded,
    Failed,
    Cancelled,
    OutcomeUnknown,
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationRiskLevel {
    Low,
    Medium,
    High,
    Critical,
}
