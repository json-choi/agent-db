use dopedb_protocol::{
    CatalogArguments, CatalogShowCommand, CatalogSnapshot, SchemaListCommand, SchemaListResult,
    TableDescribeArguments, TableDescribeCommand, TableDescribeResult,
};

use crate::client::{BrokerClient, ClientError};
use crate::commands::connection::{parse_selector, resolve_selector};
use crate::output::{self, OutputMode};

pub(crate) async fn show(connection: &str, mode: OutputMode) -> Result<(), ClientError> {
    let client = BrokerClient::discover()?;
    let connection = resolve_selector(&client, parse_selector(connection)?).await?;
    let result: CatalogSnapshot = client
        .request::<CatalogShowCommand>(&CatalogArguments { connection })
        .await?;
    match mode {
        OutputMode::Json => output::write_json(&result),
        OutputMode::Human => output::write_human(&[
            format!("Database: {}", result.database()),
            format!("Fingerprint: {}", result.fingerprint()),
            format!(
                "{} schemas, {} relations, {} routines, {} other objects",
                result.namespaces().len(),
                result.relations().len(),
                result.routines().len(),
                result.other_objects().len()
            ),
        ]),
    }
}

pub(crate) async fn schemas(connection: &str, mode: OutputMode) -> Result<(), ClientError> {
    let client = BrokerClient::discover()?;
    let connection = resolve_selector(&client, parse_selector(connection)?).await?;
    let result: SchemaListResult = client
        .request::<SchemaListCommand>(&CatalogArguments { connection })
        .await?;
    match mode {
        OutputMode::Json => output::write_json(&result),
        OutputMode::Human => output::write_human(
            &result
                .schemas
                .iter()
                .map(|schema| {
                    format!(
                        "{}  {} relations  {} routines  {} objects",
                        schema.name,
                        schema.relation_count,
                        schema.routine_count,
                        schema.object_count
                    )
                })
                .collect::<Vec<_>>(),
        ),
    }
}

pub(crate) async fn describe(
    connection: &str,
    table: String,
    mode: OutputMode,
) -> Result<(), ClientError> {
    let client = BrokerClient::discover()?;
    let connection = resolve_selector(&client, parse_selector(connection)?).await?;
    let result: TableDescribeResult = client
        .request::<TableDescribeCommand>(&TableDescribeArguments { connection, table })
        .await?;
    match mode {
        OutputMode::Json => output::write_json(&result),
        OutputMode::Human => {
            let relation = &result.relation;
            let qualified = relation
                .object
                .namespace
                .as_ref()
                .map(|namespace| format!("{namespace}.{}", relation.object.name))
                .unwrap_or_else(|| relation.object.name.clone());
            let mut lines = vec![qualified];
            lines.extend(relation.columns.iter().map(|column| {
                format!(
                    "  {}  {}{}",
                    column.name,
                    column.native_type,
                    if column.nullable { "" } else { " NOT NULL" }
                )
            }));
            output::write_human(&lines)
        }
    }
}
