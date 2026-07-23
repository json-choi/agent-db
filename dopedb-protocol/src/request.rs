//! Versioned request envelope and command names for the local broker.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;
use zeroize::Zeroizing;

/// Version-1 command catalog. Any addition, removal, or meaning change requires a
/// command-schema version bump and an explicitly negotiated compatibility range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CommandName {
    #[serde(rename = "version")]
    Version,
    #[serde(rename = "status")]
    Status,
    #[serde(rename = "app.open")]
    AppOpen,
    #[serde(rename = "skills.list")]
    SkillsList,
    #[serde(rename = "skills.get")]
    SkillsGet,
    #[serde(rename = "skill.status")]
    SkillStatus,
    #[serde(rename = "skill.install")]
    SkillInstall,
    #[serde(rename = "skill.repair")]
    SkillRepair,
    #[serde(rename = "skill.remove")]
    SkillRemove,
    #[serde(rename = "connection.list")]
    ConnectionList,
    #[serde(rename = "connection.show")]
    ConnectionShow,
    #[serde(rename = "connection.test")]
    ConnectionTest,
    #[serde(rename = "catalog.show")]
    CatalogShow,
    #[serde(rename = "schema.list")]
    SchemaList,
    #[serde(rename = "table.describe")]
    TableDescribe,
    #[serde(rename = "query.plan")]
    QueryPlan,
    #[serde(rename = "query.run")]
    QueryRun,
    #[serde(rename = "query.cancel")]
    QueryCancel,
    #[serde(rename = "sql.propose")]
    SqlPropose,
    #[serde(rename = "operation.show")]
    OperationShow,
    #[serde(rename = "operation.wait")]
    OperationWait,
    #[serde(rename = "operation.cancel")]
    OperationCancel,
}

impl CommandName {
    pub const ALL: [Self; 22] = [
        Self::Version,
        Self::Status,
        Self::AppOpen,
        Self::SkillsList,
        Self::SkillsGet,
        Self::SkillStatus,
        Self::SkillInstall,
        Self::SkillRepair,
        Self::SkillRemove,
        Self::ConnectionList,
        Self::ConnectionShow,
        Self::ConnectionTest,
        Self::CatalogShow,
        Self::SchemaList,
        Self::TableDescribe,
        Self::QueryPlan,
        Self::QueryRun,
        Self::QueryCancel,
        Self::SqlPropose,
        Self::OperationShow,
        Self::OperationWait,
        Self::OperationCancel,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Version => "version",
            Self::Status => "status",
            Self::AppOpen => "app.open",
            Self::SkillsList => "skills.list",
            Self::SkillsGet => "skills.get",
            Self::SkillStatus => "skill.status",
            Self::SkillInstall => "skill.install",
            Self::SkillRepair => "skill.repair",
            Self::SkillRemove => "skill.remove",
            Self::ConnectionList => "connection.list",
            Self::ConnectionShow => "connection.show",
            Self::ConnectionTest => "connection.test",
            Self::CatalogShow => "catalog.show",
            Self::SchemaList => "schema.list",
            Self::TableDescribe => "table.describe",
            Self::QueryPlan => "query.plan",
            Self::QueryRun => "query.run",
            Self::QueryCancel => "query.cancel",
            Self::SqlPropose => "sql.propose",
            Self::OperationShow => "operation.show",
            Self::OperationWait => "operation.wait",
            Self::OperationCancel => "operation.cancel",
        }
    }
}

impl fmt::Display for CommandName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Terminal-scoped broker authentication. The token is an ephemeral local
/// capability, not a database credential. Its Debug representation is always redacted.
#[derive(PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionAuthentication {
    pub terminal_session_id: Uuid,
    token: Zeroizing<String>,
}

impl SessionAuthentication {
    pub fn new(terminal_session_id: Uuid, token: impl Into<String>) -> Self {
        Self {
            terminal_session_id,
            token: Zeroizing::new(token.into()),
        }
    }

    pub fn token(&self) -> &str {
        &self.token
    }
}

impl fmt::Debug for SessionAuthentication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionAuthentication")
            .field("terminal_session_id", &self.terminal_session_id)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// One length-prefixed broker control request.
#[derive(PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestEnvelope {
    pub protocol_version: u16,
    pub command_schema_version: u16,
    pub request_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authentication: Option<SessionAuthentication>,
    pub command: CommandName,
    #[serde(default)]
    pub arguments: Value,
}

impl fmt::Debug for RequestEnvelope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RequestEnvelope")
            .field("protocol_version", &self.protocol_version)
            .field("command_schema_version", &self.command_schema_version)
            .field("request_id", &self.request_id)
            .field("authentication", &self.authentication)
            .field("command", &self.command)
            .field("arguments", &"<redacted>")
            .finish()
    }
}
