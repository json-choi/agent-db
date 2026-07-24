//! Transport-neutral multi-statement SQL script execution.

use std::fmt;

use chrono::Utc;
use sqlx::AssertSqlSafe;
use uuid::Uuid;

use crate::audit::{self, RecordArgs};
use crate::connection::{
    ConnectionAccess, ConnectionLease, ConnectionManager, ConnectionOperationScope, DbPool,
};
use crate::error::{AppError, AppResult};
use crate::executor;
use crate::model::{HistoryEntry, QueryKind, ScriptOutcome, ScriptStatement};
use crate::safety;
use crate::store::{PinnedConnection, Store};

/// Desktop script input. The legacy approval boolean remains isolated here until
/// exact Operation approvals replace it in FND-04.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DesktopScriptRunRequest {
    pub(crate) connection_id: Uuid,
    pub(crate) sql: String,
    pub(crate) approved: bool,
    pub(crate) query_id: Option<Uuid>,
    pub(crate) origin: Option<String>,
}

/// Successful script execution retaining target authority until the adapter has
/// serialized the established [`ScriptOutcome`] payload.
pub(crate) struct DesktopScriptRunReceipt {
    outcome: ScriptOutcome,
    _lease: ConnectionLease,
}

impl serde::Serialize for DesktopScriptRunReceipt {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serde::Serialize::serialize(&self.outcome, serializer)
    }
}

#[derive(Debug)]
pub(crate) enum DesktopScriptRunError {
    Application(AppError),
    Scoped(DesktopScriptScopedFailure),
    Execution(Box<DesktopScriptExecutionFailure>),
}

impl DesktopScriptRunError {
    pub(crate) fn into_error(self) -> AppError {
        match self {
            Self::Application(error) => error,
            Self::Scoped(failure) => failure.into_error(),
            Self::Execution(failure) => failure.into_error(),
        }
    }
}

pub(crate) struct DesktopScriptScopedFailure {
    error: AppError,
    _scope: ConnectionOperationScope,
}

impl fmt::Debug for DesktopScriptScopedFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DesktopScriptScopedFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl DesktopScriptScopedFailure {
    fn into_error(self) -> AppError {
        self.error
    }
}

pub(crate) struct DesktopScriptExecutionFailure {
    error: AppError,
    _lease: ConnectionLease,
}

impl fmt::Debug for DesktopScriptExecutionFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DesktopScriptExecutionFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl DesktopScriptExecutionFailure {
    fn into_error(self) -> AppError {
        self.error
    }
}

#[derive(Clone)]
pub(crate) struct ScriptService {
    store: Store,
    connections: ConnectionManager,
}

struct PreparedScriptRun {
    operation_scope: ConnectionOperationScope,
    operation_pin: PinnedConnection,
    request: DesktopScriptRunRequest,
    statements: Vec<String>,
    kinds: Vec<QueryKind>,
    settings: crate::model::SafetySettings,
    engine: crate::model::Engine,
    history_origin: String,
}

impl ScriptService {
    pub(super) fn new(store: Store, connections: ConnectionManager) -> Self {
        Self { store, connections }
    }

    /// Split and classify every statement, then execute either sequentially on the
    /// read-only pool or atomically on the write pool under the legacy Phase 1 gates.
    pub(crate) async fn run_desktop(
        &self,
        request: DesktopScriptRunRequest,
    ) -> Result<DesktopScriptRunReceipt, DesktopScriptRunError> {
        let operation_scope = self.connections.begin_operation_scope().await;
        let operation_pin = match operation_scope.pin_connection(request.connection_id).await {
            Ok(pin) => pin,
            Err(error) => {
                return Err(DesktopScriptRunError::Scoped(DesktopScriptScopedFailure {
                    error,
                    _scope: operation_scope,
                }))
            }
        };
        let settings = match self.store.get_safety(request.connection_id).await {
            Ok(settings) => settings,
            Err(error) => {
                return Err(DesktopScriptRunError::Scoped(DesktopScriptScopedFailure {
                    error,
                    _scope: operation_scope,
                }))
            }
        };
        let engine = operation_pin.profile.engine;
        let history_origin = request.origin.clone().unwrap_or_else(|| "manual".into());
        let statements = crate::sql_script::split_statements(&request.sql, engine);
        if statements.is_empty() {
            return Err(DesktopScriptRunError::Scoped(DesktopScriptScopedFailure {
                error: AppError::Config("no executable statements in the script".into()),
                _scope: operation_scope,
            }));
        }
        let kinds = match statements
            .iter()
            .map(|statement| safety::classify(statement, engine).map(|result| result.kind))
            .collect::<AppResult<Vec<_>>>()
        {
            Ok(kinds) => kinds,
            Err(error) => {
                return Err(DesktopScriptRunError::Scoped(DesktopScriptScopedFailure {
                    error,
                    _scope: operation_scope,
                }))
            }
        };

        let has_write = script_has_write(&kinds);
        let prepared = PreparedScriptRun {
            operation_scope,
            operation_pin,
            request,
            statements,
            kinds,
            settings,
            engine,
            history_origin,
        };
        if has_write {
            self.run_write(prepared).await
        } else {
            self.run_reads(prepared).await
        }
    }

