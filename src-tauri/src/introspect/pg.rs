//! Postgres introspection via `information_schema` + `pg_catalog`.

use std::collections::HashMap;
use std::fmt::Write as _;

use sqlx::{PgPool, Row};

use crate::error::{AppError, AppResult};

use super::{Catalog, Column, ForeignKey, Index, Table};

const COLS_SQL: &str = r#"
SELECT c.table_schema, c.table_name, c.column_name, c.data_type, c.is_nullable,
       COALESCE(pk.is_pk, false) AS is_pk
FROM information_schema.columns c
LEFT JOIN (
    SELECT tc.table_schema, tc.table_name, kcu.column_name, true AS is_pk
    FROM information_schema.table_constraints tc
    JOIN information_schema.key_column_usage kcu
      ON tc.constraint_name = kcu.constraint_name
     AND tc.table_schema = kcu.table_schema
    WHERE tc.constraint_type = 'PRIMARY KEY'
) pk ON pk.table_schema = c.table_schema
    AND pk.table_name = c.table_name
    AND pk.column_name = c.column_name
WHERE c.table_schema NOT IN ('pg_catalog', 'information_schema')
  -- Hide objects owned by an extension (e.g. pg_stat_statements) — they are noise in
  -- a table browser and some error on SELECT *.
  AND NOT EXISTS (
    SELECT 1 FROM pg_depend dep
    JOIN pg_class pc ON pc.oid = dep.objid
    JOIN pg_namespace pn ON pn.oid = pc.relnamespace
    WHERE dep.deptype = 'e'
      AND pn.nspname = c.table_schema
      AND pc.relname = c.table_name
  )
ORDER BY c.table_schema, c.table_name, c.ordinal_position
"#;

// Tables vs. views. information_schema.columns returns both, so classify per relation.
const KIND_SQL: &str = r#"
SELECT table_schema, table_name, table_type
FROM information_schema.tables
WHERE table_schema NOT IN ('pg_catalog', 'information_schema')
"#;

// FK edges resolved on pg_catalog so composite keys stay per-column-correct. Zipping
// conkey/confkey WITH ORDINALITY pairs each local column to the matching referenced
// column (the old key-name join produced NxN garbage for composite FKs and cross-joined
// same-named constraints across tables).
const FK_SQL: &str = r#"
SELECT cn.nspname   AS table_schema,
       cl.relname   AS table_name,
       att.attname  AS column_name,
       fn.nspname   AS foreign_schema,
       fcl.relname  AS foreign_table,
       fatt.attname AS foreign_column
FROM pg_constraint con
JOIN pg_class cl       ON cl.oid = con.conrelid
JOIN pg_namespace cn   ON cn.oid = cl.relnamespace
JOIN pg_class fcl      ON fcl.oid = con.confrelid
JOIN pg_namespace fn   ON fn.oid = fcl.relnamespace
JOIN LATERAL unnest(con.conkey, con.confkey) WITH ORDINALITY AS k(conkey, confkey, ord) ON true
JOIN pg_attribute att  ON att.attrelid = con.conrelid  AND att.attnum = k.conkey
JOIN pg_attribute fatt ON fatt.attrelid = con.confrelid AND fatt.attnum = k.confkey
WHERE con.contype = 'f'
  AND cn.nspname NOT IN ('pg_catalog', 'information_schema')
ORDER BY cn.nspname, cl.relname, con.conname, k.ord
"#;

// Secondary indexes (PK indexes excluded — the PK is already on the columns). Expression
// columns (indkey = 0) surface as "(expression)".
const IDX_SQL: &str = r#"
SELECT n.nspname AS table_schema,
       t.relname AS table_name,
       ic.relname AS index_name,
       i.indisunique AS is_unique,
       COALESCE(a.attname, '(expression)') AS column_name
