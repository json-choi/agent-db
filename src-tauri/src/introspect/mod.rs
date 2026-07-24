//! Schema introspection into a serde [`Catalog`]. Always reads through the
//! connection's READ-ONLY pool. The catalog backs `get_schema`/`get_table_ddl`
//! and the MCP `describe_table` tool.

mod catalog_v2;
mod mysql;
mod pg;
mod sqlite;

pub(crate) use catalog_v2::{
    load_cached_catalog, load_catalog, load_catalog_snapshot, CatalogReadMode,
};

use serde::{Deserialize, Serialize};

use crate::connection::{DbPool, Live};
use crate::error::{AppError, AppResult};

/// A relational column.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Column {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub pk: bool,
}

/// A foreign-key edge from one column to a referenced table column.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

/// A non-tabular database object shown in the explorer. Keeping these separate from
/// [`Table`] prevents routines, triggers, and sequences from accidentally flowing into
/// data reads, schema diffs, SQL completion, or MCP table tools.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseObject {
    /// Schema/namespace (None for single-schema engines).
    pub schema: Option<String>,
    pub name: String,
    /// "function" | "procedure" | "trigger" | "sequence" | "materialized_view".
    pub kind: String,
    /// Compact, non-secret metadata such as a routine signature or trigger event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Owning table for a trigger, when the engine exposes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

/// The introspected schema for one connection.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    pub tables: Vec<Table>,
    /// Added after the original schema-cache contract. Defaulting keeps old cached JSON
    /// readable and lets document databases return an empty object list.
    #[serde(default)]
    pub objects: Vec<DatabaseObject>,
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

/// The CREATE-TABLE DDL for one table, read through the read-only pool.
///
/// - MySQL: `SHOW CREATE TABLE` (server-authoritative).
/// - SQLite: the stored `sqlite_master.sql` for the table plus its indexes.
/// - Postgres: synthesized from the catalog (NOT pg_dump-exact — see `pg::table_ddl`).
pub(crate) async fn table_ddl(live: &Live, schema: Option<&str>, table: &str) -> AppResult<String> {
    match live {
        Live::Sql(live) => match live.ro() {
            DbPool::Postgres(pool) => pg::table_ddl(pool, schema, table).await,
            DbPool::Mysql(pool) => mysql::table_ddl(pool, table).await,
            DbPool::Sqlite(pool) => sqlite::table_ddl(pool, table).await,
        },
        Live::Mongo(_) => Err(AppError::Config(
            "MongoDB collections have no SQL DDL".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::Catalog;

    #[test]
    fn catalog_keeps_pre_object_cache_json_compatible() {
        let catalog: Catalog = serde_json::from_str(r#"{"tables":[]}"#).unwrap();

        assert!(catalog.tables.is_empty());
        assert!(catalog.objects.is_empty());
    }
}