    async fn run_reads(
        &self,
        prepared: PreparedScriptRun,
    ) -> Result<DesktopScriptRunReceipt, DesktopScriptRunError> {
        let PreparedScriptRun {
            operation_scope,
            operation_pin,
            request,
            statements,
            kinds: _,
            settings,
            engine,
            history_origin,
        } = prepared;
        if !settings.auto_run_reads && !request.approved {
            let reason = "reads require approval for this connection".to_string();
            record_script_run(
                &self.store,
                &operation_pin,
                ScriptRunRecord {
                    sql: &request.sql,
                    kind: QueryKind::Read,
                    action: "blocked",
                    status: "blocked",
                    row_count: None,
                    error: Some(reason.clone()),
                    origin: &history_origin,
                },
            )
            .await;
            return Err(DesktopScriptRunError::Scoped(DesktopScriptScopedFailure {
                error: AppError::Blocked { reason },
                _scope: operation_scope,
            }));
        }

        let lease = match operation_scope
            .connect(operation_pin.clone(), ConnectionAccess::Read)
            .await
        {
            Ok(lease) => lease,
            Err(error) => {
                record_script_run(
                    &self.store,
                    &operation_pin,
                    ScriptRunRecord {
                        sql: &request.sql,
                        kind: QueryKind::Read,
                        action: "script:execute",
                        status: "error",
                        row_count: None,
                        error: Some(error.to_string()),
                        origin: &history_origin,
                    },
                )
                .await;
                return Err(DesktopScriptRunError::Application(error));
            }
        };
        let live = match lease.live().sql() {
            Ok(live) => live,
            Err(error) => {
                return Err(DesktopScriptRunError::Execution(Box::new(
                    DesktopScriptExecutionFailure {
                        error,
                        _lease: lease,
                    },
                )));
            }
        };
        let mut outcomes = Vec::with_capacity(statements.len());
        let mut failure = None;
        for statement in &statements {
            if failure.is_some() {
                outcomes.push(statement_skipped(statement));
                continue;
            }
            match executor::run_read(live, engine, statement, settings.max_rows, request.query_id)
                .await
            {
                Ok(result) => outcomes.push(ScriptStatement {
                    sql: statement.clone(),
                    result: Some(result),
                    affected: None,
                    error: None,
                }),
                Err(error) => {
                    let message = error.to_string();
                    outcomes.push(statement_error(statement, message.clone()));
                    failure = Some(message);
                }
            }
        }
        let total = outcomes
            .iter()
            .filter_map(|statement| statement.result.as_ref())
            .map(|result| result.row_count as i64)
            .sum();
        let (status, error) = match failure {
            Some(error) => ("error", Some(error)),
            None => ("ok", None),
        };
        record_script_run(
            &self.store,
            &operation_pin,
            ScriptRunRecord {
                sql: &request.sql,
                kind: QueryKind::Read,
                action: "script:execute",
                status,
                row_count: Some(total),
                error,
                origin: &history_origin,
            },
        )
        .await;
        Ok(DesktopScriptRunReceipt {
            outcome: ScriptOutcome {
                statements: outcomes,
                committed: false,
                all_reads: true,
            },
            _lease: lease,
        })
    }

