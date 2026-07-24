use dopedb_protocol::{
    ConnectionListCommand, ConnectionListResult, ConnectionSelector, ConnectionSelectorArguments,
    ConnectionShowCommand, ConnectionSummary, ConnectionTestCommand, ConnectionTestResult,
    EmptyArguments,
};

use crate::client::{BrokerClient, ClientError};
use crate::output::{self, OutputMode};

pub(crate) async fn list(mode: OutputMode) -> Result<(), ClientError> {
    let client = BrokerClient::discover()?;
    let result: ConnectionListResult = client
        .request::<ConnectionListCommand>(&EmptyArguments::default())
        .await?;
    match mode {
        OutputMode::Json => output::write_json(&result),
        OutputMode::Human => output::write_human(
            &result
                .connections
                .iter()
                .map(|connection| {
                    format!(
                        "{}  {}  {:?}  {}",
                        connection.id, connection.name, connection.engine, connection.database
                    )
                })
                .collect::<Vec<_>>(),
        ),
    }
}

pub(crate) async fn show(selector: &str, mode: OutputMode) -> Result<(), ClientError> {
    let client = BrokerClient::discover()?;
    let selector = resolve_selector(&client, parse_selector(selector)?).await?;
    let result: ConnectionSummary = client
        .request::<ConnectionShowCommand>(&ConnectionSelectorArguments {
            connection: selector,
        })
        .await?;
    write_summary(&result, mode)
}

pub(crate) async fn test(selector: &str, mode: OutputMode) -> Result<(), ClientError> {
    let client = BrokerClient::discover()?;
    let selector = resolve_selector(&client, parse_selector(selector)?).await?;
    let result: ConnectionTestResult = client
        .request::<ConnectionTestCommand>(&ConnectionSelectorArguments {
            connection: selector,
        })
        .await?;
    match mode {
        OutputMode::Json => output::write_json(&result),
        OutputMode::Human => {
            output::write_human(&[format!("{} is reachable", result.connection.name)])
        }
    }
}

pub(crate) fn parse_selector(value: &str) -> Result<ConnectionSelector, ClientError> {
    value.parse().map_err(|_| ClientError::InvalidArguments)
}

pub(crate) async fn resolve_selector(
    client: &BrokerClient,
    selector: ConnectionSelector,
) -> Result<ConnectionSelector, ClientError> {
    let ConnectionSelector::Name(name) = selector else {
        return Ok(selector);
    };
    let result: ConnectionListResult = client
        .request::<ConnectionListCommand>(&EmptyArguments::default())
        .await?;
    let matches = result
        .connections
        .into_iter()
        .filter(|connection| connection.name == name)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [connection] => Ok(ConnectionSelector::Id(connection.id)),
        [] => Err(ClientError::ConnectionNotFound),
        _ => Err(ClientError::AmbiguousConnection(
            matches
                .into_iter()
                .map(|connection| connection.id)
                .collect(),
        )),
    }
}

fn write_summary(result: &ConnectionSummary, mode: OutputMode) -> Result<(), ClientError> {
    match mode {
        OutputMode::Json => output::write_json(result),
        OutputMode::Human => output::write_human(&[
            format!("{} ({})", result.name, result.id),
            format!("{:?} / {}", result.engine, result.database),
            format!(
                "Read-only: {}  Writes allowed: {}",
                result.readonly, result.allow_writes
            ),
        ]),
    }
}
