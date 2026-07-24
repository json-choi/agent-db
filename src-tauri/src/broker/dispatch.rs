//! Broker envelope validation and transport-only application-service dispatch.

use std::collections::BTreeMap;
use std::time::Duration;

use dopedb_protocol::{
    decode_arguments, encode_frame, AppOpenCommand, AppOpenResult, CatalogArguments,
    CatalogShowCommand, CatalogSnapshot, CommandName, CommandSpec, ConnectionListCommand,
    ConnectionListResult, ConnectionSelector, ConnectionSelectorArguments, ConnectionShowCommand,
    ConnectionSummary, ConnectionTestCommand, ConnectionTestResult, DatabaseEngine, EmptyArguments,
    ErrorCode, OperationCancelCommand, OperationShowCommand, OperationSummary,
    OperationWaitArguments, OperationWaitCommand, ProtocolError, QueryCancelCommand, QueryHealth,
    QueryPlanArguments, QueryPlanCommand, QueryPlanResult, QueryResultPage, QueryRunArguments,
    QueryRunCommand, QueryRunResult, RequestEnvelope, ResponseEnvelope, SchemaListCommand,
    SchemaListResult, SchemaSummary, SqlProposeArguments, SqlProposeCommand, StatusCommand,
    StatusResult, TableDescribeArguments, TableDescribeCommand, TableDescribeResult,
    VersionCommand, VersionResult, COMMAND_SCHEMA_VERSION, MAX_RESPONSE_BYTES, MAX_STRING_BYTES,
    PROTOCOL_MAX, PROTOCOL_MIN,
};
use serde::Serialize;
use tauri::Manager;
use uuid::Uuid;

use crate::error::AppError;
use crate::model::{Engine, QueryResult};
use crate::monitoring::HealthSnapshot;
use crate::services::{
    AgentConnectionSummary, AgentQueryPlanError, AgentQueryRunError, AgentQueryRunPrepareError,
    ApplicationServices, CatalogReadPolicy, CliConnectionResolutionError, TerminalAuthority,
    TerminalQueryPlanRequest, TerminalSqlProposalRequest,
};

use super::session::{AuthenticatedSession, BrokerCapability, BrokerSessionRegistry};

const MAX_SQL_BYTES: usize = MAX_STRING_BYTES;
const MAX_TABLE_SELECTOR_BYTES: usize = 512;
const MAX_OPERATION_WAIT: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub(crate) struct BrokerDispatcher {
    runtime_id: Uuid,
    app_version: &'static str,
    sessions: BrokerSessionRegistry,
    services: Option<ApplicationServices>,
    app_handle: Option<tauri::AppHandle>,
}

impl BrokerDispatcher {
    pub(crate) fn new(
        runtime_id: Uuid,
        app_version: &'static str,
        sessions: BrokerSessionRegistry,
        services: Option<ApplicationServices>,
        app_handle: Option<tauri::AppHandle>,
    ) -> Self {
        Self {
            runtime_id,
            app_version,
            sessions,
            services,
            app_handle,
        }
    }

    pub(crate) async fn dispatch(&self, request: RequestEnvelope) -> ResponseEnvelope {
        let requested_protocol = request.protocol_version;
        let response_protocol = if (PROTOCOL_MIN..=PROTOCOL_MAX).contains(&requested_protocol) {
            requested_protocol
        } else {
            PROTOCOL_MAX
        };
        let response = self.dispatch_current(request).await;
        response_at_protocol(response, response_protocol)
    }