    async fn run_write(
        &self,
        prepared: PreparedScriptRun,
    ) -> Result<DesktopScriptRunReceipt, DesktopScriptRunError> {
        let PreparedScriptRun {
            operation_scope,
            operation_pin,
            request,
            statements,
            kinds,
            settings,
            engine,
            history_origin,
        } = prepared;
        if !operation_pin.profile.workspace_access.can_write() {
            return Err(DesktopScriptRunError::Scoped(DesktopScriptScopedFailure {
                error: AppError::Blocked {
                    reason: "your workspace role grants read-only database access".into(),
                },
                _scope: operation_scope,
            }));
        }
        if !settings.allow_writes {
            let reason = "writing is disabled for this connection (writes are off by default). \
                          Enable writes in the connection's safety settings to run this script."
                .to_string();
            record_script_run(
                &self.store,
                &operation_pin,
                ScriptRunRecord {
                    sql: &request.sql,
                    kind: QueryKind::Write,
                    action: "blocked",
                    status: "blocked",
                    row_count: None,
                    error: Some(reason.clone()),
                    origin: &history_origin,
                },
            )
            .await;
            return Err(DesktopScriptRunError::Scoped(DesktopScriptScopedFailure {
                error: AppError::Blocked { reason },
                _scope: operation_scope,
            }));
        }
        if !request.approved {
            let reason = "this script modifies data and requires explicit approval".to_string();
            record_script_run(
                &self.store,
                &operation_pin,
                ScriptRunRecord {
                    sql: &request.sql,
                    kind: QueryKind::Write,
                    action: "blocked",
                    status: "blocked",
                    row_count: None,
                    error: Some(reason.clone()),
                    origin: &history_origin,
                },
            )
            .await;
            return Err(DesktopScriptRunError::Scoped(DesktopScriptScopedFailure {
                error: AppError::Blocked { reason },
                _scope: operation_scope,
            }));
        }

        let has_ddl = kinds.iter().any(|kind| matches!(kind, QueryKind::Ddl));
        let script_kind = if has_ddl {
            QueryKind::Ddl
        } else if kinds
            .iter()
            .any(|kind| matches!(kind, QueryKind::Privilege))
        {
            QueryKind::Privilege
        } else {
            QueryKind::Write
        };
        if let Err(error) = audit::record(
            &self.store,
            RecordArgs {
                connection_id: request.connection_id,
                engine,
                agent_prompt: None,
                sql: request.sql.clone(),
                kind: script_kind,
                action: "script:execute:attempt".into(),
                approved_by: None,
                affected_estimate: None,
                error: None,
            },
        )
        .await
        {
            return Err(DesktopScriptRunError::Scoped(DesktopScriptScopedFailure {
                error: AppError::Config(format!(
                    "audit pre-record failed — refusing to run script: {error}"
                )),
                _scope: operation_scope,
            }));
        }

        let lease = match operation_scope
            .connect(operation_pin.clone(), ConnectionAccess::Write)
            .await
        {
            Ok(lease) => lease,
            Err(error) => {
                record_script_run(
                    &self.store,
                    &operation_pin,
                    ScriptRunRecord {
                        sql: &request.sql,
                        kind: script_kind,
                        action: "script:execute",
                        status: "error",
                        row_count: None,
                        error: Some(error.to_string()),
                        origin: &history_origin,
                    },
                )
                .await;
                return Err(DesktopScriptRunError::Application(error));
            }
        };
        let live = match lease.live().sql() {
            Ok(live) => live,
            Err(error) => {
                record_script_run(
                    &self.store,
                    &operation_pin,
                    ScriptRunRecord {
                        sql: &request.sql,
                        kind: script_kind,
                        action: "script:execute",
                        status: "error",
                        row_count: None,
                        error: Some(error.to_string()),
                        origin: &history_origin,
                    },
                )
                .await;
                return Err(DesktopScriptRunError::Execution(Box::new(
                    DesktopScriptExecutionFailure {
                        error,
                        _lease: lease,
                    },
                )));
            }
        };
        let transaction = async {
            Ok::<_, AppError>(execute_script_transaction(&live.write_pool, &statements).await)
        };
        let (outcomes, committed) = match executor::cancel::guard(
            request.query_id,
            executor::cancel::QUERY_TIMEOUT,
            transaction,
        )
        .await
        {
            Ok(result) => result,
            Err(error) => {
                record_script_run(
                    &self.store,
                    &operation_pin,
                    ScriptRunRecord {
                        sql: &request.sql,
                        kind: script_kind,
                        action: "script:execute",
                        status: "error",
                        row_count: None,
                        error: Some(error.to_string()),
                        origin: &history_origin,
                    },
                )
                .await;
                return Err(DesktopScriptRunError::Execution(Box::new(
                    DesktopScriptExecutionFailure {
                        error,
                        _lease: lease,
                    },
                )));
            }
        };

        if committed && has_ddl {
            let _ = self.store.clear_schema_cache(request.connection_id).await;
        }
        let total = outcomes
            .iter()
            .filter_map(|statement| statement.affected)
            .sum();
        let first_error = outcomes
            .iter()
            .find_map(|statement| statement.error.clone());
        record_script_run(
            &self.store,
            &operation_pin,
            ScriptRunRecord {
                sql: &request.sql,
                kind: script_kind,
                action: "script:execute",
                status: if committed { "ok" } else { "error" },
                row_count: Some(total),
                error: first_error,
                origin: &history_origin,
            },
        )
        .await;
        Ok(DesktopScriptRunReceipt {
            outcome: ScriptOutcome {
                statements: outcomes,
                committed,
                all_reads: false,
            },
            _lease: lease,
        })
    }
}