FROM pg_index i
JOIN pg_class t      ON t.oid = i.indrelid
JOIN pg_class ic     ON ic.oid = i.indexrelid
JOIN pg_namespace n  ON n.oid = t.relnamespace
JOIN LATERAL unnest(i.indkey) WITH ORDINALITY AS k(attnum, ord) ON true
LEFT JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum
WHERE n.nspname NOT IN ('pg_catalog', 'information_schema')
  AND NOT i.indisprimary
ORDER BY n.nspname, t.relname, ic.relname, k.ord
"#;

const EST_SQL: &str = r#"
SELECT n.nspname AS table_schema, c.relname AS table_name, c.reltuples::bigint AS estimate
FROM pg_class c
JOIN pg_namespace n ON n.oid = c.relnamespace
WHERE c.relkind IN ('r', 'p')
  AND n.nspname NOT IN ('pg_catalog', 'information_schema')
"#;

pub async fn introspect(pool: &PgPool) -> AppResult<Catalog> {
    let mut tables: Vec<Table> = Vec::new();
    let mut idx: HashMap<(String, String), usize> = HashMap::new();

    for r in sqlx::query(COLS_SQL).fetch_all(pool).await? {
        let schema: String = r.try_get("table_schema")?;
        let name: String = r.try_get("table_name")?;
        let i = *idx
            .entry((schema.clone(), name.clone()))
            .or_insert_with(|| {
                tables.push(Table {
                    schema: Some(schema),
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
        tables[i].columns.push(Column {
            name: r.try_get("column_name")?,
            data_type: r.try_get("data_type")?,
            nullable: nullable.eq_ignore_ascii_case("YES"),
            pk: r.try_get("is_pk")?,
        });
    }

    for r in sqlx::query(KIND_SQL).fetch_all(pool).await? {
        let key: (String, String) = (r.try_get("table_schema")?, r.try_get("table_name")?);
        if let Some(&i) = idx.get(&key) {
            let ty: String = r.try_get("table_type")?;
            if ty.eq_ignore_ascii_case("VIEW") {
                tables[i].kind = "view".into();
            }
        }
    }

    for r in sqlx::query(FK_SQL).fetch_all(pool).await? {
        let key: (String, String) = (r.try_get("table_schema")?, r.try_get("table_name")?);
        if let Some(&i) = idx.get(&key) {
            tables[i].foreign_keys.push(ForeignKey {
                column: r.try_get("column_name")?,
                references_table: r.try_get("foreign_table")?,
                references_column: r.try_get("foreign_column")?,
                references_schema: r.try_get("foreign_schema").ok(),
            });
        }
    }

    // Group index rows (already ordered by table/index/position) into per-index columns.
    for r in sqlx::query(IDX_SQL).fetch_all(pool).await? {
        let key: (String, String) = (r.try_get("table_schema")?, r.try_get("table_name")?);
        let Some(&i) = idx.get(&key) else { continue };
        let iname: String = r.try_get("index_name")?;
        let col: String = r.try_get("column_name")?;
        let unique: bool = r.try_get("is_unique")?;
        let idxs = &mut tables[i].indexes;
        match idxs.last_mut() {
            Some(last) if last.name == iname => last.columns.push(col),
            _ => idxs.push(Index {
                name: iname,
                columns: vec![col],
                unique,
            }),
        }
    }

    for r in sqlx::query(EST_SQL).fetch_all(pool).await? {
        let key: (String, String) = (r.try_get("table_schema")?, r.try_get("table_name")?);
        if let Some(&i) = idx.get(&key) {
            // reltuples is -1 for a relation that has never been ANALYZEd (PG 14+);
            // treat any negative value as "unknown" so the UI shows nothing, not "~-1".
            tables[i].row_estimate = r.try_get::<i64, _>("estimate").ok().filter(|&n| n >= 0);
        }
    }

    Ok(Catalog { tables })
}

/// Synthesize CREATE TABLE + CREATE INDEX from the introspected catalog. This is a
/// best-effort reconstruction (types come from information_schema, composite FKs emit
/// one line per column), NOT a pg_dump-exact dump.
pub async fn table_ddl(pool: &PgPool, schema: Option<&str>, table: &str) -> AppResult<String> {
    let cat = introspect(pool).await?;
    let t = cat
        .tables
        .iter()
        .find(|t| t.name == table && schema.is_none_or(|s| t.schema.as_deref() == Some(s)))
        .ok_or_else(|| AppError::NotFound(format!("table {table}")))?;
    Ok(synthesize_ddl(t))
}

fn q(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

fn qualified(schema: Option<&str>, name: &str) -> String {
    match schema {
        Some(s) => format!("{}.{}", q(s), q(name)),
        None => q(name),
    }
}

fn synthesize_ddl(t: &Table) -> String {
    let full = qualified(t.schema.as_deref(), &t.name);
    let mut out = String::new();
    let _ = writeln!(out, "-- Synthesized from catalog (not pg_dump-exact).");
    let _ = writeln!(out, "CREATE TABLE {full} (");

    let mut lines: Vec<String> = t
        .columns
        .iter()
        .map(|c| {
            format!(
                "    {} {}{}",
                q(&c.name),
                c.data_type,
                if c.nullable { "" } else { " NOT NULL" }
            )
        })
        .collect();

    let pk: Vec<String> = t
        .columns
        .iter()
        .filter(|c| c.pk)
        .map(|c| q(&c.name))
        .collect();
    if !pk.is_empty() {
        lines.push(format!("    PRIMARY KEY ({})", pk.join(", ")));
    }
    for fk in &t.foreign_keys {
        lines.push(format!(
            "    FOREIGN KEY ({}) REFERENCES {} ({})",
            q(&fk.column),
            qualified(fk.references_schema.as_deref(), &fk.references_table),
            q(&fk.references_column),
        ));
    }
    out.push_str(&lines.join(",\n"));
    let _ = write!(out, "\n);");

    for i in &t.indexes {
        let cols = i
            .columns
            .iter()
            .map(|c| q(c))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = write!(
            out,
            "\n{} {} ON {} ({});",
            if i.unique {
                "CREATE UNIQUE INDEX"
            } else {
                "CREATE INDEX"
            },
            q(&i.name),
            full,
            cols,
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthesize_ddl_covers_pk_fk_index() {
        let t = Table {
            schema: Some("public".into()),
            name: "orders".into(),
            kind: "table".into(),
            columns: vec![
                Column {
                    name: "id".into(),
                    data_type: "integer".into(),
                    nullable: false,
                    pk: true,
                },
                Column {
                    name: "user_id".into(),
                    data_type: "integer".into(),
                    nullable: false,
                    pk: false,
                },
                Column {
                    name: "note".into(),
                    data_type: "text".into(),
                    nullable: true,
                    pk: false,
                },
            ],
            foreign_keys: vec![ForeignKey {
                column: "user_id".into(),
                references_table: "users".into(),
                references_column: "id".into(),
                references_schema: Some("public".into()),
            }],
            indexes: vec![Index {
                name: "idx_orders_user".into(),
                columns: vec!["user_id".into()],
                unique: false,
            }],
            row_estimate: None,
        };
        let ddl = synthesize_ddl(&t);
        assert!(ddl.contains("CREATE TABLE \"public\".\"orders\""));
        assert!(ddl.contains("\"id\" integer NOT NULL"));
        assert!(ddl.contains("\"note\" text\n") || ddl.contains("\"note\" text,"));
        assert!(ddl.contains("PRIMARY KEY (\"id\")"));
        assert!(ddl.contains("FOREIGN KEY (\"user_id\") REFERENCES \"public\".\"users\" (\"id\")"));
        assert!(ddl
            .contains("CREATE INDEX \"idx_orders_user\" ON \"public\".\"orders\" (\"user_id\");"));
    }
}