    async fn dispatch_current(&self, request: RequestEnvelope) -> ResponseEnvelope {
        let request_id = request.request_id;
        let client_protocol_version = request.protocol_version;
        if request.protocol_version < PROTOCOL_MIN
            || request.protocol_version > PROTOCOL_MAX
            || request.command_schema_version != COMMAND_SCHEMA_VERSION
        {
            return failure(request_id, ErrorCode::ProtocolMismatch, false);
        }

        match request.command {
            CommandName::Version => self.execute_public::<VersionCommand>(
                &request,
                VersionResult {
                    app_version: self.app_version.into(),
                    protocol_min: PROTOCOL_MIN,
                    protocol_max: PROTOCOL_MAX,
                    command_schema_version: COMMAND_SCHEMA_VERSION,
                    runtime_id: self.runtime_id,
                },
            ),
            CommandName::Status => self.execute_public::<StatusCommand>(
                &request,
                StatusResult {
                    app_version: self.app_version.into(),
                    protocol_min: PROTOCOL_MIN,
                    protocol_max: PROTOCOL_MAX,
                    runtime_id: self.runtime_id,
                },
            ),
            CommandName::AppOpen => {
                let arguments = match decode_arguments::<AppOpenCommand>(&request) {
                    Ok(arguments) => arguments,
                    Err(_) => return failure(request_id, ErrorCode::InvalidRequest, false),
                };
                let _wait = arguments.wait;
                respond(request_id, self.focus_app())
            }
            CommandName::ConnectionList => {
                let session = match self.authenticate(&request, BrokerCapability::ConnectionRead) {
                    Ok(session) => session,
                    Err(code) => return failure(request_id, code, false),
                };
                if decode_arguments::<ConnectionListCommand>(&request).is_err() {
                    return failure(request_id, ErrorCode::InvalidRequest, false);
                }
                respond(
                    request_id,
                    self.connection_list(&session, client_protocol_version)
                        .await,
                )
            }
            CommandName::ConnectionShow => {
                let session = match self.authenticate(&request, BrokerCapability::ConnectionRead) {
                    Ok(session) => session,
                    Err(code) => return failure(request_id, code, false),
                };
                let arguments = match decode_arguments::<ConnectionShowCommand>(&request) {
                    Ok(arguments) => arguments,
                    Err(_) => return failure(request_id, ErrorCode::InvalidRequest, false),
                };
                respond(
                    request_id,
                    self.resolve_connection(
                        &session,
                        &arguments.connection,
                        client_protocol_version,
                    )
                    .await,
                )
            }
            CommandName::ConnectionTest => {
                let session = match self.authenticate(&request, BrokerCapability::ConnectionTest) {
                    Ok(session) => session,
                    Err(code) => return failure(request_id, code, false),
                };
                let arguments = match decode_arguments::<ConnectionTestCommand>(&request) {
                    Ok(arguments) => arguments,
                    Err(_) => return failure(request_id, ErrorCode::InvalidRequest, false),
                };
                respond(
                    request_id,
                    self.connection_test(&session, &arguments, client_protocol_version)
                        .await,
                )
            }
            CommandName::CatalogShow => {
                let session = match self.authenticate(&request, BrokerCapability::CatalogRead) {
                    Ok(session) => session,
                    Err(code) => return failure(request_id, code, false),
                };
                let arguments = match decode_arguments::<CatalogShowCommand>(&request) {
                    Ok(arguments) => arguments,
                    Err(_) => return failure(request_id, ErrorCode::InvalidRequest, false),
                };
                respond(
                    request_id,
                    self.catalog(&session, &arguments, client_protocol_version)
                        .await,
                )
            }
            CommandName::SchemaList => {
                let session = match self.authenticate(&request, BrokerCapability::CatalogRead) {
                    Ok(session) => session,
                    Err(code) => return failure(request_id, code, false),
                };
                let arguments = match decode_arguments::<SchemaListCommand>(&request) {
                    Ok(arguments) => arguments,
                    Err(_) => return failure(request_id, ErrorCode::InvalidRequest, false),
                };
                respond(
                    request_id,
                    self.schema_list(&session, &arguments, client_protocol_version)
                        .await,
                )
            }
            CommandName::TableDescribe => {
                let session = match self.authenticate(&request, BrokerCapability::CatalogRead) {
                    Ok(session) => session,
                    Err(code) => return failure(request_id, code, false),
                };
                let arguments = match decode_arguments::<TableDescribeCommand>(&request) {
                    Ok(arguments) => arguments,
                    Err(_) => return failure(request_id, ErrorCode::InvalidRequest, false),
                };
                respond(
                    request_id,
                    self.table_describe(&session, &arguments, client_protocol_version)
                        .await,
                )
            }
            CommandName::QueryPlan => {
                let session = match self.authenticate(&request, BrokerCapability::QueryPlan) {
                    Ok(session) => session,
                    Err(code) => return failure(request_id, code, false),
                };
                let arguments = match decode_arguments::<QueryPlanCommand>(&request) {
                    Ok(arguments) => arguments,
                    Err(_) => return failure(request_id, ErrorCode::InvalidRequest, false),
                };
                respond(
                    request_id,
                    self.query_plan(&session, arguments, client_protocol_version)
                        .await,
                )
            }
            CommandName::QueryRun => {
                let session = match self.authenticate(&request, BrokerCapability::QueryRun) {
                    Ok(session) => session,
                    Err(code) => return failure(request_id, code, false),
                };
                let arguments = match decode_arguments::<QueryRunCommand>(&request) {
                    Ok(arguments) => arguments,
                    Err(_) => return failure(request_id, ErrorCode::InvalidRequest, false),
                };
                respond(
                    request_id,
                    self.query_run(&session, arguments, client_protocol_version)
                        .await,
                )
            }
            CommandName::QueryCancel => {
                let session = match self.authenticate(&request, BrokerCapability::OperationCancel) {
                    Ok(session) => session,
                    Err(code) => return failure(request_id, code, false),
                };
                let arguments = match decode_arguments::<QueryCancelCommand>(&request) {
                    Ok(arguments) => arguments,
                    Err(_) => return failure(request_id, ErrorCode::InvalidRequest, false),
                };
                respond(
                    request_id,
                    self.cancel_operation(
                        &session,
                        arguments.operation_id,
                        client_protocol_version,
                    )
                    .await,
                )
            }
            CommandName::SqlPropose => {
                let session = match self.authenticate(&request, BrokerCapability::SqlPropose) {
                    Ok(session) => session,
                    Err(code) => return failure(request_id, code, false),
                };
                let arguments = match decode_arguments::<SqlProposeCommand>(&request) {
                    Ok(arguments) => arguments,
                    Err(_) => return failure(request_id, ErrorCode::InvalidRequest, false),
                };
                respond(
                    request_id,
                    self.sql_propose(&session, arguments, client_protocol_version)
                        .await,
                )
            }
            CommandName::OperationShow => {
                let session = match self.authenticate(&request, BrokerCapability::OperationRead) {
                    Ok(session) => session,
                    Err(code) => return failure(request_id, code, false),
                };
                let arguments = match decode_arguments::<OperationShowCommand>(&request) {
                    Ok(arguments) => arguments,
                    Err(_) => return failure(request_id, ErrorCode::InvalidRequest, false),
                };
                respond(
                    request_id,
                    self.show_operation(&session, arguments.operation_id, client_protocol_version)
                        .await,
                )
            }
            CommandName::OperationWait => {
                let session = match self.authenticate(&request, BrokerCapability::OperationRead) {
                    Ok(session) => session,
                    Err(code) => return failure(request_id, code, false),
                };
                let arguments = match decode_arguments::<OperationWaitCommand>(&request) {
                    Ok(arguments) => arguments,
                    Err(_) => return failure(request_id, ErrorCode::InvalidRequest, false),
                };
                respond(
                    request_id,
                    self.wait_operation(&session, arguments, client_protocol_version)
                        .await,
                )
            }
            CommandName::OperationCancel => {
                let session = match self.authenticate(&request, BrokerCapability::OperationCancel) {
                    Ok(session) => session,
                    Err(code) => return failure(request_id, code, false),
                };
                let arguments = match decode_arguments::<OperationCancelCommand>(&request) {
                    Ok(arguments) => arguments,
                    Err(_) => return failure(request_id, ErrorCode::InvalidRequest, false),
                };
                respond(
                    request_id,
                    self.cancel_operation(
                        &session,
                        arguments.operation_id,
                        client_protocol_version,
                    )
                    .await,
                )
            }
            CommandName::SkillsList
            | CommandName::SkillsGet
            | CommandName::SkillStatus
            | CommandName::SkillInstall
            | CommandName::SkillRepair
            | CommandName::SkillRemove
            | CommandName::Unknown => failure(request_id, ErrorCode::InvalidRequest, false),
        }
    }

