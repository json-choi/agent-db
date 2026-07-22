//! L1 — parse & classify. A **UX pre-filter only** (L2 is authoritative).
//!
//! Contract with the rest of the engine:
//! - `> 1` top-level statement → High risk, kind `Write` (stacked-injection guard).
//! - `Query` bodies are recursed for DML CTEs; any `INSERT`/`UPDATE` inside a CTE
//!   reclassifies the whole statement to `Write`.
//! - `UPDATE`/`DELETE` with `selection.is_none()` → `no_where` + High risk.
//! - **Any parse error or ambiguity → `Write` / High risk (fail safe), never an
//!   `Err`** — a swallowed statement is a data-loss bug, so we surface it as a
//!   write the gate can hard-stop.

use sqlparser::ast::{FromTable, Query, SetExpr, Statement, TableFactor, TableWithJoins};
use sqlparser::dialect::{Dialect, MySqlDialect, PostgreSqlDialect, SQLiteDialect};
use sqlparser::parser::Parser;

use crate::error::AppResult;
use crate::model::{Classification, Engine, QueryKind, RiskLevel};

fn dialect_for(engine: Engine) -> Option<Box<dyn Dialect>> {
    match engine {
        Engine::Postgres => Some(Box::new(PostgreSqlDialect {})),
        Engine::Mysql => Some(Box::new(MySqlDialect {})),
        Engine::Sqlite => Some(Box::new(SQLiteDialect {})),
        Engine::Mongodb => None,
    }
}

/// Fail-safe classification: treat as a High-risk write so the gate can stop it.
fn fail_safe(note: impl Into<String>) -> Classification {
    Classification {
        kind: QueryKind::Write,
        risk: RiskLevel::High,
        statement_count: 1,
        no_where: false,
        tables: Vec::new(),
        notes: vec![note.into()],
        rollback_safe: false,
    }
}

/// Classify one SQL string. Never returns `Err` for a *statement-level* problem
/// (those become fail-safe writes); the `AppResult` signature is kept so callers
/// have a uniform error channel for genuinely impossible states.
pub fn classify(sql: &str, engine: Engine) -> AppResult<Classification> {
    let Some(dialect) = dialect_for(engine) else {
        return Ok(fail_safe(
            "MongoDB document operations must use the typed document-query API",
        ));
    };
    let statements = match Parser::parse_sql(&*dialect, sql) {
        Ok(s) => s,
        Err(e) => {
            return Ok(fail_safe(format!(
                "parse error — treated as a write (fail-safe): {e}"
            )))
        }
    };

    if statements.is_empty() {
        return Ok(fail_safe(
            "no parseable statement — treated as a write (fail-safe)",
        ));
    }

    if statements.len() > 1 {
        let mut tables = Vec::new();
        for s in &statements {
            collect_tables(s, &mut tables);
        }
        dedup(&mut tables);
        return Ok(Classification {
            kind: QueryKind::Write,
            risk: RiskLevel::High,
            statement_count: statements.len() as u32,
            no_where: false,
            tables,
            notes: vec![format!(
                "{} statements found — only single statements are allowed",
                statements.len()
            )],
            rollback_safe: false,
        });
    }

    let stmt = &statements[0];
    let mut notes = Vec::new();
    let mut no_where = false;

    let kind = classify_stmt(stmt, &mut notes, &mut no_where);

    if no_where {
        notes.push("UPDATE/DELETE without a WHERE clause — affects every row".into());
    }

    let risk = match kind {
        QueryKind::Read => RiskLevel::Low,
        QueryKind::Write if no_where => RiskLevel::High,
        QueryKind::Write => RiskLevel::Medium,
        QueryKind::Ddl | QueryKind::Privilege => RiskLevel::High,
    };

    let mut tables = Vec::new();
    collect_tables(stmt, &mut tables);
    dedup(&mut tables);

    Ok(Classification {
        kind,
        risk,
        statement_count: 1,
        no_where,
        tables,
        notes,
        // Only direct DML has the transaction semantics required by L3's
        // execute-and-ROLLBACK preview. Utility statements and write-like query
        // forms stay gated but are never preview-executed.
        rollback_safe: matches!(
            stmt,
            Statement::Insert(_) | Statement::Update(_) | Statement::Delete(_)
        ),
    })
}

