//! MySQL/MariaDB introspection via `information_schema`. On PlanetScale/Vitess
//! FK metadata is unreliable (sharded), so `skip_fk` drops it (ARCHITECTURE §5.2).

use std::collections::HashMap;

use sqlx::{MySqlPool, Row};

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

    // Group ordered rows into per-index column lists.
    for r in sqlx::query(IDX_SQL).fetch_all(pool).await? {
        let name: String = r.try_get("table_name")?;
        let Some(&i) = idx.get(&name) else { continue };
        let iname: String = r.try_get("index_name")?;
        let col: String = r.try_get("column_name")?;
        let non_unique: i64 = r.try_get("non_unique")?;
        let idxs = &mut tables[i].indexes;
        match idxs.last_mut() {
            Some(last) if last.name == iname => last.columns.push(col),
            _ => idxs.push(Index { name: iname, columns: vec![col], unique: non_unique == 0 }),
        }
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

/// `SHOW CREATE TABLE` — the server's own, authoritative DDL. Also works for views
/// (returns `CREATE VIEW`). The table name is backtick-quoted (identifiers escaped).
pub async fn table_ddl(pool: &MySqlPool, table: &str) -> AppResult<String> {
    let quoted = format!("`{}`", table.replace('`', "``"));
    let row = sqlx::query(&format!("SHOW CREATE TABLE {quoted}"))
        .fetch_one(pool)
        .await?;
    // Column 1 is "Create Table" (or "Create View"); fetch by index to cover both.
    row.try_get::<String, _>(1)
        .map_err(|e| AppError::NotFound(format!("no DDL for {table}: {e}")))
}