    fn execute_public<C>(&self, request: &RequestEnvelope, result: C::Result) -> ResponseEnvelope
    where
        C: CommandSpec<Arguments = EmptyArguments>,
    {
        if decode_arguments::<C>(request).is_err() {
            return failure(request.request_id, ErrorCode::InvalidRequest, false);
        }
        success(request.request_id, &result)
    }

    fn authenticate(
        &self,
        request: &RequestEnvelope,
        capability: BrokerCapability,
    ) -> Result<AuthenticatedSession, ErrorCode> {
        let authentication = request
            .authentication
            .as_ref()
            .ok_or(ErrorCode::AuthenticationDenied)?;
        let session = self
            .sessions
            .authenticate(authentication)
            .map_err(|_| ErrorCode::AuthenticationDenied)?;
        session
            .require(capability)
            .map_err(|_| ErrorCode::ScopeDenied)?;
        Ok(session)
    }

    fn services(&self) -> Result<&ApplicationServices, ErrorCode> {
        self.services.as_ref().ok_or(ErrorCode::Internal)
    }

    fn focus_app(&self) -> Result<AppOpenResult, ErrorCode> {
        let app_handle = self.app_handle.as_ref().ok_or(ErrorCode::Internal)?;
        let window = app_handle
            .get_webview_window("main")
            .ok_or(ErrorCode::Internal)?;
        window.show().map_err(|_| ErrorCode::Internal)?;
        window.unminimize().map_err(|_| ErrorCode::Internal)?;
        window.set_focus().map_err(|_| ErrorCode::Internal)?;
        Ok(AppOpenResult {
            runtime_id: Some(self.runtime_id),
            launched: false,
            ready: true,
        })
    }

    async fn connection_list(
        &self,
        session: &AuthenticatedSession,
        client_protocol_version: u16,
    ) -> Result<ConnectionListResult, ErrorCode> {
        let services = self.services()?;
        let authority = terminal_authority(session, client_protocol_version);
        let connections = services
            .connections
            .list_terminal_summaries(&authority)
            .await
            .map_err(|_| ErrorCode::ScopeDenied)?
            .iter()
            .map(connection_summary)
            .collect();
        Ok(ConnectionListResult { connections })
    }

    async fn resolve_connection(
        &self,
        session: &AuthenticatedSession,
        selector: &ConnectionSelector,
        client_protocol_version: u16,
    ) -> Result<ConnectionSummary, ErrorCode> {
        let services = self.services()?;
        let authority = terminal_authority(session, client_protocol_version);
        let current = services
            .connections
            .terminal_summary(&authority)
            .await
            .map_err(|_| ErrorCode::ScopeDenied)?;
        match selector {
            ConnectionSelector::Current => Ok(connection_summary(&current)),
            ConnectionSelector::Id(id) => {
                if *id == current.id {
                    Ok(connection_summary(&current))
                } else {
                    Err(ErrorCode::ScopeDenied)
                }
            }
            ConnectionSelector::Name(_) => {
                let resolved = services
                    .connections
                    .resolve_terminal_cli(&authority, selector)
                    .await
                    .map_err(map_application_error)?;
                match resolved {
                    Ok(resolved) if resolved.id == current.id => Ok(connection_summary(&resolved)),
                    Ok(_) => Err(ErrorCode::ScopeDenied),
                    Err(CliConnectionResolutionError::NoMatch)
                    | Err(CliConnectionResolutionError::Ambiguous { .. }) => {
                        Err(ErrorCode::InvalidRequest)
                    }
                }
            }
        }
    }

    async fn connection_test(
        &self,
        session: &AuthenticatedSession,
        arguments: &ConnectionSelectorArguments,
        client_protocol_version: u16,
    ) -> Result<ConnectionTestResult, ErrorCode> {
        let connection = self
            .resolve_connection(session, &arguments.connection, client_protocol_version)
            .await?;
        let services = self.services()?;
        services
            .connections
            .test_terminal(&terminal_authority(session, client_protocol_version))
            .await
            .map_err(map_target_error)?;
        Ok(ConnectionTestResult {
            connection,
            reachable: true,
        })
    }