/// Recursive statement classification. Recurses for `EXPLAIN ANALYZE`, which
/// actually EXECUTES its inner statement, so it must inherit that statement's kind.
fn classify_stmt(stmt: &Statement, notes: &mut Vec<String>, no_where: &mut bool) -> QueryKind {
    match stmt {
        Statement::Query(q) => {
            if query_has_dml(q) {
                notes.push("write DML inside a CTE — reclassified as a write".into());
                QueryKind::Write
            } else if query_selects_into(q) {
                // SELECT ... INTO <table> creates and populates a table — not a read.
                notes.push("SELECT ... INTO creates a table — reclassified as a write".into());
                QueryKind::Write
            } else if !q.locks.is_empty() {
                // FOR UPDATE / FOR SHARE takes row locks (would fail on a read-only txn).
                notes.push(
                    "SELECT ... FOR UPDATE/SHARE takes row locks — reclassified as a write".into(),
                );
                QueryKind::Write
            } else {
                QueryKind::Read
            }
        }
        // Plain EXPLAIN just plans (Read); EXPLAIN ANALYZE runs the statement, so
        // classify by the boxed inner statement (EXPLAIN ANALYZE DELETE = Write/high).
        Statement::Explain {
            analyze, statement, ..
        } => {
            if *analyze {
                notes.push(
                    "EXPLAIN ANALYZE executes the statement — classified by its inner statement"
                        .into(),
                );
                classify_stmt(statement, notes, no_where)
            } else {
                QueryKind::Read
            }
        }

        Statement::Insert(_) => QueryKind::Write,
        Statement::Update(update) => {
            *no_where = update.selection.is_none();
            QueryKind::Write
        }
        Statement::Delete(del) => {
            *no_where = del.selection.is_none();
            QueryKind::Write
        }

        Statement::CreateTable(_)
        | Statement::CreateIndex(_)
        | Statement::CreateView { .. }
        | Statement::CreateSchema { .. }
        | Statement::CreateDatabase { .. }
        | Statement::AlterTable { .. }
        | Statement::Drop { .. }
        | Statement::Truncate { .. } => QueryKind::Ddl,

        Statement::Grant { .. } | Statement::Revoke { .. } => QueryKind::Privilege,

        // Unknown / unmodeled statement: fail safe to write so it is gated.
        other => {
            notes.push(format!(
                "unrecognized statement shape — treated as a write (fail-safe): {}",
                short_kind(other)
            ));
            QueryKind::Write
        }
    }
}

/// True for `SELECT ... INTO <table>` at the top-level select body.
fn query_selects_into(q: &Query) -> bool {
    matches!(&*q.body, SetExpr::Select(s) if s.into.is_some())
}

fn short_kind(stmt: &Statement) -> &'static str {
    match stmt {
        Statement::Query(_) => "Query",
        Statement::Insert(_) => "Insert",
        Statement::Update(_) => "Update",
        Statement::Delete(_) => "Delete",
        _ => "Other",
    }
}

// ---- DML-in-CTE detection -------------------------------------------------

fn query_has_dml(q: &Query) -> bool {
    if let Some(with) = &q.with {
        if with.cte_tables.iter().any(|cte| query_has_dml(&cte.query)) {
            return true;
        }
    }
    setexpr_has_dml(&q.body)
}

fn setexpr_has_dml(se: &SetExpr) -> bool {
    match se {
        // sqlparser wraps writable-CTE bodies as these variants.
        SetExpr::Insert(_) | SetExpr::Update(_) => true,
        SetExpr::Query(q) => query_has_dml(q),
        SetExpr::SetOperation { left, right, .. } => {
            setexpr_has_dml(left) || setexpr_has_dml(right)
        }
        _ => false,
    }
}

// ---- Table collection (best-effort; UX only) ------------------------------
//
// ponytail: walks the stable `TableFactor::Table` / `Derived` / CTE nodes only.
// Skips INSERT target tables and nested-join relations (deep, version-fragile
// AST shapes) — this list feeds the approval card, not any safety decision, so
// L2 stays authoritative regardless of what we miss here.

fn collect_tables(stmt: &Statement, out: &mut Vec<String>) {
    match stmt {
        Statement::Query(q) => walk_query(q, out),
        // Only the update target table; the optional `FROM` join sources are a
        // version-fragile AST shape and are UX-only, so we skip them.
        Statement::Update(update) => walk_twj(&update.table, out),
        Statement::Delete(del) => match &del.from {
            FromTable::WithFromKeyword(v) | FromTable::WithoutKeyword(v) => {
                for twj in v {
                    walk_twj(twj, out);
                }
            }
        },
        Statement::Insert(ins) => {
            if let Some(src) = &ins.source {
                walk_query(src, out);
            }
        }
        _ => {}
    }
}

