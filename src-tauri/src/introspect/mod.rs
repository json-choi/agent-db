//! Schema introspection into a serde [`Catalog`]. Always reads through the
//! connection's READ-ONLY pool. The catalog backs `get_schema`/`get_table_ddl`
//! and the MCP `describe_table` tool.

mod mysql;
mod pg;
mod sqlite;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::connection::{DbPool, Live};
use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// A relational column.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Column {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub pk: bool,
}

/// A foreign-key edge from one column to a referenced table column.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForeignKey {
    pub column: String,
    pub references_table: String,
    pub references_column: String,
    /// Schema of the referenced table (Some for Postgres cross-schema FKs; None otherwise).
    #[serde(default)]
    pub references_schema: Option<String>,
}

/// A secondary index on a table (primary-key indexes are excluded — the PK is already
/// carried on the columns).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Index {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
}

fn default_kind() -> String {
    "table".into()
}

/// A table (or view) with its columns, foreign keys, indexes, and a row-count estimate.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Table {
    /// Schema/namespace (None for SQLite / single-schema MySQL).
    pub schema: Option<String>,
    pub name: String,
    /// "table" | "view". `#[serde(default)]` so pre-existing schema-cache JSON (which
    /// predates this field) deserializes as a plain table.
    #[serde(default = "default_kind")]
    pub kind: String,
    pub columns: Vec<Column>,
    pub foreign_keys: Vec<ForeignKey>,
    #[serde(default)]
    pub indexes: Vec<Index>,
    /// Planner/statistics row estimate — not an exact count.
    pub row_estimate: Option<i64>,
}

/// The introspected schema for one connection.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    pub tables: Vec<Table>,
}

/// Introspect a live connection's schema. SQL engines read via the read-only
/// pool; MongoDB lists collections with sampled field structure.
pub async fn introspect(conn: &Live) -> AppResult<Catalog> {
    match conn {
        Live::Sql(live) => match live.ro() {
            DbPool::Postgres(pool) => pg::introspect(pool).await,
            DbPool::Mysql(pool) => mysql::introspect(pool, live.skip_fk_metadata).await,
            DbPool::Sqlite(pool) => sqlite::introspect(pool).await,
        },
        Live::Mongo(conn) => crate::mongo::introspect::introspect(conn).await,
    }
}

/// Get (opening/caching on first use) a live connection. Mirrors `commands::get_live` —
/// kept here because that helper is private to the commands module and this command
/// lives outside it.
async fn live_for(state: &AppState, id: Uuid) -> AppResult<Live> {
    if let Some(existing) = state.connections.lock().unwrap().get(&id) {
        return Ok(existing.clone());
    }
    let profile = state.store.get_connection(id).await?;
    let secret = crate::connection::fetch_secret(&id).unwrap_or_default();
    let live = crate::connection::connect(&profile, &secret).await?;
    let handle = live.clone();
    state.connections.lock().unwrap().insert(id, live);
    Ok(handle)
}

/// The CREATE-TABLE DDL for one table, read through the read-only pool.
///
/// - MySQL: `SHOW CREATE TABLE` (server-authoritative).
/// - SQLite: the stored `sqlite_master.sql` for the table plus its indexes.
/// - Postgres: synthesized from the catalog (NOT pg_dump-exact — see `pg::table_ddl`).
#[tauri::command]
pub async fn get_table_ddl(
    state: tauri::State<'_, AppState>,
    id: Uuid,
    schema: Option<String>,
    table: String,
) -> AppResult<String> {
    let live = live_for(&state, id).await?;
    match &live {
        Live::Sql(live) => match live.ro() {
            DbPool::Postgres(pool) => pg::table_ddl(pool, schema.as_deref(), &table).await,
            DbPool::Mysql(pool) => mysql::table_ddl(pool, &table).await,
            DbPool::Sqlite(pool) => sqlite::table_ddl(pool, &table).await,
        },
        Live::Mongo(_) => Err(AppError::Config(
            "MongoDB collections have no SQL DDL".into(),
        )),
    }
}