    async fn catalog(
        &self,
        session: &AuthenticatedSession,
        arguments: &CatalogArguments,
        client_protocol_version: u16,
    ) -> Result<CatalogSnapshot, ErrorCode> {
        self.resolve_connection(session, &arguments.connection, client_protocol_version)
            .await?;
        self.services()?
            .catalog
            .load_terminal_snapshot(
                &terminal_authority(session, client_protocol_version),
                CatalogReadPolicy::CacheFirst,
            )
            .await
            .map_err(map_application_error)
    }

    async fn schema_list(
        &self,
        session: &AuthenticatedSession,
        arguments: &CatalogArguments,
        client_protocol_version: u16,
    ) -> Result<SchemaListResult, ErrorCode> {
        let catalog = self
            .catalog(session, arguments, client_protocol_version)
            .await?;
        let mut counts = BTreeMap::<String, [u64; 3]>::new();
        for namespace in catalog.namespaces() {
            counts.entry(namespace.name.clone()).or_default();
        }
        for relation in catalog.relations() {
            counts
                .entry(namespace_name(&relation.object.namespace))
                .or_default()[0] += 1;
        }
        for routine in catalog.routines() {
            counts
                .entry(namespace_name(&routine.object.namespace))
                .or_default()[1] += 1;
        }
        for object in catalog.other_objects() {
            counts
                .entry(namespace_name(&object.object.namespace))
                .or_default()[2] += 1;
        }
        Ok(SchemaListResult {
            connection_id: catalog.connection_id(),
            schemas: counts
                .into_iter()
                .map(|(name, counts)| SchemaSummary {
                    name,
                    relation_count: counts[0],
                    routine_count: counts[1],
                    object_count: counts[2],
                })
                .collect(),
        })
    }

    async fn table_describe(
        &self,
        session: &AuthenticatedSession,
        arguments: &TableDescribeArguments,
        client_protocol_version: u16,
    ) -> Result<TableDescribeResult, ErrorCode> {
        if arguments.table.is_empty()
            || arguments.table.len() > MAX_TABLE_SELECTOR_BYTES
            || arguments.table.chars().any(char::is_control)
        {
            return Err(ErrorCode::InvalidRequest);
        }
        let catalog = self
            .catalog(
                session,
                &CatalogArguments {
                    connection: arguments.connection.clone(),
                },
                client_protocol_version,
            )
            .await?;
        let relation = if let Some((namespace, name)) = arguments.table.rsplit_once('.') {
            catalog
                .relations()
                .iter()
                .find(|relation| {
                    relation.object.namespace.as_deref() == Some(namespace)
                        && relation.object.name == name
                })
                .cloned()
                .ok_or(ErrorCode::InvalidRequest)?
        } else {
            let matches = catalog
                .relations()
                .iter()
                .filter(|relation| relation.object.name == arguments.table)
                .collect::<Vec<_>>();
            match matches.as_slice() {
                [relation] => (*relation).clone(),
                _ => return Err(ErrorCode::InvalidRequest),
            }
        };
        Ok(TableDescribeResult {
            connection_id: catalog.connection_id(),
            relation,
        })
    }

    async fn query_plan(
        &self,
        session: &AuthenticatedSession,
        arguments: QueryPlanArguments,
        client_protocol_version: u16,
    ) -> Result<QueryPlanResult, ErrorCode> {
        validate_sql(&arguments.sql)?;
        if arguments.max_rows == Some(0) {
            return Err(ErrorCode::InvalidRequest);
        }
        let connection = self
            .resolve_connection(session, &arguments.connection, client_protocol_version)
            .await?;
        let receipt = self
            .services()?
            .query
            .plan_terminal_read(TerminalQueryPlanRequest {
                connection_id: connection.id,
                sql: arguments.sql,
                max_rows: arguments.max_rows,
                authority: terminal_authority(session, client_protocol_version),
            })
            .await;
        let receipt = match receipt {
            Ok(receipt) => receipt,
            Err(AgentQueryPlanError::DocumentConnection) => return Err(ErrorCode::InvalidRequest),
            Err(AgentQueryPlanError::NotSingleRead(rejected)) => {
                rejected.audit_after_result().await;
                return Err(ErrorCode::PolicyBlocked);
            }
            Err(AgentQueryPlanError::Application(error)) => {
                return Err(map_application_error(error))
            }
        };
        let plan = receipt.plan();
        Ok(QueryPlanResult {
            connection_id: plan.connection_id,
            connection_name: plan.connection_name.clone(),
            environment: plan.environment.clone(),
            plan_id: plan.plan_id,
            decision: plan.decision.clone(),
            notices: plan.notices.clone(),
            suggestions: plan.suggestions.clone(),
            estimated_rows: plan.estimated_rows,
            health: query_health(&plan.health),
            expires_at: plan.expires_at,
        })
    }

