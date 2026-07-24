//! Typed catalog, schema, and relation command payloads.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    AuthenticationRequirement, CatalogSnapshot, CommandName, CommandSpec, ConnectionSelector,
    Relation,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogArguments {
    pub connection: ConnectionSelector,
}

pub struct CatalogShowCommand;

impl CommandSpec for CatalogShowCommand {
    type Arguments = CatalogArguments;
    type Result = CatalogSnapshot;

    const NAME: CommandName = CommandName::CatalogShow;
    const AUTHENTICATION: AuthenticationRequirement = AuthenticationRequirement::TerminalSession;
}

pub struct SchemaListCommand;

impl CommandSpec for SchemaListCommand {
    type Arguments = CatalogArguments;
    type Result = SchemaListResult;

    const NAME: CommandName = CommandName::SchemaList;
    const AUTHENTICATION: AuthenticationRequirement = AuthenticationRequirement::TerminalSession;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SchemaSummary {
    pub name: String,
    pub relation_count: u64,
    pub routine_count: u64,
    pub object_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SchemaListResult {
    pub connection_id: Uuid,
    pub schemas: Vec<SchemaSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TableDescribeArguments {
    pub connection: ConnectionSelector,
    pub table: String,
}

pub struct TableDescribeCommand;

impl CommandSpec for TableDescribeCommand {
    type Arguments = TableDescribeArguments;
    type Result = TableDescribeResult;

    const NAME: CommandName = CommandName::TableDescribe;
    const AUTHENTICATION: AuthenticationRequirement = AuthenticationRequirement::TerminalSession;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TableDescribeResult {
    pub connection_id: Uuid,
    pub relation: Relation,
}
