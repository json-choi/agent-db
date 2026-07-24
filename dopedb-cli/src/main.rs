mod args;
mod client;
mod commands;
mod exit_code;
mod output;

use std::process::ExitCode;

use clap::Parser;

use args::{
    AppCommand, CatalogCommand, Cli, Command, ConnectionCommand, OperationCommand, QueryCommand,
    SchemaCommand, SqlCommand, TableCommand,
};
use output::OutputMode;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Version(arguments) => {
            commands::app::version(OutputMode::from_json_flag(arguments.json)).await
        }
        Command::Status(arguments) => {
            commands::app::status(OutputMode::from_json_flag(arguments.json)).await
        }
        Command::Completion { shell } => commands::completion::write(shell),
        Command::App(arguments) => match arguments.command {
            AppCommand::Open { wait, output } => {
                commands::app::open(wait, OutputMode::from_json_flag(output.json)).await
            }
        },
        Command::Connection(arguments) => match arguments.command {
            ConnectionCommand::List(output) => {
                commands::connection::list(OutputMode::from_json_flag(output.json)).await
            }
            ConnectionCommand::Show { selector, output } => {
                commands::connection::show(&selector, OutputMode::from_json_flag(output.json)).await
            }
            ConnectionCommand::Test { selector, output } => {
                commands::connection::test(&selector, OutputMode::from_json_flag(output.json)).await
            }
        },
        Command::Catalog(arguments) => match arguments.command {
            CatalogCommand::Show { connection, output } => {
                commands::catalog::show(&connection, OutputMode::from_json_flag(output.json)).await
            }
        },
        Command::Schema(arguments) => match arguments.command {
            SchemaCommand::List { connection, output } => {
                commands::catalog::schemas(&connection, OutputMode::from_json_flag(output.json))
                    .await
            }
        },
        Command::Table(arguments) => match arguments.command {
            TableCommand::Describe {
                table,
                connection,
                output,
            } => {
                commands::catalog::describe(
                    &connection,
                    table,
                    OutputMode::from_json_flag(output.json),
                )
                .await
            }
        },
        Command::Query(arguments) => match arguments.command {
            QueryCommand::Plan {
                connection,
                file,
                max_rows,
                output,
            } => {
                commands::query::plan(
                    &connection,
                    &file,
                    max_rows,
                    OutputMode::from_json_flag(output.json),
                )
                .await
            }
            QueryCommand::Run { plan, output } => {
                commands::query::run(&plan, OutputMode::from_json_flag(output.json)).await
            }
            QueryCommand::Cancel {
                operation_id,
                output,
            } => {
                commands::query::cancel(&operation_id, OutputMode::from_json_flag(output.json))
                    .await
            }
        },
        Command::Sql(arguments) => match arguments.command {
            SqlCommand::Propose {
                connection,
                file,
                output,
            } => {
                commands::query::propose(
                    &connection,
                    &file,
                    OutputMode::from_json_flag(output.json),
                )
                .await
            }
        },
        Command::Operation(arguments) => match arguments.command {
            OperationCommand::Show {
                operation_id,
                output,
            } => {
                commands::operation::show(&operation_id, OutputMode::from_json_flag(output.json))
                    .await
            }
            OperationCommand::Wait {
                operation_id,
                timeout_ms,
                output,
            } => {
                commands::operation::wait(
                    &operation_id,
                    timeout_ms,
                    OutputMode::from_json_flag(output.json),
                )
                .await
            }
            OperationCommand::Cancel {
                operation_id,
                output,
            } => {
                commands::operation::cancel(&operation_id, OutputMode::from_json_flag(output.json))
                    .await
            }
        },
    };
    match result {
        Ok(()) => ExitCode::from(exit_code::SUCCESS),
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(exit_code::for_client_error(&error))
        }
    }
}
