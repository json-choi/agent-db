//! Secret-free connection selectors and command payloads.

use std::fmt;
use std::str::FromStr;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;
use uuid::Uuid;

use crate::{AuthenticationRequirement, CommandName, CommandSpec, DatabaseEngine, EmptyArguments};

pub const MAX_CONNECTION_NAME_BYTES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionSelector {
    Id(Uuid),
    Name(String),
    Current,
}

impl FromStr for ConnectionSelector {
    type Err = ConnectionSelectorError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value == "current" {
            return Ok(Self::Current);
        }
        if let Some(id) = value.strip_prefix("id:") {
            return Uuid::parse_str(id)
                .map(Self::Id)
                .map_err(|_| ConnectionSelectorError::InvalidId);
        }
        if let Some(name) = value.strip_prefix("name:") {
            if name.is_empty()
                || name.len() > MAX_CONNECTION_NAME_BYTES
                || name.chars().any(char::is_control)
            {
                return Err(ConnectionSelectorError::InvalidName);
            }
            return Ok(Self::Name(name.into()));
        }
        Err(ConnectionSelectorError::MissingKind)
    }
}

impl fmt::Display for ConnectionSelector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Id(id) => write!(formatter, "id:{id}"),
            Self::Name(name) => write!(formatter, "name:{name}"),
            Self::Current => formatter.write_str("current"),
        }
    }
}

impl Serialize for ConnectionSelector {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ConnectionSelector {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ConnectionSelectorError {
    #[error("connection selector must use id:<uuid>, name:<exact-name>, or current")]
    MissingKind,
    #[error("connection id selector is invalid")]
    InvalidId,
    #[error("connection name selector is invalid")]
    InvalidName,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectionSummary {
    pub id: Uuid,
    pub name: String,
    pub engine: DatabaseEngine,
    pub database: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    pub readonly: bool,
    pub allow_writes: bool,
}

pub struct ConnectionListCommand;

impl CommandSpec for ConnectionListCommand {
    type Arguments = EmptyArguments;
    type Result = ConnectionListResult;

    const NAME: CommandName = CommandName::ConnectionList;
    const AUTHENTICATION: AuthenticationRequirement = AuthenticationRequirement::TerminalSession;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectionListResult {
    pub connections: Vec<ConnectionSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectionSelectorArguments {
    pub connection: ConnectionSelector,
}

pub struct ConnectionShowCommand;

impl CommandSpec for ConnectionShowCommand {
    type Arguments = ConnectionSelectorArguments;
    type Result = ConnectionSummary;

    const NAME: CommandName = CommandName::ConnectionShow;
    const AUTHENTICATION: AuthenticationRequirement = AuthenticationRequirement::TerminalSession;
}

pub struct ConnectionTestCommand;

impl CommandSpec for ConnectionTestCommand {
    type Arguments = ConnectionSelectorArguments;
    type Result = ConnectionTestResult;

    const NAME: CommandName = CommandName::ConnectionTest;
    const AUTHENTICATION: AuthenticationRequirement = AuthenticationRequirement::TerminalSession;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectionTestResult {
    pub connection: ConnectionSummary,
    pub reachable: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selector_requires_an_explicit_kind() {
        assert_eq!(
            "7d444840-9dc0-11d1-b245-5ffdce74fad2".parse::<ConnectionSelector>(),
            Err(ConnectionSelectorError::MissingKind)
        );
        assert!(matches!(
            "id:7d444840-9dc0-11d1-b245-5ffdce74fad2".parse(),
            Ok(ConnectionSelector::Id(_))
        ));
        assert_eq!(
            "name:Prod".parse(),
            Ok(ConnectionSelector::Name("Prod".into()))
        );
        assert_eq!("current".parse(), Ok(ConnectionSelector::Current));
    }

    #[test]
    fn selector_round_trips_as_one_stable_string() {
        let selector = ConnectionSelector::Name("Prod DB".into());
        let encoded = serde_json::to_string(&selector).unwrap();
        assert_eq!(encoded, r#""name:Prod DB""#);
        assert_eq!(
            serde_json::from_str::<ConnectionSelector>(&encoded).unwrap(),
            selector
        );
    }
}