    async fn query_run(
        &self,
        session: &AuthenticatedSession,
        arguments: QueryRunArguments,
        client_protocol_version: u16,
    ) -> Result<QueryRunResult, ErrorCode> {
        let authority = terminal_authority(session, client_protocol_version);
        let prepared = self
            .services()?
            .query
            .prepare_terminal_run(arguments.plan_id, &authority)
            .await
            .map_err(map_prepare_error)?;
        let receipt = prepared.execute().await.map_err(map_query_run_error)?;
        let run = receipt.run();
        Ok(QueryRunResult {
            connection_id: run.connection_id,
            connection_name: run.connection_name.clone(),
            plan_id: run.plan_id,
            query_run_id: run.query_run_id,
            planning_decision: run.planning_decision.clone(),
            result: query_result(&run.result),
        })
    }

    async fn sql_propose(
        &self,
        session: &AuthenticatedSession,
        arguments: SqlProposeArguments,
        client_protocol_version: u16,
    ) -> Result<OperationSummary, ErrorCode> {
        validate_sql(&arguments.sql)?;
        let connection = self
            .resolve_connection(session, &arguments.connection, client_protocol_version)
            .await?;
        let authority = terminal_authority(session, client_protocol_version);
        let receipt = self
            .services()?
            .query
            .propose_terminal_sql(TerminalSqlProposalRequest {
                connection_id: connection.id,
                sql: arguments.sql,
                authority: authority.clone(),
            })
            .await
            .map_err(|error| map_application_error(error.into_error()))?;
        self.services()?
            .operation
            .show_terminal(&authority, receipt.operation_id)
            .await
            .map_err(map_operation_error)
    }

    async fn show_operation(
        &self,
        session: &AuthenticatedSession,
        operation_id: Uuid,
        client_protocol_version: u16,
    ) -> Result<OperationSummary, ErrorCode> {
        self.services()?
            .operation
            .show_terminal(
                &terminal_authority(session, client_protocol_version),
                operation_id,
            )
            .await
            .map_err(map_operation_error)
    }

    async fn wait_operation(
        &self,
        session: &AuthenticatedSession,
        arguments: OperationWaitArguments,
        client_protocol_version: u16,
    ) -> Result<OperationSummary, ErrorCode> {
        let timeout = Duration::from_millis(arguments.timeout_ms);
        if timeout.is_zero() || timeout > MAX_OPERATION_WAIT {
            return Err(ErrorCode::InvalidRequest);
        }
        self.services()?
            .operation
            .wait_terminal(
                &terminal_authority(session, client_protocol_version),
                arguments.operation_id,
                timeout,
            )
            .await
            .map_err(|error| {
                if matches!(error, AppError::Safety(_)) {
                    ErrorCode::Timeout
                } else {
                    map_operation_error(error)
                }
            })
    }

    async fn cancel_operation(
        &self,
        session: &AuthenticatedSession,
        operation_id: Uuid,
        client_protocol_version: u16,
    ) -> Result<OperationSummary, ErrorCode> {
        self.services()?
            .operation
            .cancel_terminal(
                &terminal_authority(session, client_protocol_version),
                operation_id,
            )
            .await
            .map_err(map_operation_error)
    }
}

fn terminal_authority(
    session: &AuthenticatedSession,
    client_protocol_version: u16,
) -> TerminalAuthority {
    TerminalAuthority {
        terminal_session_id: session.terminal_session_id,
        workspace_id: session.workspace_id,
        account_scope: session.account_scope.clone(),
        connection_id: session.connection_id,
        connection_revision: session.connection_revision,
        client_protocol_version,
    }
}

fn connection_summary(summary: &AgentConnectionSummary) -> ConnectionSummary {
    ConnectionSummary {
        id: summary.id,
        name: summary.name.clone(),
        engine: database_engine(summary.engine),
        database: summary.database.clone(),
        environment: summary.environment.clone(),
        readonly: summary.readonly,
        allow_writes: summary.allow_writes,
    }
}

const fn database_engine(engine: Engine) -> DatabaseEngine {
    match engine {
        Engine::Postgres => DatabaseEngine::Postgres,
        Engine::Mysql => DatabaseEngine::Mysql,
        Engine::Sqlite => DatabaseEngine::Sqlite,
        Engine::Mongodb => DatabaseEngine::Mongodb,
    }
}

fn query_health(health: &HealthSnapshot) -> QueryHealth {
    QueryHealth {
        level: health.level.clone(),
        coverage: health.coverage.clone(),
        total_connections: health.total_connections,
        max_connections: health.max_connections,
        connection_usage_percent: health.connection_usage_percent,
        active_queries: health.active_queries,
        long_running_queries: health.long_running_queries,
        lock_waits: health.lock_waits,
        replication_lag_seconds: health.replication_lag_seconds,
        reasons: health.reasons.clone(),
        captured_at: health.captured_at,
    }
}

fn query_result(result: &QueryResult) -> QueryResultPage {
    QueryResultPage {
        columns: result.columns.clone(),
        rows: result.rows.clone(),
        row_count: result.row_count,
        truncated: result.truncated,
        duration_ms: result.duration_ms,
    }
}

fn namespace_name(namespace: &Option<String>) -> String {
    namespace.clone().unwrap_or_else(|| "default".into())
}

fn validate_sql(sql: &str) -> Result<(), ErrorCode> {
    if sql.trim().is_empty() || sql.len() > MAX_SQL_BYTES || sql.contains('\0') {
        Err(ErrorCode::InvalidRequest)
    } else {
        Ok(())
    }
}

