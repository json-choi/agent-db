//! Typed command payloads shared by the Desktop broker and CLI.
//!
//! The outer envelope intentionally carries a JSON value for protocol evolution,
//! but an active dispatcher must decode through one of these closed command specs
//! before it can reach an application service.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::{CommandName, RequestEnvelope};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthenticationRequirement {
    None,
    TerminalSession,
}

pub trait CommandSpec {
    type Arguments: Serialize + DeserializeOwned;
    type Result: Serialize + DeserializeOwned;

    const NAME: CommandName;
    const AUTHENTICATION: AuthenticationRequirement;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CommandPayloadError {
    #[error("request command does not match the typed command payload")]
    CommandMismatch,
    #[error("request arguments do not match the typed command payload")]
    InvalidArguments,
}

pub fn decode_arguments<C: CommandSpec>(
    request: &RequestEnvelope,
) -> Result<C::Arguments, CommandPayloadError> {
    if request.command != C::NAME {
        return Err(CommandPayloadError::CommandMismatch);
    }
    serde_json::from_value(request.arguments.clone())
        .map_err(|_| CommandPayloadError::InvalidArguments)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmptyArguments {}

pub struct VersionCommand;

impl CommandSpec for VersionCommand {
    type Arguments = EmptyArguments;
    type Result = VersionResult;

    const NAME: CommandName = CommandName::Version;
    const AUTHENTICATION: AuthenticationRequirement = AuthenticationRequirement::None;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VersionResult {
    pub app_version: String,
    pub protocol_min: u16,
    pub protocol_max: u16,
    pub command_schema_version: u16,
    pub runtime_id: Uuid,
}

pub struct StatusCommand;

impl CommandSpec for StatusCommand {
    type Arguments = EmptyArguments;
    type Result = StatusResult;

    const NAME: CommandName = CommandName::Status;
    const AUTHENTICATION: AuthenticationRequirement = AuthenticationRequirement::None;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StatusResult {
    pub app_version: String,
    pub protocol_min: u16,
    pub protocol_max: u16,
    pub runtime_id: Uuid,
}

pub struct AppOpenCommand;

impl CommandSpec for AppOpenCommand {
    type Arguments = AppOpenArguments;
    type Result = AppOpenResult;

    const NAME: CommandName = CommandName::AppOpen;
    const AUTHENTICATION: AuthenticationRequirement = AuthenticationRequirement::None;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppOpenArguments {
    #[serde(default)]
    pub wait: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppOpenResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_id: Option<Uuid>,
    pub launched: bool,
    pub ready: bool,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{COMMAND_SCHEMA_VERSION, PROTOCOL_MAX};

    fn request(command: CommandName, arguments: serde_json::Value) -> RequestEnvelope {
        RequestEnvelope {
            protocol_version: PROTOCOL_MAX,
            command_schema_version: COMMAND_SCHEMA_VERSION,
            request_id: Uuid::from_u128(1),
            authentication: None,
            command,
            arguments,
        }
    }

    #[test]
    fn active_empty_payloads_reject_unknown_fields() {
        let request = request(CommandName::Status, json!({"approved": true}));
        assert_eq!(
            decode_arguments::<StatusCommand>(&request),
            Err(CommandPayloadError::InvalidArguments)
        );
    }

    #[test]
    fn typed_decoder_rejects_a_different_command() {
        let request = request(CommandName::Version, json!({}));
        assert_eq!(
            decode_arguments::<StatusCommand>(&request),
            Err(CommandPayloadError::CommandMismatch)
        );
    }
}