fn statement_ok(sql: &str, affected: u64) -> ScriptStatement {
    ScriptStatement {
        sql: sql.to_string(),
        result: None,
        affected: Some(affected as i64),
        error: None,
    }
}

fn statement_error(sql: &str, message: String) -> ScriptStatement {
    ScriptStatement {
        sql: sql.to_string(),
        result: None,
        affected: None,
        error: Some(message),
    }
}

fn statement_skipped(sql: &str) -> ScriptStatement {
    statement_error(sql, "skipped — transaction rolled back".into())
}

fn script_has_write(kinds: &[QueryKind]) -> bool {
    kinds.iter().any(|kind| !matches!(kind, QueryKind::Read))
}

/// Execute every statement in one write-pool transaction. MySQL may implicitly
/// commit DDL, so mixed MySQL DDL scripts retain the existing best-effort caveat.
async fn execute_script_transaction(
    pool: &DbPool,
    statements: &[String],
) -> (Vec<ScriptStatement>, bool) {
    macro_rules! run_transaction {
        ($pool:expr) => {{
            let mut outcomes = Vec::with_capacity(statements.len());
            match $pool.begin().await {
                Ok(mut transaction) => {
                    let mut succeeded = true;
                    for statement in statements {
                        match sqlx::query(AssertSqlSafe(statement.as_str()))
                            .execute(&mut *transaction)
                            .await
                        {
                            Ok(result) => {
                                outcomes.push(statement_ok(statement, result.rows_affected()))
                            }
                            Err(error) => {
                                outcomes.push(statement_error(statement, error.to_string()));
                                succeeded = false;
                                break;
                            }
                        }
                    }
                    if !succeeded {
                        let _ = transaction.rollback().await;
                        while outcomes.len() < statements.len() {
                            outcomes.push(statement_skipped(&statements[outcomes.len()]));
                        }
                        (outcomes, false)
                    } else if let Err(error) = transaction.commit().await {
                        let message = format!("commit failed — nothing was saved: {error}");
                        for outcome in &mut outcomes {
                            outcome.error = Some(message.clone());
                            outcome.affected = None;
                        }
                        (outcomes, false)
                    } else {
                        (outcomes, true)
                    }
                }
                Err(error) => (
                    statements
                        .iter()
                        .map(|statement| {
                            statement_error(
                                statement,
                                format!("could not begin transaction: {error}"),
                            )
                        })
                        .collect(),
                    false,
                ),
            }
        }};
    }
    match pool {
        DbPool::Postgres(pool) => run_transaction!(pool),
        DbPool::Mysql(pool) => run_transaction!(pool),
        DbPool::Sqlite(pool) => run_transaction!(pool),
    }
}