fn map_prepare_error(error: AgentQueryRunPrepareError) -> ErrorCode {
    match error {
        AgentQueryRunPrepareError::UnknownOrAlreadyUsed => ErrorCode::OperationConflict,
        AgentQueryRunPrepareError::Expired => ErrorCode::OperationExpired,
        AgentQueryRunPrepareError::SessionMismatch
        | AgentQueryRunPrepareError::AuthorityChanged => ErrorCode::ScopeDenied,
        AgentQueryRunPrepareError::StoredPlanInvalid => ErrorCode::InvalidRequest,
        AgentQueryRunPrepareError::Application(error) => map_application_error(error),
    }
}

fn map_query_run_error(error: AgentQueryRunError) -> ErrorCode {
    match error {
        AgentQueryRunError::Connection(_) => ErrorCode::TargetExecutionFailed,
        AgentQueryRunError::Execution(failure) => map_query_execution_error(failure.error()),
        AgentQueryRunError::ConsentHandlePersistence(_) => ErrorCode::Internal,
    }
}

fn map_query_execution_error(error: &AppError) -> ErrorCode {
    match error {
        AppError::Safety(reason) if reason == "query cancelled" => ErrorCode::Cancelled,
        AppError::Safety(reason) if reason.starts_with("query timed out after ") => {
            ErrorCode::Timeout
        }
        _ => ErrorCode::TargetExecutionFailed,
    }
}

fn map_target_error(error: AppError) -> ErrorCode {
    match error {
        AppError::Blocked { .. } => ErrorCode::ScopeDenied,
        AppError::Config(_) | AppError::Parse(_) => ErrorCode::InvalidRequest,
        AppError::Db(_) | AppError::Mongo(_) | AppError::Network(_) => {
            ErrorCode::TargetExecutionFailed
        }
        _ => ErrorCode::Internal,
    }
}

fn map_operation_error(error: AppError) -> ErrorCode {
    match error {
        AppError::NotFound(_) => ErrorCode::OperationConflict,
        AppError::Blocked { .. } => ErrorCode::ScopeDenied,
        AppError::OutcomeUnknown(_) => ErrorCode::OperationConflict,
        other => map_application_error(other),
    }
}

fn map_application_error(error: AppError) -> ErrorCode {
    match error {
        AppError::Blocked { .. } => ErrorCode::PolicyBlocked,
        AppError::Safety(_) => ErrorCode::PolicyBlocked,
        AppError::NotFound(_) | AppError::Config(_) | AppError::Parse(_) => {
            ErrorCode::InvalidRequest
        }
        AppError::Db(_) | AppError::Mongo(_) => ErrorCode::TargetExecutionFailed,
        AppError::OutcomeUnknown(_) => ErrorCode::OperationConflict,
        AppError::Agent(_)
        | AppError::Network(_)
        | AppError::Keychain(_)
        | AppError::Io(_)
        | AppError::Serialization(_) => ErrorCode::Internal,
    }
}

fn respond<T: Serialize>(request_id: Uuid, result: Result<T, ErrorCode>) -> ResponseEnvelope {
    match result {
        Ok(result) => success(request_id, &result),
        Err(code) => failure(request_id, code, false),
    }
}

fn response_at_protocol(response: ResponseEnvelope, protocol_version: u16) -> ResponseEnvelope {
    if let Some(result) = response.result() {
        ResponseEnvelope::success(protocol_version, response.request_id(), result.clone())
    } else if let Some(error) = response.error() {
        ResponseEnvelope::failure(protocol_version, response.request_id(), error.clone())
    } else {
        ResponseEnvelope::failure(
            protocol_version,
            response.request_id(),
            ProtocolError::new(ErrorCode::Internal, false),
        )
    }
}

fn success<T: Serialize>(request_id: Uuid, result: &T) -> ResponseEnvelope {
    let response = match serde_json::to_value(result) {
        Ok(result) => ResponseEnvelope::success(PROTOCOL_MAX, request_id, result),
        Err(_) => return failure(request_id, ErrorCode::Internal, false),
    };
    if encode_frame(&response, MAX_RESPONSE_BYTES).is_err() {
        failure(request_id, ErrorCode::ResponseTooLarge, false)
    } else {
        response
    }
}

