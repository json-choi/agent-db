//! MySQL/MariaDB introspection via `information_schema`. On PlanetScale/Vitess
//! FK metadata is unreliable (sharded), so `skip_fk` drops it.

use std::collections::HashMap;

use sqlx::mysql::MySqlRow;
use sqlx::{AssertSqlSafe, MySqlPool, Row};

use crate::error::{AppError, AppResult};

use super::{Catalog, Column, ForeignKey, Index, Table};

const COLS_SQL: &str = r#"
SELECT table_name, column_name, column_type, is_nullable, column_key
FROM information_schema.columns
WHERE table_schema = DATABASE()
ORDER BY table_name, ordinal_position
"#;

const FK_SQL: &str = r#"
SELECT table_name, column_name, referenced_table_name, referenced_column_name
FROM information_schema.key_column_usage
WHERE table_schema = DATABASE() AND referenced_table_name IS NOT NULL
"#;

// Secondary indexes (bulk equivalent of `SHOW INDEX` — one round trip). PRIMARY excluded.
const IDX_SQL: &str = r#"
SELECT table_name, index_name, non_unique, column_name
FROM information_schema.statistics
WHERE table_schema = DATABASE() AND index_name <> 'PRIMARY'
ORDER BY table_name, index_name, seq_in_index
"#;

// MySQL 8.0.13+ exposes the exact text for functional key parts in EXPRESSION.
// Older MySQL and MariaDB releases do not have that column, so introspect() falls
// back to IDX_SQL when this query is rejected.
const IDX_EXPR_SQL: &str = r#"
SELECT table_name, index_name, non_unique, column_name, `EXPRESSION` AS index_expression
FROM information_schema.statistics
WHERE table_schema = DATABASE() AND index_name <> 'PRIMARY'
ORDER BY table_name, index_name, seq_in_index
"#;

// table_type is 'BASE TABLE' or 'VIEW'; estimate is meaningful only for base tables.
const EST_SQL: &str = r#"
SELECT table_name, table_type, CAST(table_rows AS SIGNED) AS estimate
FROM information_schema.tables
WHERE table_schema = DATABASE()
"#;

pub async fn introspect(pool: &MySqlPool, skip_fk: bool) -> AppResult<Catalog> {
    let mut tables: Vec<Table> = Vec::new();
    let mut idx: HashMap<String, usize> = HashMap::new();

    for r in sqlx::query(COLS_SQL).fetch_all(pool).await? {
        let name: String = r.try_get("table_name")?;
        let i = *idx.entry(name.clone()).or_insert_with(|| {
            tables.push(Table {
                schema: None,
                name,
                kind: "table".into(),
                columns: Vec::new(),
                foreign_keys: Vec::new(),
                indexes: Vec::new(),
                row_estimate: None,
            });
            tables.len() - 1
        });
        let nullable: String = r.try_get("is_nullable")?;
        let key: String = r.try_get("column_key")?;
        tables[i].columns.push(Column {
            name: r.try_get("column_name")?,
            data_type: r.try_get("column_type")?,
            nullable: nullable.eq_ignore_ascii_case("YES"),
            pk: key == "PRI",
        });
    }

    if !skip_fk {
        for r in sqlx::query(FK_SQL).fetch_all(pool).await? {
            let name: String = r.try_get("table_name")?;
            if let Some(&i) = idx.get(&name) {
                tables[i].foreign_keys.push(ForeignKey {
                    column: r.try_get("column_name")?,
                    references_table: r.try_get("referenced_table_name")?,
                    references_column: r.try_get("referenced_column_name")?,
                    references_schema: None,
                });
            }
        }
    }

    // Group ordered rows into per-index column lists. Prefer the MySQL 8 query
    // that preserves functional expressions, but remain compatible with servers
    // whose information_schema.statistics predates the EXPRESSION column.
    let (index_rows, has_expression) = fetch_index_rows(pool).await?;
    for r in index_rows {
        let name: String = r.try_get("table_name")?;
        let Some(&i) = idx.get(&name) else { continue };
        let iname: String = r.try_get("index_name")?;
        let col: Option<String> = r.try_get("column_name")?;
        let expression = if has_expression {
            r.try_get::<Option<String>, _>("index_expression")?
        } else {
            None
        };
        let non_unique: i64 = r.try_get("non_unique")?;
        push_index_part(
            &mut tables[i].indexes,
            iname,
            col,
            expression,
            non_unique == 0,
        );
    }

    for r in sqlx::query(EST_SQL).fetch_all(pool).await? {
        let name: String = r.try_get("table_name")?;
        if let Some(&i) = idx.get(&name) {
            let ty: String = r.try_get("table_type")?;
            if ty.eq_ignore_ascii_case("VIEW") {
                tables[i].kind = "view".into();
            } else {
                tables[i].row_estimate = r
                    .try_get::<Option<i64>, _>("estimate")
                    .unwrap_or(None)
                    .filter(|&n| n >= 0);
            }
        }
    }

    Ok(Catalog { tables })
}

async fn fetch_index_rows(pool: &MySqlPool) -> Result<(Vec<MySqlRow>, bool), sqlx::Error> {
    match sqlx::query(IDX_EXPR_SQL).fetch_all(pool).await {
        Ok(rows) => Ok((rows, true)),
        Err(_) => sqlx::query(IDX_SQL)
            .fetch_all(pool)
            .await
            .map(|rows| (rows, false)),
    }
}

fn push_index_part(
    indexes: &mut Vec<Index>,
    name: String,
    column: Option<String>,
    expression: Option<String>,
    unique: bool,
) {
    let column = column
        .filter(|value| !value.is_empty())
        .or_else(|| expression.filter(|value| !value.is_empty()))
        .unwrap_or_else(|| "<expression>".into());
    match indexes.last_mut() {
        Some(last) if last.name == name => last.columns.push(column),
        _ => indexes.push(Index {
            name,
            columns: vec![column],
            unique,
        }),
    }
}

/// `SHOW CREATE TABLE` — the server's own, authoritative DDL. Also works for views
/// (returns `CREATE VIEW`). The table name is backtick-quoted (identifiers escaped).
pub async fn table_ddl(pool: &MySqlPool, table: &str) -> AppResult<String> {
    let quoted = format!("`{}`", table.replace('`', "``"));
    let row = sqlx::query(AssertSqlSafe(format!("SHOW CREATE TABLE {quoted}")))
        .fetch_one(pool)
        .await?;
    // Column 1 is "Create Table" (or "Create View"); fetch by index to cover both.
    row.try_get::<String, _>(1)
        .map_err(|e| AppError::NotFound(format!("no DDL for {table}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn functional_index_part_does_not_abort_index_grouping() {
        let mut indexes = Vec::new();

        push_index_part(
            &mut indexes,
            "mixed_idx".into(),
            Some("tenant_id".into()),
            None,
            false,
        );
        push_index_part(
            &mut indexes,
            "mixed_idx".into(),
            None,
            Some("lower(`email`)".into()),
            false,
        );

        assert_eq!(indexes.len(), 1);
        assert_eq!(indexes[0].name, "mixed_idx");
        assert_eq!(indexes[0].columns, ["tenant_id", "lower(`email`)"]);
        assert!(!indexes[0].unique);
    }

    #[test]
    fn missing_expression_metadata_uses_a_visible_placeholder() {
        let mut indexes = Vec::new();

        push_index_part(&mut indexes, "expr_idx".into(), None, None, true);

        assert_eq!(indexes[0].columns, ["<expression>"]);
        assert!(indexes[0].unique);
    }
}
