use std::time::Duration;

use dopedb_protocol::{
    OperationArguments, OperationCancelCommand, OperationShowCommand, OperationSummary,
    OperationWaitArguments, OperationWaitCommand,
};

use crate::client::{BrokerClient, ClientError};
use crate::commands::query::{parse_uuid, write_operation};
use crate::output::OutputMode;

pub(crate) async fn show(operation_id: &str, mode: OutputMode) -> Result<(), ClientError> {
    let operation_id = parse_uuid(operation_id)?;
    let client = BrokerClient::discover()?;
    let result: OperationSummary = client
        .request::<OperationShowCommand>(&OperationArguments { operation_id })
        .await?;
    write_operation(&result, mode)
}

pub(crate) async fn wait(
    operation_id: &str,
    timeout_ms: u64,
    mode: OutputMode,
) -> Result<(), ClientError> {
    if timeout_ms == 0 || Duration::from_millis(timeout_ms) > Duration::from_secs(30) {
        return Err(ClientError::InvalidArguments);
    }
    let operation_id = parse_uuid(operation_id)?;
    let client = BrokerClient::discover()?;
    let result: OperationSummary = client
        .request::<OperationWaitCommand>(&OperationWaitArguments {
            operation_id,
            timeout_ms,
        })
        .await?;
    write_operation(&result, mode)
}

pub(crate) async fn cancel(operation_id: &str, mode: OutputMode) -> Result<(), ClientError> {
    let operation_id = parse_uuid(operation_id)?;
    let client = BrokerClient::discover()?;
    let result: OperationSummary = client
        .request::<OperationCancelCommand>(&OperationArguments { operation_id })
        .await?;
    write_operation(&result, mode)
}
