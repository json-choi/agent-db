//! SQLite introspection via `sqlite_master` + `PRAGMA table_info/foreign_key_list`.
//! PRAGMA args can't be bound, so table names are interpolated with `"`-quoting
//! (identifiers come from `sqlite_master`, not user input, but we quote regardless).

use sqlx::{AssertSqlSafe, Row, SqlitePool};

use crate::error::{AppError, AppResult};

use super::{Catalog, Column, DatabaseObject, ForeignKey, Index, Table};

/// Quote an identifier for interpolation into a PRAGMA/COUNT statement.
fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

pub async fn introspect(pool: &SqlitePool) -> AppResult<Catalog> {
    // Both tables and views; keep the type so the sidebar can group them.
    let entries: Vec<(String, String)> = sqlx::query(
        "SELECT name, type FROM sqlite_master
         WHERE type IN ('table','view') AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| {
        Ok((
            r.try_get::<String, _>("name")?,
            r.try_get::<String, _>("type")?,
        ))
    })
    .collect::<AppResult<_>>()?;

    let mut tables = Vec::with_capacity(entries.len());
    for (name, ty) in entries {
        let q = quote_ident(&name);

        let mut columns = Vec::new();
        for r in sqlx::query(AssertSqlSafe(format!("PRAGMA table_info({q})")))
            .fetch_all(pool)
            .await?
        {
            let notnull: i64 = r.try_get("notnull")?;
            let pk: i64 = r.try_get("pk")?;
            columns.push(Column {
                name: r.try_get("name")?,
                data_type: r.try_get("type")?,
                nullable: notnull == 0,
                pk: pk > 0,
            });
        }

        let mut foreign_keys = Vec::new();
        for r in sqlx::query(AssertSqlSafe(format!("PRAGMA foreign_key_list({q})")))
            .fetch_all(pool)
            .await?
        {
            // `to` is NULL when the FK references the parent's primary key.
            let to: Option<String> = r.try_get("to")?;
            let ref_table: String = r.try_get("table")?;
            foreign_keys.push(ForeignKey {
                column: r.try_get("from")?,
                references_table: ref_table,
                references_column: to.unwrap_or_default(),
                references_schema: None,
            });
        }

        // Indexes: index_list gives name/unique/origin; index_info gives the columns.
        // origin 'pk' = the implicit primary-key index, dropped (PK is on the columns).
        let mut indexes = Vec::new();
        for r in sqlx::query(AssertSqlSafe(format!("PRAGMA index_list({q})")))
            .fetch_all(pool)
            .await?
        {
            let origin: String = r.try_get("origin")?;
            if origin == "pk" {
                continue;
            }
            let iname: String = r.try_get("name")?;
            let unique: i64 = r.try_get("unique")?;
            let iq = quote_ident(&iname);
            let mut cols = Vec::new();
            for ir in sqlx::query(AssertSqlSafe(format!("PRAGMA index_info({iq})")))
                .fetch_all(pool)
                .await?
            {
                // `name` is NULL for an expression column.
                let cn: Option<String> = ir.try_get("name")?;
                cols.push(cn.unwrap_or_else(|| "(expression)".into()));
            }
            indexes.push(Index {
                name: iname,
                columns: cols,
                unique: unique != 0,
            });
        }

        // ponytail: exact COUNT(*) — SQLite has no cheap planner estimate. Fine for
        // local files; upgrade to sampling only if someone opens a giant DB. Skipped for
        // views (a view COUNT can be arbitrarily expensive and is not a "row estimate").
        let row_estimate: Option<i64> = if ty == "view" {
            None
        } else {
            sqlx::query(AssertSqlSafe(format!("SELECT COUNT(*) AS n FROM {q}")))
                .fetch_one(pool)
                .await
                .ok()
                .and_then(|r| r.try_get::<i64, _>("n").ok())
        };

        tables.push(Table {
            schema: None,
            name,
            kind: if ty == "view" {
                "view".into()
            } else {
                "table".into()
            },
            columns,
            foreign_keys,
            indexes,
            row_estimate,
        });
    }

    let objects = sqlx::query(
        "SELECT name, tbl_name, sql FROM sqlite_master
         WHERE type = 'trigger' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|row| {
        Ok(DatabaseObject {
            schema: None,
            name: row.try_get("name")?,
            kind: "trigger".into(),
            detail: row.try_get("sql")?,
            parent: row.try_get("tbl_name")?,
        })
    })
    .collect::<AppResult<Vec<_>>>()?;

    Ok(Catalog { tables, objects })
}

/// The stored DDL for a table/view plus the DDL of its (non-auto) indexes, as SQLite
/// itself recorded them in `sqlite_master`.
pub async fn table_ddl(pool: &SqlitePool, table: &str) -> AppResult<String> {
    let sql: Option<String> = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE type IN ('table','view') AND name = ?1",
    )
    .bind(table)
    .fetch_optional(pool)
    .await?;
    let mut out = sql.ok_or_else(|| AppError::NotFound(format!("table {table}")))?;
    out.push(';');

    // Auto-created indexes (UNIQUE/PK constraints) have a NULL sql; skip them.
    let index_sql: Vec<String> = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE type = 'index' AND tbl_name = ?1 AND sql IS NOT NULL
         ORDER BY name",
    )
    .bind(table)
    .fetch_all(pool)
    .await?;
    for s in index_sql {
        out.push_str("\n\n");
        out.push_str(&s);
        out.push(';');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::SqlitePoolOptions;

    use super::*;

    #[tokio::test]
    async fn introspect_lists_tables_views_and_triggers() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT NOT NULL)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE VIEW active_users AS SELECT id, email FROM users")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TRIGGER users_email_guard
             BEFORE UPDATE OF email ON users
             BEGIN
               SELECT CASE WHEN NEW.email = '' THEN RAISE(ABORT, 'email required') END;
             END",
        )
        .execute(&pool)
        .await
        .unwrap();

        let catalog = introspect(&pool).await.unwrap();

        assert!(catalog
            .tables
            .iter()
            .any(|table| table.name == "users" && table.kind == "table"));
        assert!(catalog
            .tables
            .iter()
            .any(|table| table.name == "active_users" && table.kind == "view"));
        let trigger = catalog
            .objects
            .iter()
            .find(|object| object.kind == "trigger")
            .unwrap();
        assert_eq!(trigger.name, "users_email_guard");
        assert_eq!(trigger.parent.as_deref(), Some("users"));
        assert!(trigger
            .detail
            .as_deref()
            .is_some_and(|ddl| ddl.contains("BEFORE UPDATE")));
    }
}