struct ScriptRunRecord<'a> {
    sql: &'a str,
    kind: QueryKind,
    action: &'a str,
    status: &'a str,
    row_count: Option<i64>,
    error: Option<String>,
    origin: &'a str,
}

async fn record_script_run(store: &Store, pin: &PinnedConnection, record: ScriptRunRecord<'_>) {
    if let Err(error) = audit::record(
        store,
        RecordArgs {
            connection_id: pin.connection_id,
            engine: pin.profile.engine,
            agent_prompt: None,
            sql: record.sql.to_string(),
            kind: record.kind,
            action: record.action.to_string(),
            approved_by: None,
            affected_estimate: record.row_count,
            error: record.error.clone(),
        },
    )
    .await
    {
        tracing::error!(
            connection_id = %pin.connection_id,
            action = record.action,
            %error,
            "script audit record failed"
        );
    }
    if let Err(error) = store
        .insert_history_if_current(
            pin,
            &HistoryEntry {
                id: Uuid::new_v4(),
                connection_id: pin.connection_id,
                sql: record.sql.to_string(),
                kind: record.kind,
                status: record.status.to_string(),
                row_count: record.row_count,
                duration_ms: None,
                error: record.error,
                executed_at: Utc::now(),
                origin: record.origin.to_string(),
            },
        )
        .await
    {
        tracing::error!(
            connection_id = %pin.connection_id,
            %error,
            "script history insert failed"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;
    use std::str::FromStr;
    use std::time::Duration;

    use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
    use tempfile::TempDir;

    use super::*;
    use crate::model::{
        ConnectionProfile, Engine, Provider, WorkspaceConnectionAccess, WorkspaceCredentialMode,
    };
    use crate::store::TEST_SCHEMA;

    struct ScriptHarness {
        directory: TempDir,
        store: Store,
        connections: ConnectionManager,
        service: ScriptService,
        connection_id: Uuid,
        profile: ConnectionProfile,
        target_path: std::path::PathBuf,
    }

    impl ScriptHarness {
        async fn new() -> Self {
            let app_options = SqliteConnectOptions::from_str("sqlite::memory:")
                .unwrap()
                .foreign_keys(true);
            let app_pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(app_options)
                .await
                .unwrap();
            sqlx::raw_sql(TEST_SCHEMA).execute(&app_pool).await.unwrap();
            let store = Store::from_pool_for_test(app_pool);
            let directory = TempDir::new().unwrap();
            let target_path = directory.path().join("script-target.sqlite");
            initialize_target(&target_path).await;
            let connection_id = Uuid::new_v4();
            let profile = ConnectionProfile {
                id: connection_id,
                name: "script-test".into(),
                engine: Engine::Sqlite,
                provider: Provider::Generic,
                driver_id: Some("sqlx-sqlite".into()),
                host: String::new(),
                port: 0,
                database: target_path.display().to_string(),
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
            };
            store.upsert_connection(&profile).await.unwrap();
            let connections = ConnectionManager::new(store.clone());
            let service = ScriptService::new(store.clone(), connections.clone());
            Self {
                directory,
                store,
                connections,
                service,
                connection_id,
                profile,
                target_path,
            }
        }

        async fn configure(&self, allow_writes: bool, auto_run_reads: bool) {
            let mut profile = self.profile.clone();
            profile.allow_writes = allow_writes;
            self.store.upsert_connection(&profile).await.unwrap();
            let mut settings = self.store.get_safety(self.connection_id).await.unwrap();
            settings.allow_writes = allow_writes;
            settings.auto_run_reads = auto_run_reads;
            self.store
                .set_safety(self.connection_id, &settings)
                .await
                .unwrap();
        }

        async fn user_names(&self) -> Vec<String> {
            let options = SqliteConnectOptions::new()
                .filename(&self.target_path)
                .read_only(true);
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(options)
                .await
                .unwrap();
            let names = sqlx::query_scalar("SELECT name FROM users ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
            pool.close().await;
            names
        }

        async fn audit_actions(&self) -> Vec<String> {
            let (mut entries, valid, first_bad) = audit::snapshot(&self.store, self.connection_id)
                .await
                .unwrap();
            assert!(valid);
            assert_eq!(first_bad, None);
            entries.reverse();
            entries.into_iter().map(|entry| entry.action).collect()
        }

        async fn close(self) {
            let mutation = self
                .connections
                .begin_connection_mutation(self.connection_id, ConnectionAccess::Read)
                .await
                .unwrap();
            mutation.retire_connection(self.connection_id).await;
            let Self {
                directory,
                store,
                connections,
                service,
                ..
            } = self;
            drop(service);
            drop(connections);
            store.pool().close().await;
            drop(store);
            directory
                .close()
                .expect("temporary script directory must be removable after pool shutdown");
        }
    }

    async fn initialize_target(path: &Path) {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
             INSERT INTO users (id, name) VALUES (1, 'Ada'), (2, 'Linus');",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
    }

    async fn standalone_sqlite(tag: &str) -> SqlitePool {
        let path =
            std::env::temp_dir().join(format!("dopedb-script-{tag}-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true);
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap()
    }

    #[test]
    fn write_path_only_when_a_statement_writes() {
        assert!(!script_has_write(&[QueryKind::Read, QueryKind::Read]));
        assert!(script_has_write(&[QueryKind::Read, QueryKind::Write]));
        assert!(script_has_write(&[QueryKind::Ddl]));
        assert!(script_has_write(&[QueryKind::Privilege]));
    }

    #[tokio::test]
    async fn transaction_rolls_back_the_whole_script_on_error() {
        let pool = standalone_sqlite("rollback").await;
        sqlx::raw_sql("CREATE TABLE t (id INTEGER);")
            .execute(&pool)
            .await
            .unwrap();
        let db = DbPool::Sqlite(pool.clone());
        let (rows, committed) = execute_script_transaction(
            &db,
            &[
                "INSERT INTO t VALUES (1)".into(),
                "INSERT INTO t VALUES (2)".into(),
                "THIS IS NOT SQL".into(),
                "INSERT INTO t VALUES (3)".into(),
            ],
        )
        .await;
        assert!(!committed);
        assert!(rows[0].error.is_none() && rows[1].error.is_none());
        assert!(rows[2].error.is_some());
        assert!(rows[3].error.is_some());
        let count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM t")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
        pool.close().await;
    }

    #[tokio::test]
    async fn transaction_commits_all_on_success() {
        let pool = standalone_sqlite("commit").await;
        let db = DbPool::Sqlite(pool.clone());
        let (rows, committed) = execute_script_transaction(
            &db,
            &[
                "CREATE TABLE t (id INTEGER)".into(),
                "INSERT INTO t VALUES (1)".into(),
                "INSERT INTO t VALUES (2)".into(),
            ],
        )
        .await;
        assert!(committed);
        assert!(rows.iter().all(|row| row.error.is_none()));
        let count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM t")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 2);
        pool.close().await;
    }

    #[tokio::test]
    async fn read_script_preserves_wire_history_and_lease() {
        let harness = ScriptHarness::new().await;
        let receipt = harness
            .service
            .run_desktop(DesktopScriptRunRequest {
                connection_id: harness.connection_id,
                sql: "SELECT id FROM users ORDER BY id; SELECT name FROM users ORDER BY id".into(),
                approved: false,
                query_id: None,
                origin: Some("sql".into()),
            })
            .await
            .unwrap();
        assert!(receipt.outcome.all_reads);
        assert!(!receipt.outcome.committed);
        assert_eq!(receipt.outcome.statements.len(), 2);
        assert_eq!(
            serde_json::to_value(&receipt).unwrap(),
            serde_json::to_value(&receipt.outcome).unwrap(),
            "script receipt must preserve the literal legacy ScriptOutcome wire"
        );
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                harness.connections.begin_scope_mutation(),
            )
            .await
            .is_err(),
            "script receipt must retain authority through adapter serialization"
        );
        let history = harness
            .store
            .list_history(harness.connection_id)
            .await
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].origin, "sql");
        assert_eq!(history[0].status, "ok");
        assert_eq!(history[0].row_count, Some(4));
        assert_eq!(harness.audit_actions().await, ["script:execute"]);
        drop(receipt);
        let mutation = tokio::time::timeout(
            Duration::from_secs(5),
            harness.connections.begin_scope_mutation(),
        )
        .await
        .expect("scope mutation must proceed after script receipt drop");
        drop(mutation);
        harness.close().await;
    }

    #[tokio::test]
    async fn read_script_approval_gate_preserves_exact_block_contract() {
        let harness = ScriptHarness::new().await;
        harness.configure(false, false).await;
        let failure = match harness
            .service
            .run_desktop(DesktopScriptRunRequest {
                connection_id: harness.connection_id,
                sql: "SELECT id FROM users".into(),
                approved: false,
                query_id: None,
                origin: None,
            })
            .await
        {
            Err(DesktopScriptRunError::Scoped(failure)) => failure,
            _ => panic!("manual approval must remain required when auto-run reads are off"),
        };
        let error = failure.into_error();
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            serde_json::json!({
                "kind": "blocked",
                "message": "blocked: reads require approval for this connection"
            })
        );
        assert_eq!(harness.audit_actions().await, ["blocked"]);
        let history = harness
            .store
            .list_history(harness.connection_id)
            .await
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].status, "blocked");
        assert_eq!(history[0].origin, "manual");
        harness.close().await;
    }

    #[tokio::test]
    async fn write_script_gates_preserve_exact_errors_and_never_touch_target() {
        let harness = ScriptHarness::new().await;
        let writes_disabled = match harness
            .service
            .run_desktop(DesktopScriptRunRequest {
                connection_id: harness.connection_id,
                sql: "UPDATE users SET name = 'Grace' WHERE id = 1".into(),
                approved: true,
                query_id: None,
                origin: None,
            })
            .await
        {
            Err(error) => error.into_error(),
            Ok(_) => panic!("writes-disabled script must be rejected"),
        };
        assert_eq!(
            serde_json::to_value(&writes_disabled).unwrap(),
            serde_json::json!({
                "kind": "blocked",
                "message": "blocked: writing is disabled for this connection (writes are off by default). Enable writes in the connection's safety settings to run this script."
            })
        );

        harness.configure(true, true).await;
        let approval_required = match harness
            .service
            .run_desktop(DesktopScriptRunRequest {
                connection_id: harness.connection_id,
                sql: "UPDATE users SET name = 'Grace' WHERE id = 1".into(),
                approved: false,
                query_id: None,
                origin: Some("sql".into()),
            })
            .await
        {
            Err(error) => error.into_error(),
            Ok(_) => panic!("unapproved write script must be rejected"),
        };
        assert_eq!(
            serde_json::to_value(&approval_required).unwrap(),
            serde_json::json!({
                "kind": "blocked",
                "message": "blocked: this script modifies data and requires explicit approval"
            })
        );
        assert_eq!(harness.user_names().await, ["Ada", "Linus"]);
        assert_eq!(harness.audit_actions().await, ["blocked", "blocked"]);
        let history = harness
            .store
            .list_history(harness.connection_id)
            .await
            .unwrap();
        assert_eq!(history.len(), 2);
        assert!(history
            .iter()
            .any(|entry| entry.status == "blocked" && entry.origin == "manual"));
        assert!(history
            .iter()
            .any(|entry| entry.status == "blocked" && entry.origin == "sql"));
        harness.close().await;
    }

    #[tokio::test]
    async fn write_script_is_atomic_and_closes_attempt_ledger() {
        let harness = ScriptHarness::new().await;
        harness.configure(true, true).await;
        let receipt = harness
            .service
            .run_desktop(DesktopScriptRunRequest {
                connection_id: harness.connection_id,
                sql: "UPDATE users SET name = 'Grace' WHERE id = 1;\
                      UPDATE users SET name = 'Ken' WHERE id = 2"
                    .into(),
                approved: true,
                query_id: None,
                origin: Some("data-view".into()),
            })
            .await
            .unwrap();
        assert!(receipt.outcome.committed);
        assert!(!receipt.outcome.all_reads);
        assert_eq!(receipt.outcome.statements.len(), 2);
        drop(receipt);
        assert_eq!(harness.user_names().await, ["Grace", "Ken"]);
        assert_eq!(
            harness.audit_actions().await,
            ["script:execute:attempt", "script:execute"]
        );
        let history = harness
            .store
            .list_history(harness.connection_id)
            .await
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].origin, "data-view");
        assert_eq!(history[0].status, "ok");
        assert_eq!(history[0].row_count, Some(2));
        harness.close().await;
    }

    #[tokio::test]
    async fn committed_ddl_script_invalidates_schema_cache() {
        let harness = ScriptHarness::new().await;
        harness.configure(true, true).await;
        harness
            .store
            .set_schema_cache(harness.connection_id, r#"{"tables":[]}"#)
            .await
            .unwrap();
        let receipt = harness
            .service
            .run_desktop(DesktopScriptRunRequest {
                connection_id: harness.connection_id,
                sql: "CREATE TABLE widgets (id INTEGER PRIMARY KEY);\
                      INSERT INTO widgets (id) VALUES (1)"
                    .into(),
                approved: true,
                query_id: None,
                origin: None,
            })
            .await
            .unwrap();
        assert!(receipt.outcome.committed);
        drop(receipt);
        assert_eq!(
            harness
                .store
                .get_schema_cache(harness.connection_id)
                .await
                .unwrap(),
            None
        );
        let (audit, valid, first_bad) = audit::snapshot(&harness.store, harness.connection_id)
            .await
            .unwrap();
        assert!(valid);
        assert_eq!(first_bad, None);
        assert!(audit.iter().all(|entry| entry.kind == QueryKind::Ddl));
        harness.close().await;
    }

    #[tokio::test]
    async fn failed_write_script_rolls_back_and_returns_statement_outcomes() {
        let harness = ScriptHarness::new().await;
        harness.configure(true, true).await;
        let receipt = harness
            .service
            .run_desktop(DesktopScriptRunRequest {
                connection_id: harness.connection_id,
                sql: "UPDATE users SET name = 'Grace' WHERE id = 1;\
                      UPDATE missing_users SET name = 'Ken' WHERE id = 2;\
                      UPDATE users SET name = 'Dennis' WHERE id = 2"
                    .into(),
                approved: true,
                query_id: None,
                origin: None,
            })
            .await
            .unwrap();
        assert!(!receipt.outcome.committed);
        assert_eq!(receipt.outcome.statements.len(), 3);
        assert!(receipt.outcome.statements[0].error.is_none());
        assert!(receipt.outcome.statements[1]
            .error
            .as_deref()
            .is_some_and(|message| message.contains("missing_users")));
        assert_eq!(
            receipt.outcome.statements[2].error.as_deref(),
            Some("skipped — transaction rolled back")
        );
        drop(receipt);
        assert_eq!(harness.user_names().await, ["Ada", "Linus"]);
        assert_eq!(
            harness.audit_actions().await,
            ["script:execute:attempt", "script:execute"]
        );
        let history = harness
            .store
            .list_history(harness.connection_id)
            .await
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].status, "error");
        assert!(history[0]
            .error
            .as_deref()
            .is_some_and(|message| message.contains("missing_users")));
        harness.close().await;
    }

    #[tokio::test]
    async fn write_script_fails_closed_when_attempt_audit_is_unavailable() {
        let harness = ScriptHarness::new().await;
        harness.configure(true, true).await;
        sqlx::raw_sql(
            "CREATE TRIGGER fail_script_attempt
             BEFORE INSERT ON audit_log
             BEGIN
               SELECT RAISE(FAIL, 'forced script attempt audit failure');
             END;",
        )
        .execute(harness.store.pool())
        .await
        .unwrap();
        let error = match harness
            .service
            .run_desktop(DesktopScriptRunRequest {
                connection_id: harness.connection_id,
                sql: "UPDATE users SET name = 'Grace' WHERE id = 1".into(),
                approved: true,
                query_id: None,
                origin: None,
            })
            .await
        {
            Err(error) => error.into_error(),
            Ok(_) => {
                panic!("script must fail before target touch when attempt audit is unavailable")
            }
        };
        assert!(matches!(
            error,
            AppError::Config(message)
                if message.starts_with("audit pre-record failed — refusing to run script:")
                    && message.contains("forced script attempt audit failure")
        ));
        assert_eq!(harness.user_names().await, ["Ada", "Linus"]);
        assert!(harness.audit_actions().await.is_empty());
        assert!(harness
            .store
            .list_history(harness.connection_id)
            .await
            .unwrap()
            .is_empty());
        harness.close().await;
    }
}