fn failure(request_id: Uuid, code: ErrorCode, retryable: bool) -> ResponseEnvelope {
    ResponseEnvelope::failure(
        PROTOCOL_MAX,
        request_id,
        ProtocolError::new(code, retryable),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::str::FromStr;

    use dopedb_protocol::{
        ConnectionListResult, QueryPlanArguments, QueryPlanResult, QueryRunArguments,
        QueryRunResult, SessionAuthentication,
    };
    use serde_json::json;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use tempfile::TempDir;

    use super::*;
    use crate::connection::ConnectionManager;
    use crate::model::{
        ConnectionProfile, Provider, WorkspaceConnectionAccess, WorkspaceCredentialMode,
    };
    use crate::operations::OperationRuntime;
    use crate::store::{Store, TEST_SCHEMA};

    fn request(command: CommandName, arguments: serde_json::Value) -> RequestEnvelope {
        RequestEnvelope {
            protocol_version: PROTOCOL_MAX,
            command_schema_version: COMMAND_SCHEMA_VERSION,
            request_id: Uuid::new_v4(),
            authentication: None,
            command,
            arguments,
        }
    }

    fn dispatcher() -> BrokerDispatcher {
        let runtime_id = Uuid::new_v4();
        BrokerDispatcher::new(
            runtime_id,
            "0.3.3",
            BrokerSessionRegistry::new(runtime_id),
            None,
            None,
        )
    }

    #[tokio::test]
    async fn status_uses_the_typed_empty_payload_and_safe_projection() {
        let dispatcher = dispatcher();
        let accepted = dispatcher
            .dispatch(request(CommandName::Status, json!({})))
            .await;
        assert!(accepted.is_ok());
        assert_eq!(
            accepted.result().unwrap()["appVersion"],
            serde_json::Value::String("0.3.3".into())
        );

        let rejected = dispatcher
            .dispatch(request(
                CommandName::Status,
                json!({"token": "must-not-pass"}),
            ))
            .await;
        assert_eq!(
            rejected.error().map(ProtocolError::code),
            Some(ErrorCode::InvalidRequest)
        );
    }

    #[tokio::test]
    async fn db_commands_require_terminal_auth_before_payload_decode() {
        let response = dispatcher()
            .dispatch(request(
                CommandName::QueryPlan,
                json!({"connection": "invalid", "sql": ""}),
            ))
            .await;
        assert_eq!(
            response.error().map(ProtocolError::code),
            Some(ErrorCode::AuthenticationDenied)
        );
    }

    #[tokio::test]
    async fn incompatible_protocol_fails_before_command_decode() {
        let mut request = request(CommandName::Status, json!({}));
        request.protocol_version = PROTOCOL_MAX + 1;
        let response = dispatcher().dispatch(request).await;
        assert_eq!(
            response.error().map(ProtocolError::code),
            Some(ErrorCode::ProtocolMismatch)
        );
    }

    #[tokio::test]
    async fn future_command_decodes_only_to_return_a_stable_schema_error() {
        let future = json!({
            "protocolVersion": PROTOCOL_MAX,
            "commandSchemaVersion": COMMAND_SCHEMA_VERSION + 1,
            "requestId": Uuid::new_v4(),
            "command": "future.command",
            "arguments": {"untrusted": true}
        });
        let request: RequestEnvelope = serde_json::from_value(future.clone()).unwrap();
        assert_eq!(request.command, CommandName::Unknown);
        let response = dispatcher().dispatch(request).await;
        assert_eq!(
            response.error().map(ProtocolError::code),
            Some(ErrorCode::ProtocolMismatch)
        );

        let mut same_schema = future;
        same_schema["commandSchemaVersion"] = serde_json::json!(COMMAND_SCHEMA_VERSION);
        let response = dispatcher()
            .dispatch(serde_json::from_value(same_schema).unwrap())
            .await;
        assert_eq!(
            response.error().map(ProtocolError::code),
            Some(ErrorCode::InvalidRequest)
        );
    }

    #[test]
    fn oversized_results_fail_as_a_stable_error_before_transport_write() {
        let response = success(
            Uuid::new_v4(),
            &"x".repeat(dopedb_protocol::MAX_STRING_BYTES + 1),
        );
        assert_eq!(
            response.error().map(ProtocolError::code),
            Some(ErrorCode::ResponseTooLarge)
        );
        assert!(response.result().is_none());
    }

    #[test]
    fn response_projection_preserves_payload_and_uses_the_negotiated_version() {
        let request_id = Uuid::new_v4();
        let response = response_at_protocol(success(request_id, &json!({"ready": true})), 7);
        assert_eq!(response.protocol_version(), 7);
        assert_eq!(response.request_id(), request_id);
        assert_eq!(response.result().unwrap(), &json!({"ready": true}));
    }

    #[test]
    fn terminal_query_interruptions_keep_stable_cancel_and_timeout_codes() {
        assert_eq!(
            map_query_execution_error(&AppError::Safety("query cancelled".into())),
            ErrorCode::Cancelled
        );
        assert_eq!(
            map_query_execution_error(&AppError::Safety(
                "query timed out after 300s and was aborted".into()
            )),
            ErrorCode::Timeout
        );
    }

    struct ServiceHarness {
        dispatcher: BrokerDispatcher,
        primary_session: (Uuid, String),
        other_session: (Uuid, String),
        store: Store,
        connections: ConnectionManager,
        connection_id: Uuid,
        _directory: TempDir,
    }

    impl ServiceHarness {
        async fn new() -> Self {
            let options = SqliteConnectOptions::from_str("sqlite::memory:")
                .unwrap()
                .foreign_keys(true);
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(options)
                .await
                .unwrap();
            sqlx::raw_sql(TEST_SCHEMA).execute(&pool).await.unwrap();
            let store = Store::from_pool_for_test(pool);

            let directory = tempfile::tempdir().unwrap();
            let target = directory.path().join("broker-target.db");
            let target_options = SqliteConnectOptions::new()
                .filename(&target)
                .create_if_missing(true)
                .foreign_keys(true);
            let target_pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(target_options)
                .await
                .unwrap();
            sqlx::raw_sql(
                "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
                 INSERT INTO users (id, name) VALUES (1, 'Ada'), (2, 'Linus');",
            )
            .execute(&target_pool)
            .await
            .unwrap();
            target_pool.close().await;

            let connection_id = Uuid::new_v4();
            store
                .upsert_connection(&ConnectionProfile {
                    id: connection_id,
                    name: "fixture".into(),
                    engine: Engine::Sqlite,
                    provider: Provider::Generic,
                    driver_id: Some("sqlx-sqlite".into()),
                    host: String::new(),
                    port: 0,
                    database: target.to_string_lossy().into_owned(),
                    username: String::new(),
                    sslmode: "disable".into(),
                    extra_params: HashMap::new(),
                    readonly_default: true,
                    allow_writes: false,
                    secret_ref: None,
                    env: Some("test".into()),
                    schema_group: None,
                    workspace_access: WorkspaceConnectionAccess::Local,
                    credential_mode: WorkspaceCredentialMode::Local,
                })
                .await
                .unwrap();
            let pin = store.pin_connection_for_read(connection_id).await.unwrap();
            let connections = ConnectionManager::new(store.clone());
            let (operation, _) = OperationRuntime::new(&store);
            let runtime_id = operation.runtime_id();
            let services = ApplicationServices::new(store.clone(), connections.clone(), operation);
            let sessions = BrokerSessionRegistry::new(runtime_id);
            let primary = sessions
                .issue(
                    Uuid::new_v4(),
                    &pin,
                    BrokerCapability::ALL,
                    Duration::from_secs(60),
                )
                .unwrap();
            let other = sessions
                .issue(
                    Uuid::new_v4(),
                    &pin,
                    BrokerCapability::ALL,
                    Duration::from_secs(60),
                )
                .unwrap();
            let primary_session = (primary.terminal_session_id, primary.token().to_string());
            let other_session = (other.terminal_session_id, other.token().to_string());
            Self {
                dispatcher: BrokerDispatcher::new(
                    runtime_id,
                    "0.3.3",
                    sessions,
                    Some(services),
                    None,
                ),
                primary_session,
                other_session,
                store,
                connections,
                connection_id,
                _directory: directory,
            }
        }

        fn request<T: Serialize>(
            &self,
            command: CommandName,
            arguments: &T,
            session: &(Uuid, String),
        ) -> RequestEnvelope {
            RequestEnvelope {
                protocol_version: PROTOCOL_MAX,
                command_schema_version: COMMAND_SCHEMA_VERSION,
                request_id: Uuid::new_v4(),
                authentication: Some(SessionAuthentication::new(session.0, session.1.clone())),
                command,
                arguments: serde_json::to_value(arguments).unwrap(),
            }
        }

        async fn close(self) {
            let mutation = self
                .connections
                .begin_connection_mutation(
                    self.connection_id,
                    crate::connection::ConnectionAccess::Read,
                )
                .await
                .unwrap();
            mutation.retire_connection(self.connection_id).await;
            self.store.pool().close().await;
        }
    }

    #[tokio::test]
    async fn service_dispatch_is_secret_free_and_blocks_cross_terminal_plan_reuse() {
        let harness = ServiceHarness::new().await;
        let list = harness
            .dispatcher
            .dispatch(harness.request(
                CommandName::ConnectionList,
                &EmptyArguments::default(),
                &harness.primary_session,
            ))
            .await;
        let list: ConnectionListResult =
            serde_json::from_value(list.result().cloned().unwrap()).unwrap();
        assert_eq!(list.connections.len(), 1);
        let serialized = serde_json::to_string(&list).unwrap();
        for forbidden in ["host", "username", "password", "secret", "credential"] {
            assert!(!serialized.to_ascii_lowercase().contains(forbidden));
        }

        let wrong_connection = harness
            .dispatcher
            .dispatch(harness.request(
                CommandName::QueryPlan,
                &QueryPlanArguments {
                    connection: ConnectionSelector::Id(Uuid::new_v4()),
                    sql: "SELECT 1".into(),
                    max_rows: None,
                },
                &harness.primary_session,
            ))
            .await;
        assert_eq!(
            wrong_connection.error().map(ProtocolError::code),
            Some(ErrorCode::ScopeDenied)
        );

        let mut plan_request = harness.request(
            CommandName::QueryPlan,
            &QueryPlanArguments {
                connection: ConnectionSelector::Id(harness.connection_id),
                sql: "SELECT id, name FROM users ORDER BY id".into(),
                max_rows: None,
            },
            &harness.primary_session,
        );
        plan_request.protocol_version = PROTOCOL_MIN;
        let planned = harness.dispatcher.dispatch(plan_request).await;
        let plan: QueryPlanResult =
            serde_json::from_value(planned.result().cloned().unwrap()).unwrap();
        let provenance: String =
            sqlx::query_scalar("SELECT actor_provenance_json FROM operations WHERE id = ?1")
                .bind(plan.plan_id.to_string())
                .fetch_one(harness.store.pool())
                .await
                .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&provenance).unwrap()
                ["clientProtocolVersion"],
            serde_json::json!(PROTOCOL_MIN)
        );

        let denied = harness
            .dispatcher
            .dispatch(harness.request(
                CommandName::QueryRun,
                &QueryRunArguments {
                    plan_id: plan.plan_id,
                },
                &harness.other_session,
            ))
            .await;
        assert_eq!(
            denied.error().map(ProtocolError::code),
            Some(ErrorCode::ScopeDenied)
        );

        let executed = harness
            .dispatcher
            .dispatch(harness.request(
                CommandName::QueryRun,
                &QueryRunArguments {
                    plan_id: plan.plan_id,
                },
                &harness.primary_session,
            ))
            .await;
        let run: QueryRunResult =
            serde_json::from_value(executed.result().cloned().unwrap()).unwrap();
        assert_eq!(run.result.row_count, 2);
        assert_eq!(run.result.rows.len(), 2);

        harness.close().await;
    }
}
