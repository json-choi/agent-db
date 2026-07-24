//! Typed read-plan, read-run, SQL-proposal, and cancellation payloads.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    AuthenticationRequirement, CommandName, CommandSpec, ConnectionSelector, OperationSummary,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueryPlanArguments {
    pub connection: ConnectionSelector,
    pub sql: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rows: Option<u64>,
}

pub struct QueryPlanCommand;

impl CommandSpec for QueryPlanCommand {
    type Arguments = QueryPlanArguments;
    type Result = QueryPlanResult;

    const NAME: CommandName = CommandName::QueryPlan;
    const AUTHENTICATION: AuthenticationRequirement = AuthenticationRequirement::TerminalSession;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueryHealth {
    pub level: String,
    pub coverage: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_connections: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_connections: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_usage_percent: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_queries: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub long_running_queries: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_waits: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replication_lag_seconds: Option<f64>,
    pub reasons: Vec<String>,
    pub captured_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueryPlanResult {
    pub connection_id: Uuid,
    pub connection_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    pub plan_id: Uuid,
    pub decision: String,
    pub notices: Vec<String>,
    pub suggestions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_rows: Option<i64>,
    pub health: QueryHealth,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueryRunArguments {
    pub plan_id: Uuid,
}

pub struct QueryRunCommand;

impl CommandSpec for QueryRunCommand {
    type Arguments = QueryRunArguments;
    type Result = QueryRunResult;

    const NAME: CommandName = CommandName::QueryRun;
    const AUTHENTICATION: AuthenticationRequirement = AuthenticationRequirement::TerminalSession;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueryResultPage {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    pub row_count: usize,
    pub truncated: bool,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueryRunResult {
    pub connection_id: Uuid,
    pub connection_name: String,
    pub plan_id: Uuid,
    pub query_run_id: Uuid,
    pub planning_decision: String,
    pub result: QueryResultPage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueryCancelArguments {
    pub operation_id: Uuid,
}

pub struct QueryCancelCommand;

impl CommandSpec for QueryCancelCommand {
    type Arguments = QueryCancelArguments;
    type Result = OperationSummary;

    const NAME: CommandName = CommandName::QueryCancel;
    const AUTHENTICATION: AuthenticationRequirement = AuthenticationRequirement::TerminalSession;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SqlProposeArguments {
    pub connection: ConnectionSelector,
    pub sql: String,
}

pub struct SqlProposeCommand;

impl CommandSpec for SqlProposeCommand {
    type Arguments = SqlProposeArguments;
    type Result = OperationSummary;

    const NAME: CommandName = CommandName::SqlPropose;
    const AUTHENTICATION: AuthenticationRequirement = AuthenticationRequirement::TerminalSession;
}