fn walk_query(q: &Query, out: &mut Vec<String>) {
    if let Some(with) = &q.with {
        for cte in &with.cte_tables {
            walk_query(&cte.query, out);
        }
    }
    walk_setexpr(&q.body, out);
}

fn walk_setexpr(se: &SetExpr, out: &mut Vec<String>) {
    match se {
        SetExpr::Select(sel) => {
            for twj in &sel.from {
                walk_twj(twj, out);
            }
        }
        SetExpr::Query(q) => walk_query(q, out),
        SetExpr::SetOperation { left, right, .. } => {
            walk_setexpr(left, out);
            walk_setexpr(right, out);
        }
        SetExpr::Insert(stmt) | SetExpr::Update(stmt) => collect_tables(stmt, out),
        _ => {}
    }
}

fn walk_twj(twj: &TableWithJoins, out: &mut Vec<String>) {
    walk_tf(&twj.relation, out);
    for join in &twj.joins {
        walk_tf(&join.relation, out);
    }
}

fn walk_tf(tf: &TableFactor, out: &mut Vec<String>) {
    match tf {
        TableFactor::Table { name, .. } => out.push(name.to_string()),
        TableFactor::Derived { subquery, .. } => walk_query(subquery, out),
        _ => {}
    }
}

fn dedup(v: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    v.retain(|t| seen.insert(t.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(sql: &str) -> Classification {
        classify(sql, Engine::Postgres).unwrap()
    }

    #[test]
    fn select_is_read() {
        let r = c("SELECT id FROM users WHERE id = 1");
        assert_eq!(r.kind, QueryKind::Read);
        assert_eq!(r.risk, RiskLevel::Low);
        assert!(r.tables.contains(&"users".to_string()));
    }

    #[test]
    fn delete_without_where_is_high_risk_write() {
        let r = c("DELETE FROM orders");
        assert_eq!(r.kind, QueryKind::Write);
        assert!(r.no_where);
        assert_eq!(r.risk, RiskLevel::High);
    }

    #[test]
    fn update_with_where_is_medium() {
        let r = c("UPDATE users SET name = 'x' WHERE id = 1");
        assert!(!r.no_where);
        assert_eq!(r.risk, RiskLevel::Medium);
    }

    #[test]
    fn multi_statement_rejected() {
        let r = c("SELECT 1; DROP TABLE users");
        assert!(r.statement_count > 1);
        assert_eq!(r.risk, RiskLevel::High);
    }

    #[test]
    fn writable_cte_reclassified_as_write() {
        let r = c("WITH d AS (INSERT INTO log VALUES (1) RETURNING id) SELECT * FROM d");
        assert_eq!(r.kind, QueryKind::Write);
    }

    #[test]
    fn ddl_is_ddl() {
        assert_eq!(c("DROP TABLE users").kind, QueryKind::Ddl);
    }

    #[test]
    fn plain_explain_is_read() {
        assert_eq!(c("EXPLAIN SELECT * FROM users").kind, QueryKind::Read);
    }

    #[test]
    fn explain_analyze_delete_is_write() {
        // EXPLAIN ANALYZE actually runs the DELETE — must be a write, not a read.
        let r = c("EXPLAIN ANALYZE DELETE FROM orders");
        assert_eq!(r.kind, QueryKind::Write);
        assert!(r.no_where);
        assert_eq!(r.risk, RiskLevel::High);
    }

    #[test]
    fn select_into_is_not_read() {
        let r = c("SELECT * INTO backup FROM users");
        assert_ne!(r.kind, QueryKind::Read);
        assert_eq!(r.kind, QueryKind::Write);
    }

    #[test]
    fn select_for_update_is_write() {
        let r = c("SELECT id FROM users WHERE id = 1 FOR UPDATE");
        assert_eq!(r.kind, QueryKind::Write);
        assert_eq!(r.risk, RiskLevel::Medium);
    }

    #[test]
    fn garbage_fails_safe_to_write() {
        let r = c("this is not sql");
        assert_eq!(r.kind, QueryKind::Write);
        assert_eq!(r.risk, RiskLevel::High);
    }

    #[test]
    fn mongodb_is_rejected_by_the_sql_classifier() {
        let r = classify(r#"{ "find": "users" }"#, Engine::Mongodb).unwrap();
        assert_eq!(r.kind, QueryKind::Write);
        assert_eq!(r.risk, RiskLevel::High);
        assert!(r.notes[0].contains("typed document-query API"));
    }
}
